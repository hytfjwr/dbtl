//! dbtl-wasm — the dbtl engine packaged for WebAssembly.
//!
//! Two layers:
//!
//! - [`engine`] — a pure, in-memory Rust API. Every operation takes and
//!   returns owned values (serde-ready), touches no filesystem, clock,
//!   randomness, or threads, and produces exactly the bytes the CLI prints.
//!   Native callers — the integration parity tests, the docs site's Node
//!   check — link it as an ordinary crate.
//! - [`bindings`] — the thin `#[wasm_bindgen]` surface. JSON strings in,
//!   JSON strings out; errors reach JavaScript as exceptions. All logic
//!   lives in [`engine`], so the boundary can never drift from what the
//!   native tests cover.

pub mod bindings;
pub mod engine;
