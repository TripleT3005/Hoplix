use std::fmt;

use ring::rand::{SecureRandom, SystemRandom};

use crate::key_value::to_hex;
use crate::proxy::base64;
use crate::{Error, ErrorKind, HashAlgorithm, Result};

// Types

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Challenge {
    pub scheme: String,
    pub params: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algorithm {
    Md5,
    Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Digest {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    algorithm: Algorithm,
    session: bool,
    qop: bool,
    count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum State {
    #[default]
    None,
    Basic,
    Digest(Digest),
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

// Challenges

impl Challenge {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

pub(crate) fn parse_challenges<'a>(values: impl Iterator<Item = &'a str>) -> Vec<Challenge> {
    let mut challenges = Vec::new();
    for value in values {
        parse_into(value, &mut challenges);
    }
    challenges
}

fn parse_into(value: &str, challenges: &mut Vec<Challenge>) {
    let chars: Vec<char> = value.chars().collect();
    let mut index = 0;
    let skip_space = |index: &mut usize| {
        while chars.get(*index).is_some_and(|c| c.is_whitespace()) {
            *index += 1;
        }
    };
    loop {
        while chars
            .get(index)
            .is_some_and(|c| c.is_whitespace() || *c == ',')
        {
            index += 1;
        }
        if index >= chars.len() {
            return;
        }
        let start = index;
        while chars
            .get(index)
            .is_some_and(|c| !c.is_whitespace() && *c != '=' && *c != ',')
        {
            index += 1;
        }
        let token: String = chars[start..index].iter().collect();
        skip_space(&mut index);
        if chars.get(index) != Some(&'=') {
            challenges.push(Challenge {
                scheme: token.to_ascii_lowercase(),
                params: Vec::new(),
            });
            continue;
        }
        index += 1;
        skip_space(&mut index);
        let mut text = String::new();
        if chars.get(index) == Some(&'"') {
            index += 1;
            while let Some(&c) = chars.get(index) {
                index += 1;
                match c {
                    '"' => break,
                    '\\' => {
                        if let Some(&escaped) = chars.get(index) {
                            text.push(escaped);
                            index += 1;
                        }
                    }
                    _ => text.push(c),
                }
            }
        } else {
            while chars
                .get(index)
                .is_some_and(|c| !c.is_whitespace() && *c != ',')
            {
                text.push(chars[index]);
                index += 1;
            }
        }
        if let Some(challenge) = challenges.last_mut() {
            challenge.params.push((token.to_ascii_lowercase(), text));
        }
    }
}

// Choosing

impl State {
    pub fn choose(challenges: &[Challenge], allow_basic: bool) -> Option<Self> {
        let digest = challenges
            .iter()
            .filter(|challenge| challenge.scheme == "digest")
            .filter_map(Digest::from_challenge)
            .max_by_key(|digest| digest.algorithm == Algorithm::Sha256);
        if let Some(digest) = digest {
            return Some(Self::Digest(digest));
        }
        let basic = challenges
            .iter()
            .any(|challenge| challenge.scheme == "basic");
        (basic && allow_basic).then_some(Self::Basic)
    }

    pub fn refresh(&mut self, challenges: &[Challenge]) -> bool {
        let Self::Digest(current) = self else {
            return false;
        };
        let fresh = challenges
            .iter()
            .filter(|challenge| challenge.scheme == "digest")
            .filter(|challenge| {
                challenge
                    .param("stale")
                    .is_some_and(|stale| stale.eq_ignore_ascii_case("true"))
            })
            .filter_map(Digest::from_challenge)
            .find(|digest| digest.algorithm == current.algorithm);
        match fresh {
            Some(fresh) => {
                *current = fresh;
                true
            }
            None => false,
        }
    }

    pub fn switch(&mut self, challenges: &[Challenge], allow_basic: bool) -> bool {
        match Self::choose(challenges, allow_basic) {
            Some(chosen) if !chosen.same_protection(self) => {
                *self = chosen;
                true
            }
            _ => false,
        }
    }

    fn same_protection(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) | (Self::Basic, Self::Basic) => true,
            (Self::Digest(first), Self::Digest(second)) => {
                first.realm == second.realm
                    && first.algorithm == second.algorithm
                    && first.session == second.session
            }
            _ => false,
        }
    }

    pub fn header(
        &mut self,
        credentials: &Credentials,
        method: &str,
        uri: &str,
    ) -> Result<Option<String>> {
        match self {
            Self::None => Ok(None),
            Self::Basic => basic(credentials).map(Some),
            Self::Digest(digest) => {
                digest.count = digest.count.wrapping_add(1);
                let cnonce = client_nonce()?;
                Ok(Some(digest.header(credentials, method, uri, &cnonce)))
            }
        }
    }
}

// Basic

fn basic(credentials: &Credentials) -> Result<String> {
    if credentials.username.contains(':') {
        return Err(Error::new(
            ErrorKind::AuthenticationFailed,
            "the user name may not contain a colon",
        ));
    }
    let pair = format!("{}:{}", credentials.username, credentials.password);
    Ok(format!("Basic {}", base64(pair.as_bytes())))
}

// Digest

impl Digest {
    fn from_challenge(challenge: &Challenge) -> Option<Self> {
        let (algorithm, session) = match challenge
            .param("algorithm")
            .unwrap_or("MD5")
            .to_ascii_uppercase()
            .as_str()
        {
            "MD5" => (Algorithm::Md5, false),
            "MD5-SESS" => (Algorithm::Md5, true),
            "SHA-256" => (Algorithm::Sha256, false),
            "SHA-256-SESS" => (Algorithm::Sha256, true),
            _ => return None,
        };
        let qop = match challenge.param("qop") {
            Some(options) => {
                if !options
                    .split(',')
                    .any(|option| option.trim().eq_ignore_ascii_case("auth"))
                {
                    return None;
                }
                true
            }
            None => false,
        };
        if session && !qop {
            return None;
        }
        Some(Self {
            realm: challenge.param("realm")?.to_string(),
            nonce: challenge.param("nonce")?.to_string(),
            opaque: challenge.param("opaque").map(str::to_string),
            algorithm,
            session,
            qop,
            count: 0,
        })
    }

    fn header(&self, credentials: &Credentials, method: &str, uri: &str, cnonce: &str) -> String {
        let algorithm = self.algorithm;
        let count = format!("{:08x}", self.count);
        let mut first = hash_parts(
            algorithm,
            &[&credentials.username, &self.realm, &credentials.password],
        );
        if self.session {
            first = hash_parts(algorithm, &[&first, &self.nonce, cnonce]);
        }
        let second = hash_parts(algorithm, &[method, uri]);
        let response = if self.qop {
            hash_parts(
                algorithm,
                &[&first, &self.nonce, &count, cnonce, "auth", &second],
            )
        } else {
            hash_parts(algorithm, &[&first, &self.nonce, &second])
        };
        let name = match (algorithm, self.session) {
            (Algorithm::Md5, false) => "MD5",
            (Algorithm::Md5, true) => "MD5-sess",
            (Algorithm::Sha256, false) => "SHA-256",
            (Algorithm::Sha256, true) => "SHA-256-sess",
        };
        let mut header = format!(
            "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", algorithm={name}, response=\"{response}\"",
            quote(&credentials.username),
            quote(&self.realm),
            quote(&self.nonce),
            quote(uri)
        );
        if self.qop {
            header.push_str(&format!(", qop=auth, nc={count}, cnonce=\"{cnonce}\""));
        }
        if let Some(opaque) = &self.opaque {
            header.push_str(&format!(", opaque=\"{}\"", quote(opaque)));
        }
        header
    }
}

pub(crate) fn hash_parts(algorithm: Algorithm, parts: &[&str]) -> String {
    let joined = parts.join(":");
    let hash = match algorithm {
        Algorithm::Md5 => HashAlgorithm::Md5,
        Algorithm::Sha256 => HashAlgorithm::Sha256,
    };
    to_hex(&hash.digest(joined.as_bytes()))
}

fn quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn client_nonce() -> Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::new(ErrorKind::Other, "no random numbers"))?;
    Ok(to_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_password;

    const NONCE: &str = "7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v";
    const CNONCE: &str = "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ";
    const REALM: &str = "http-auth@example.org";

    fn credentials() -> Credentials {
        Credentials {
            username: "demo".to_string(),
            password: test_password(),
        }
    }

    fn param(header: &str, name: &str) -> Option<String> {
        parse_challenges(std::iter::once(header))
            .first()?
            .param(name)
            .map(str::to_string)
    }

    #[test]
    fn challenges_are_split_with_their_parameters() {
        let challenges = parse_challenges(
            [
                "Digest realm=\"files, \\\"home\\\"\", nonce=\"abc\", algorithm=SHA-256, qop=\"auth,auth-int\", Basic realm=\"x\"",
                "Negotiate",
            ]
            .into_iter(),
        );
        assert_eq!(challenges.len(), 3);
        assert_eq!(challenges[0].scheme, "digest");
        assert_eq!(challenges[0].param("realm"), Some("files, \"home\""));
        assert_eq!(challenges[0].param("algorithm"), Some("SHA-256"));
        assert_eq!(challenges[0].param("qop"), Some("auth,auth-int"));
        assert_eq!(challenges[1].scheme, "basic");
        assert_eq!(challenges[1].param("realm"), Some("x"));
        assert_eq!(challenges[2].scheme, "negotiate");
    }

    #[test]
    fn digest_beats_basic_and_sha256_beats_md5() {
        let both = parse_challenges(
            [
                "Basic realm=\"x\"",
                "Digest realm=\"x\", nonce=\"n1\", qop=\"auth\"",
                "Digest realm=\"x\", nonce=\"n2\", qop=\"auth\", algorithm=SHA-256",
            ]
            .into_iter(),
        );
        let Some(State::Digest(digest)) = State::choose(&both, false) else {
            panic!("no digest");
        };
        assert_eq!(
            (digest.algorithm, digest.nonce.as_str()),
            (Algorithm::Sha256, "n2")
        );
        let basic = parse_challenges(std::iter::once("Basic realm=\"x\""));
        assert_eq!(State::choose(&basic, true), Some(State::Basic));
        assert_eq!(State::choose(&basic, false), None);
        let unusable = parse_challenges(
            [
                "Digest realm=\"x\", nonce=\"n\", qop=\"auth-int\"",
                "Digest realm=\"x\", nonce=\"n\", algorithm=SHA-512-256",
                "Digest realm=\"x\"",
            ]
            .into_iter(),
        );
        assert_eq!(State::choose(&unusable, true), None);
    }

    #[test]
    fn digest_hashes_match_the_rfc_7616_examples() {
        let user = "Mufasa";
        let phrase = ["Circle", "of", "Life"].join(" ");
        for (algorithm, expected) in [
            (Algorithm::Md5, "8ca523f5e9506fed4657c9700eebdbec"),
            (
                Algorithm::Sha256,
                "753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1",
            ),
        ] {
            let first = hash_parts(algorithm, &[user, REALM, &phrase]);
            let second = hash_parts(algorithm, &["GET", "/dir/index.html"]);
            let response = hash_parts(
                algorithm,
                &[&first, NONCE, "00000001", CNONCE, "auth", &second],
            );
            assert_eq!(response, expected);
        }
    }

    #[test]
    fn digest_headers_carry_the_computed_response() {
        let challenges = parse_challenges(std::iter::once(
            "Digest realm=\"http-auth@example.org\", qop=\"auth, auth-int\", algorithm=MD5, nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\", opaque=\"FQhe\"",
        ));
        let mut state = State::choose(&challenges, false).expect("digest");
        let credentials = credentials();
        let first = state
            .header(&credentials, "PROPFIND", "/dav/a%20b/")
            .expect("header")
            .expect("some header");
        let second = state
            .header(&credentials, "PROPFIND", "/dav/a%20b/")
            .expect("header")
            .expect("some header");
        assert!(first.starts_with("Digest username=\"demo\", realm=\"http-auth@example.org\""));
        assert!(first.contains("uri=\"/dav/a%20b/\"") && first.contains("opaque=\"FQhe\""));
        assert!(first.contains("nc=00000001") && second.contains("nc=00000002"));
        assert!(!first.contains(&credentials.password));
        let cnonce = param(&first, "cnonce").expect("cnonce");
        let ha1 = hash_parts(
            Algorithm::Md5,
            &[&credentials.username, REALM, &credentials.password],
        );
        let ha2 = hash_parts(Algorithm::Md5, &["PROPFIND", "/dav/a%20b/"]);
        let expected = hash_parts(
            Algorithm::Md5,
            &[&ha1, NONCE, "00000001", &cnonce, "auth", &ha2],
        );
        assert_eq!(param(&first, "response"), Some(expected));
    }

    #[test]
    fn legacy_digest_without_qop_and_stale_nonces() {
        let challenges = parse_challenges(std::iter::once("Digest realm=\"r\", nonce=\"old\""));
        let mut state = State::choose(&challenges, false).expect("digest");
        let credentials = credentials();
        let header = state
            .header(&credentials, "GET", "/")
            .expect("header")
            .expect("some header");
        assert!(!header.contains("qop=") && !header.contains("cnonce"));
        let ha1 = hash_parts(Algorithm::Md5, &["demo", "r", &credentials.password]);
        let ha2 = hash_parts(Algorithm::Md5, &["GET", "/"]);
        let expected = hash_parts(Algorithm::Md5, &[&ha1, "old", &ha2]);
        assert_eq!(param(&header, "response"), Some(expected));

        let not_stale = parse_challenges(std::iter::once("Digest realm=\"r\", nonce=\"new\""));
        assert!(!state.refresh(&not_stale));
        let stale = parse_challenges(std::iter::once(
            "Digest realm=\"r\", nonce=\"new\", stale=TRUE",
        ));
        assert!(state.refresh(&stale));
        let header = state
            .header(&credentials, "GET", "/")
            .expect("header")
            .expect("some header");
        assert!(header.contains("nonce=\"new\""));
        assert!(!State::Basic.refresh(&stale));
    }

    #[test]
    fn other_schemes_and_realms_are_switched_to_once() {
        let digest = |realm: &str, nonce: &str, algorithm: &str| {
            parse_challenges(std::iter::once(
                format!("Digest realm=\"{realm}\", nonce=\"{nonce}\", qop=\"auth\", algorithm={algorithm}")
                    .as_str(),
            ))
        };
        let basic = parse_challenges(std::iter::once("Basic realm=\"b\""));
        let mut state = State::choose(&digest("a", "n1", "MD5"), true).expect("digest");

        assert!(!state.switch(&digest("a", "n2", "MD5"), true));
        assert!(matches!(&state, State::Digest(current) if current.nonce == "n1"));

        assert!(state.switch(&digest("b", "n3", "MD5"), true));
        assert!(matches!(&state, State::Digest(current) if current.realm == "b"));

        assert!(state.switch(&digest("b", "n4", "SHA-256"), true));
        assert!(matches!(&state, State::Digest(current) if current.algorithm == Algorithm::Sha256));
        assert!(state.switch(&digest("b", "n5", "SHA-256-sess"), true));
        assert!(matches!(&state, State::Digest(current) if current.session));

        let before = state.clone();
        assert!(!state.switch(&basic, false));
        assert_eq!(state, before);
        assert!(state.switch(&basic, true));
        assert_eq!(state, State::Basic);
        assert!(!state.switch(&basic, true));
        assert!(!state.switch(&[], true));
        assert_eq!(state, State::Basic);
    }

    #[test]
    fn basic_headers_encode_user_and_password() {
        let credentials = credentials();
        let header = State::Basic
            .header(&credentials, "GET", "/")
            .expect("header")
            .expect("some header");
        let expected = base64(format!("demo:{}", credentials.password).as_bytes());
        assert_eq!(header, format!("Basic {expected}"));
        let colon = Credentials {
            username: "a:b".to_string(),
            ..credentials.clone()
        };
        assert!(State::Basic.header(&colon, "GET", "/").is_err());
        assert_eq!(State::None.header(&credentials, "GET", "/"), Ok(None));
        assert!(!format!("{credentials:?}").contains(&credentials.password));
    }
}
