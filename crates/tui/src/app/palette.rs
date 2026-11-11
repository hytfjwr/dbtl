//! The command palette's Dag-aware half: [`App::palette_rows`] builds the row
//! model the modal draws (Selection commands + `Recent`/`Commands` headers +
//! `@`-search model results, each command carrying its availability mark) and
//! [`App::unavailable_reason`] is the shared "would this Action do nothing
//! right now" predicate the palette — and later the menu — uses to
//! grey rows. Also home to the two persisted recall lists: `search_history`
//! (`/`-mode Up/Down) and `palette_recent` (the Recent section).

use crate::action::{
    palette_binding_named, palette_candidates, Action, KeyBinding, Mode, PaletteRow, SearchTarget,
};
use crate::coverage_gap;
use crate::model_list::match_indices;

use super::{App, ListFilter};

/// Persisted caps: the last 20 confirmed list-search
/// queries, the last 5 palette-run commands.
pub const SEARCH_HISTORY_MAX: usize = 20;
pub const PALETTE_RECENT_MAX: usize = 5;

impl App {
    /// The palette's display rows for `query` — what the modal draws and what
    /// the reducer's Up/Down/Enter navigate. Section headers are display-only;
    /// `PaletteState.selected` indexes the NON-header rows (see
    /// [`palette_selectable_rows`](Self::palette_selectable_rows)).
    pub fn palette_rows(&self, query: &str) -> Vec<PaletteRow> {
        // `@` switches the palette to model search: candidates are the ACTIVE
        // list's models (the set the list pane can select — every row offered
        // is a row Enter can re-root to). `@` alone lists them all; the rest
        // is the same fuzzy subsequence match the list search uses on names.
        if let Some(model_q) = query.strip_prefix('@') {
            let model_q = model_q.trim();
            return self
                .active_list()
                .models
                .iter()
                .filter(|m| model_q.is_empty() || !match_indices(&m.name, model_q).is_empty())
                .map(|m| PaletteRow::Model {
                    name: m.name.clone(),
                    uid: m.unique_id.clone(),
                })
                .collect();
        }
        let mut rows = Vec::new();
        // Recent first — only at an EMPTY query (any filter text means the
        // user is hunting, not picking a favourite). Recent rows mirror live
        // BINDINGS entries — `palette_binding_named` drops names whose action
        // is gone, so the section can never become a second command source.
        // The full list follows UNFILTERED by recency (all commands still
        // follow — a recent command appears twice, once as the shortcut).
        if query.trim().is_empty() && !self.palette_recent.is_empty() {
            let recents: Vec<PaletteRow> = self
                .palette_recent
                .iter()
                .filter_map(|n| palette_binding_named(n))
                .map(|b| self.palette_command_row(b))
                .collect();
            if !recents.is_empty() {
                rows.push(PaletteRow::Header("Recent"));
                rows.extend(recents);
                rows.push(PaletteRow::Header("Commands"));
            }
        }
        rows.extend(
            palette_candidates(query)
                .iter()
                .map(|b| self.palette_command_row(b)),
        );
        rows
    }

    /// The palette's SELECTABLE rows (headers stripped) — the list
    /// `PaletteState.selected` and the move/confirm arms index into.
    pub fn palette_selectable_rows(&self, query: &str) -> Vec<PaletteRow> {
        self.palette_rows(query)
            .into_iter()
            .filter(|r| !matches!(r, PaletteRow::Header(_)))
            .collect()
    }

    fn palette_command_row(&self, b: &'static KeyBinding) -> PaletteRow {
        PaletteRow::Command {
            action: b.action,
            help: b.help,
            keys: b.key_label(),
            unavailable: self.unavailable_reason(b.action),
        }
    }

    /// Record a palette-confirmed command in `palette_recent`: the `Action`'s
    /// Debug name (the persisted form), most-recent-first, deduped, capped.
    /// `Quit` is the exception — session-ending noise, not a workflow command
    /// the Recent shortcut exists for.
    pub fn record_palette_run(&mut self, action: Action) {
        if matches!(action, Action::Quit) {
            return;
        }
        let name = format!("{action:?}");
        self.palette_recent.retain(|n| n != &name);
        self.palette_recent.insert(0, name);
        self.palette_recent.truncate(PALETTE_RECENT_MAX);
    }

