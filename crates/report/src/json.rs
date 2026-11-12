//! The `json` format — the [`Report`] document serialized as-is, pretty
//! printed. This is the canonical machine form: everything the other
//! formats render derives from the same structure a JSON consumer reads
//! here.

use super::*;

pub fn render(report: &Report, _ctx: &RenderCtx<'_>) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(report).expect("the report model serializes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::*;

    /// The canonical document: schema marker, gates, summary — and the
    /// conditional blocks absent when nothing engaged them.
    #[test]
    fn serializes_the_model_verbatim() {
        let out = render(&report(), &ctx(None));
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(v["schema"], "dbtl.report/v1");
        assert_eq!(v["gates"].as_array().unwrap().len(), 4);
        assert_eq!(v["summary"]["errors"], 2);
        assert_eq!(v["gates"][1]["findings"][2]["baselined"], true);
        assert!(v.get("impact").is_none() && v.get("docs_drift").is_none());
    }
}
