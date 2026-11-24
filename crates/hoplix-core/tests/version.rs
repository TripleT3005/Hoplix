use std::fs;
use std::path::{Path, PathBuf};

// Constants

const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNRELEASED: &str = "Unreleased";
const CRATES: [&str; 3] = ["hoplix-core", "hoplix-app", "hoplix-cli"];

// Helpers

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path))
        .unwrap_or_else(|error| panic!("reading {path} failed: {error}"))
}

fn changelog_versions(changelog: &str) -> Vec<String> {
    changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## ["))
        .filter_map(|line| line.split_once(']'))
        .map(|(version, _)| version.to_string())
        .collect()
}

// Tests

#[test]
fn workspace_version_is_the_only_source() {
    let manifest = read("Cargo.toml");
    assert!(
        manifest.contains(&format!("version = \"{VERSION}\"")),
        "Cargo.toml does not declare {VERSION}"
    );
    for name in CRATES {
        let manifest = read(&format!("crates/{name}/Cargo.toml"));
        assert!(
            manifest.contains("version.workspace = true"),
            "{name} has its own version"
        );
    }
}

#[test]
fn lockfile_matches_the_workspace_version() {
    let lockfile = read("Cargo.lock");
    for name in CRATES {
        let entry = format!("name = \"{name}\"\nversion = \"{VERSION}\"");
        assert!(
            lockfile.replace("\r\n", "\n").contains(&entry),
            "Cargo.lock has another version of {name}"
        );
    }
}

#[test]
fn changelog_starts_with_the_current_version() {
    let versions = changelog_versions(&read("CHANGELOG.md"));
    let newest = versions
        .first()
        .expect("CHANGELOG.md has no version section");
    assert!(
        newest == UNRELEASED || newest == VERSION,
        "the newest CHANGELOG.md section is {newest}, expected {UNRELEASED} or {VERSION}"
    );
    if newest == UNRELEASED {
        assert!(
            versions.iter().skip(1).all(|version| version != VERSION),
            "{VERSION} is already released; raise the version for the unreleased changes"
        );
    }
}

#[test]
fn changelog_sections_are_parsed() {
    let text =
        "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] – 2026-12-01\n\n## [0.1.0] – 2026-11-30\n";
    assert_eq!(changelog_versions(text), ["Unreleased", "0.2.0", "0.1.0"]);
}
