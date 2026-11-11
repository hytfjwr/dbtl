//! The single source of truth for user intent and key bindings.
//!
//! Every interaction is reified as an [`Action`]. A mode-aware [`dispatch`]
//! maps `(Mode, KeyEvent) -> Option<Action>`, and the SAME binding table feeds
//! [`help_lines`], so the `?` overlay can never drift from what the keys
//! actually do. `dispatch` is PURE and SIZE-UNAWARE: size-aware work
//! (scroll-follow, mouse hit-test) stays in the event loop.
//!
//! Split by concern, all re-exported below so `crate::action::X` paths are
//! unchanged:
//! - [`types`]    — `Action`, `Direction`, `SearchTarget`, `SearchState`.
//! - [`views`]    — the overlay snapshot payloads (`DetailView`, `SqlView`,
//!   `StatsView`, `DiffView`, `PaletteState`/`PaletteRow`, `HelpView`,
//!   `ColumnRow`/`ColumnView`, `NoticeLevel`, `NotificationEntry`,
//!   `NotificationsView`).
//! - [`menu`]     — the node context menu model (`MenuMark`/`MenuEntry`/
//!   `MenuAnchor`/`MenuState`/`MenuSlot`, `NODE_MENU_LAYOUT`, slot helpers).
//! - [`mode`]     — `Mode` and `ModeKind`.
//! - [`keymap`]   — `Trigger`, `KeyBinding`, the `BINDINGS` table, `Hint*` and
//!   `status_hints`.
//! - [`dispatch`] — `dispatch`, `esc_action`, `yank_bindings`, `help_lines`,
//!   `palette_candidates`, `reanchors_lineage`, `palette_binding_named`.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{ColumnChange, Focus, LineageLens, NodeDetail, PackageScope, PrImpact, TestInfo};

mod dispatch;
mod keymap;
mod menu;
mod mode;
#[cfg(test)]
mod tests;
mod types;
mod views;

pub use dispatch::{
    dispatch, esc_action, help_lines, palette_binding_named, palette_candidates, reanchors_lineage,
    yank_bindings, HelpLine,
};
#[cfg(test)]
use keymap::compact_keys;
pub use keymap::{status_hints, Hint, HintCtx, KeyBinding, StatusHint, Trigger, BINDINGS};
pub(crate) use menu::{menu_slot_label, menu_slot_mark};
pub use menu::{MenuAnchor, MenuEntry, MenuMark, MenuSlot, MenuState, NODE_MENU_LAYOUT};
pub use mode::{Mode, ModeKind};
pub use types::{Action, Direction, SearchState, SearchTarget};
pub use views::{
    ColumnRow, ColumnView, DetailView, DiffView, HelpView, InspectorView, NoticeLevel,
    NotificationEntry, NotificationsView, PaletteRow, PaletteState, SqlView, StatsView,
};
