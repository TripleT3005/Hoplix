mod http;
mod json;
mod version;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Error, ErrorKind, Result};

use json::Json;
pub use version::Version;

// Constants

pub const RELEASES_URL: &str = "https://api.github.com/repos/TripleT3005/Hoplix/releases?per_page=10";
pub const RELEASES_PAGE: &str = "https://github.com/TripleT3005/Hoplix/releases";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_PAGE_PREFIX: &str = "https://github.com/TripleT3005/Hoplix/releases/";

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub url: String,
    pub draft: bool,
    pub prerelease: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    Available(Release),
}

// Releases

pub fn parse_releases(text: &str) -> Result<Vec<Release>> {
    let Some(Json::Array(items)) = json::parse(text) else {
        return Err(Error::new(ErrorKind::Other, "invalid release list"));
    };
    Ok(items.iter().filter_map(release).collect())
}

fn release(item: &Json) -> Option<Release> {
    let version = Version::parse(item.get("tag_name")?.as_str()?)?;
    let url = item.get("html_url")?.as_str()?;
    if !is_release_page(url) {
        return None;
    }
    Some(Release {
        version,
        url: url.to_string(),
        draft: item.get("draft").and_then(Json::as_bool).unwrap_or(true),
        prerelease: item
            .get("prerelease")
            .and_then(Json::as_bool)
            .unwrap_or(true),
    })
}

pub fn is_release_page(url: &str) -> bool {
    url == RELEASES_PAGE
        || url.strip_prefix(RELEASE_PAGE_PREFIX).is_some_and(|rest| {
            rest.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._/+".contains(&byte))
        })
}

pub fn latest<'a>(releases: &'a [Release], current: &Version) -> Option<&'a Release> {
    releases
        .iter()
        .filter(|release| !release.draft)
        .filter(|release| {
            current.is_unstable() || !(release.prerelease || release.version.is_prerelease())
        })
        .max_by(|left, right| left.version.cmp(&right.version))
}

pub fn status(releases: &[Release], current: &Version) -> UpdateStatus {
    match latest(releases, current) {
        Some(release) if release.version > *current => UpdateStatus::Available(release.clone()),
        _ => UpdateStatus::UpToDate,
    }
}

pub fn evaluate(text: &str, current: &str) -> Result<UpdateStatus> {
    let current = Version::parse(current)
        .ok_or_else(|| Error::new(ErrorKind::Other, format!("invalid version {current}")))?;
    Ok(status(&parse_releases(text)?, &current))
}

// Schedule

pub fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub fn is_due(last_check: Option<u64>, now: u64) -> bool {
    match last_check {
        None => true,
        Some(last) if last > now => true,
        Some(last) => now - last >= CHECK_INTERVAL.as_secs(),
    }
}

// Network

pub fn user_agent() -> String {
    format!("Hoplix/{CURRENT_VERSION}")
}

