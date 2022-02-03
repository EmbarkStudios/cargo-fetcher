mod download;
mod insert;
mod list;
mod properties;

use anyhow::Error;
use std::fmt;

pub use list::parse_list_body;
pub use list::EnumerationResults;

pub struct PropertiesResponse {
    pub last_modified: String,
}

pub struct Blob {
    account: String,
    key: String,
    container: String,
    version_value: String,
    azurite: bool,
}

impl Blob {
    pub fn new(account: &str, key: &str, container: &str, azurite: bool) -> Self {
        Self {
            account: account.to_owned(),
            key: key.to_owned(),
            container: container.to_owned(),
            version_value: String::from("2015-02-21"),
            azurite,
        }
    }

    fn container_uri(&self) -> String {
        if self.azurite {
            format!("http://127.0.0.1:10000/{}/{}", self.account, self.container)
        } else {
            format!(
                "https://{}.blob.core.windows.net/{}",
                self.account, self.container
            )
        }
    }

    fn sign(
        &self,
        action: &Actions,
        path: &str,
        time_str: &str,
        content_length: usize,
    ) -> Result<String, Error> {
        let string_to_sign = prepare_to_sign(
            &self.account,
            path,
            action,
            time_str,
            content_length,
            &self.version_value,
        );

        hmacsha256(&self.key, &string_to_sign)
    }
}

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blob: {:#?}", self)
    }
}

enum Actions {
    Download,
    Insert,
    Properties,
    List,
}

impl From<&Actions> for http::Method {
    fn from(action: &Actions) -> Self {
        match action {
            Actions::Download | Actions::List => http::Method::GET,
            Actions::Insert => http::Method::PUT,
            Actions::Properties => http::Method::HEAD,
        }
    }
}

pub fn hmacsha256(key: &str, string_to_sign: &str) -> Result<String, Error> {
    use ring::hmac;

    let key_bytes = base64::decode(key)?;

    let key = hmac::Key::new(hmac::HMAC_SHA256, &key_bytes);
    let tag = hmac::sign(&key, string_to_sign.as_bytes());

    Ok(base64::encode(tag.as_ref()))
}

fn prepare_to_sign(
    account: &str,
    path: &str,
    action: &Actions,
    time_str: &str,
    content_length: usize,
    version_value: &str,
) -> String {
    {
        let content_encoding = "";
        let content_language = "";
        let content_length = {
            if content_length == 0 {
                String::from("")
            } else {
                content_length.to_string()
            }
        };
        let content_md5 = "";
        let content_type = "";
        let date = "";
        let if_modified_since = "";
        let if_match = "";
        let if_none_match = "";
        let if_unmodified_since = "";
        let range = "";
        let canonicalized_headers = match action {
            Actions::Properties => {
                format!("x-ms-date:{}\nx-ms-version:{}", time_str, version_value)
            }
            _ => format!(
                "x-ms-blob-type:{}\nx-ms-date:{}\nx-ms-version:{}",
                "BlockBlob", time_str, version_value
            ),
        };
        // let canonicalized_headers =
        //     format!("x-ms-date:{}\nx-ms-version:{}", time_str, version_value);
        let verb = http::Method::from(action).to_string();
        let canonicalized_resource = match action {
            Actions::List => format!("/{}{}\ncomp:list\nrestype:container", account, path),
            _ => format!("/{}{}", account, path),
        };
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            verb,
            content_encoding,
            content_language,
            content_length,
            content_md5,
            content_type,
            date,
            if_modified_since,
            if_match,
            if_none_match,
            if_unmodified_since,
            range,
            canonicalized_headers,
            canonicalized_resource,
        )
    }
}
