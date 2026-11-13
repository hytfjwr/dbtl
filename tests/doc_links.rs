//! Repository documentation integrity: every relative link and `#anchor`
//! in the committed Markdown resolves, no file repeats a heading anchor,
//! and every `dbtl` invocation shown in a fenced code block or an inline
//! `code` span names a real subcommand with flags that subcommand accepts.
//! A fixture-command table also proves the documented workflows execute
//! with their documented exit codes.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use common::{dbtl_cmd, manifest_path, repo_root};

/// The Markdown files whose links are part of the public surface: the
/// top-level docs plus everything under docs/ (hand-written and generated)
/// and the PR template. Issue templates are YAML, not Markdown.
fn markdown_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = vec![
        root.join("README.md"),
        root.join("CHANGELOG.md"),
        root.join("CONTRIBUTING.md"),
        root.join("RELEASING.md"),
        root.join("SECURITY.md"),
        root.join(".github/PULL_REQUEST_TEMPLATE.md"),
    ];
    collect_md(&root.join("docs"), &mut files);
    files.sort();
    files
}

fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_md(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// The files whose `dbtl` invocations are checked: the repo docs above plus
/// the site content pages. Site pages join the command check only — their
/// relative links resolve to site routes, not repo paths, and
/// `site/scripts/check-links.mjs` already validates them.
fn command_files() -> Vec<PathBuf> {
    let mut files = markdown_files();
    collect_md(&repo_root().join("site/src/content/docs"), &mut files);
    files.sort();
    files
}

/// GitHub's anchor algorithm: lowercase, keep letters/digits/underscore/
/// hyphen, drop every other punctuation mark, turn spaces into hyphens.
fn slug(heading: &str) -> String {
    let mut out = String::new();
    for ch in heading.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('-');
        }
    }
    out
}

/// Heading slugs of one file, with the fenced code blocks removed (a `#`
/// inside a ``` block is a comment, not a heading).
fn heading_slugs(text: &str) -> Vec<String> {
    let mut in_fence = false;
    let mut slugs = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !line.starts_with('#') {
            continue;
        }
        let level = line.chars().take_while(|&c| c == '#').count();
        if level <= 6 && line[level..].starts_with([' ', '\t']) {
            slugs.push(slug(line[level..].trim()));
        }
    }
    slugs
}

/// Link targets in one file: `[text](target)` plus `href="target"` (the
/// README's HTML nav). Fenced code blocks are excluded. External URLs,
/// `mailto:`, and fragments are separated out by the caller.
fn link_targets(text: &str) -> Vec<String> {
    let mut in_fence = false;
    let mut targets = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
                if let Some(end) = line[i + 2..].find(')') {
                    let target = line[i + 2..i + 2 + end].trim();
                    // `](<target>)` and `](target "title")` forms.
                    let target = target
                        .strip_prefix('<')
                        .and_then(|t| t.split('>').next())
                        .unwrap_or_else(|| target.split_whitespace().next().unwrap_or(target));
                    if !target.is_empty() {
                        targets.push(target.to_string());
                    }
                    i += end + 3;
                    continue;
                }
            }
            if line
                .get(i..)
                .is_some_and(|rest| rest.starts_with("href=\""))
            {
                if let Some(end) = line[i + 6..].find('"') {
                    targets.push(line[i + 6..i + 6 + end].to_string());
                    i += end + 7;
                    continue;
                }
            }
            i += 1;
        }
    }
    targets
}

