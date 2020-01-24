use crate::Krate;
use anyhow::{anyhow, Context, Error};
use reqwest::Client;
use std::convert::TryFrom;
use tame_gcs::{BucketName, ObjectName};

fn acquire_gcs_token(cred_path: &std::path::Path) -> Result<tame_oauth::Token, Error> {
    // If we're not completing whatever task in under an hour then
    // have more problems than the token expiring
    use tame_oauth::gcp;

    #[cfg(feature = "gcs")]
    log::debug!("using credentials in {}", cred_path.display());

    let svc_account_info =
        gcp::ServiceAccountInfo::deserialize(std::fs::read_to_string(&cred_path)?)
            .context("failed to deserilize service account")?;
    let svc_account_access = gcp::ServiceAccountAccess::new(svc_account_info)?;

    let token = match svc_account_access.get_token(&[tame_gcs::Scopes::ReadWrite])? {
        gcp::TokenOrRequest::Request {
            request,
            scope_hash,
            ..
        } => {
            let (parts, body) = request.into_parts();

            let client = reqwest::Client::new();

            let uri = parts.uri.to_string();

            let builder = match parts.method {
                http::Method::GET => client.get(&uri),
                http::Method::POST => client.post(&uri),
                http::Method::DELETE => client.delete(&uri),
                http::Method::PUT => client.put(&uri),
                method => unimplemented!("{} not implemented", method),
            };

            let req = builder
                .headers(parts.headers)
                .body(reqwest::Body::new(std::io::Cursor::new(body)))
                .build()?;

            let mut res = client.execute(req)?;

            let response = convert_response(&mut res)?;
            svc_account_access.parse_token_response(scope_hash, response)?
        }
        _ => unreachable!(),
    };

    Ok(token)
}

pub struct GcsBackend {
    client: Client,
    bucket: BucketName<'static>,
    prefix: String,
}

impl GcsBackend {
    pub fn new(loc: crate::GcsLocation<'_>, credentials: &std::path::Path) -> Result<Self, Error> {
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

        let client = Client::builder().default_headers(hm).gzip(false).build()?;

        Ok(Self {
            bucket,
            client,
            prefix: loc.prefix.to_owned(),
        })
    }

    #[inline]
    fn obj_name(&self, krate: &Krate) -> Result<ObjectName<'static>, Error> {
        Ok(ObjectName::try_from(format!(
            "{}{}",
            self.prefix,
            krate.cloud_id()
        ))?)
    }
}

#[async_trait::async_trait]
impl crate::Backend for GcsBackend {
    async fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error> {
        let dl_req =
            tame_gcs::objects::Object::download(&(&self.bucket, &self.obj_name(krate)?), None)?;

        let (parts, _) = dl_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;

        let mut response = self.client.execute(request).await?.error_for_status()?;
        let res = convert_response(&mut response).await?;
        let content = res.into_body();

        Ok(content)
    }

    async fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<(), Error> {
        use bytes::Buf;
        use tame_gcs::objects::{InsertObjectOptional, Object};

        let content_len = source.len() as u64;

        let insert_req = Object::insert_simple(
            &(&self.bucket, &self.obj_name(krate)?),
            source,
            content_len,
            Some(InsertObjectOptional {
                content_type: Some("application/x-tar"),
                ..Default::default()
            }),
        )?;

        let (parts, body) = insert_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.post(&uri);

        let request = builder.headers(parts.headers).body(body).build()?;

        self.client.execute(request).await?.error_for_status()?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        use tame_gcs::objects::{ListOptional, ListResponse, Object};

        // Get a list of all crates already present in gcs, the list
        // operation can return a maximum of 1000 entries per request,
        // so we may have to send multiple requests to determine all
        // of the available crates
        let mut names = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let ls_req = Object::list(
                &self.bucket,
                Some(ListOptional {
                    // We only care about a single directory
                    delimiter: Some("/"),
                    prefix: Some(&self.prefix),
                    page_token: page_token.as_ref().map(|s| s.as_ref()),
                    ..Default::default()
                }),
            )?;

            let (parts, _) = ls_req.into_parts();

            let uri = parts.uri.to_string();
            let builder = self.client.get(&uri);

            let request = builder.headers(parts.headers).build()?;

            let mut res = self.client.execute(request).await?;

            let response = convert_response(&mut res).await?;
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

    async fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        use tame_gcs::objects::{GetObjectOptional, GetObjectResponse, Object};

        let get_req = Object::get(
            &(&self.bucket, &self.obj_name(krate)?),
            Some(GetObjectOptional {
                standard_params: tame_gcs::common::StandardQueryParameters {
                    fields: Some("updated"),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )?;

        let (parts, _) = get_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;

        let mut response = self.client.execute(request).await?.error_for_status()?;

        let response = convert_response(&mut response).await?;
        let get_response = GetObjectResponse::try_from(response)?;

        Ok(get_response.metadata.updated)
    }

    fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_owned();
    }
}

pub async fn convert_response(
    res: &mut reqwest::Response,
) -> Result<http::Response<bytes::Bytes>, Error> {
    let mut builder = http::Response::builder();

    builder.status(res.status()).version(res.version());

    let headers = builder
        .headers_mut()
        .ok_or_else(|| anyhow!("failed to convert response headers"))?;

    headers.extend(
        res.headers()
            .into_iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );

    let body = res.bytes().await?;

    Ok(builder.body(body)?)
}
