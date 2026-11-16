//! Thin `#[wasm_bindgen]` wrappers over the `*_json` cores below: JSON
//! strings in and out (`serde_json`), and every failure reaches JavaScript
//! as a thrown `Error` carrying the message the core produced. No logic
//! lives here — each method is a one-line `map_err` shell over a pure
//! core, so the boundary can never drift from what the native tests cover.

use crate::engine::{self, Engine};
use serde::de::DeserializeOwned;
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// The engine handle JavaScript holds: `new WasmEngine()`, `load(...)`
/// once per manifest, then any operation — each taking its arguments as a
/// JSON string and returning a JSON string. Failures throw an `Error`
/// whose message is the operation's CLI-flavored wording.
#[wasm_bindgen]
pub struct WasmEngine {
    engine: Option<Engine>,
}

// The `*_json` cores return `Result<_, String>` — the exact message the
// JavaScript caller sees — because `JsError` itself cannot be
// constructed off-wasm (`__wbindgen_error_new` panics on non-wasm
// targets). `js_err` wraps the message at the boundary; `EngineError`'s
// `Display` is the CLI wording, so `to_string` is the only conversion
// an engine failure ever needs.

/// Parse the operation's JSON argument object; a malformed payload is a
/// plain `Error`, never a panic.
fn parse_input<I: DeserializeOwned>(input: &str) -> Result<I, String> {
    serde_json::from_str(input).map_err(|e| format!("invalid input: {e}"))
}

/// Serialize an operation's output — infallible for the engine's
/// owned-value output types.
fn to_json<O: Serialize>(out: &O) -> String {
    serde_json::to_string(out).expect("engine outputs serialize")
}

/// Wrap a core's message as the `Error` JS catches.
fn js_err(message: String) -> JsError {
    JsError::new(&message)
}

/// The input shape for the single-string-argument operations.
#[derive(serde::Deserialize)]
struct UniqueIdInput {
    unique_id: String,
}

/// The loaded engine, or the error JS sees when an operation runs before
/// `load` succeeded.
fn loaded(engine: Option<&Engine>) -> Result<&Engine, String> {
    engine.ok_or_else(|| "no manifest loaded".to_string())
}

fn loaded_mut(engine: Option<&mut Engine>) -> Result<&mut Engine, String> {
    engine.ok_or_else(|| "no manifest loaded".to_string())
}

/// `load`'s core: parse → build the engine → serialize the report. The
/// caller stores the returned engine only on success.
fn load_json(input: &str) -> Result<(Engine, String), String> {
    let input: engine::LoadInput = parse_input(input)?;
    let (engine, out) = Engine::load(&input).map_err(|e| e.to_string())?;
    Ok((engine, to_json(&out)))
}

fn select_json(engine: Option<&Engine>, input: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Input {
        selector: String,
    }
    let input: Input = parse_input(input)?;
    let out = loaded(engine)?
        .select(&input.selector)
        .map_err(|e| e.to_string())?;
    Ok(to_json(&out))
}

fn node_detail_json(engine: Option<&Engine>, input: &str) -> Result<String, String> {
    let input: UniqueIdInput = parse_input(input)?;
    let out = loaded(engine)?
        .node_detail(&input.unique_id)
        .map_err(|e| e.to_string())?;
    Ok(to_json(&out))
}

fn lineage_json(engine: Option<&Engine>, input: &str) -> Result<String, String> {
    let input: engine::LineageInput = parse_input(input)?;
    let out = loaded(engine)?.lineage(&input).map_err(|e| e.to_string())?;
    Ok(to_json(&out))
}

fn lint_json(engine: Option<&Engine>, input: &str) -> Result<String, String> {
    let input: engine::LintOptions = parse_input(input)?;
    let out = loaded(engine)?.lint(&input).map_err(|e| e.to_string())?;
    Ok(to_json(&out))
}

fn columns_json(engine: Option<&Engine>, input: &str) -> Result<String, String> {
    let input: engine::ColumnsInput = parse_input(input)?;
    let out = loaded(engine)?.columns(&input).map_err(|e| e.to_string())?;
    Ok(to_json(&out))
}

