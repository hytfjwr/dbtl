//! Repository-text hygiene: comments, docs, names, and messages must not
//! reference local-only planning documents — issue numbers, section
//! symbols, planning codes, [s]print/[o]rchestration labels, or spec-doc
//! names. A reference points at a file the reader does not have, so it
//! explains nothing.
//!
//! Every pattern below is written decomposed — `[i]ssue`, `\u{a7}` — so
//! this file's own source never matches them and the whole tree is scanned
//! with no exclusion list. `target/` and non-UTF-8 files are skipped.
//!
//! CJK text is deliberately NOT a pattern: kana and kanji are legitimate
//! test data (cell-width inputs, SQL highlighting strings), and a regex
//! cannot tell quoted planning prose from data without an exclusion list.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

/// (what the failure line calls it, the pattern). The planning-code
/// pattern stays case-sensitive so variable names like `m1`/`t0` are not
/// caught; the rest are insensitive.
const PATTERNS: &[(&str, &str)] = &[
    ("issue-number reference", r"(?i)[i]ssue[\s_-]?\d"),
    ("section symbol", r"\u{a7}"),
    ("planning code", r"\b[MTH]\d(-[a-z])?\b"),
    ("[s]print label", r"(?i)[s]print"),
    ("[o]rchestration label", r"(?i)[o]rchestration"),
    ("spec-doc name", r"(?i)\b([t]he|[p]er|[s]creen)[ -]spec\b"),
    ("spec-doc code", r"[s]pec D\d"),
];

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "target" {
                collect(&path, out);
            }
        } else {
            out.push(path);
        }
    }
}

#[test]
fn repository_text_carries_no_local_planning_references() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![root.join("README.md")];
    for dir in ["src", "crates", "tests", "benches"] {
        collect(&root.join(dir), &mut files);
    }

    let patterns: Vec<(&str, Regex)> = PATTERNS
        .iter()
        .map(|(label, pat)| (*label, Regex::new(pat).expect("pattern compiles")))
        .collect();

    let mut hits = Vec::new();
    for file in &files {
        // Non-UTF-8 files are out of scope.
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let rel = file.strip_prefix(root).unwrap_or(file);
        for (line_no, line) in text.lines().enumerate() {
            for (label, re) in &patterns {
                if re.is_match(line) {
                    hits.push(format!(
                        "{}:{}: {} ({})",
                        rel.display(),
                        line_no + 1,
                        line.trim(),
                        label
                    ));
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "reference to a local planning document — describe the content in \
         words instead of naming the document:\n{}",
        hits.join("\n")
    );
}
