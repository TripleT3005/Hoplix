use hoplix_core::Protocol;

// Constants

pub const ANONYMOUS: &str = "anonymous";
const SITE_SCHEME: &str = "site";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Local(String),
    Remote(RemoteTarget),
    Site(SiteTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteTarget {
    pub reference: String,
    pub path: Option<RemotePath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub path: RemotePath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemotePath {
    Home(String),
    Absolute(String),
}

// Parsing

impl Target {
    pub fn parse(text: &str) -> Result<Self, String> {
        let Some((scheme, rest)) = text.split_once("://") else {
            return Ok(Self::Local(text.to_string()));
        };
        if scheme.eq_ignore_ascii_case(SITE_SCHEME) {
            return SiteTarget::parse(rest).map(Self::Site);
        }
        let protocol = Protocol::ALL
            .into_iter()
            .find(|protocol| protocol.scheme().eq_ignore_ascii_case(scheme))
            .ok_or_else(|| format!("unsupported protocol: {scheme}"))?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        if protocol.is_s3() {
            return Self::parse_s3(text, authority, path);
        }
        let (username, address) = match authority.rsplit_once('@') {
            Some((username, address)) => (decode(username)?, address),
            None if !protocol.supports_key_files() => (ANONYMOUS.to_string(), authority),
            None => return Err(format!("missing user name in {text}")),
        };
        if username.is_empty() {
            return Err(format!("missing user name in {text}"));
        }
        let (host, port) = split_host_port(address, protocol.default_port())?;
        if host.is_empty() {
            return Err(format!("missing host in {text}"));
        }
        let path = RemotePath::parse(&decode(path)?);
        Ok(Self::Remote(RemoteTarget {
            protocol,
            host,
            port,
            username,
            path,
        }))
    }
}

impl Target {
    fn parse_s3(text: &str, bucket: &str, path: &str) -> Result<Self, String> {
        if bucket.contains('@') {
            return Err(format!(
                "do not put credentials into {text}, use --access-key and --secret-env"
            ));
        }
        let bucket = decode(bucket)?;
        if !hoplix_core::is_valid_bucket(&bucket) {
            return Err(format!("invalid bucket name in {text}"));
        }
        let path = decode(path)?;
        let path = if path.is_empty() {
            "/".to_string()
        } else {
            path
        };
        Ok(Self::Remote(RemoteTarget {
            protocol: Protocol::S3,
            host: bucket,
            port: Protocol::S3.default_port(),
            username: String::new(),
            path: RemotePath::Absolute(path),
        }))
    }
}

impl SiteTarget {
    fn parse(rest: &str) -> Result<Self, String> {
        let (reference, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        let reference = decode(reference)?;
        if reference.trim().is_empty() {
            return Err(format!("missing site name in site://{rest}"));
        }
        let path = decode(path)?;
        Ok(Self {
            reference,
            path: (!path.is_empty() && path != "/").then(|| RemotePath::parse(&path)),
        })
    }
}

impl RemotePath {
    pub fn parse(path: &str) -> Self {
        if path.is_empty() || path == "/~" {
            Self::Home(String::new())
        } else if let Some(relative) = path.strip_prefix("/~/") {
            Self::Home(relative.to_string())
        } else {
            Self::Absolute(path.to_string())
        }
    }

    pub fn from_site(path: &str) -> Self {
        if path.starts_with('/') {
            Self::Absolute(path.to_string())
        } else {
            Self::Home(path.to_string())
        }
    }
}

impl RemoteTarget {
    pub fn same_server(&self, other: &Self) -> bool {
        self.protocol == other.protocol
            && self.host.eq_ignore_ascii_case(&other.host)
            && self.port == other.port
            && self.username == other.username
    }
}

// Helpers

pub(crate) fn split_host_port(address: &str, default_port: u16) -> Result<(String, u16), String> {
    let parse_port = |port: &str| {
        port.parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| format!("invalid port: {port}"))
    };
    if let Some(rest) = address.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("invalid address: {address}"))?;
        let port = match after.strip_prefix(':') {
            Some(port) => parse_port(port)?,
            None if after.is_empty() => default_port,
            None => return Err(format!("invalid address: {address}")),
        };
        return Ok((host.to_string(), port));
    }
    match address.rsplit_once(':') {
        Some((host, port)) => Ok((host.to_string(), parse_port(port)?)),
        None => Ok((address.to_string(), default_port)),
    }
}