fn replace_sql_json(engine: Option<&mut Engine>, input: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Input {
        unique_id: String,
        sql: String,
    }
    let input: Input = parse_input(input)?;
    let out = loaded_mut(engine)?
        .replace_sql(&input.unique_id, &input.sql)
        .map_err(|e| e.to_string())?;
    Ok(to_json(&out))
}

fn revert_sql_json(engine: Option<&mut Engine>, input: &str) -> Result<String, String> {
    let input: UniqueIdInput = parse_input(input)?;
    let out = loaded_mut(engine)?
        .revert_sql(&input.unique_id)
        .map_err(|e| e.to_string())?;
    Ok(to_json(&out))
}

#[wasm_bindgen]
impl WasmEngine {
    /// An unloaded engine — [`load`](WasmEngine::load) must succeed before
    /// any operation runs.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        WasmEngine { engine: None }
    }

    /// `{manifest, files?, include_packages?}` →
    /// `{project, counts, nodes, warnings}`.
    pub fn load(&mut self, input: &str) -> Result<String, JsError> {
        let (engine, out) = load_json(input).map_err(js_err)?;
        self.engine = Some(engine);
        Ok(out)
    }

    /// `{selector}` → `{nodes: [{unique_id, name, resource_type,
    /// materialized, path}]}` — `dbtl ls`'s match set.
    pub fn select(&self, input: &str) -> Result<String, JsError> {
        select_json(self.engine.as_ref(), input).map_err(js_err)
    }

    /// `{unique_id}` → the node-detail payload (name, kind, path, raw SQL,
    /// compiled-SQL flag, declared columns).
    #[wasm_bindgen(js_name = nodeDetail)]
    pub fn node_detail(&self, input: &str) -> Result<String, JsError> {
        node_detail_json(self.engine.as_ref(), input).map_err(js_err)
    }

    /// `{root|roots, direction?, depth?, glyphs?, density?}` → the laid-out
    /// lineage grid (`rows`, `classes`, `emphasis`, `rects`, `selected`).
    pub fn lineage(&self, input: &str) -> Result<String, JsError> {
        lineage_json(self.engine.as_ref(), input).map_err(js_err)
    }

    /// `{config?, selector?, fail_on?}` → `{text, ansi, json, exit_code,
    /// error, warnings}` — `dbtl lint`'s output as data.
    pub fn lint(&self, input: &str) -> Result<String, JsError> {
        lint_json(self.engine.as_ref(), input).map_err(js_err)
    }

    /// `{target, upstream?}` → `{json, markdown, warnings}` —
    /// `dbtl columns <model>.<column>`.
    pub fn columns(&self, input: &str) -> Result<String, JsError> {
        columns_json(self.engine.as_ref(), input).map_err(js_err)
    }

    /// `{unique_id, sql}` → `{changed}` — rewrite a model/snapshot's raw
    /// SQL (and drop its stale compiled SQL) in memory.
    #[wasm_bindgen(js_name = replaceSql)]
    pub fn replace_sql(&mut self, input: &str) -> Result<String, JsError> {
        replace_sql_json(self.engine.as_mut(), input).map_err(js_err)
    }

    /// `{unique_id}` → `{changed}` — restore a node's SQL to its loaded
    /// state; `changed` is `false` when it was never edited.
    #[wasm_bindgen(js_name = revertSql)]
    pub fn revert_sql(&mut self, input: &str) -> Result<String, JsError> {
        revert_sql_json(self.engine.as_mut(), input).map_err(js_err)
    }
}

