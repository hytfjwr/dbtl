//! Thin `#[wasm_bindgen]` wrappers over [`crate::engine`]: JSON strings in
//! and out (`serde_json`), and every failure reaches JavaScript as a
//! thrown `Error` carrying the CLI-flavored message. No logic lives here —
//! each method parses its input, delegates to the engine, and serializes
//! the output, so the boundary can never drift from what the native tests
//! cover.

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

// `EngineError` reaches JS as a thrown `Error` — wasm_bindgen's blanket
// `From<E: StdError>` picks it up, so `?` converts it and the message
// carries the CLI wording; the exit-code class (engine::ErrorKind) stays
// in the type for native callers.

/// Parse the operation's JSON argument object; a malformed payload is a
/// plain `Error`, never a panic.
fn parse_input<I: DeserializeOwned>(input: &str) -> Result<I, JsError> {
    serde_json::from_str(input).map_err(|e| JsError::new(&format!("invalid input: {e}")))
}

/// Serialize an operation's output — infallible for the engine's
/// owned-value output types.
fn to_json<O: Serialize>(out: &O) -> String {
    serde_json::to_string(out).expect("engine outputs serialize")
}

/// The input shape for the single-string-argument operations.
#[derive(serde::Deserialize)]
struct UniqueIdInput {
    unique_id: String,
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
        let input: engine::LoadInput = parse_input(input)?;
        let (engine, out) = engine::Engine::load(&input)?;
        self.engine = Some(engine);
        Ok(to_json(&out))
    }

    /// `{selector}` → `{nodes: [{unique_id, name, resource_type,
    /// materialized, path}]}` — `dbtl ls`'s match set.
    pub fn select(&self, input: &str) -> Result<String, JsError> {
        #[derive(serde::Deserialize)]
        struct Input {
            selector: String,
        }
        let input: Input = parse_input(input)?;
        Ok(to_json(&self.engine()?.select(&input.selector)?))
    }

    /// `{unique_id}` → the node-detail payload (name, kind, path, raw SQL,
    /// compiled-SQL flag, declared columns).
    #[wasm_bindgen(js_name = nodeDetail)]
    pub fn node_detail(&self, input: &str) -> Result<String, JsError> {
        let input: UniqueIdInput = parse_input(input)?;
        Ok(to_json(&self.engine()?.node_detail(&input.unique_id)?))
    }

    /// `{root|roots, direction?, depth?, glyphs?, density?}` → the laid-out
    /// lineage grid (`rows`, `classes`, `emphasis`, `rects`, `selected`).
    pub fn lineage(&self, input: &str) -> Result<String, JsError> {
        let input: engine::LineageInput = parse_input(input)?;
        Ok(to_json(&self.engine()?.lineage(&input)?))
    }

    /// `{config?, selector?, fail_on?}` → `{text, ansi, json, exit_code,
    /// error, warnings}` — `dbtl lint`'s output as data.
    pub fn lint(&self, input: &str) -> Result<String, JsError> {
        let input: engine::LintOptions = parse_input(input)?;
        Ok(to_json(&self.engine()?.lint(&input)?))
    }

    /// `{target, upstream?}` → `{json, markdown, warnings}` —
    /// `dbtl columns <model>.<column>`.
    pub fn columns(&self, input: &str) -> Result<String, JsError> {
        let input: engine::ColumnsInput = parse_input(input)?;
        Ok(to_json(&self.engine()?.columns(&input)?))
    }

    /// `{unique_id, sql}` → `{changed}` — rewrite a model/snapshot's raw
    /// SQL (and drop its stale compiled SQL) in memory.
    #[wasm_bindgen(js_name = replaceSql)]
    pub fn replace_sql(&mut self, input: &str) -> Result<String, JsError> {
        #[derive(serde::Deserialize)]
        struct Input {
            unique_id: String,
            sql: String,
        }
        let input: Input = parse_input(input)?;
        let engine = self.engine_mut()?;
        Ok(to_json(&engine.replace_sql(&input.unique_id, &input.sql)?))
    }

    /// `{unique_id}` → `{changed}` — restore a node's SQL to its loaded
    /// state; `changed` is `false` when it was never edited.
    #[wasm_bindgen(js_name = revertSql)]
    pub fn revert_sql(&mut self, input: &str) -> Result<String, JsError> {
        let input: UniqueIdInput = parse_input(input)?;
        let engine = self.engine_mut()?;
        Ok(to_json(&engine.revert_sql(&input.unique_id)?))
    }

    fn engine(&self) -> Result<&Engine, JsError> {
        self.engine
            .as_ref()
            .ok_or_else(|| JsError::new("no manifest loaded"))
    }

    fn engine_mut(&mut self) -> Result<&mut Engine, JsError> {
        self.engine
            .as_mut()
            .ok_or_else(|| JsError::new("no manifest loaded"))
    }
}

impl Default for WasmEngine {
    fn default() -> Self {
        Self::new()
    }
}
