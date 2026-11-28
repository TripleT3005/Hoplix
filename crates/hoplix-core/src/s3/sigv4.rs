use std::fmt;

use ring::{digest, hmac};

use crate::key_value::to_hex;

// Constants

pub(crate) const ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub(crate) const SERVICE: &str = "s3";
pub(crate) const EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const TERMINATOR: &str = "aws4_request";

// Types

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Scope<'a> {
    pub date: &'a str,
    pub region: &'a str,
    pub service: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CanonicalRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [(String, String)],
    pub headers: &'a [(String, String)],
    pub payload_hash: &'a str,
}

// Credentials

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credentials(***)")
    }
}

// Encoding

pub(crate) fn uri_encode(text: &str, keep_slash: bool) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub(crate) fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(name, value)| (uri_encode(name, false), uri_encode(value, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    to_hex(digest::digest(&digest::SHA256, data).as_ref())
}

// Canonical requests

impl CanonicalRequest<'_> {
    fn sorted_headers(&self) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = Vec::new();
        for (name, value) in self.headers {
            let name = name.trim().to_ascii_lowercase();
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            match headers.iter_mut().find(|(existing, _)| *existing == name) {
                Some((_, existing)) => {
                    existing.push(',');
                    existing.push_str(&value);
                }
                None => headers.push((name, value)),
            }
        }
        headers.sort();
        headers
    }

    pub(crate) fn signed_headers(&self) -> String {
        self.sorted_headers()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
            .join(";")
    }

    pub(crate) fn text(&self) -> String {
        let headers: String = self
            .sorted_headers()
            .iter()
            .map(|(name, value)| format!("{name}:{value}\n"))
            .collect();
        format!(
            "{}\n{}\n{}\n{headers}\n{}\n{}",
            self.method,
            self.path,
            canonical_query(self.query),
            self.signed_headers(),
            self.payload_hash
        )
    }
}

// Signing

impl Scope<'_> {
    pub(crate) fn text(&self) -> String {
        format!(
            "{}/{}/{}/{TERMINATOR}",
            self.date, self.region, self.service
        )
    }
}

pub(crate) fn signing_key(secret_access_key: &str, scope: &Scope<'_>) -> hmac::Key {
    let step = |key: &[u8], data: &str| {
        let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes());
        tag.as_ref().to_vec()
    };
    let date = step(format!("AWS4{secret_access_key}").as_bytes(), scope.date);
    let region = step(&date, scope.region);
    let service = step(&region, scope.service);
    let signing = step(&service, TERMINATOR);
    hmac::Key::new(hmac::HMAC_SHA256, &signing)
}

pub(crate) fn string_to_sign(timestamp: &str, scope: &Scope<'_>, canonical: &str) -> String {
    format!(
        "{ALGORITHM}\n{timestamp}\n{}\n{}",
        scope.text(),
        sha256_hex(canonical.as_bytes())
    )
}

pub(crate) fn signature(
    secret_access_key: &str,
    timestamp: &str,
    scope: &Scope<'_>,
    request: &CanonicalRequest<'_>,
) -> String {
    let text = string_to_sign(timestamp, scope, &request.text());
    let key = signing_key(secret_access_key, scope);
    to_hex(hmac::sign(&key, text.as_bytes()).as_ref())
}

