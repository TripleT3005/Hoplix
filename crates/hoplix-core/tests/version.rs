use std::fs;
use std::path::{Path, PathBuf};

// Constants

const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNRELEASED: &str = "Unreleased";
const CRATES: [&str; 3] = ["hoplix-core", "hoplix-app", "hoplix-cli"];
const INSTALLER: &str = "installer/hoplix.iss";
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";
const RESOURCES: &str = "build/resources.rs";
const DEBIAN_CONTROL: &str = "installer/linux/control";
const RPM_SPEC: &str = "installer/linux/hoplix.spec";
const PORTABLE_MARKER: &str = "installer/hoplix.portable";

// Helpers

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path))
        .unwrap_or_else(|error| panic!("reading {path} failed: {error}"))
        .replace("\r\n", "\n")
}

fn changelog_versions(changelog: &str) -> Vec<String> {
    changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## ["))
        .filter_map(|line| line.split_once(']'))
        .map(|(version, _)| version.to_string())
        .collect()
}

fn changelog_section<'a>(changelog: &'a str, version: &str) -> Option<&'a str> {
    let heading = format!("\n## [{version}]");
    let start = changelog.find(&heading)? + heading.len();
    let rest = &changelog[start..];
    Some(rest.find("\n## [").map_or(rest, |end| &rest[..end]))
}

fn is_semantic_version(text: &str) -> bool {
    let (core, suffix) = match text.split_once('-') {
        Some((core, suffix)) => (core, Some(suffix)),
        None => (text, None),
    };
    let parts: Vec<_> = core.split('.').collect();
    let core_is_valid = parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    let suffix_is_valid = suffix.is_none_or(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    });
    core_is_valid && suffix_is_valid
}

fn contains_version_literal(line: &str) -> bool {
    line.split(|c: char| !c.is_ascii_digit() && c != '.')
        .any(|word| word.split('.').filter(|part| !part.is_empty()).count() >= 3)
}

// Tests

#[test]
fn version_is_semantic() {
    assert!(
        is_semantic_version(VERSION),
        "{VERSION} is not a semantic version"
    );
}

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
            lockfile.contains(&entry),
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
fn changelog_section_of_the_current_version_is_bilingual() {
    let changelog = read("CHANGELOG.md");
    if let Some(section) = changelog_section(&changelog, VERSION) {
        assert!(
            section.contains("\n### English\n") && section.contains("\n### Deutsch\n"),
            "the CHANGELOG.md section of {VERSION} needs English and Deutsch"
        );
    }
}

#[test]
fn executables_take_the_package_version() {
    let resources = read(RESOURCES);
    assert!(
        resources.contains("env::var(\"CARGO_PKG_VERSION\")"),
        "{RESOURCES} must read CARGO_PKG_VERSION"
    );
    for key in ["FileVersion", "ProductVersion"] {
        assert!(
            resources.contains(&format!("(\"{key}\", version.as_str())")),
            "{RESOURCES} must use the package version for {key}"
        );
    }
}

#[test]
fn installer_takes_the_version_from_the_release_workflow() {
    let installer = read(INSTALLER);
    assert!(
        installer.contains("#ifndef AppVersion\n  #error"),
        "{INSTALLER} must fail without /DAppVersion"
    );
    assert!(
        installer.contains("AppVersion={#AppVersion}"),
        "{INSTALLER} must use AppVersion"
    );
    for line in installer.lines() {
        assert!(
            !line.trim_start().starts_with("#define AppVersion"),
            "{INSTALLER} defines its own version: {line}"
        );
        assert!(
            line.starts_with("MinVersion=") || !contains_version_literal(line),
            "{INSTALLER} hard-codes a version: {line}"
        );
    }
}

#[test]
fn release_workflow_uses_the_workspace_version() {
    let workflow = read(RELEASE_WORKFLOW);
    for expected in [
        r#"sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml"#,
        r#"echo "tag=v$version""#,
        r#""/DAppVersion=$env:VERSION" installer\hoplix.iss"#,
        "VersionInfo.ProductVersion",
        "heading=\"## [$VERSION]\"",
    ] {
        assert!(
            workflow.contains(expected),
            "{RELEASE_WORKFLOW} lacks {expected}"
        );
    }
}

#[test]
fn linux_packages_take_the_workspace_version() {
    for path in [DEBIAN_CONTROL, RPM_SPEC] {
        let text = read(path);
        assert!(
            text.contains("
Version: APP_VERSION
"),
            "{path} must take the version from the release workflow"
        );
        for line in text.lines() {
            assert!(
                !contains_version_literal(line),
                "{path} hard-codes a version: {line}"
            );
        }
    }
    let workflow = read(RELEASE_WORKFLOW);
    for expected in [
        r#"package_version="${VERSION//-/\~}""#,
        r#"sed -e "s/APP_VERSION/$package_version/""#,
        r#"sed "s/APP_VERSION/$package_version/" installer/linux/hoplix.spec"#,
        r#"dpkg-deb --field "$deb" Version"#,
        "rpm -qp --queryformat '%{VERSION}'",
    ] {
        assert!(
            workflow.contains(expected),
            "{RELEASE_WORKFLOW} lacks {expected}"
        );
    }
}

#[test]
fn only_the_portable_archive_ships_the_portable_marker() {
    assert!(
        root().join(PORTABLE_MARKER).is_file(),
        "{PORTABLE_MARKER} is missing"
    );
    assert!(
        read(RELEASE_WORKFLOW).contains("cp installer/hoplix.portable \"dist/$name/\""),
        "{RELEASE_WORKFLOW} must copy the portable marker into the portable archive"
    );
    assert!(
        !read(INSTALLER).contains("hoplix.portable"),
        "{INSTALLER} must not install the portable marker"
    );
}

#[test]
fn changelog_sections_are_parsed() {
    let text = "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] – 2026-12-01\n\nnew\n\n## [0.1.0] – 2026-11-30\n";
    assert_eq!(changelog_versions(text), ["Unreleased", "0.2.0", "0.1.0"]);
    assert_eq!(
        changelog_section(text, "0.2.0"),
        Some(" – 2026-12-01\n\nnew\n")
    );
    assert_eq!(changelog_section(text, "0.3.0"), None);
}

#[test]
fn version_helpers_detect_versions() {
    assert!(is_semantic_version("1.2.3"));
    assert!(is_semantic_version("0.3.0-beta.1"));
    assert!(!is_semantic_version("1.2"));
    assert!(!is_semantic_version("1.2.3-"));
    assert!(contains_version_literal("AppVersion=0.2.0"));
    assert!(!contains_version_literal("AppVersion={#AppVersion}"));
}
