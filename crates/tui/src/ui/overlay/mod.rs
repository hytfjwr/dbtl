//! Modal overlays drawn on top of the base panes. Each is a plain function (no
//! trait/stack apparatus) selected by `render::draw` from the current `Mode`.
//! Search has no modal — its query renders inline in the list / lineage pane
//! titles.
//!
//! One file per modal surface, all re-exported below so `super::overlay::X`
//! and `crate::ui::overlay::X` paths are unchanged:
//! - [`scaffold`]      — the shared scrollable-modal frame: `modal_rect` +
//!   content-width / scroll clamp / `draw_scrollable_modal` (border + title +
//!   interior + right-border thumb), plus `wrap_prefixed`.
//! - [`help`]          — the `?` help overlay (generated from `help_lines`).
//! - [`detail`]        — the structure modal (type / location / tags /
//!   description / columns / tests).
//! - [`sql`]           — the `s` SQL preview + its line tokenizer.
//! - [`stats`]         — the `S` stats dashboard (gauges, worklists).
//! - [`diff`]          — the `--diff` baseline summary modal.
//! - [`columns`]       — the column-lineage modal.
//! - [`palette`]       — the Ctrl-p command palette, the `y` yank menu, and
//!   `dim_exterior` (the §9 underlay dim).
//! - [`menu`]          — the node context menu.
//! - [`notifications`] — the notification centre + the toast stack.
//! - [`tooltip`]       — the settled-hover tooltip.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::action::{
    palette_candidates, ColumnView, DetailView, DiffView, HelpView, MenuEntry, MenuState, ModeKind,
    NoticeLevel, NotificationsView, PaletteRow, PaletteState, SqlView, StatsView,
};

use super::geom;
use super::geom::{centered_rect, modal_rect};
use super::render::ToastLine;
use super::{chrome, header_style, selected_style, stamp_scrollbar, theme, SELECTED_MARKER};

mod columns;
mod detail;
mod diff;
mod help;
mod menu;
mod notifications;
mod palette;
mod scaffold;
mod sql;
mod stats;
mod tooltip;

pub(crate) use scaffold::{
    clamp_modal_scroll, draw_accent_modal, draw_scrollable_modal, modal_content_width,
};

pub use columns::clamp_columns_scroll;
pub(crate) use columns::draw_columns;
pub use detail::clamp_detail_scroll;
pub(crate) use detail::{draw_detail, test_row_text};
pub use diff::clamp_diff_scroll;
pub(crate) use diff::draw_diff;
pub use help::clamp_help_scroll;
pub(crate) use help::draw_help;
pub(crate) use menu::draw_menu;
pub use notifications::clamp_notifications_scroll;
pub(crate) use notifications::{draw_notifications, draw_toasts};
pub(crate) use palette::{dim_exterior, draw_palette, draw_yank_menu};
pub(crate) use scaffold::wrap_prefixed;
pub use sql::clamp_sql_scroll;
pub(crate) use sql::draw_sql;
pub use stats::clamp_stats_scroll;
pub(crate) use stats::draw_stats;
pub(crate) use tooltip::draw_tooltip;
pub use tooltip::TooltipView;