    /// Record a confirmed list-search query in `search_history` — the list
    /// `/`-mode Up/Down recall browses this (same most-recent-first /
    /// dedup / cap shape as the palette recents). Empty queries don't
    /// record: there is nothing to recall.
    pub fn record_search_query(&mut self, query: &str) {
        if query.trim().is_empty() {
            return;
        }
        self.search_history.retain(|q| q != query);
        self.search_history.insert(0, query.to_string());
        self.search_history.truncate(SEARCH_HISTORY_MAX);
    }

    /// "Would running `action` right now do nothing?" — `Some(reason)` means
    /// the palette greys the row and Enter toasts the reason instead of
    /// running it. The predicate mirrors each apply arm's own no-op
    /// conditions (e.g. `D` with no baseline loaded), so the palette's marks
    /// can't drift from real behaviour — and the menu greys the same
    /// commands. `None` = runs. `String` not `&'static str`: the list-empty
    /// reasons are built at runtime (glyph-mode dash, the active filter's
    /// name).
    pub fn unavailable_reason(&self, action: Action) -> Option<String> {
        use Action as A;
        let no_selection = self.selected_unique_id().is_none();
        match action {
            // `D` opens the diff view only when a baseline is loaded.
            A::DiffOpen => self.diff().is_none().then(|| "needs --diff".to_string()),
            // `e` needs the selected node's on-disk SQL path.
            A::OpenEditor => self
                .selected_file_path()
                .is_none()
                .then(|| "no file path for this node".to_string()),
            // Enter/`s` need a focus target; an empty list has none.
            A::DetailOpen | A::SqlOpen => self
                .focus_target()
                .is_none()
                .then(|| "no model selected".to_string()),
            // `K` needs a traced column — or the structure modal's column
            // cursor, which the palette (a Selection surface) can't see.
            A::ColumnsOpen => (self.tab().column_focus.is_none()
                && !matches!(self.mode, Mode::Detail(_)))
            .then(|| "no traced column".to_string()),
            A::ColumnFocusExit => self
                .tab()
                .column_focus
                .is_none()
                .then(|| "not tracing a column".to_string()),
            A::BookmarkToggle => no_selection.then(|| "no model selected".to_string()),
            // `z` always homes the cursor — the arm is unconditional.
            // `p` CLEARS a live anchor even with no selection (the arm's first
            // branch), so it is only dead when both are absent.
            A::PathToggle => (self.tab().path_anchor.is_none() && no_selection)
                .then(|| "no model selected".to_string()),
            // The cycle targets bookmarks in the ACTIVE (filtered) list — a
            // bookmark filtered out by the persistent filter can't be cycled
            // to, which is the arm's real no-op condition (`cycle_to` → None).
            A::BookmarkCycle | A::BookmarkCycleBack => (!self
                .active_list()
                .models
                .iter()
                .any(|m| self.bookmarks.contains(&m.unique_id)))
            .then(|| "no bookmarks in the list".to_string()),
            A::GapNext | A::GapPrev => (!self.active_list().models.iter().any(coverage_gap))
                .then(|| "no untested models".to_string()),
            A::HistoryBack => self
                .tab()
                .back
                .is_empty()
                .then(|| "no earlier model".to_string()),
            A::HistoryForward => self
                .tab()
                .forward
                .is_empty()
                .then(|| "no later model".to_string()),
            // Movement is focus-routed exactly like apply: list focus forwards
            // to `reduce_selection` (dead on an empty list — its own stamped
            // reason is the accurate WHY), lineage focus needs a cursor (a
            // selection). h/l have no list arm at all — always dead there.
            A::MoveDown | A::MoveUp => {
                if self.ui_state.focus() == crate::Focus::List {
                    self.list_empty_reason()
                } else {
                    self.lineage_cursor_uid()
                        .is_none()
                        .then(|| "no model selected".to_string())
                }
            }
            A::MoveLeft | A::MoveRight => {
                if self.ui_state.focus() == crate::Focus::List {
                    Some("lineage pane only".to_string())
                } else {
                    self.lineage_cursor_uid()
                        .is_none()
                        .then(|| "no model selected".to_string())
                }
            }
            // g/G/PgUp/PgDn are list-side commands (`jump_*`/`page_move` no-op
            // unless the list pane is focused): dead with lineage focus, dead
            // on an empty list.
            A::JumpTop | A::JumpBottom | A::PageDown | A::PageUp => {
                if self.ui_state.focus() != crate::Focus::List {
                    Some("list pane only".to_string())
                } else {
                    self.list_empty_reason()
                }
            }
            // H/L act on the lineage regardless of focus — still need a cursor.
            A::LineageLeftmost | A::LineageRightmost => self
                .lineage_cursor_uid()
                .is_none()
                .then(|| "no model selected".to_string()),
            // The apply-time gates, mirrored 1:1 — overview blocks the five
            // view filters + density; a column trace blocks the filters.
            A::ToggleUpstream
            | A::ToggleDownstream
            | A::DepthDecrease
            | A::DepthIncrease
            | A::ResetView => {
                if self.ui_state.overview() {
                    Some("off in overview".to_string())
                } else {
                    self.active_column_focus()
                        .is_some()
                        .then(|| "off while tracing a column".to_string())
                }
            }
            A::ToggleDensity => self
                .ui_state
                .overview()
                .then(|| "off in overview".to_string()),
            // `w` on the home body has no lineage to overview — the arm
            // toasts and returns instead of leaking the flag.
            A::ToggleOverview => self.home_active().then(|| "needs a model tab".to_string()),
            // Node-scoped yanks dead-end at `yank_target_uid` when nothing is
            // selected.
            A::YankId | A::YankName | A::YankImpact | A::YankSelector => {
                no_selection.then(|| "no model selected".to_string())
            }
            // The lineage yanks and the `e` export all derive from
            // `yank_subgraph` → `lineage_subgraph`: EMPTY nodes is their
            // shared no-op (mermaid/dot/ascii return None; the export never
            // emits its effect). Non-overview that means nothing selected;
            // under the overview's whole-graph pull it means the GRAPH is
            // empty — the case "no selection" alone would have missed.
            A::YankMermaid | A::YankDot | A::YankAscii | A::ExportLineage => {
                if self.subgraph_rc().nodes.is_empty() {
                    Some(
                        if self.ui_state.overview() {
                            "empty lineage"
                        } else {
                            "no model selected"
                        }
                        .to_string(),
                    )
                } else {
                    None
                }
            }
            // `yC`/`yI` need the node's recorded SQL — the arm's own
            // `raw_sql_for`/`compiled` predicates (blank counts as absent).
            A::YankSql => match self.yank_target_uid() {
                None => Some("no model selected".to_string()),
                Some(uid) => self
                    .raw_sql_for(&uid)
                    .is_none()
                    .then(|| "no raw SQL".to_string()),
            },
            A::YankCompiledSql => match self.yank_target_uid() {
                None => Some("no model selected".to_string()),
                Some(uid) => {
                    let has = self
                        .dag
                        .compiled(&uid)
                        .is_some_and(|c| !c.code.trim().is_empty());
                    (!has).then(|| "no compiled SQL".to_string())
                }
            },
            _ => None,
        }
    }

