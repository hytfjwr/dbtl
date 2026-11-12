//! [`apply_action`] — the size-unaware domain reducer: the exhaustive
//! `Action` → state transition table, plus its small shared arms
//! (yank-with-notice, the modal scroll slot, the wrap-around list cycle).
//!
//! The dispatch table stays here; the arm BODIES live in per-concern sibling
//! files, all `pub(super)` so `super::reducer::X` paths are unchanged:
//! - [`movement`] — cursor moves / jumps / paging / focus + depth toggles.
//! - [`overlays`] — help toggle, structure-modal open/close, the `y` prefix;
//!   also the `DetailView` / `InspectorView` snapshot builders.
//! - [`menu`]     — the node context menu + pointer row picks (menu / yank /
//!   palette / column).
//! - [`detail`]   — the structure modal's column cursor, section toggle, and
//!   scroll arms (via [`modal_scroll_mut`]).
//! - [`columns`]  — the column-lineage focus (`b`) and columns modal (`B`).
//! - [`modals`]   — palette / SQL preview / stats / notifications / diff opens
//!   (and the SQL compiled toggle).
//! - [`search`]   — `/` and `n`/`N` search input, moves, cancel, confirm.
//! - [`effects`]  — arms that produce [`Effect`]s for the loop (editor / yanks /
//!   exports / reload / parse) plus recenter / depth / history.
//! - [`toggles`]  — lenses, view toggles (minimap / density / overview /
//!   inspector / path / theme), pane widths.
//! - [`list`]     — bookmarks, list filters, group folds, gap jumps, sort cycle.
//! - [`tabs`]     — tab open / close / cycle / activate.
//! - [`home`]     — the home screen's row moves and opens.
//!
//! Semantics note: an arm that used to `return` straight out of `apply_action`
//! (skipping the yank-menu epilogue) now returns [`Arm::Early`]; the dispatch
//! below turns it back into a real `return`, so the epilogue's reach is
//! byte-for-byte the same as when this was one giant match.

use crate::action::{
    Action, DetailView, Direction, HelpView, MenuAnchor, MenuEntry, Mode, PaletteRow, PaletteState,
    SearchState, SearchTarget, SqlView, BINDINGS,
};
use crate::effect::Effect;
use crate::{build_model_list_grouped, reduce_selection, Focus, LineageLens};

use super::{App, HomeItem, LineageView, ListFilter, Outcome};

mod columns;
mod detail;
mod effects;
mod home;
mod list;
mod menu;
mod modals;
mod movement;
mod overlays;
mod search;
mod tabs;
mod toggles;

pub(in crate::app) use overlays::inspector_view_for;

/// One arm's result. `Done` is the ordinary match value — it flows into the
/// post-match epilogue (yank-menu close + `pending_*` clear). `Early` is the
/// faithful translation of a `return` inside an arm: `apply_action` returns it
/// immediately, epilogue skipped — exactly like the original single match.
/// `Pass` means the variant belongs to a different group; the dispatch makes
/// it unreachable, and the unreachable arm name says which group misrouted.
pub(super) enum Arm {
    Done(Outcome),
    Early(Outcome),
    Pass,
}

/// Unpack a group arm's [`Arm`] in the dispatch match: `Early` becomes a real
/// `return`, `Done` becomes the match value, `Pass` is a routing bug.
macro_rules! arm {
    ($app:expr, $action:expr, $f:path) => {
        match $f($app, $action) {
            Arm::Early(o) => return o,
            Arm::Done(o) => o,
            Arm::Pass => unreachable!("apply_action: variant routed to the wrong arm group"),
        }
    };
}

