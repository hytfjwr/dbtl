//! dbtl core library — the crate root is a thin facade: every module's public
//! surface is re-exported here so callers (the binary, the integration tests)
//! keep flat `dbtl::X` paths regardless of where a type lives.
//!
//! The workspace's member crates:
//! - [`dbtl_core`] — the dbt domain (manifest/source parsing → [`Dag`], diff,
//!   layer taxonomy, Mermaid export core); no terminal dependencies.
//! - [`dbtl_tui`] — the interactive app (keymap, reducer, ratatui rendering).
//! - [`dbtl_docs`] — the non-interactive `dbtl docs` Markdown generator.
//! - [`dbtl_report`] — the `check`/`lint` report model, its eight
//!   renderers, and the lint-gate evaluation; no filesystem, terminal, or
//!   clap dependencies.
//!
//! The data flow: a `manifest.json` or a dbt project tree becomes a pruned
//! `Dag` (`dbtl-core`), which `dbtl-tui`'s `App` owns and renders as an ASCII
//! lineage diagram.

pub use dbtl_core::*;
pub use dbtl_docs as docs;
// A named re-export — a glob would collide with `dbtl_tui::RenderCtx`.
pub use dbtl_docs::{
    generate_docs, generate_docs_with_findings, generate_docs_with_options, DocsOptions,
    GENERATED_MARKER,
};
pub use dbtl_report as report;
pub use dbtl_tui::*;
