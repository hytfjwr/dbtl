//! The `sarif` format — [SARIF 2.1.0](https://docs.oasis-open.org/sarif/sarif/v2.1.0/),
//! the static-analysis interchange GitHub code scanning and most review tools
//! ingest. `tool.driver.rules` declares the WHOLE registry (a consumer can
//! document a rule that never fired); `results` carries the node-bound
//! counted findings — gates without findings (coverage, docs drift) and
//! baselined findings are never results. Each result fingerprints to the
//! finding's stable key so re-uploads dedupe.

use super::*;
use serde_json::json;

pub fn render(report: &Report, ctx: &RenderCtx<'_>) -> String {
    let rules: Vec<serde_json::Value> = dbtl::REGISTRY
        .iter()
        .map(|info| {
            json!({
                "id": info.id,
                "shortDescription": { "text": info.summary },
                "fullDescription": { "text": info.rationale },
                "help": { "text": info.help },
                "helpUri": info.rule.docs_url(),
                "defaultConfiguration": { "level": sarif_level(info.default_severity) },
                "properties": {
                    "category": info.category.name(),
                    "requires": info.requires,
                },
            })
        })
        .collect();

    let results: Vec<serde_json::Value> = report
        .gates
        .iter()
        .flat_map(|g| g.counted())
        .map(|f| {
            let mut result = json!({
                "ruleId": f.rule.name(),
                "level": sarif_level(f.severity),
                "message": { "text": f.message },
                "partialFingerprints": { "dbtl.finding/v1": finding_key(f) },
            });
            if let Some(loc) = &f.location {
                let uri = match ctx.path_prefix {
                    Some(prefix) => format!("{prefix}/{}", loc.file),
                    None => loc.file.clone(),
                };
                // Code scanning needs a `startLine`, so the file-scoped
                // findings report 1 here — the one format allowed to
                // guess. Columns and ends emit only when known AND inside
                // the `line` gate: a column without its line would point
                // at nothing.
                let mut region = json!({ "startLine": loc.line.unwrap_or(1) });
                if loc.line.is_some() {
                    if let Some(column) = loc.column {
                        region["startColumn"] = json!(column);
                    }
                    if let Some(end_line) = loc.end_line {
                        region["endLine"] = json!(end_line);
                    }
                    if let Some(end_column) = loc.end_column {
                        region["endColumn"] = json!(end_column);
                    }
                }
                result["locations"] = json!([{
                    "physicalLocation": {
                        "artifactLocation": { "uri": uri },
                        "region": region,
                    },
                }]);
            }
            result
        })
        .collect();

    let doc = json!({
        "version": "2.1.0",
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "runs": [{
            // Our columns count `char`s, which SARIF consumers read as
            // Unicode code points.
            "columnKind": "unicodeCodePoints",
            "tool": {
                "driver": {
                    "name": "dbtl",
                    "version": report.tool_version,
                    "informationUri": env!("CARGO_PKG_REPOSITORY"),
                    "rules": rules,
                },
            },
            "results": results,
        }],
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).expect("sarif serializes")
    )
}

/// SARIF `level` vocabulary — `dbtl`'s `warn` maps to `warning`.
fn sarif_level(severity: dbtl::Severity) -> &'static str {
    match severity {
        dbtl::Severity::Error => "error",
        dbtl::Severity::Warn => "warning",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::report::samples::*;

    /// The document shape: SARIF 2.1.0, the driver's rule table is exactly
    /// the registry, and only counted node-bound findings land in `results`.
    #[test]
    fn document_shape_and_rule_table() {
        let out = render(&report(), &ctx(Some("sub")));
        let doc: serde_json::Value = serde_json::from_str(&out).expect("sarif parses");
        assert_eq!(doc["version"], "2.1.0");
        assert_eq!(
            doc["runs"][0]["columnKind"], "unicodeCodePoints",
            "columns count `char`s — declared to consumers"
        );
        let driver = &doc["runs"][0]["tool"]["driver"];
        let rule_ids: std::collections::BTreeSet<String> = driver["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        let registry_ids: std::collections::BTreeSet<String> =
            dbtl::REGISTRY.iter().map(|i| i.id.to_string()).collect();
        assert_eq!(rule_ids, registry_ids, "driver declares the whole registry");

        let results = doc["runs"][0]["results"].as_array().unwrap();
        // 2 counted errors + 1 counted warn; the baselined finding and the
        // finding-free gates (coverage, skip) emit nothing.
        assert_eq!(results.len(), 3);
        let first = &results[0];
        assert_eq!(first["ruleId"], "root_models");
        assert_eq!(first["level"], "error");
        assert_eq!(
            first["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "sub/models/a.sql",
            "--path-prefix lands on artifact URIs"
        );
        // The full known region: start + exclusive end, all 1-based.
        let region = &first["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"], 7);
        assert_eq!(region["startColumn"], 3);
        assert_eq!(region["endLine"], 7);
        assert_eq!(region["endColumn"], 5);
        assert_eq!(
            first["partialFingerprints"]["dbtl.finding/v1"],
            "root_models/model.proj.a/"
        );
        // The file-less finding emits no locations array but stays a result.
        let warn = results
            .iter()
            .find(|r| r["ruleId"] == "undocumented_models")
            .unwrap();
        assert_eq!(warn["level"], "warning");
        assert!(warn.get("locations").is_none());
    }

    /// Every driver rule carries its fix guidance and its reference anchor:
    /// `help.text` is the registry `help` and `helpUri` points at the rule's
    /// `#<id>` anchor on the lint-rules page.
    #[test]
    fn rule_table_carries_help_and_docs_links() {
        let out = render(&report(), &ctx(None));
        let doc: serde_json::Value = serde_json::from_str(&out).expect("sarif parses");
        let rules = doc["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap();
        for (r, info) in rules.iter().zip(dbtl::REGISTRY.iter()) {
            assert_eq!(r["help"]["text"], info.help, "{} help.text", info.id);
            assert_eq!(
                r["helpUri"],
                format!("{}#{}", dbtl::LINT_RULE_DOCS_BASE, info.id),
                "{} helpUri",
                info.id
            );
        }
    }

    /// A file-scoped finding reports `startLine: 1` — the one place a
    /// fallback is allowed, because code scanning requires `startLine` —
    /// and no other region fields are invented.
    #[test]
    fn unpositioned_finding_keeps_only_the_required_start_line() {
        let mut r = Report::new("proj", "root project only");
        r.gates = vec![gate(
            "lint:root_models",
            "lint root_models",
            dbtl::Severity::Error,
            GateStatus::Fail,
            "lint root_models: 1 violation(s): a",
            vec![unpositioned(
                dbtl::LintRule::RootModels,
                "model.proj.a",
                "a has no parents",
                dbtl::Severity::Error,
                "models/a.sql",
                false,
            )],
        )];
        r.summary = Report::summarize(&r.gates);
        let out = render(&r, &ctx(None));
        let doc: serde_json::Value = serde_json::from_str(&out).expect("sarif parses");
        let region = &doc["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"], 1);
        assert!(
            region.get("startColumn").is_none()
                && region.get("endLine").is_none()
                && region.get("endColumn").is_none(),
            "nothing else is invented: {region}"
        );
    }
}
