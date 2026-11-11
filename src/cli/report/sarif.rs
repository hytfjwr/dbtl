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
                result["locations"] = json!([{
                    "physicalLocation": {
                        "artifactLocation": { "uri": uri },
                        "region": { "startLine": loc.line.unwrap_or(1) },
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
        assert_eq!(
            first["locations"][0]["physicalLocation"]["region"]["startLine"],
            7
        );
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
}