#[test]
fn relative_links_and_anchors_resolve() {
    let mut failures = Vec::new();
    let slug_cache: BTreeMap<PathBuf, Vec<String>> = markdown_files()
        .iter()
        .map(|f| {
            (
                f.clone(),
                heading_slugs(&std::fs::read_to_string(f).unwrap()),
            )
        })
        .collect();

    for file in markdown_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        for target in link_targets(&text) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let (path_part, anchor) = match target.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (target.as_str(), None),
            };
            let resolved = if path_part.is_empty() {
                file.clone()
            } else {
                file.parent().unwrap().join(path_part)
            };
            if !resolved.exists() {
                failures.push(format!(
                    "{} -> {target} ({} does not exist)",
                    file.display(),
                    resolved.display()
                ));
                continue;
            }
            if let Some(anchor) = anchor {
                let slugs = match slug_cache.get(&resolved) {
                    Some(s) => s.clone(),
                    None => heading_slugs(
                        &std::fs::read_to_string(&resolved)
                            .unwrap_or_else(|e| panic!("read {}: {e}", resolved.display())),
                    ),
                };
                if !slugs.iter().any(|s| s == anchor) {
                    failures.push(format!(
                        "{} -> {target} (no heading anchors to #{anchor})",
                        file.display()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "broken links:\n{}",
        failures.join("\n")
    );
}

/// Two headings with the same slug are only a collision when they sit in
/// the same section — a changelog legitimately repeats `### Added` under
/// each `## [version]`. A `#`/`##` heading starts a new scope.
#[test]
fn no_duplicate_heading_anchors() {
    let mut failures = Vec::new();
    for file in markdown_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        let mut in_fence = false;
        let mut seen = BTreeSet::new();
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || !line.starts_with('#') {
                continue;
            }
            let level = line.chars().take_while(|&c| c == '#').count();
            if level > 6 || !line[level..].starts_with([' ', '\t']) {
                continue;
            }
            if level <= 2 {
                seen.clear();
            }
            let s = slug(line[level..].trim());
            if !seen.insert(s.clone()) {
                failures.push(format!(
                    "{} has two headings anchoring to #{s} in one section",
                    file.display()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "duplicate headings:\n{}",
        failures.join("\n")
    );
}

// ---- documented commands -------------------------------------------------

/// `dbtl ...` invocations, from fenced ```console|sh|bash|shell|zsh|yaml
/// blocks (with `\`-continuations joined) and from inline `code` spans.
/// Inside a scanned fence a `dbtl ` anywhere before the line's first `#`
/// comment marks a documented invocation (`dbtl …`, `$ dbtl …`, `run: dbtl
/// …`, `- dbtl …`); everything before it is a prompt or a list marker.
/// Returns (command, file, line number).
fn documented_commands() -> Vec<(String, PathBuf, usize)> {
    let mut out = Vec::new();
    for file in command_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        let mut in_fence = false;
        let mut fence_scans = false;
        let mut pending: Option<(String, usize)> = None;
        for (idx, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                if in_fence {
                    in_fence = false;
                    pending = None; // an unterminated continuation dies with its block
                } else {
                    in_fence = true;
                    let lang = line.trim_start().trim_start_matches('`').trim();
                    fence_scans = matches!(
                        lang,
                        "console" | "sh" | "bash" | "shell" | "zsh" | "yaml" | "yml"
                    );
                }
                continue;
            }
            if in_fence {
                if !fence_scans {
                    continue;
                }
                // The first `#` starts a comment in every scanned fence
                // language — a `dbtl` mention after it is prose, not a
                // documented invocation, and quoting one inside a comment
                // would smuggle a stray backtick into the command.
                let code = line.split('#').next().unwrap_or(line);
                if let Some((mut cmd, start)) = pending.take() {
                    let trimmed = code.trim_end();
                    cmd.push(' ');
                    cmd.push_str(trimmed.trim_end_matches('\\').trim());
                    if trimmed.ends_with('\\') {
                        pending = Some((cmd, start));
                    } else {
                        out.push((cmd, file.clone(), start));
                    }
                    continue;
                }
                if let Some(pos) = code.find("dbtl ") {
                    let rest = code[pos + 5..].trim_end();
                    let cmd = format!("dbtl {}", rest.trim_end_matches('\\').trim_end());
                    if rest.ends_with('\\') {
                        pending = Some((cmd, idx + 1));
                    } else {
                        out.push((cmd, file.clone(), idx + 1));
                    }
                }
                continue;
            }
            // Prose: inline `code` spans that start with `dbtl ` are commands
            // (`` `dbtl` `` alone — no trailing space — is a product mention).
            let mut rest = line;
            while let Some(open) = rest.find('`') {
                let after = &rest[open + 1..];
                match after.find('`') {
                    Some(close) => {
                        if let Some(cmd) = after[..close].strip_prefix("dbtl ") {
                            out.push((format!("dbtl {}", cmd.trim()), file.clone(), idx + 1));
                        }
                        rest = &after[close + 1..];
                    }
                    None => break,
                }
            }
        }
    }
    out
}

/// Every `--long` flag each command accepts, read from its `--help` surface
/// so the check can't drift from the implementation. The root set covers
/// the TUI (global + TUI-only flags); a subcommand's set is its own args
/// plus the global flags it consumes — `dbtl <sub> --help` is pruned to
/// exactly those.
fn accepted_flags() -> (
    BTreeSet<String>,
    BTreeMap<String, BTreeSet<String>>,
    BTreeSet<String>,
) {
    const SUBCOMMANDS: [&str; 9] = [
        "docs",
        "impact",
        "check",
        "lint",
        "ls",
        "columns",
        "completions",
        "man",
        "cli-reference",
    ];
    let scrape = |args: &[&str]| -> BTreeSet<String> {
        let out = dbtl_cmd()
            .args(args)
            .arg("--help")
            .output()
            .expect("dbtl --help runs");
        let text = String::from_utf8_lossy(&out.stdout);
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .filter(|w| w.starts_with("--") && w.len() > 2)
            .map(str::to_string)
            .collect()
    };
    let root = scrape(&[]);
    let mut per_sub = BTreeMap::new();
    let mut subs = BTreeSet::new();
    for sub in SUBCOMMANDS {
        subs.insert(sub.to_string());
        per_sub.insert(sub.to_string(), scrape(&[sub]));
    }
    // `help` is clap's built-in pseudo-subcommand — it anchors a documented
    // command; the root set is the lenient validation for it.
    subs.insert("help".to_string());
    per_sub.insert("help".to_string(), root.clone());
    (root, per_sub, subs)
}

#[test]
fn documented_subcommands_and_flags_exist() {
    let (root_flags, sub_flags, subs) = accepted_flags();
    let mut failures = Vec::new();
    for (cmd, file, line) in documented_commands() {
        let tokens: Vec<&str> = cmd.split_whitespace().collect();
        // Any token equal to a real subcommand anchors the command; flags
        // validate against THAT command's accepted set — a global flag the
        // subcommand doesn't consume is a usage error, so documenting one
        // is a doc bug. Other bare words are positional args (paths,
        // selectors, node names).
        let positioned = tokens.iter().position(|t| subs.contains(*t));
        let accepted = positioned.map_or(&root_flags, |i| &sub_flags[tokens[i]]);
        for token in &tokens {
            if let Some(flag) = token.strip_prefix("--") {
                let name = flag.split('=').next().unwrap_or(flag);
                let candidate = format!("--{name}");
                if !accepted.contains(&candidate) {
                    let context = positioned
                        .map(|i| format!("'{}'", tokens[i]))
                        .unwrap_or_else(|| "the TUI".to_string());
                    failures.push(format!(
                        "{}:{line} `{cmd}` — flag {candidate} is not accepted by {context}",
                        file.display()
                    ));
                }
            }
        }
        // A documented invocation must name a known subcommand or only
        // root flags/args — catch a typo'd subcommand like `dbtl lnt`.
        if positioned.is_none()
            && tokens
                .get(1)
                .is_some_and(|t| !t.starts_with('-') && !t.contains('/') && !t.contains(':'))
        {
            // Bare first word that isn't a path/selector — suspicious, but
            // model names like `dbtl stg_orders` never occur in docs.
            let first = tokens[1];
            failures.push(format!(
                "{}:{line} `{cmd}` — `{first}` is not a subcommand or flag",
                file.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "invalid documented commands:\n{}",
        failures.join("\n")
    );
}

// ---- fixture execution ---------------------------------------------------

/// The documented commands that run against the bundled fixture manifest,
/// with the exit code the docs promise. `lint` exits 1 because the fixture
/// genuinely violates error-severity rules — that, too, is documented.
#[test]
fn documented_fixture_commands_execute() {
    let manifest = manifest_path();
    let m = manifest.to_str().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let docs_dir = tmp.path().join("docs-out");

    let cases: Vec<(Vec<String>, i32)> = vec![
        // Non-interactive subcommands over the fixture.
        (vec!["check".into(), "--manifest".into(), m.into()], 0),
        (
            vec![
                "check".into(),
                "--manifest".into(),
                m.into(),
                "--lint".into(),
                "--format".into(),
                "json".into(),
            ],
            1,
        ),
        (
            vec![
                "lint".into(),
                "--manifest".into(),
                m.into(),
                "--format".into(),
                "text".into(),
            ],
            1,
        ),
        (
            vec![
                "lint".into(),
                "--manifest".into(),
                m.into(),
                "--list-rules".into(),
            ],
            0,
        ),
        (
            vec![
                "lint".into(),
                "--manifest".into(),
                m.into(),
                "--explain".into(),
                "model_fanout".into(),
            ],
            0,
        ),
        (
            vec!["ls".into(), "--manifest".into(), m.into(), "stg_*".into()],
            0,
        ),
        (
            vec![
                "impact".into(),
                "--manifest".into(),
                m.into(),
                "--diff".into(),
                m.into(),
            ],
            0,
        ),
        (
            vec![
                "docs".into(),
                "--manifest".into(),
                m.into(),
                "--out".into(),
                docs_dir.to_str().unwrap().into(),
            ],
            0,
        ),
        (
            vec![
                "columns".into(),
                "--manifest".into(),
                m.into(),
                "seed.jaffle_finance.budgets.id".into(),
            ],
            0,
        ),
        (vec!["completions".into(), "bash".into()], 0),
        (vec!["man".into()], 0),
        // Global flags work before the subcommand too.
        (vec!["--manifest".into(), m.into(), "check".into()], 0),
    ];

    for (args, expected) in cases {
        let out = dbtl_cmd().args(&args).output().expect("dbtl runs");
        let code = out.status.code().unwrap_or(-1);
        assert_eq!(
            code,
            expected,
            "`dbtl {}` exited {code}, expected {expected}:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // docs --check agrees the tree it just wrote is fresh (exit 0).
    let out = dbtl_cmd()
        .args([
            "docs",
            "--manifest",
            m,
            "--out",
            docs_dir.to_str().unwrap(),
            "--check",
        ])
        .output()
        .expect("dbtl docs --check runs");
    assert!(
        out.status.success(),
        "docs --check on a fresh tree: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
