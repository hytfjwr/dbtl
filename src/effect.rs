//! Side effects requested by the (pure) reducer and performed by the event loop.
//!
//! The reducer never suspends the terminal, shells out, or touches the
//! filesystem; impure work (open `$EDITOR`, clipboard, reload) is recorded as an
//! [`Effect`] for the loop to perform. Effects-as-data keeps the reducer fully
//! unit-testable and the impure surface tiny.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Suspend the TUI and open this file path in `$EDITOR` / `$VISUAL`.
    OpenEditor(String),
    /// Copy this text to the system clipboard.
    Yank(String),
    /// Re-read the manifest from disk and rebuild the app (preserving selection).
    ReloadManifest,
    /// Write `contents` to `path` (relative to the cwd). Used by the lineage
    /// export; overwrites an existing file like a shell redirect would.
    WriteFile { path: String, contents: String },
}
