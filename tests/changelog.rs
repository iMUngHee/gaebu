//! Every version has patch notes: the version in Cargo.toml needs a
//! `## <version>` heading in CHANGELOG.md, and the mod ships the same version.
//! A version bump without its entry fails `cargo test`, and so CI.

const CHANGELOG: &str = include_str!("../CHANGELOG.md");
const PLUGIN: &str = include_str!("../mod/.claude-plugin/plugin.json");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The versions CHANGELOG.md has entries for, in file order.
fn entries() -> Vec<&'static str> {
    CHANGELOG
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .map(|h| h.split_whitespace().next().unwrap_or(""))
        .collect()
}

#[test]
fn current_version_has_patch_notes() {
    let entries = entries();
    assert_eq!(
        entries.first().copied(),
        Some(VERSION),
        "CHANGELOG.md must open with `## {VERSION} — <date>` for the version in Cargo.toml \
         (found {entries:?}): write the patch notes in the commit that bumps the version"
    );
    let body: String = CHANGELOG
        .split("\n## ")
        .nth(1)
        .unwrap_or("")
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(!body.is_empty(), "the {VERSION} entry is empty");
}

#[test]
fn entries_are_unique_and_newest_first() {
    let entries = entries();
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|n| n.parse().unwrap_or_else(|_| panic!("not a version: {v}")))
            .collect()
    };
    for pair in entries.windows(2) {
        assert!(
            parse(pair[0]) > parse(pair[1]),
            "CHANGELOG.md entries must be unique and newest first: {} then {}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn mod_version_follows_the_crate() {
    let plugin: serde_json::Value = serde_json::from_str(PLUGIN).unwrap();
    assert_eq!(
        plugin["version"].as_str(),
        Some(VERSION),
        "mod/.claude-plugin/plugin.json must carry version {VERSION}"
    );
}