fn decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let value = text
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| format!("invalid escape in {text}"))?;
            result.push(value);
            index += 3;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(result).map_err(|_| format!("invalid UTF-8 in {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(text: &str) -> RemoteTarget {
        match Target::parse(text) {
            Ok(Target::Remote(target)) => target,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn plain_paths_are_local() {
        assert_eq!(
            Target::parse("C:\\data\\file.txt"),
            Ok(Target::Local("C:\\data\\file.txt".to_string()))
        );
        assert_eq!(Target::parse("./a"), Ok(Target::Local("./a".to_string())));
    }

    #[test]
    fn urls_split_into_parts() {
        let target = remote("sftp://demo@files.example.test:2222/srv/www");
        assert_eq!(target.host, "files.example.test");
        assert_eq!(target.port, 2222);
        assert_eq!(target.username, "demo");
        assert_eq!(target.path, RemotePath::Absolute("/srv/www".to_string()));

        let target = remote("SFTP://demo@files.example.test");
        assert_eq!(target.port, 22);
        assert_eq!(target.path, RemotePath::Home(String::new()));
    }

    #[test]
    fn home_paths_ipv6_and_escapes() {
        assert_eq!(
            remote("sftp://demo@host/~/docs/a%20b.txt").path,
            RemotePath::Home("docs/a b.txt".to_string())
        );
        let target = remote("sftp://me%40work@[::1]:2200/tmp");
        assert_eq!(target.username, "me@work");
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, 2200);
        assert_eq!(remote("sftp://demo@[fe80::1]/").port, 22);
    }

    #[test]
    fn ftp_urls_choose_the_protocol_and_default_to_anonymous() {
        let target = remote("ftp://files.example.test/pub");
        assert_eq!(target.protocol, Protocol::Ftp);
        assert_eq!(target.username, ANONYMOUS);
        assert_eq!(target.port, 21);
        let target = remote("ftpes://demo@files.example.test/");
        assert_eq!(target.protocol, Protocol::FtpsExplicit);
        assert_eq!(target.port, 21);
        let target = remote("ftps://demo@files.example.test:2990/");
        assert_eq!(target.protocol, Protocol::FtpsImplicit);
        assert_eq!(target.port, 2990);
        assert_eq!(remote("ftps://demo@files.example.test/").port, 990);
    }

    #[test]
    fn s3_urls_name_the_bucket_and_key() {
        let target = remote("s3://demo-bucket/backups/a%20b.txt");
        assert_eq!(target.protocol, Protocol::S3);
        assert_eq!(target.host, "demo-bucket");
        assert_eq!(
            target.path,
            RemotePath::Absolute("/backups/a b.txt".to_string())
        );
        assert_eq!(
            remote("S3://demo-bucket").path,
            RemotePath::Absolute("/".to_string())
        );
        for text in ["s3://key@demo-bucket/", "s3:///path", "s3://a b/"] {
            assert!(Target::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn site_targets_name_a_saved_site() {
        assert_eq!(
            Target::parse("site://Web%20server"),
            Ok(Target::Site(SiteTarget {
                reference: "Web server".to_string(),
                path: None,
            }))
        );
        assert_eq!(
            Target::parse("SITE://Work%2FWeb/var/www"),
            Ok(Target::Site(SiteTarget {
                reference: "Work/Web".to_string(),
                path: Some(RemotePath::Absolute("/var/www".to_string())),
            }))
        );
        assert_eq!(
            Target::parse("site://web/~/logs"),
            Ok(Target::Site(SiteTarget {
                reference: "web".to_string(),
                path: Some(RemotePath::Home("logs".to_string())),
            }))
        );
        assert!(Target::parse("site:///path").is_err());
        assert_eq!(
            RemotePath::from_site("www"),
            RemotePath::Home("www".to_string())
        );
        assert_eq!(
            RemotePath::from_site("/srv"),
            RemotePath::Absolute("/srv".to_string())
        );
    }

    #[test]
    fn invalid_urls_are_rejected() {
        for text in [
            "gopher://demo@host/",
            "sftp://host/path",
            "sftp://@host/",
            "sftp://demo@/x",
            "sftp://demo@host:0/",
            "sftp://demo@host:abc/",
            "sftp://demo@[::1/",
            "sftp://demo@host/%zz",
        ] {
            assert!(Target::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn same_server_ignores_paths_and_host_case() {
        let a = remote("sftp://demo@Host.test/a");
        let b = remote("sftp://demo@host.test/b");
        let c = remote("sftp://other@host.test/b");
        assert!(a.same_server(&b));
        assert!(!a.same_server(&c));
    }
}
