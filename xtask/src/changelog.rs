//! The pure transformation behind `cargo xtask changelog-release`: move the
//! `## [Unreleased]` body of the changelog into a dated `## [X.Y.Z]` section,
//! leave an empty `Unreleased` on top, and repoint the footer compare links.
//! Everything outside the rewritten regions stays byte-identical, and the
//! file's line-ending style is preserved.

/// Cut a `[X.Y.Z] - <date>` section out of `input`'s `## [Unreleased]` body.
///
/// Fails (without touching the input) when the version or date is malformed,
/// a section for `version` already exists, `version` is not newer than the
/// latest section, the `## [Unreleased]` body holds no `- ` items (a bare
/// `###` heading does not count) and `allow_empty` is not set, or the
/// `[Unreleased]:` compare link is missing or unrecognised.
pub fn release_section(
    input: &str,
    version: &str,
    date: &str,
    allow_empty: bool,
) -> Result<String, String> {
    let (_, ver) = parse_version(version)
        .ok_or_else(|| format!("invalid version `{version}` — expected X.Y.Z"))?;
    if !valid_date(date) {
        return Err(format!("invalid date `{date}` — expected YYYY-MM-DD"));
    }

    // Work in LF space so CRLF files transform cleanly, then convert back.
    let crlf = input.contains("\r\n");
    let text = if crlf {
        input.replace("\r\n", "\n")
    } else {
        input.to_string()
    };
    let out = transform(&text, ver, version, date, allow_empty)?;
    Ok(if crlf { out.replace('\n', "\r\n") } else { out })
}

fn transform(
    text: &str,
    ver: (u64, u64, u64),
    version: &str,
    date: &str,
    allow_empty: bool,
) -> Result<String, String> {
    let lines: Vec<&str> = text.split('\n').collect();

    // Every `## ` heading must be a `## [<name>]` section heading.
    let headings: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("## "))
        .map(|(i, _)| i)
        .collect();
    for &i in &headings {
        let name = section_name(lines[i])
            .ok_or_else(|| format!("unexpected section heading `{}`", lines[i]))?;
        if name == version {
            return Err(format!("a `## [{version}]` section already exists"));
        }
    }

    let unr = headings
        .iter()
        .find(|&&i| lines[i] == "## [Unreleased]")
        .copied()
        .ok_or("no `## [Unreleased]` section found")?;

    // The Unreleased body runs to the next `## ` heading or to the footer
    // link definitions when Unreleased is the last section.
    let mut body_end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(unr + 1) {
        if line.starts_with("## ") || is_link_def(line) {
            body_end = i;
            break;
        }
    }

    // The immediately following section names the previous release — the
    // base of the new version's compare link.
    let prev = if body_end < lines.len() && lines[body_end].starts_with("## ") {
        let name = section_name(lines[body_end]).unwrap();
        let (_, v) = parse_version(name).ok_or_else(|| {
            format!(
                "`{}` is not a `## [X.Y.Z]` version section",
                lines[body_end]
            )
        })?;
        Some((name, v))
    } else {
        None
    };
    if let Some((name, pv)) = &prev {
        if ver <= *pv {
            return Err(format!(
                "version {version} is not newer than the latest section {name}"
            ));
        }
    }

    // The moved content is the Unreleased body minus its leading/trailing
    // blanks; `###` headings with no `- ` items do not come along.
    let body = &lines[unr + 1..body_end];
    let mut start = 0;
    let mut end = body.len();
    while start < end && body[start].trim().is_empty() {
        start += 1;
    }
    while end > start && body[end - 1].trim().is_empty() {
        end -= 1;
    }
    let inner = &body[start..end];

    let content: Vec<String> = if inner.iter().any(|l| is_item(l)) {
        drop_empty_headings(inner)
    } else if allow_empty {
        vec![
            "### Changed".to_string(),
            String::new(),
            "- No notable changes.".to_string(),
        ]
    } else {
        return Err(
            "the `## [Unreleased]` section has no entries to release — add \
             changelog entries under `## [Unreleased]`, or pass `--allow-empty` \
             to cut a `- No notable changes.` section"
                .to_string(),
        );
    };

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + content.len() + 4);
    out.extend(lines[..unr].iter().map(|l| l.to_string()));
    out.push("## [Unreleased]".to_string());
    out.push(String::new());
    out.push(format!("## [{version}] - {date}"));
    out.push(String::new());
    out.extend(content);
    out.push(String::new());
    out.extend(lines[body_end..].iter().map(|l| l.to_string()));

    // Footer links: repoint [Unreleased] at the new version and insert the
    // new version's compare link right after it.
    let pos = out
        .iter()
        .position(|l| l.starts_with("[Unreleased]:"))
        .ok_or("missing `[Unreleased]:` link definition")?;
    let url = out[pos]
        .split_once(':')
        .map(|(_, u)| u.trim())
        .unwrap_or_default();
    let base = url
        .strip_suffix("...HEAD")
        .and_then(|u| u.rsplit_once("/compare/").map(|(_, b)| b))
        .ok_or_else(|| format!("unrecognised `[Unreleased]:` compare link `{url}`"))?;
    let prefix = url
        .rsplit_once("/compare/")
        .map(|(p, _)| format!("{p}/compare/"))
        .unwrap();
    let v = if base.starts_with('v') { "v" } else { "" };

    out[pos] = format!("[Unreleased]: {prefix}{v}{version}...HEAD");
    let new_link = match prev {
        Some((name, _)) => format!("[{version}]: {prefix}{v}{name}...{v}{version}"),
        None => {
            let repo = prefix.trim_end_matches("/compare/");
            format!("[{version}]: {repo}/releases/tag/{v}{version}")
        }
    };
    out.insert(pos + 1, new_link);

    Ok(out.join("\n"))
}

