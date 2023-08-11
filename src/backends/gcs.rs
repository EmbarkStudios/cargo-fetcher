use crate::{
    util::{self, send_request_with_retry},
    CloudId, HttpClient, Path,
};
use anyhow::{Context as _, Result};
use tame_gcs::{objects::Object, BucketName, ObjectName};
use tracing::debug;

fn acquire_gcs_token(cred_path: &Path) -> Result<tame_oauth::Token> {
    // If we're not completing whatever task in under an hour then we
    // have more problems than the token expiring
    use tame_oauth::gcp::{self, TokenProvider};

    #[cfg(feature = "gcs")]
    debug!("using credentials in {cred_path}");

    let svc_account_info =
        gcp::ServiceAccountInfo::deserialize(std::fs::read_to_string(cred_path)?)
            .context("failed to deserilize service account")?;
    let svc_account_access = gcp::ServiceAccountProvider::new(svc_account_info)?;

    let token = match svc_account_access.get_token(&[tame_gcs::Scopes::ReadWrite])? {
        gcp::TokenOrRequest::Request {
            request,
            scope_hash,
            ..
        } => {
            let (parts, body) = request.into_parts();

            let client = reqwest::blocking::Client::new();

            let uri = parts.uri.to_string();

            let builder = match parts.method {
                http::Method::GET => client.get(&uri),
                http::Method::POST => client.post(&uri),
                http::Method::DELETE => client.delete(&uri),
                http::Method::PUT => client.put(&uri),
                method => unreachable!("{method} not implemented"),
            };

            let req = builder.headers(parts.headers).body(body).build()?;
            let res = client.execute(req)?;

            let mut builder = http::Response::builder()
                .status(res.status())
                .version(res.version());

            let headers = builder
                .headers_mut()
                .context("failed to convert response headers")?;

            headers.extend(
                res.headers()
                    .into_iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );

            let body = res.bytes()?;
            let response = builder.body(body)?;

            svc_account_access.parse_token_response(scope_hash, response)?
        }
        gcp::TokenOrRequest::Token(_) => unreachable!(),
    };

    Ok(token)
}

pub struct GcsBackend {
    client: HttpClient,
    bucket: BucketName<'static>,
    prefix: String,
    obj: Object,
}

impl GcsBackend {
    pub fn new(
        loc: crate::GcsLocation<'_>,
        credentials: &Path,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        let bucket = BucketName::try_from(loc.bucket.to_owned())?;

        let token = acquire_gcs_token(credentials)?;

        use reqwest::header;

        let hm = {
            let mut hm = header::HeaderMap::new();
            hm.insert(
                header::AUTHORIZATION,
                <tame_oauth::Token as std::convert::TryInto<header::HeaderValue>>::try_into(token)?,
            );
            hm
        };

        let client = HttpClient::builder()
            .default_headers(hm)
            .use_rustls_tls()
            .timeout(timeout)
            .build()?;

        Ok(Self {
            bucket,
            client,
            prefix: loc.prefix.to_owned(),
            obj: Object::default(),
        })
    }

    #[inline]
    fn obj_name(&self, id: CloudId<'_>) -> Result<ObjectName<'static>> {
        Ok(ObjectName::try_from(format!("{}{id}", self.prefix))?)
    }
}

use std::fmt;

impl fmt::Debug for GcsBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("gcs")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish()
    }
}

#[async_trait::async_trait]
impl crate::Backend for GcsBackend {
    async fn fetch(&self, id: CloudId<'_>) -> Result<bytes::Bytes> {
        let dl_req = self
            .obj
            .download(&(&self.bucket, &self.obj_name(id)?), None)?;

        let content = send_request_with_retry(&self.client, util::convert_request(dl_req))
            .await?
            .error_for_status()?
            .bytes()
            .await?;

        Ok(content)
    }

    async fn upload(&self, source: bytes::Bytes, id: CloudId<'_>) -> Result<usize> {
        use tame_gcs::objects::InsertObjectOptional;

        let content_len = source.len() as u64;

        let insert_req = self.obj.insert_simple(
            &(&self.bucket, &self.obj_name(id)?),
            source,
            content_len,
            Some(InsertObjectOptional {
                content_type: Some("application/x-tar"),
                ..Default::default()
            }),
        )?;

        send_request_with_retry(&self.client, insert_req.try_into()?)
            .await?
            .error_for_status()?;

        Ok(content_len as usize)
    }

    async fn list(&self) -> Result<Vec<String>> {
        use tame_gcs::objects::{ListOptional, ListResponse};

        // Get a list of all crates already present in gcs, the list
        // operation can return a maximum of 1000 entries per request,
        // so we may have to send multiple requests to determine all
        // of the available crates
        let mut names = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let ls_req = self.obj.list(
                &self.bucket,
                Some(ListOptional {
                    // We only care about a single directory
                    delimiter: Some("/"),
                    prefix: Some(&self.prefix),
                    page_token: page_token.as_ref().map(|s| s.as_ref()),
                    ..Default::default()
                }),
            )?;

            let response = util::convert_response(
                send_request_with_retry(&self.client, util::convert_request(ls_req)).await?,
            )
            .await?;
            let list_response = ListResponse::try_from(response)?;

            let name_block: Vec<_> = list_response
                .objects
                .into_iter()
                .filter_map(|obj| obj.name)
                .collect();
            names.push(name_block);

            page_token = list_response.page_token;

            if page_token.is_none() {
                break;
            }
        }

        let len = self.prefix.len();

        Ok(names
            .into_iter()
            .flat_map(|v| v.into_iter().map(|p| p[len..].to_owned()))
            .collect())
    }

    async fn updated(&self, id: CloudId<'_>) -> Result<Option<crate::Timestamp>> {
        use tame_gcs::objects::{GetObjectOptional, GetObjectResponse};

        let get_req = self.obj.get(
            &(&self.bucket, &self.obj_name(id)?),
            Some(GetObjectOptional {
                standard_params: tame_gcs::common::StandardQueryParameters {
                    fields: Some("updated"),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )?;

        let response = util::convert_response(
            send_request_with_retry(&self.client, util::convert_request(get_req)).await?,
        )
        .await?;
        let get_response = GetObjectResponse::try_from(response)?;

        Ok(get_response.metadata.updated)
    }
}
