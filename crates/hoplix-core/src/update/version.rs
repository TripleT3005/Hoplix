use std::cmp::Ordering;
use std::fmt;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pre: Vec<Identifier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Identifier {
    Numeric(u64),
    Text(String),
}

// Parsing

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(version, build)| {
            if build.is_empty() { "" } else { version }
        });
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.').map(parse_number);
        let (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) = (
            numbers.next(),
            numbers.next(),
            numbers.next(),
            numbers.next(),
        ) else {
            return None;
        };
        let pre = match pre {
            Some(pre) => pre
                .split('.')
                .map(parse_identifier)
                .collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    pub fn is_unstable(&self) -> bool {
        self.major == 0 || self.is_prerelease()
    }
}

fn parse_number(text: &str) -> Option<u64> {
    let valid = !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    valid.then(|| text.parse().ok()).flatten()
}

fn parse_identifier(text: &str) -> Option<Identifier> {
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return parse_number(text).map(Identifier::Numeric);
    }
    Some(Identifier::Text(text.to_string()))
}

// Ordering

impl Ord for Identifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Numeric(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for Identifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Display

impl fmt::Display for Identifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Numeric(number) => write!(formatter, "{number}"),
            Self::Text(text) => formatter.write_str(text),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (index, identifier) in self.pre.iter().enumerate() {
            formatter.write_str(if index == 0 { "-" } else { "." })?;
            write!(formatter, "{identifier}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text} did not parse"))
    }

    #[test]
    fn versions_parse_with_prefix_suffix_and_build() {
        assert_eq!(version("v1.2.3").to_string(), "1.2.3");
        assert_eq!(version(" 0.2.1 ").to_string(), "0.2.1");
        assert_eq!(version("1.0.0-beta.1").to_string(), "1.0.0-beta.1");
        assert_eq!(version("1.0.0-rc.1+build.7").to_string(), "1.0.0-rc.1");
        assert!(version("1.0.0-alpha").is_prerelease());
        assert!(!version("1.0.0").is_prerelease());
        assert!(version("0.9.0").is_unstable());
        assert!(version("2.0.0-beta").is_unstable());
        assert!(!version("1.0.0").is_unstable());
    }

    #[test]
    fn invalid_versions_are_rejected() {
        for text in [
            "", "1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "a.b.c", "1.2.3-", "1.2.3-a..b",
            "1.2.3-01", "1.2.3-ä", "1.2.3+", "-1.2.3", "1.2.x",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?} parsed");
        }
    }

    #[test]
    fn versions_follow_semver_precedence() {
        let ordered = [
            "0.2.1",
            "0.2.10",
            "0.10.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "2.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(version(pair[0]) < version(pair[1]), "{pair:?}");
            assert!(version(pair[1]) > version(pair[0]), "{pair:?}");
        }
        assert_eq!(version("1.0.0+a").cmp(&version("1.0.0+b")), Ordering::Equal);
        assert_eq!(version("v1.0.0"), version("1.0.0"));
    }
}