    /// The one-line reason the list pane stamps on its first interior row
    /// when the ACTIVE list is empty — display-only chrome, never a row (the
    /// two-coordinate-space invariant). `None` when the list has models.
    /// A live list search reports ITS miss first; otherwise the persistent
    /// filter names what it removed.
    pub fn list_empty_reason(&self) -> Option<String> {
        if !self.active_list().is_empty() {
            return None;
        }
        if let Mode::Search(s) = &self.mode {
            if s.target == SearchTarget::List && !s.query.is_empty() {
                // A `?` full-text miss just says "no match" — the hint already
                // tells the user what `?` does.
                return Some(if s.query.starts_with('?') {
                    "no match".to_string()
                } else {
                    let dash = crate::ui::chrome(self.glyph_mode).dash;
                    format!("no match {dash} try ?text for full-text search")
                });
            }
        }
        if self.model_list.is_empty() {
            let dash = crate::ui::chrome(self.glyph_mode).dash;
            return Some(format!("no models in scope {dash} try --include-packages"));
        }
        // The persistent filter narrows BEFORE the fold stage, so when the
        // fold-free base is already empty the filter is the real miss.
        if self.unfolded_base().is_empty() {
            return Some(match self.list_filter {
                ListFilter::Untested => "no untested models".to_string(),
                ListFilter::Bookmarked => "no bookmarked models".to_string(),
                // `All` ⇒ unfolded_base == model_list — an empty list already
                // took the scope reason above; kept only as the defensive
                // fallback.
                ListFilter::All => "no models".to_string(),
            });
        }
        // The only stage left that can empty the pane: every group folded.
        // The headers still render — this line lands between them, so it
        // names the fold (with `-`'s twin) rather than a phantom filter miss.
        let dash = crate::ui::chrome(self.glyph_mode).dash;
        Some(format!("all groups folded {dash} press - to expand"))
    }
}
