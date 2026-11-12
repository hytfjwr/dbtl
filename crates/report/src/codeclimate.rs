//! The `codeclimate` format — the Code Quality JSON array GitLab's
//! `codequality` artifact and Code Climate ingest. One issue per counted
//! finding; the fingerprint is FNV-1a-64 of the finding's stable key so the
//! same finding keeps the same identity across runs (GitLab dedupes on it)
//! and different findings never collide accidentally.

use super::*;
use serde_json::json;

pub fn render(report: &Report, ctx: &RenderCtx<'_>) -> String {
    let issues: Vec<serde_json::Value> = report
        .gates
        .iter()
        .flat_map(|g| g.counted())
        .map(|f| {
            let mut issue = json!({
                "type": "issue",
                "check_name": f.rule.name(),
                "description": f.message,
                "categories": [category(f.rule.info().category)],
                "severity": match f.severity {
                    dbtl_core::Severity::Error => "major",
                    dbtl_core::Severity::Warn => "minor",
                },
                "fingerprint": format!("{:016x}", fnv1a_64(finding_key(f).as_bytes())),
            });
            // `location.path` must be a real repository path — a finding
            // without a file location omits the block rather than filing the
            // issue against a `unique_id` that resolves to no file. When the
            // (line, column) pair is known the issue spans a `positions`
            // range (begin AND end, ends defaulting to the start for a
            // point); otherwise the line-only `lines` form carries whatever
            // line is known — with `begin` format-required, 1 is the
            // file-level stand-in.
            if let Some(loc) = &f.location {
                let path = match ctx.path_prefix {
                    Some(prefix) => format!("{prefix}/{}", loc.file),
                    None => loc.file.clone(),
                };
                // `positions` needs the (line, column) pair — a column
                // without its line points at nothing, so anything missing
                // a line falls back to the line-only `lines` form.
                issue["location"] = match (loc.line, loc.column) {
                    (Some(line), Some(column)) => json!({
                        "path": path,
                        "positions": {
                            "begin": { "line": line, "column": column },
                            "end": {
                                "line": loc.end_line.unwrap_or(line),
                                "column": loc.end_column.unwrap_or(column),
                            },
                        },
                    }),
                    _ => json!({
                        "path": path,
                        "lines": { "begin": loc.line.unwrap_or(1) },
                    }),
                };
            }
            issue
        })
        .collect();
    format!(
        "{}\n",
        serde_json::to_string_pretty(&issues).expect("issues serialize")
    )
}

/// Map a lint [`dbtl_core::Category`] onto the Code Climate remediation-category
/// vocabulary (`Bug Risk`, `Clarity`, `Compatibility`, `Complexity`,
/// `Duplication`, `Performance`, `Security`, `Style`) — the fixed taxonomy
/// GitLab groups findings by.
fn category(c: dbtl_core::Category) -> &'static str {
    match c {
        dbtl_core::Category::Modeling => "Complexity",
        dbtl_core::Category::Testing => "Bug Risk",
        dbtl_core::Category::Documentation => "Clarity",
        dbtl_core::Category::Structure => "Style",
        dbtl_core::Category::Performance => "Performance",
        dbtl_core::Category::Governance | dbtl_core::Category::Warehouse => "Compatibility",
        dbtl_core::Category::Sql => "Bug Risk",
    }
}

/// FNV-1a 64-bit — the small, well-defined hash the fingerprint contract
/// needs (no crate, no platform variance).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::*;

    /// The published FNV-1a-64 reference vectors pin the implementation —
    /// a fingerprint that drifted would silently re-file every issue.
    #[test]
    fn fnv1a_matches_reference_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171f73967e8);
    }

    /// Column-known findings carry a `positions` begin/end range; the
    /// file-scoped ones fall back to `lines.begin` — the format-required
    /// line stand-in.
    #[test]
    fn positions_when_columns_known_lines_otherwise() {
        let mut r = Report::new("proj", "root project only");
        r.gates = vec![gate(
            "lint:root_models",
            "lint root_models",
            dbtl_core::Severity::Error,
            GateStatus::Fail,
            "lint root_models: 2 violation(s): a, b",
            vec![
                finding(
                    dbtl_core::LintRule::RootModels,
                    "model.proj.a",
                    "a has no parents",
                    dbtl_core::Severity::Error,
                    Some("models/a.sql"),
                    false,
                ),
                unpositioned(
                    dbtl_core::LintRule::RootModels,
                    "model.proj.b",
                    "b has no parents",
                    dbtl_core::Severity::Error,
                    "models/b.sql",
                    false,
                ),
            ],
        )];
        r.summary = Report::summarize(&r.gates);
        let out = render(&r, &ctx(None));
        let v: serde_json::Value = serde_json::from_str(&out).expect("issues parse");
        let issues = v.as_array().unwrap();
        assert_eq!(issues.len(), 2);
        let pos = &issues[0]["location"]["positions"];
        assert_eq!(pos["begin"], serde_json::json!({"line": 7, "column": 3}));
        assert_eq!(pos["end"], serde_json::json!({"line": 7, "column": 5}));
        assert!(
            issues[0]["location"].get("lines").is_none(),
            "positions replaces lines"
        );
        let loc = &issues[1]["location"];
        assert_eq!(loc["lines"]["begin"], 1, "the format-required stand-in");
        assert!(
            loc.get("positions").is_none(),
            "no positions without columns"
        );
    }
}