/// The `<name>` of a `## [<name>] ...` section heading.
fn section_name(line: &str) -> Option<&str> {
    line.strip_prefix("## [")?.split(']').next()
}

/// A footer link definition line: `[name]: target`.
fn is_link_def(line: &str) -> bool {
    line.starts_with('[')
        && line
            .find(']')
            .is_some_and(|i| line[i + 1..].starts_with(':') && !line[i + 2..].is_empty())
}

/// A changelog entry line (top-level or nested bullet).
fn is_item(line: &str) -> bool {
    line.trim_start().starts_with("- ")
}

/// A numeric `X.Y.Z` as a comparable tuple plus the original string. Each
/// component must be ASCII digits only — checked before parsing because
/// `u64::parse` would otherwise accept a leading `+`.
fn parse_version(s: &str) -> Option<(&str, (u64, u64, u64))> {
    let digits = |p: Option<&str>| -> Option<u64> {
        let p = p?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let mut parts = s.split('.');
    let v = (
        digits(parts.next())?,
        digits(parts.next())?,
        digits(parts.next())?,
    );
    if parts.next().is_some() {
        return None;
    }
    Some((s, v))
}

/// Strict `YYYY-MM-DD` with sane month/day ranges.
fn valid_date(s: &str) -> bool {
    let digits = |r: &str| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit());
    s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && digits(&s[0..4])
        && digits(&s[5..7])
        && digits(&s[8..10])
        && (1..=12).contains(&s[5..7].parse::<u32>().unwrap_or(0))
        && (1..=31).contains(&s[8..10].parse::<u32>().unwrap_or(0))
}

