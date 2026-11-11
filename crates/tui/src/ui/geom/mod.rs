//! Pure screen geometry: pane rects, pane interiors, and the centered-overlay
//! rect. Computed the same way by `render` and by the tests, so a test can
//! isolate exactly the lineage pane interior. No state here.
//!
//! Split by surface, all re-exported below so `crate::ui::geom::X` paths are
//! unchanged:
//! - [`panes`]    — the three-pane layout (list / lineage / inspector), widths,
//!   splitters, and the responsive verdicts.
//! - [`overlays`] — centered / modal / menu / yank / tooltip rects and the
//!   stacked-underlay resolver.
//! - [`lineage`]  — the lineage pane's centred content rect, clipped-edge
//!   markers, and the scrollbar thumb.
//! - [`tabs`]     — the tab strip's cell layout.
//! - [`status`]   — the status line (chips + spans) and lineage-title span
//!   placement shared by draw and hit-test.
//! - [`hit`]      — `HitTarget` / `HitGeometry` / `hit_target` / `hit_test`,
//!   the point→target router.
//! - [`minimap`]  — the minimap inset rect + scrub/pan mappings.
//! - [`home`]     — the home screen's vertical line plan.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::widgets::{Block, Borders};

use crate::model_list::DisplayRow;
use crate::{ModelList, NodeRect};

mod hit;
mod home;
mod lineage;
mod minimap;
mod overlays;
mod panes;
mod status;
mod tabs;
#[cfg(test)]
mod tests;

pub use hit::{hit_target, hit_test, list_target_row, HitGeometry, HitTarget};
pub use home::{home_line_of, home_lines, home_window, HomeLine};
pub use lineage::{clip_edges, lineage_content_rect, scrollbar_thumb, ClipEdges};
pub use minimap::{minimap_rect, minimap_scroll_to, pan_offset};
pub(crate) use overlays::centered_rect;
pub use overlays::{
    menu_rect, modal_rect, overlay_rect, tooltip_rect, underlay_mode, yank_menu_rect, TOOLTIP_MAX_W,
};
pub use panes::{
    clamp_inspector_width, clamp_list_width, list_drawer_width, pane_interior, pane_layout,
    splitter_drag_width, terminal_too_small, PaneConfig, PaneRects, Splitter,
    DEFAULT_INSPECTOR_WIDTH, DEFAULT_LIST_WIDTH, INSPECTOR_AUTO_W, LIST_AUTO_HIDE_W,
    MIN_INSPECTOR_WIDTH, MIN_LINEAGE_WIDTH, MIN_LIST_WIDTH, MIN_TERMINAL_H, MIN_TERMINAL_W,
};
pub use status::{
    status_layout, StatusChip, StatusLayout, StatusSpan, TitleKind, TitleLayout, TitleSpan,
};
pub use tabs::{tab_bar_layout, TabBarLayout, TabCell};
