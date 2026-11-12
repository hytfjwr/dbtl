//! CHANGELOG.md keeps the shape the release flow consumes: `## [Unreleased]`
//! on top, dated `## [X.Y.Z]` sections newest-first, and a footer compare
//! link per section — including one for the workspace version.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn changelog() -> String {
    std::fs::read_to_string(repo_root().join("CHANGELOG.md")).unwrap()
}

/// `(version, date)` for each `## [X.Y.Z] - YYYY-MM-DD` heading, in file order.
fn version_sections(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| l.starts_with("## ["))
        .filter_map(|l| {
            let rest = l.strip_prefix("## [")?;
            let (ver, date) = rest.split_once("] - ")?;
            Some((ver.to_string(), date.to_string()))
        })
        .collect()
}

/// `(name, url)` for each footer link definition `[name]: url`.
fn link_defs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| l.starts_with('['))
        .filter_map(|l| {
            let (name, rest) = l.split_once("]:")?;
            let name = name.strip_prefix('[')?;
            let url = rest.trim();
            (!url.is_empty()).then(|| (name.to_string(), url.to_string()))
        })
        .collect()
}

fn version_tuple(v: &str) -> (u64, u64, u64) {
    let mut p = v.split('.').map(|x| x.parse::<u64>().unwrap_or(u64::MAX));
    (
        p.next().unwrap_or(u64::MAX),
        p.next().unwrap_or(u64::MAX),
        p.next().unwrap_or(u64::MAX),
    )
}

fn is_x_y_z(v: &str) -> bool {
    v.split('.').count() == 3
        && v.split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_date(s: &str) -> bool {
    let d = s.as_bytes();
    s.len() == 10
        && d[4] == b'-'
        && d[7] == b'-'
        && s.bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
}

#[test]
fn unreleased_is_the_first_section() {
    let text = changelog();
    let first = text
        .lines()
        .find(|l| l.starts_with("## "))
        .expect("a ## heading");
    assert_eq!(first, "## [Unreleased]");
}

#[test]
fn version_sections_are_dated_and_newest_first() {
    let text = changelog();
    let sections = version_sections(&text);
    assert!(sections.len() > 1, "expected released sections");
    for (ver, date) in &sections {
        assert!(is_x_y_z(ver), "section name `{ver}` is not X.Y.Z");
        assert!(
            is_date(date),
            "section `{ver}` date `{date}` is not YYYY-MM-DD"
        );
    }
    for pair in sections.windows(2) {
        let (a, b) = (version_tuple(&pair[0].0), version_tuple(&pair[1].0));
        assert!(
            a > b,
            "sections out of order: {} then {}",
            pair[0].0,
            pair[1].0
        );
    }
}

#[test]
fn every_section_and_link_pair_up() {
    let text = changelog();
    let sections = version_sections(&text);
    let defs = link_defs(&text);

    let mut failures = Vec::new();
    for (i, (ver, _)) in sections.iter().enumerate() {
        let url = match defs.iter().filter(|(n, _)| n == ver).collect::<Vec<_>>()[..] {
            [(_, u)] => u.clone(),
            ref hits => {
                failures.push(format!("`[{ver}]` link defined {} times", hits.len()));
                continue;
            }
        };
        let expected = if i + 1 < sections.len() {
            format!("/compare/v{}...v{ver}", sections[i + 1].0)
        } else {
            format!("/releases/tag/v{ver}")
        };
        if !url.ends_with(&expected) {
            failures.push(format!(
                "`[{ver}]` link `{url}` does not end with `{expected}`"
            ));
        }
    }
    // The [Unreleased] base is the newest released version.
    let unreleased = defs.iter().find(|(n, _)| n == "Unreleased");
    match unreleased {
        Some((_, url)) => {
            let expected = format!("/compare/v{}...HEAD", sections[0].0);
            if !url.ends_with(&expected) {
                failures.push(format!("`[Unreleased]` link `{url}` is not `{expected}`"));
            }
        }
        None => failures.push("missing `[Unreleased]` link".to_string()),
    }
    // No orphan version links without a matching section.
    for (name, _) in defs.iter().filter(|(n, _)| n != "Unreleased") {
        if !sections.iter().any(|(v, _)| v == name) {
            failures.push(format!("`[{name}]` link has no matching section"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The workspace version always has a section: on `main` it is the latest
/// release, and the release flow adds the section alongside the version bump.
#[test]
fn workspace_version_has_a_section() {
    let cargo = std::fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    let section = cargo
        .split_once("[workspace.package]")
        .expect("[workspace.package] table")
        .1;
    let version = section
        .lines()
        .take_while(|l| !l.starts_with('['))
        .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("workspace.package.version");
    let text = changelog();
    assert!(
        version_sections(&text).iter().any(|(v, _)| v == version),
        "no `## [{version}]` section for workspace.package.version"
    );
}