impl Default for WasmEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    /// `{manifest: "<manifest text>"}` — the minimal `load` argument.
    fn load_input() -> String {
        serde_json::json!({"manifest": fixture("manifest.json")}).to_string()
    }

    fn loaded_engine() -> Engine {
        load_json(&load_input()).expect("fixture loads").0
    }

    #[test]
    fn parse_input_reports_the_exact_js_message() {
        let id: UniqueIdInput = parse_input("{\"unique_id\":\"model.p.x\"}").unwrap();
        assert_eq!(id.unique_id, "model.p.x");
        // A missing required field and malformed JSON both read
        // `invalid input: <serde's message>` — the string JS catches.
        for input in ["{}", "not json", "{\"unique_id\":1}"] {
            let err = parse_input::<UniqueIdInput>(input).err().unwrap();
            assert!(err.starts_with("invalid input: "), "{input}: {err}");
        }
    }

    #[test]
    fn operations_before_load_report_unloaded() {
        let mut none: Option<Engine> = None;
        for res in [
            select_json(None, "{\"selector\":\"x\"}"),
            node_detail_json(None, "{\"unique_id\":\"x\"}"),
            lineage_json(None, "{}"),
            lint_json(None, "{}"),
            columns_json(None, "{\"target\":\"x.y\"}"),
            replace_sql_json(none.as_mut(), "{\"unique_id\":\"x\",\"sql\":\"select 1\"}"),
            revert_sql_json(none.as_mut(), "{\"unique_id\":\"x\"}"),
        ] {
            assert_eq!(res.unwrap_err(), "no manifest loaded");
        }
    }

    #[test]
    fn load_json_round_trips_and_rejects() {
        let (_engine, out) = load_json(&load_input()).expect("fixture loads");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["project"], "jaffle_finance");
        assert!(v["nodes"].as_u64().unwrap() > 0);

        // Malformed input JSON → the parse message; malformed manifest →
        // the engine's CLI wording, verbatim.
        assert!(load_json("{not json")
            .err()
            .unwrap()
            .starts_with("invalid input: "),);
        let err = load_json("{\"manifest\":\"not json\"}").err().unwrap();
        assert!(
            err.starts_with("failed to parse manifest JSON: "),
            "message: {err}"
        );
    }

    #[test]
    fn node_detail_and_select_json_serve_the_loaded_engine() {
        let engine = loaded_engine();
        let uid = "model.jaffle_finance.fct_subscription_process";
        let out = node_detail_json(
            Some(&engine),
            &serde_json::json!({"unique_id": uid}).to_string(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["name"], "fct_subscription_process");
        assert_eq!(v["resource_type"], "model");
        assert_eq!(v["materialized"], "table");

        // An engine error crosses as its CLI message — the same string
        // `dbtl` prints.
        let err = node_detail_json(Some(&engine), "{\"unique_id\":\"model.nope.n\"}").unwrap_err();
        assert_eq!(err, "no node 'model.nope.n'");

        let out =
            select_json(Some(&engine), "{\"selector\":\"fct_subscription_process\"}").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["nodes"][0]["unique_id"], uid);
    }

    #[test]
    fn replace_and_revert_json_edit_in_place() {
        let mut engine = loaded_engine();
        let uid = "model.jaffle_finance.fct_subscription_process";
        let detail = |e: &Engine| {
            let out = node_detail_json(Some(e), &serde_json::json!({"unique_id": uid}).to_string())
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&out).unwrap()
        };

        let out = replace_sql_json(
            Some(&mut engine),
            &serde_json::json!({"unique_id": uid, "sql": "select 1 as id"}).to_string(),
        )
        .unwrap();
        assert_eq!(out, "{\"changed\":true}");
        assert_eq!(detail(&engine)["raw_code"], "select 1 as id");

        let out = revert_sql_json(
            Some(&mut engine),
            &serde_json::json!({"unique_id": uid}).to_string(),
        )
        .unwrap();
        assert_eq!(out, "{\"changed\":true}");
        // A second revert is a no-op — `changed` reads `false`.
        let out = revert_sql_json(
            Some(&mut engine),
            &serde_json::json!({"unique_id": uid}).to_string(),
        )
        .unwrap();
        assert_eq!(out, "{\"changed\":false}");
        // An out-of-graph uid errors even though `revert` never edits.
        let err =
            revert_sql_json(Some(&mut engine), "{\"unique_id\":\"model.nope.n\"}").unwrap_err();
        assert_eq!(err, "no node 'model.nope.n'");
    }
}