pub(crate) fn authorization(
    credentials: &Credentials,
    timestamp: &str,
    scope: &Scope<'_>,
    request: &CanonicalRequest<'_>,
) -> String {
    format!(
        "{ALGORITHM} Credential={}/{}, SignedHeaders={}, Signature={}",
        credentials.access_key_id,
        scope.text(),
        request.signed_headers(),
        signature(&credentials.secret_access_key, timestamp, scope, request)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_value::{Section, parse_sections};

    struct Vector {
        name: String,
        secret: String,
        key_id: String,
        timestamp: String,
        region: String,
        service: String,
        method: String,
        path: String,
        query: Vec<(String, String)>,
        headers: Vec<(String, String)>,
        payload: String,
        signed_headers: String,
        canonical_hash: Option<String>,
        signature: String,
        authorization: Option<String>,
    }

    fn vectors() -> Vec<Vector> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/s3/sigv4.conf");
        let text = std::fs::read_to_string(path).expect("the fixture is missing");
        parse_sections(&text)
            .into_iter()
            .filter(|section| !section.name.is_empty())
            .map(|section| vector(&section))
            .collect()
    }

    fn vector(section: &Section) -> Vector {
        let field = |key: &str| section.get(key).unwrap_or_default().to_string();
        let pairs = |key: &str, separator: char| -> Vec<(String, String)> {
            section
                .pairs
                .iter()
                .filter(|(name, _)| name == key)
                .filter_map(|(_, value)| value.split_once(separator))
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect()
        };
        Vector {
            name: section.name.clone(),
            secret: field("signing_secret"),
            key_id: field("key_id"),
            timestamp: field("timestamp"),
            region: field("region"),
            service: field("service"),
            method: field("method"),
            path: field("path"),
            query: pairs("query", '='),
            headers: pairs("header", ':'),
            payload: field("payload"),
            signed_headers: field("signed_headers"),
            canonical_hash: section.get("canonical_hash").map(str::to_string),
            signature: field("signature"),
            authorization: section.get("authorization").map(str::to_string),
        }
    }

    #[test]
    fn official_vectors_produce_the_published_signatures() {
        let vectors = vectors();
        assert!(vectors.len() >= 6);
        for vector in vectors {
            let payload_hash = sha256_hex(vector.payload.as_bytes());
            let request = CanonicalRequest {
                method: &vector.method,
                path: &vector.path,
                query: &vector.query,
                headers: &vector.headers,
                payload_hash: &payload_hash,
            };
            let scope = Scope {
                date: &vector.timestamp[..8],
                region: &vector.region,
                service: &vector.service,
            };
            assert_eq!(
                request.signed_headers(),
                vector.signed_headers,
                "{}",
                vector.name
            );
            if let Some(hash) = &vector.canonical_hash {
                assert_eq!(
                    &sha256_hex(request.text().as_bytes()),
                    hash,
                    "{}",
                    vector.name
                );
            }
            assert_eq!(
                signature(&vector.secret, &vector.timestamp, &scope, &request),
                vector.signature,
                "{}",
                vector.name
            );
            if let Some(expected) = &vector.authorization {
                let credentials = Credentials {
                    access_key_id: vector.key_id.clone(),
                    secret_access_key: vector.secret.clone(),
                    session_token: None,
                };
                assert_eq!(
                    &authorization(&credentials, &vector.timestamp, &scope, &request),
                    expected,
                    "{}",
                    vector.name
                );
            }
        }
    }

    #[test]
    fn signing_keys_match_the_documented_derivation() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/s3/sigv4.conf");
        let text = std::fs::read_to_string(path).expect("the fixture is missing");
        let sections = parse_sections(&text);
        let derivation = sections
            .iter()
            .find(|section| section.name.is_empty())
            .expect("no derivation");
        let field = |key: &str| derivation.get(key).unwrap_or_default();
        let scope = Scope {
            date: field("derivation_date"),
            region: field("derivation_region"),
            service: field("derivation_service"),
        };
        let key = signing_key(field("derivation_secret"), &scope);
        let probe = hmac::sign(&key, b"probe");
        let expected = crate::key_value::from_hex(field("derivation_key")).expect("hex");
        let reference = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &expected), b"probe");
        assert_eq!(probe.as_ref(), reference.as_ref());
    }

    #[test]
    fn uri_encoding_follows_the_s3_rules() {
        assert_eq!(uri_encode("a b/c~d_e-f.g", true), "a%20b/c~d_e-f.g");
        assert_eq!(uri_encode("a b/c", false), "a%20b%2Fc");
        assert_eq!(uri_encode("Grüße+$&=", true), "Gr%C3%BC%C3%9Fe%2B%24%26%3D");
        assert_eq!(uri_encode("*", true), "%2A");
    }

    #[test]
    fn queries_are_encoded_and_sorted() {
        let query = vec![
            ("prefix".to_string(), "a b/".to_string()),
            ("list-type".to_string(), "2".to_string()),
            ("delimiter".to_string(), "/".to_string()),
            ("uploads".to_string(), String::new()),
        ];
        assert_eq!(
            canonical_query(&query),
            "delimiter=%2F&list-type=2&prefix=a%20b%2F&uploads="
        );
        assert_eq!(canonical_query(&[]), "");
    }

    #[test]
    fn headers_are_lowercased_trimmed_and_merged() {
        let headers = vec![
            ("X-Amz-Meta-B".to_string(), "  two   words ".to_string()),
            ("Host".to_string(), "example.test".to_string()),
            ("x-amz-meta-b".to_string(), "second".to_string()),
        ];
        let request = CanonicalRequest {
            method: "GET",
            path: "/",
            query: &[],
            headers: &headers,
            payload_hash: EMPTY_SHA256,
        };
        assert_eq!(request.signed_headers(), "host;x-amz-meta-b");
        assert_eq!(
            request.text(),
            format!(
                "GET\n/\n\nhost:example.test\nx-amz-meta-b:two words,second\n\nhost;x-amz-meta-b\n{EMPTY_SHA256}"
            )
        );
    }

    #[test]
    fn debug_output_hides_the_credentials() {
        let secret = crate::test_support::test_password();
        let credentials = Credentials {
            access_key_id: "demo".to_string(),
            secret_access_key: secret.clone(),
            session_token: Some(secret.clone()),
        };
        let output = format!("{credentials:?}");
        assert!(!output.contains(&secret) && !output.contains("demo"));
    }
}
