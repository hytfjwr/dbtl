//! Pure UI state and the two-level reducer's bottom layer.
//!
//! - The selection space is the flat list of selectable *models* (`0..len`);
//!   group headers are drawn but never selectable.
//! - Scrolling lives in *display-row space* (`ModelList::rows`), so the unit
//!   that must stay visible is the selected model's *display row*. The renderer
//!   (`render::draw_list`) and the scroll-follow logic (`ensure_visible`) measure
//!   against the exact same row sequence.
//! - [`reduce_selection`] / [`handle_key`] are PURE and SIZE-UNAWARE: they never
//!   read pane height. Scroll-follow (`ensure_visible` / `anchor_lineage` /
//!   `ensure_lineage_visible` / `clamp_lineage`) is applied by the event loop
//!   with the terminal size.
//!
//! Split by concern, all re-exported below so `crate::ui::X` paths are
//! unchanged:
//! - [`types`]   — `LineageLens`, `InspectorPolicy`, `Focus`, `KeyOutcome`.
//! - [`uistate`] — `UiState` and its impl (selection / scroll / pref state).
//! - [`reduce`]  — `reduce_selection` + the frozen `handle_key` facade.

use ratatui::crossterm::event::KeyEvent;

use super::geom;

mod reduce;
#[cfg(test)]
mod tests;
mod types;
mod uistate;

use reduce::WHEEL_STEP;
pub use reduce::{handle_key, reduce_selection};
pub use types::{Focus, InspectorPolicy, KeyOutcome, LineageLens};
pub use uistate::UiState;
