use std::fmt;

use crate::key_value::Section;

// Constants

pub const S3_DEFAULT_REGION: &str = "us-east-1";
const HTTP_PORT: u16 = 80;
const HTTPS_PORT: u16 = 443;

// Types

#[derive(Clone, PartialEq, Eq, Default)]
pub struct S3Options {
    pub bucket: String,
    pub region: String,
    pub path_style: bool,
    pub plain_http: bool,
    pub session_token: Option<String>,
}

// Settings

impl S3Options {
    pub fn region_or_default(&self) -> &str {
        let region = self.region.trim();
        if region.is_empty() {
            S3_DEFAULT_REGION
        } else {
            region
        }
    }

    pub fn default_endpoint(&self) -> String {
        format!("s3.{}.amazonaws.com", self.region_or_default())
    }

    pub fn default_port(&self) -> u16 {
        if self.plain_http {
            HTTP_PORT
        } else {
            HTTPS_PORT
        }
    }

    pub fn scheme(&self) -> &'static str {
        if self.plain_http { "http" } else { "https" }
    }

    pub(crate) fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("s3_bucket", self.bucket.clone()),
            ("s3_region", self.region.clone()),
            ("s3_path_style", self.path_style.to_string()),
            ("s3_http", self.plain_http.to_string()),
        ]
    }

    pub(crate) fn from_section(section: &Section) -> Self {
        let text = |key: &str| section.get(key).unwrap_or_default().trim().to_string();
        Self {
            bucket: text("s3_bucket"),
            region: text("s3_region"),
            path_style: section.get("s3_path_style") == Some("true"),
            plain_http: section.get("s3_http") == Some("true"),
            session_token: None,
        }
    }
}

pub fn is_valid_bucket(name: &str) -> bool {
    (3..=255).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

impl fmt::Debug for S3Options {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("S3Options")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("path_style", &self.path_style)
            .field("plain_http", &self.plain_http)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_value::{parse_sections, write};

    #[test]
    fn settings_round_trip_without_the_session_token() {
        let token = crate::test_support::test_password();
        let options = S3Options {
            bucket: "demo-bucket".to_string(),
            region: "eu-central-1".to_string(),
            path_style: true,
            plain_http: true,
            session_token: Some(token.clone()),
        };
        let text = format!("[site]\n{}", write(&options.pairs()));
        assert!(!text.contains(&token));
        let section = parse_sections(&text).pop().expect("no section");
        let restored = S3Options::from_section(&section);
        assert_eq!(
            restored,
            S3Options {
                session_token: None,
                ..options.clone()
            }
        );
        assert!(!format!("{options:?}").contains(&token));
    }

    #[test]
    fn defaults_point_to_aws() {
        let options = S3Options::default();
        assert_eq!(options.region_or_default(), "us-east-1");
        assert_eq!(options.default_endpoint(), "s3.us-east-1.amazonaws.com");
        assert_eq!(options.default_port(), 443);
        assert_eq!(options.scheme(), "https");
        let local = S3Options {
            region: " eu-west-1 ".to_string(),
            plain_http: true,
            ..S3Options::default()
        };
        assert_eq!(local.default_endpoint(), "s3.eu-west-1.amazonaws.com");
        assert_eq!(local.default_port(), 80);
    }

    #[test]
    fn bucket_names_are_checked() {
        for name in ["demo", "my.bucket-1", "Legacy_Bucket"] {
            assert!(is_valid_bucket(name), "{name}");
        }
        for name in ["", "ab", "a/b", "a b", "bücket"] {
            assert!(!is_valid_bucket(name), "{name}");
        }
    }
}