pub async fn fetch_releases() -> Result<String> {
    let body = http::get(RELEASES_URL, &user_agent()).await?;
    String::from_utf8(body).map_err(|_| Error::new(ErrorKind::Other, "invalid release list"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASES: &str = r####"[
      {
        "url": "https://api.github.com/repos/TripleT3005/Hoplix/releases/3",
        "html_url": "https://github.com/TripleT3005/Hoplix/releases/tag/v1.1.0-beta.1",
        "id": 3,
        "author": { "login": "TripleT3005", "id": 1, "site_admin": false },
        "tag_name": "v1.1.0-beta.1",
        "target_commitish": "main",
        "name": "Hoplix 1.1.0-beta.1",
        "draft": false,
        "prerelease": true,
        "created_at": "2026-11-20T10:00:00Z",
        "assets": [
          { "name": "Hoplix-1.1.0-beta.1-windows-x64.zip", "size": 12345678, "download_count": 3 }
        ],
        "body": "### English\r\n- Ünïcödé \"quotes\" and \\ backslashes \ud83d\ude80"
      },
      {
        "html_url": "https://github.com/TripleT3005/Hoplix/releases/tag/v2.0.0",
        "tag_name": "v2.0.0",
        "draft": true,
        "prerelease": false
      },
      {
        "html_url": "https://github.com/TripleT3005/Hoplix/releases/tag/v1.0.0",
        "tag_name": "v1.0.0",
        "draft": false,
        "prerelease": false,
        "reactions": { "total_count": 0, "+1": 0 },
        "assets": []
      },
      {
        "html_url": "https://github.com/TripleT3005/Hoplix/releases/tag/v0.3.0",
        "tag_name": "v0.3.0",
        "draft": false,
        "prerelease": true
      },
      {
        "html_url": "https://evil.example/releases/tag/v9.0.0",
        "tag_name": "v9.0.0",
        "draft": false,
        "prerelease": false
      },
      {
        "html_url": "https://github.com/TripleT3005/Hoplix/releases/tag/nightly",
        "tag_name": "nightly",
        "draft": false,
        "prerelease": true
      },
      { "tag_name": "v5.0.0", "draft": false, "prerelease": false }
    ]"####;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("invalid test version")
    }

    #[test]
    fn github_release_lists_parse() {
        let releases = parse_releases(RELEASES).expect("parsing failed");
        let tags: Vec<String> = releases.iter().map(|r| r.version.to_string()).collect();
        assert_eq!(tags, ["1.1.0-beta.1", "2.0.0", "1.0.0", "0.3.0"]);
        assert!(releases[0].prerelease);
        assert!(releases[1].draft);
        assert_eq!(
            releases[2].url,
            "https://github.com/TripleT3005/Hoplix/releases/tag/v1.0.0"
        );
        assert!(parse_releases("{\"message\": \"Not Found\"}").is_err());
        assert!(parse_releases("not json").is_err());
        assert_eq!(parse_releases("[]").expect("parsing failed"), Vec::new());
    }

    #[test]
    fn drafts_are_skipped_and_prereleases_only_offered_to_unstable_versions() {
        let releases = parse_releases(RELEASES).expect("parsing failed");
        let stable = latest(&releases, &version("1.0.0")).expect("no release");
        assert_eq!(stable.version, version("1.0.0"));
        let unstable = latest(&releases, &version("0.2.1")).expect("no release");
        assert_eq!(unstable.version, version("1.1.0-beta.1"));
        let beta = latest(&releases, &version("1.1.0-alpha")).expect("no release");
        assert_eq!(beta.version, version("1.1.0-beta.1"));
    }

    #[test]
    fn updates_are_only_offered_for_newer_versions() {
        let text = RELEASES;
        assert_eq!(evaluate(text, "1.0.0").expect("failed"), UpdateStatus::UpToDate);
        assert_eq!(evaluate(text, "1.2.0").expect("failed"), UpdateStatus::UpToDate);
        assert_eq!(
            evaluate(text, "1.1.0-beta.1").expect("failed"),
            UpdateStatus::UpToDate
        );
        let UpdateStatus::Available(release) = evaluate(text, "0.2.1").expect("failed") else {
            panic!("no update offered");
        };
        assert_eq!(release.version.to_string(), "1.1.0-beta.1");
        assert!(matches!(
            evaluate(text, "0.9.9").expect("failed"),
            UpdateStatus::Available(_)
        ));
        assert!(evaluate(text, "invalid").is_err());
    }

    #[test]
    fn release_pages_must_belong_to_the_project() {
        assert!(is_release_page(RELEASES_PAGE));
        assert!(is_release_page(
            "https://github.com/TripleT3005/Hoplix/releases/tag/v1.0.0-beta.1"
        ));
        assert!(!is_release_page("https://github.com/Other/Hoplix/releases/tag/v1"));
        assert!(!is_release_page(
            "https://github.com/TripleT3005/Hoplix/releases/tag/v1\"&calc"
        ));
        assert!(!is_release_page("file:///C:/Windows"));
    }

    #[test]
    fn checks_are_due_once_per_day() {
        let day = CHECK_INTERVAL.as_secs();
        assert!(is_due(None, 1_000_000));
        assert!(!is_due(Some(1_000_000), 1_000_000));
        assert!(!is_due(Some(1_000_000), 1_000_000 + day - 1));
        assert!(is_due(Some(1_000_000), 1_000_000 + day));
        assert!(is_due(Some(2_000_000), 1_000_000));
    }

    #[test]
    fn the_user_agent_names_only_the_version() {
        assert_eq!(user_agent(), format!("Hoplix/{}", env!("CARGO_PKG_VERSION")));
    }

    #[tokio::test]
    #[ignore = "contacts api.github.com"]
    async fn the_real_release_list_can_be_fetched() {
        let text = fetch_releases().await.expect("fetching failed");
        let releases = parse_releases(&text).expect("parsing failed");
        assert!(!releases.is_empty());
        assert!(matches!(
            evaluate(&text, "0.0.1").expect("evaluating failed"),
            UpdateStatus::Available(_)
        ));
    }
}