/// Apply an [`Action`] to the app, returning the [`Outcome`] (quit + effects).
///
/// Size-unaware: it never reads pane dimensions. UiState-only actions are
/// forwarded to [`reduce_selection`]; domain actions and mode transitions are
/// handled here. The match is exhaustive over [`Action`], so a new variant is a
/// compile error until it is given behaviour.
pub fn apply_action(app: &mut App, action: Action) -> Outcome {
    // The overview shows the WHOLE graph: the rooted view filters
    // (direction/depth) and the density toggle have no visible effect while
    // it is on, so gate them to a notice instead of mutating state that
    // would surprisingly apply the moment the user drills back in.
    if app.ui_state.overview()
        && app.ui_state.focus() != Focus::Inspector
        && matches!(
            action,
            Action::ToggleUpstream
                | Action::ToggleDownstream
                | Action::DepthDecrease
                | Action::DepthIncrease
                | Action::ResetView
                | Action::ToggleDensity
        )
    {
        app.set_notice("view filters are off in overview (w to exit)");
        return Outcome::cont();
    }
    // A column trace pins the filtered graph's shape: the rooted view filters
    // (direction/depth) have no visible effect while it is on, so gate them
    // like the overview does — a silent mutation would apply the moment Esc
    // exits the trace. (`v` density stays live: Compact legitimately hides
    // the column rows.)
    if app.active_column_focus().is_some()
        && app.ui_state.focus() != Focus::Inspector
        && matches!(
            action,
            Action::ToggleUpstream
                | Action::ToggleDownstream
                | Action::DepthDecrease
                | Action::DepthIncrease
                | Action::ResetView
        )
    {
        app.set_notice("view filters are off while tracing a column (Esc to exit)");
        return Outcome::cont();
    }
    // The yank-prefix menu is one-shot: ANY action dispatched out
    // of it (a yank, the Esc/q cancel) closes it afterwards. Snapshotting the
    // mode BEFORE the match keeps the close logic in this one place instead of
    // duplicating it into every Yank-mode arm.
    let in_yank_menu = matches!(app.mode, Mode::Yank { .. });
    let outcome = match action {
        Action::Quit => Outcome::quit(),
        // Movement keys are focus-routed: lineage-pane focus moves the lineage
        // CURSOR (Dag-aware, so it lives here, not in the UiState sub-reducer);
        // list focus forwards to the sub-reducer's list selection. Neither pans
        // the viewport — the loop follows the cursor with a minimal
        // ensure-visible.
        Action::MoveDown
        | Action::MoveUp
        | Action::MoveLeft
        | Action::MoveRight
        | Action::JumpTop
        | Action::JumpBottom
        | Action::PageDown
        | Action::PageUp
        | Action::ToggleDownstream
        | Action::ToggleUpstream
        | Action::ToggleFocus
        | Action::ToggleListPane
        | Action::LineageLeftmost
        | Action::LineageRightmost => arm!(app, action, movement::apply),
        Action::HelpToggle | Action::DetailOpen | Action::DetailClose | Action::YankPrefix => {
            arm!(app, action, overlays::apply)
        }
        Action::MenuOpen
        | Action::MenuMove(_)
        | Action::MenuConfirm
        | Action::MenuInvoke(_)
        | Action::MenuSelect(_)
        | Action::YankPick(_)
        | Action::YankInvoke(_)
        | Action::PalettePick(_)
        | Action::ColumnPick(_) => arm!(app, action, menu::apply),
        Action::DetailColumnMove(_)
        | Action::DetailSectionToggle
        | Action::DetailScroll(_)
        | Action::DetailScrollPage(_)
        | Action::DetailScrollHome
        | Action::DetailScrollEnd => arm!(app, action, detail::apply),
        Action::ColumnFocusEnter
        | Action::ColumnFocusExit
        | Action::ColumnsOpen
        | Action::ColumnRowMove(_)
        | Action::ColumnJump => arm!(app, action, columns::apply),
        Action::SearchOpen
        | Action::SearchType(_)
        | Action::SearchBackspace
        | Action::SearchMove(_)
        | Action::SearchCancel
        | Action::SearchConfirm => arm!(app, action, search::apply),
        Action::OpenEditor
        | Action::YankId
        | Action::YankName
        | Action::YankMermaid
        | Action::YankDot
        | Action::YankAscii
        | Action::YankSql
        | Action::YankCompiledSql
        | Action::YankColumnLineage
        | Action::YankImpact
        | Action::YankSelector
        | Action::ExportLineage
        | Action::Reload
        | Action::DbtParse
        | Action::Recenter
        | Action::DepthDecrease
        | Action::DepthIncrease
        | Action::ResetView
        | Action::HistoryBack
        | Action::HistoryForward
        | Action::ExportPrImpact
        | Action::YankPrSelector
        | Action::YankPrSelectorWide => arm!(app, action, effects::apply),
        Action::PaletteOpen
        | Action::SqlOpen
        | Action::SqlToggleCompiled
        | Action::StatsOpen
        | Action::NotificationsOpen
        | Action::DiffOpen => arm!(app, action, modals::apply),
        Action::CycleLens
        | Action::SetLens(_)
        | Action::ToggleMinimap
        | Action::ToggleDensity
        | Action::ToggleOverview
        | Action::PaneNarrow
        | Action::PaneWiden
        | Action::PaneWidthsReset
        | Action::ToggleInspector
        | Action::InspectorColumnPick(_)
        | Action::PathToggle
        | Action::ThemeCycle => arm!(app, action, toggles::apply),
        Action::BookmarkToggle
        | Action::ToggleUntestedFilter
        | Action::ToggleBookmarkFilter
        | Action::ToggleLintFilter
        | Action::ToggleGroupFold
        | Action::BookmarkCycle
        | Action::BookmarkCycleBack
        | Action::GapNext
        | Action::GapPrev
        | Action::SortCycle => arm!(app, action, list::apply),
        Action::TabOpen
        | Action::TabClose
        | Action::TabPrev
        | Action::TabNext
        | Action::TabActivate(_) => arm!(app, action, tabs::apply),
        Action::HomeGo | Action::HomeMove(_) | Action::HomeOpen | Action::HomePick(_) => {
            arm!(app, action, home::apply)
        }
    };
    if in_yank_menu
        && !matches!(
            action,
            Action::YankPick(_)
                | Action::DetailScroll(_)
                | Action::DetailScrollPage(_)
                | Action::DetailScrollHome
                | Action::DetailScrollEnd
        )
    {
        // A yank / an unhandled key closes the one-shot menu: the mode it
        // stacked over (`back`, e.g. the structure modal) comes back — NOT a
        // jump to Selection (the nested-overlay rule). Two gestures are
        // exempt: `YankPick` (a pointer hover/row click only moves the
        // highlight) and the DetailScroll family (a wheel over the menu is a
        // no-op — the menu has no scroll slot — and must not dismiss it).
        if let Mode::Yank { back, .. } = &mut app.mode {
            if let Some(back) = back.take() {
                app.mode = *back;
            } else {
                app.mode = app.idle_mode();
            }
        }
    }
    // The yank stashes live only while the yank menu is open — that INCLUDES
    // a `?` help layer sitting on top of it (`Help` whose `back` is `Yank`):
    // Esc peels the help back to the menu, which must find the modal target
    // still stashed, or the next `y<key>` silently yanks the list selection
    // instead of the modal's node. Any other mode — after a yank, or an
    // Esc/q cancel — drops the modal context, so a stale
    // `pending_node`/`pending_col` can never leak into a later yank (both
    // are also rewritten on every `y` press; this just closes the stale-read
    // window after a cancel). `MenuInvoke` arms `pending_node` for its own
    // dispatch window and clears it right after, so it never reaches here.
    let yank_still_open = matches!(app.mode, Mode::Yank { .. })
        || matches!(&app.mode, Mode::Help(h) if matches!(h.back.as_ref(), Mode::Yank { .. }));
    if !yank_still_open {
        app.pending_col = None;
        app.pending_node = None;
    }
    outcome
}

/// The scroll slot of whichever overlay modal is open (`None` in non-modal
/// modes). The ONE place every scrollable modal is enumerated, shared by the
/// line / page / home / end scroll arms so a new modal can't be wired into
/// some arms and missed in others.
pub(super) fn modal_scroll_mut(mode: &mut Mode) -> Option<&mut usize> {
    match mode {
        Mode::Help(h) => Some(&mut h.scroll),
        Mode::Detail(dv) => Some(&mut dv.scroll),
        Mode::Sql(sv) => Some(&mut sv.scroll),
        Mode::Stats(sv) => Some(&mut sv.scroll),
        Mode::Diff(dv) => Some(&mut dv.scroll),
        Mode::Columns(cv) => Some(&mut cv.scroll),
        Mode::Notifications(nv) => Some(&mut nv.scroll),
        _ => None,
    }
}
