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
                    dbtl::Severity::Error => "major",
                    dbtl::Severity::Warn => "minor",
                },
                "fingerprint": format!("{:016x}", fnv1a_64(finding_key(f).as_bytes())),
            });
            // `location.path` must be a real repository path — a finding
            // without a file location omits the block rather than filing the
            // issue against a `unique_id` that resolves to no file.
            if let Some(loc) = &f.location {
                let path = match ctx.path_prefix {
                    Some(prefix) => format!("{prefix}/{}", loc.file),
                    None => loc.file.clone(),
                };
                issue["location"] = json!({
                    "path": path,
                    "lines": { "begin": loc.line.unwrap_or(1) },
                });
            }
            issue
        })
        .collect();
    format!(
        "{}\n",
        serde_json::to_string_pretty(&issues).expect("issues serialize")
    )
}

/// Map a lint [`dbtl::Category`] onto the Code Climate remediation-category
/// vocabulary (`Bug Risk`, `Clarity`, `Compatibility`, `Complexity`,
/// `Duplication`, `Performance`, `Security`, `Style`) — the fixed taxonomy
/// GitLab groups findings by.
fn category(c: dbtl::Category) -> &'static str {
    match c {
        dbtl::Category::Modeling => "Complexity",
        dbtl::Category::Testing => "Bug Risk",
        dbtl::Category::Documentation => "Clarity",
        dbtl::Category::Structure => "Style",
        dbtl::Category::Performance => "Performance",
        dbtl::Category::Governance | dbtl::Category::Warehouse => "Compatibility",
        dbtl::Category::Sql => "Bug Risk",
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
    use super::fnv1a_64;

    /// The published FNV-1a-64 reference vectors pin the implementation —
    /// a fingerprint that drifted would silently re-file every issue.
    #[test]
    fn fnv1a_matches_reference_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171f73967e8);
    }
}