/// Copy `inner` minus `###` headings whose section holds no `- ` items —
/// an empty heading would read as a promised category with nothing in it.
fn drop_empty_headings(inner: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let line = inner[i];
        if !line.starts_with("### ") {
            out.push(line.to_string());
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut has = false;
        while j < inner.len() && !inner[j].starts_with("### ") {
            has |= is_item(inner[j]);
            j += 1;
        }
        if has {
            out.push(line.to_string());
            i += 1;
        } else {
            // Drop the heading and the blank lines it leaves behind.
            i += 1;
            while i < inner.len() && inner[i].trim().is_empty() {
                i += 1;
            }
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "\
# Changelog

## [Unreleased]

### Breaking changes

- Old breaking.

### Added

- An added thing.

### Fixed

## [0.5.3] - 2026-11-11

### Added

- Something older.

[Unreleased]: https://github.com/hytfjwr/dbtl/compare/v0.5.3...HEAD
[0.5.3]: https://github.com/hytfjwr/dbtl/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/hytfjwr/dbtl/releases/tag/v0.5.2
";

    #[test]
    fn cuts_a_dated_section_and_repoints_links() {
        let out = release_section(BASE, "0.6.0", "2026-11-12", false).unwrap();
        let expected = "\
# Changelog

## [Unreleased]

## [0.6.0] - 2026-11-12

### Breaking changes

- Old breaking.

### Added

- An added thing.

## [0.5.3] - 2026-11-11

### Added

- Something older.

[Unreleased]: https://github.com/hytfjwr/dbtl/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/hytfjwr/dbtl/compare/v0.5.3...v0.6.0
[0.5.3]: https://github.com/hytfjwr/dbtl/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/hytfjwr/dbtl/releases/tag/v0.5.2
";
        assert_eq!(out, expected);
    }

    #[test]
    fn empty_unreleased_is_an_error() {
        let input = BASE.replace("### Breaking changes\n\n- Old breaking.\n\n", "");
        let input = input.replace("### Added\n\n- An added thing.\n\n", "");
        // Only the empty `### Fixed` heading remains under Unreleased.
        let err = release_section(&input, "0.6.0", "2026-11-12", false).unwrap_err();
        assert!(
            err.contains("--allow-empty"),
            "guidance names the flag: {err}"
        );
        let out = release_section(&input, "0.6.0", "2026-11-12", true).unwrap();
        assert!(
            out.contains("## [0.6.0] - 2026-11-12\n\n### Changed\n\n- No notable changes.\n"),
            "placeholder section:\n{out}"
        );
        assert!(
            !out.contains("### Fixed"),
            "empty headings stay home:\n{out}"
        );
    }

    #[test]
    fn existing_version_is_an_error() {
        let err = release_section(BASE, "0.5.3", "2026-11-20", false).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn older_version_is_an_error() {
        let err = release_section(BASE, "0.5.0", "2026-11-20", false).unwrap_err();
        assert!(err.contains("not newer"), "{err}");
    }

    #[test]
    fn malformed_inputs_are_errors() {
        for v in ["", "0.6", "0.6.x", "v0.6.0", "0.6.0.1"] {
            assert!(
                release_section(BASE, v, "2026-11-12", false).is_err(),
                "{v}"
            );
        }
        for d in ["", "2026-1-1", "2026-13-01", "2026-11-32", "Nov 12"] {
            assert!(release_section(BASE, "0.6.0", d, false).is_err(), "{d}");
        }
    }

    #[test]
    fn signed_components_are_errors() {
        // `u64::parse` accepts a leading `+`; version components must not.
        for v in ["+0.6.0", "0.+6.0", "0.6.+3"] {
            assert!(
                release_section(BASE, v, "2026-11-12", false).is_err(),
                "{v}"
            );
        }
    }

    #[test]
    fn first_release_links_to_the_tag() {
        let input = BASE
            .replace(
                "\n## [0.5.3] - 2026-11-11\n\n### Added\n\n- Something older.\n",
                "",
            )
            .replace(
                "[0.5.3]: https://github.com/hytfjwr/dbtl/compare/v0.5.2...v0.5.3\n",
                "",
            )
            .replace(
                "[0.5.2]: https://github.com/hytfjwr/dbtl/releases/tag/v0.5.2\n",
                "",
            );
        let out = release_section(&input, "0.6.0", "2026-11-12", false).unwrap();
        assert!(
            out.contains("[0.6.0]: https://github.com/hytfjwr/dbtl/releases/tag/v0.6.0"),
            "{out}"
        );
    }

    #[test]
    fn missing_links_are_errors() {
        let input = BASE.replace(
            "[Unreleased]: https://github.com/hytfjwr/dbtl/compare/v0.5.3...HEAD\n",
            "",
        );
        assert!(release_section(&input, "0.6.0", "2026-11-12", false).is_err());
        let input = BASE.replace(
            "[Unreleased]: https://github.com/hytfjwr/dbtl/compare/v0.5.3...HEAD",
            "[Unreleased]: https://example.com/releases",
        );
        assert!(release_section(&input, "0.6.0", "2026-11-12", false).is_err());
    }

    #[test]
    fn crlf_line_endings_are_preserved() {
        let input = BASE.replace('\n', "\r\n");
        let out = release_section(&input, "0.6.0", "2026-11-12", false).unwrap();
        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "no bare LF survives"
        );
        assert!(out.contains("## [0.6.0] - 2026-11-12\r\n"));
    }
}
