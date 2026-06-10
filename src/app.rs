//! The application: domain state + UI state in one owner, and the size-unaware
//! domain reducer the event loop actually drives.
//!
//! [`App`] holds the `Dag`, the model list (and an optional filtered view), the
//! `UiState`, and the interaction `Mode`. [`apply_action`] is the loop's real
//! entry point: it handles the DOMAIN actions itself (those need the `Dag` /
//! `ModelList`) and forwards the UiState-only arms down to
//! [`reduce_selection`](crate::ui::reduce_selection). It is **size-unaware**
//! (pane *size* is forbidden in the pure transition, not domain data),
//! so all the size-aware scroll-follow / anchoring stays in the event loop.
//!
//! Side effects (open `$EDITOR`, clipboard, reload) are never performed here —
//! [`apply_action`] records them as [`Effect`]s in its [`Outcome`], and the loop
//! runs them. This keeps the reducer fully unit-testable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::action::{
    palette_candidates, Action, DetailView, Direction, Mode, PaletteState, SearchState,
    SearchTarget, SqlView, StatsView,
};
use crate::effect::Effect;
use crate::{
    build_filtered_model_list, build_model_list, coverage_gap, load_dag, load_dag_from_source,
    reduce_selection, CellAttr, Dag, Focus, Layout, LensTint, LineageLens, MaterializationClass,
    ModelList, NodeInfo, SortMode, UiState,
};

/// What the event loop should do after an action: keep running (and run any
/// recorded effects) or quit. Effects are data; the loop performs them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub quit: bool,
    pub effects: Vec<Effect>,
}

impl Outcome {
    /// Keep running, no effects.
    pub fn cont() -> Self {
        Outcome::default()
    }
    /// Quit the app.
    pub fn quit() -> Self {
        Outcome {
            quit: true,
            effects: Vec::new(),
        }
    }
    /// Keep running and request one effect.
    pub fn effect(e: Effect) -> Self {
        Outcome {
            quit: false,
            effects: vec![e],
        }
    }
}

/// The lineage view filter: which directions to show and an optional hop-depth
/// limit. Default = the full lineage (both directions, unlimited).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageView {
    pub upstream: bool,
    pub downstream: bool,
    pub depth: Option<usize>,
}

impl Default for LineageView {
    fn default() -> Self {
        LineageView {
            upstream: true,
            downstream: true,
            depth: None,
        }
    }
}

/// The persistent left-pane filter: show every model, only the untested ones
/// (the [`coverage_gap`] set — the same predicate as the Coverage lens), or
/// only the bookmarked ones. Orthogonal to the transient name search, which
/// narrows on top of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ListFilter {
    #[default]
    All,
    Untested,
    Bookmarked,
}

/// Project-level counts for the title bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppStats {
    pub project: String,
    pub models: usize,
    pub sources: usize,
    pub seeds: usize,
    pub snapshots: usize,
}

/// The whole application state. Owns the domain (`dag`, lists) and the UI
/// (`ui_state`, `mode`) together — the single place that knows both, which is
/// what lets domain reducer arms exist while `UiState` stays untouched.
pub struct App {
    pub dag: Dag,
    /// The full, unfiltered model list (selection space when not searching).
    pub model_list: ModelList,
    /// The filtered view while a list search is active; `None` otherwise.
    pub filter: Option<ModelList>,
    /// The persistent left-pane filter (`T` untested / `*` bookmarked). Unlike
    /// the transient search `filter`, it survives mode changes; a list search
    /// narrows ON TOP of it ([`refilter`](App::refilter) builds from
    /// [`base_list`](App::base_list)).
    pub list_filter: ListFilter,
    /// The `list_filter` view of `model_list` (`None` when `list_filter` is
    /// `All`). Rebuilt ONLY by [`apply_list_filter`](App::apply_list_filter).
    filtered_base: Option<ModelList>,
    /// How the left pane orders models within each layer group (the `.`-cycle).
    /// Preserved across reload; the full + filtered lists are rebuilt with it.
    pub sort: SortMode,
    /// Bookmarked model `unique_id`s (Space toggles, `'` cycles). A `BTreeSet`
    /// for deterministic iteration; lives on `App` (not the list) so toggling a
    /// bookmark never rebuilds the list and it survives a refilter untouched.
    /// `reload` prunes ids that no longer resolve.
    pub bookmarks: BTreeSet<String>,
    pub ui_state: UiState,
    pub mode: Mode,
    /// Project name + resource counts, for the title bar.
    pub stats: AppStats,
    /// The lineage direction/depth filter.
    pub lineage_view: LineageView,
    /// Which glyph repertoire the lineage grid + chrome draw with. Defaults to
    /// Unicode; `main` overwrites it from the CLI flags / the startup
    /// ambiguous-width terminal probe.
    pub glyph_mode: crate::GlyphMode,
    /// The lineage cursor: which node of the CURRENT subgraph the movement keys
    /// highlight (`None` = the rooted selection). The event loop resets it at
    /// its selection-change chokepoint (uid-tracked, so every id-changing path —
    /// keys, mouse re-root, search confirm, history — re-homes it); the two
    /// id-PRESERVING paths reset explicitly ([`reload`](App::reload), and
    /// [`click_lineage_node`](App::click_lineage_node) on the current root).
    /// Every read goes through [`lineage_cursor_uid`](App::lineage_cursor_uid),
    /// which validates against the live subgraph, so a stale id can never escape.
    lineage_cursor: Option<String>,
    /// The transient-notice channel: a one-shot message describing the latest
    /// user-visible outcome (copied / bookmarked / exported / reloaded).
    /// Writers: the reducer arms (intent) and `main::run_effect` (which
    /// OVERWRITES the intent on effect failure — it runs after `apply_action`
    /// in the same loop iteration, so the failure always wins). The event loop
    /// drains it via [`take_notice`](App::take_notice) and owns the on-screen
    /// toast lifetime — `App` stores no clock, so the reducer stays pure.
    notice: Option<String>,
    /// Re-root history (back/forward stacks of `unique_id`s).
    back: Vec<String>,
    forward: Vec<String>,
    /// Where the `Dag` came from, so reload re-reads the same source.
    source: DataSource,
    /// dbt project root (`<root>/target/manifest.json` → `<root>`, or the project
    /// dir in source mode), used to resolve a model's `original_file_path` for
    /// the `$EDITOR` jump.
    project_root: PathBuf,
}

/// Where the `Dag` was loaded from, so `reload` re-reads the same source.
enum DataSource {
    /// A compiled `manifest.json` at this path.
    Manifest(PathBuf),
    /// A dbt project directory parsed from source (no `dbt compile`).
    Project(PathBuf),
}

impl App {
    /// Build the app from a `Dag` loaded from a compiled manifest at `manifest_path`.
    pub fn new(dag: Dag, manifest_path: PathBuf) -> Self {
        let project_root = derive_project_root(&manifest_path);
        App::with_source(dag, DataSource::Manifest(manifest_path), project_root)
    }

    /// Build the app from a `Dag` parsed from a dbt project directory (source
    /// mode). The project dir is itself the root for resolving file paths.
    pub fn from_source(dag: Dag, project_dir: PathBuf) -> Self {
        let project_root = project_dir.clone();
        App::with_source(dag, DataSource::Project(project_dir), project_root)
    }

    fn with_source(dag: Dag, source: DataSource, project_root: PathBuf) -> Self {
        let sort = SortMode::default();
        let model_list = build_model_list(&dag, sort);
        let ui_state = UiState::new(model_list.len());
        let stats = compute_stats(&dag);
        App {
            dag,
            model_list,
            filter: None,
            list_filter: ListFilter::default(),
            filtered_base: None,
            sort,
            bookmarks: BTreeSet::new(),
            ui_state,
            mode: Mode::Selection,
            stats,
            lineage_view: LineageView::default(),
            glyph_mode: crate::GlyphMode::default(),
            lineage_cursor: None,
            notice: None,
            back: Vec::new(),
            forward: Vec::new(),
            source,
            project_root,
        }
    }

    /// Record a transient notice (the toast text). Last write wins: a later
    /// writer in the same loop iteration (e.g. `run_effect` reporting a
    /// failure) deliberately replaces the reducer's optimistic intent.
    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    /// Drain the pending notice (one-shot). The event loop calls this once per
    /// frame and stamps the wall-clock there, keeping time out of the reducer.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// The lineage subgraph for the current selection under the active
    /// [`LineageView`] (direction + depth). Empty when nothing is selected.
    pub fn lineage_subgraph(&self) -> crate::Subgraph {
        match self.selected_unique_id() {
            Some(uid) => self.dag.subgraph_view(
                &uid,
                self.lineage_view.upstream,
                self.lineage_view.downstream,
                self.lineage_view.depth,
            ),
            None => crate::Subgraph {
                selected: String::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
        }
    }

    /// The lineage cursor's `unique_id`: the stored cursor if it is still a
    /// member of the current subgraph, else the rooted selection (the cursor's
    /// home position — also covers a cursor dropped by a direction/depth
    /// change). `None` only when nothing is selected.
    pub fn lineage_cursor_uid(&self) -> Option<String> {
        let sg = self.lineage_subgraph();
        if let Some(cur) = &self.lineage_cursor {
            if sg.contains(cur) {
                return Some(cur.clone());
            }
        }
        self.selected_unique_id()
    }

    /// Send the lineage cursor home (back to the rooted selection). Called by
    /// the event loop whenever the selection changes, and by `z` (recenter).
    pub fn reset_lineage_cursor(&mut self) {
        self.lineage_cursor = None;
    }

    /// Handle a lineage-pane click on `uid`: a model RE-ROOTS to it (recording
    /// history, and the cursor goes home with it — also when clicking the
    /// current root); a non-selectable node (source/seed/snapshot) just MOVES
    /// THE CURSOR there, so Enter can open its structure modal. The domain half
    /// of the mouse path (`main::handle_mouse` does the size-aware hit-test).
    pub fn click_lineage_node(&mut self, uid: &str) {
        self.jump_to(uid); // no-op for non-models
        if self.selected_unique_id().as_deref() == Some(uid) {
            // Clicked the (possibly just re-rooted) root: cursor home. The
            // loop's chokepoint only fires when the selected id CHANGES, so
            // clicking the current root must reset here.
            self.lineage_cursor = None;
        } else if self.lineage_subgraph().contains(uid) {
            self.lineage_cursor = Some(uid.to_string());
        }
    }

    /// The subgraph the lineage pane DRAWS: the rooted subgraph with `selected`
    /// swapped to the cursor, so the existing emphasis + `selected_rect`
    /// machinery highlights and viewport-follows the cursor with no new render
    /// code. Root semantics (pane title, exports, search matches, hit-test
    /// re-root) keep reading [`lineage_subgraph`](App::lineage_subgraph).
    pub fn lineage_display_subgraph(&self) -> crate::Subgraph {
        let mut sg = self.lineage_subgraph();
        if let Some(cur) = &self.lineage_cursor {
            if sg.contains(cur) {
                sg.selected = cur.clone();
            }
        }
        sg
    }

    /// Move the lineage cursor one node SPATIALLY over the current layout:
    /// Up/Down step through the cursor's own column stack, Left/Right jump to
    /// the row-nearest node of the adjacent column (every column `0..=max` is
    /// populated — longest-path layering guarantees a parent in `c-1` for any
    /// node in `c`). No-op at a graph edge or on an empty subgraph.
    ///
    /// Size-unaware by contract: this reads CharGrid geometry (which is
    /// glyph-mode-identical and pane-independent), never pane size. The
    /// deterministic tie-break (row distance, then y, then the column sort) is
    /// what keeps cursor paths reproducible across runs.
    pub fn move_lineage_cursor(&mut self, dir: Direction) {
        let lay = crate::layout(&self.lineage_subgraph());
        let Some(cur) = self.lineage_cursor_uid() else {
            return;
        };
        let (Some(&cur_rect), Some(&cur_col)) = (lay.rects.get(&cur), lay.columns.get(&cur)) else {
            return;
        };
        let target_col = match dir {
            Direction::Up | Direction::Down => cur_col,
            Direction::Left => match cur_col.checked_sub(1) {
                Some(c) => c,
                None => return, // already in the leftmost (most-upstream) column
            },
            Direction::Right => cur_col + 1,
        };
        // The candidate column, y-stacked (rects is a HashMap; sorting restores
        // determinism — y is unique within a column, uid breaks hypothetical ties).
        let mut col: Vec<_> = lay
            .rects
            .iter()
            .filter(|(uid, _)| lay.columns.get(uid.as_str()) == Some(&target_col))
            .collect();
        col.sort_by_key(|(uid, r)| (r.y, (*uid).clone()));

        let next = match dir {
            Direction::Down | Direction::Up => {
                let Some(i) = col.iter().position(|(uid, _)| uid.as_str() == cur) else {
                    return;
                };
                let j = match dir {
                    Direction::Down => i + 1,
                    _ => match i.checked_sub(1) {
                        Some(j) => j,
                        None => return, // already at the top of the column
                    },
                };
                col.get(j).map(|(uid, _)| (*uid).clone())
            }
            Direction::Left | Direction::Right => {
                // Nearest by middle-row distance; equidistant prefers the upper.
                let cur_mid = cur_rect.y + cur_rect.height / 2;
                col.iter()
                    .min_by_key(|(_, r)| (cur_mid.abs_diff(r.y + r.height / 2), r.y))
                    .map(|(uid, _)| (*uid).clone())
            }
        };
        if let Some(uid) = next {
            self.lineage_cursor = Some(uid);
        }
    }

    /// Send the lineage cursor to the leftmost (column 0) or rightmost column,
    /// picking the row-nearest node — the same middle-row metric as the h/l
    /// single-column moves, so H is "h all the way" and L is "l all the way".
    pub fn move_lineage_cursor_extreme(&mut self, rightmost: bool) {
        let lay = crate::layout(&self.lineage_subgraph());
        let Some(cur) = self.lineage_cursor_uid() else {
            return;
        };
        let Some(&cur_rect) = lay.rects.get(&cur) else {
            return;
        };
        let target_col = if rightmost {
            lay.columns.values().max().copied().unwrap_or(0)
        } else {
            0
        };
        let cur_mid = cur_rect.y + cur_rect.height / 2;
        // Nearest by middle-row distance; equidistant prefers the upper (the
        // same deterministic tie-break as move_lineage_cursor's Left/Right).
        let next = lay
            .rects
            .iter()
            .filter(|(uid, _)| lay.columns.get(uid.as_str()) == Some(&target_col))
            .min_by_key(|(uid, r)| (cur_mid.abs_diff(r.y + r.height / 2), r.y, (*uid).clone()))
            .map(|(uid, _)| uid.clone());
        if let Some(uid) = next {
            self.lineage_cursor = Some(uid);
        }
    }

    /// A short label for the lineage pane title describing the active view:
    /// direction (`↑↓` / `↑` / `↓` / `·`, or `<>` / `<` / `>` / `-` in ASCII
    /// glyph mode — left=upstream like the diagram) and depth (`≤N` / `<=N`
    /// when limited).
    pub fn lineage_view_label(&self) -> String {
        let v = &self.lineage_view;
        let (dir, leq) = match self.glyph_mode {
            crate::GlyphMode::Unicode => (
                match (v.upstream, v.downstream) {
                    (true, true) => "↑↓",
                    (true, false) => "↑",
                    (false, true) => "↓",
                    (false, false) => "·",
                },
                "≤",
            ),
            crate::GlyphMode::Ascii => (
                match (v.upstream, v.downstream) {
                    (true, true) => "<>",
                    (true, false) => "<",
                    (false, true) => ">",
                    (false, false) => "-",
                },
                "<=",
            ),
        };
        match v.depth {
            Some(d) => format!("{dir} {leq}{d}"),
            None => dir.to_string(),
        }
    }

    /// Re-root the lineage to `unique_id`, recording the move in the back stack
    /// (and clearing the forward stack) — the browser-style history for clicks /
    /// lineage-search jumps. No-op if the target isn't a selectable model.
    pub fn jump_to(&mut self, unique_id: &str) {
        let current = self.selected_unique_id();
        if self.select_by_unique_id(unique_id) {
            if let Some(cur) = current {
                if cur != unique_id {
                    self.back.push(cur);
                    self.forward.clear();
                }
            }
        }
    }

    /// Go back one step in the re-root history (pushes the current onto forward).
    pub fn history_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            if let Some(cur) = self.selected_unique_id() {
                self.forward.push(cur);
            }
            self.select_by_unique_id(&prev);
        }
    }

    /// Go forward one step in the re-root history (pushes the current onto back).
    pub fn history_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            if let Some(cur) = self.selected_unique_id() {
                self.back.push(cur);
            }
            self.select_by_unique_id(&next);
        }
    }

    /// The currently selected node in the active list, if any. The single
    /// accessor for "the selected node"; every `selected_*` projection derives
    /// from it, so the (which-list, no-guard) definition lives in one place.
    pub fn selected_node(&self) -> Option<&NodeInfo> {
        self.active_list().model_at(self.ui_state.selected())
    }

    /// The selected node's `unique_id`, if any.
    pub fn selected_unique_id(&self) -> Option<String> {
        self.selected_node().map(|n| n.unique_id.clone())
    }

    /// The selected node's display name, if any.
    pub fn selected_name(&self) -> Option<String> {
        self.selected_node().map(|n| n.name.clone())
    }

    /// The `unique_id` a focus-routed overlay (`Enter`/`s`) acts on: the lineage
    /// cursor when the lineage pane is focused (so the action targets whatever the
    /// cursor is on), else the list selection. The single source of that policy —
    /// `DetailOpen` and `SqlOpen` both call it, so a focus-routing change touches
    /// one place.
    pub fn focus_target(&self) -> Option<String> {
        if self.ui_state.focus() == Focus::RightPane {
            self.lineage_cursor_uid()
        } else {
            self.selected_unique_id()
        }
    }

    /// A node's display name, falling back to the `unique_id` itself when the node
    /// is unknown to the Dag — the repeated overlay-title idiom.
    pub fn node_name_or_uid(&self, uid: &str) -> String {
        self.dag
            .get(uid)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| uid.to_string())
    }

    /// The selected model's SQL file path, resolved against the project root
    /// (`<root>/<original_file_path>`). `None` if nothing is selected or the node
    /// has no recorded file path.
    pub fn selected_file_path(&self) -> Option<String> {
        let uid = self.selected_node()?.unique_id.clone();
        let ofp = self.dag.detail(&uid)?.original_file_path.clone()?;
        Some(self.project_root.join(ofp).to_string_lossy().into_owned())
    }

    /// The list currently driving the left pane: the search view if a search
    /// is narrowing it, else the persistent-filter view, else the full list.
    /// Selection indexes into THIS list.
    pub fn active_list(&self) -> &ModelList {
        self.filter.as_ref().unwrap_or_else(|| self.base_list())
    }

    /// The list a search narrows FROM: the persistent-filter view when one is
    /// active, else the full list.
    fn base_list(&self) -> &ModelList {
        self.filtered_base.as_ref().unwrap_or(&self.model_list)
    }

    /// The short status/title tag for the active persistent filter. BARE text
    /// (no brackets): the list title and the status segment each add their own
    /// `[..]` wrapper.
    pub fn list_filter_label(&self) -> Option<&'static str> {
        match self.list_filter {
            ListFilter::All => None,
            ListFilter::Untested => Some("untested"),
            ListFilter::Bookmarked => Some("marked"),
        }
    }

    /// Recompute the persistent-filter view from the current `model_list` /
    /// `bookmarks` and re-resolve the selection by `unique_id` (else top).
    /// Called on every input the view depends on: the `T`/`*` toggles, a sort
    /// change, a reload, and a bookmark toggle while the Bookmarked filter is
    /// live. With a list search open it defers selection to `refilter`, which
    /// narrows from the new base.
    pub fn apply_list_filter(&mut self) {
        let current = self.selected_node().map(|n| n.unique_id.clone());
        self.filtered_base = match self.list_filter {
            ListFilter::All => None,
            ListFilter::Untested => Some(crate::model_list::filter_model_list(
                &self.model_list,
                coverage_gap,
            )),
            ListFilter::Bookmarked => Some(crate::model_list::filter_model_list(
                &self.model_list,
                |m| self.bookmarks.contains(&m.unique_id),
            )),
        };
        if matches!(&self.mode, Mode::Search(s) if s.target == SearchTarget::List) {
            self.refilter();
            return;
        }
        self.ui_state.set_model_count(self.active_list().len());
        let resolved = current
            .as_deref()
            .is_some_and(|uid| self.select_by_unique_id(uid));
        if !resolved {
            self.ui_state.set_selected(0);
        }
    }

    /// Move the selection to the model with this NAME in the active list, if
    /// present (dbt model names are project-unique). The `--select` startup
    /// flag's primitive.
    pub fn select_by_name(&mut self, name: &str) -> bool {
        let uid = self
            .active_list()
            .models
            .iter()
            .find(|n| n.name == name)
            .map(|n| n.unique_id.clone());
        uid.is_some_and(|uid| self.select_by_unique_id(&uid))
    }

    /// The on-disk origin of the loaded data, for the `--watch` poller:
    /// `(path, is_dir)` — the manifest FILE in manifest mode, the project DIR
    /// in source mode.
    pub fn watch_root(&self) -> (&Path, bool) {
        match &self.source {
            DataSource::Manifest(path) => (path, false),
            DataSource::Project(dir) => (dir, true),
        }
    }

    /// Move the selection to the model with this `unique_id` in the active list,
    /// if present. Returns whether it was found. The selection-by-identity
    /// primitive used by reload-restore, mouse re-root, and lineage search.
    pub fn select_by_unique_id(&mut self, unique_id: &str) -> bool {
        let idx = self
            .active_list()
            .models
            .iter()
            .position(|n| n.unique_id == unique_id);
        match idx {
            Some(i) => {
                self.ui_state.set_selected(i);
                true
            }
            None => false,
        }
    }

    /// Render the current lineage subgraph as a Mermaid `graph LR` diagram
    /// (deterministic: the subgraph's nodes/edges are already sorted). Node IDs
    /// are the sanitized `unique_id`; labels carry the name + materialization.
    /// `None` when nothing is selected / the subgraph is empty.
    ///
    /// The diagram is wrapped in a ```` ```mermaid ```` fenced code block so that
    /// pasting the yank straight into a Markdown surface (GitHub, Notion,
    /// Confluence, Slack, Obsidian, …) renders an actual diagram instead of plain
    /// text. The fence is the only thing standing between "valid Mermaid source"
    /// and "renders where people paste it". (Trade-off: a raw editor like
    /// mermaid.live wants the body without the fence — strip the first/last line
    /// there.)
    pub fn lineage_mermaid(&self) -> Option<String> {
        let sg = self.lineage_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
        let mut out = String::from("```mermaid\ngraph LR\n");
        for n in &sg.nodes {
            let kind = node_kind(n);
            let mark = if n.unique_id == sg.selected { " *" } else { "" };
            out.push_str(&format!(
                "  {}[\"{} ({}){}\"]\n",
                mermaid_id(&n.unique_id),
                mermaid_label(&n.name),
                kind,
                mark,
            ));
        }
        for e in &sg.edges {
            out.push_str(&format!(
                "  {} --> {}\n",
                mermaid_id(&e.parent),
                mermaid_id(&e.child)
            ));
        }
        out.push_str("```\n");
        Some(out)
    }

    /// Render the current lineage subgraph as a Graphviz DOT digraph (`rankdir=LR`).
    /// Node ids are the quoted `unique_id`; labels carry name + materialization.
    /// `None` when nothing is selected / the subgraph is empty.
    pub fn lineage_dot(&self) -> Option<String> {
        let sg = self.lineage_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
        let mut out = String::from("digraph lineage {\n  rankdir=LR;\n  node [shape=box];\n");
        for n in &sg.nodes {
            let kind = node_kind(n);
            out.push_str(&format!(
                "  \"{}\" [label=\"{}\\n({})\"];\n",
                dot_escape(&n.unique_id),
                dot_escape(&n.name),
                kind,
            ));
        }
        for e in &sg.edges {
            out.push_str(&format!(
                "  \"{}\" -> \"{}\";\n",
                dot_escape(&e.parent),
                dot_escape(&e.child)
            ));
        }
        out.push_str("}\n");
        Some(out)
    }

    /// Render the current lineage subgraph as plain text — the exact diagram
    /// the lineage pane draws (same `layout_mode` + glyph repertoire), minus
    /// colours. Pasteable into tickets/docs as a monospace block. `None` when
    /// nothing is selected / the subgraph is empty.
    pub fn lineage_ascii(&self) -> Option<String> {
        let sg = self.lineage_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
        Some(crate::layout_mode(&sg, self.glyph_mode).grid.to_text())
    }

    /// The selected node's raw (uncompiled) SQL, cloned from the `Dag::sql`
    /// side map. `None` for nodes without source SQL — manifests record seeds
    /// with `raw_code: ""`, so blank SQL is treated as absent (an empty yank
    /// would silently clobber the clipboard with nothing).
    pub fn selected_raw_sql(&self) -> Option<String> {
        let uid = self.selected_unique_id()?;
        self.dag
            .raw_code(&uid)
            .filter(|code| !code.trim().is_empty())
            .map(str::to_string)
    }

    /// A Markdown blast-radius report for the selected node: direct +
    /// transitive up/down counts and the sorted member name lists. The closures
    /// come from `HashSet`s, so both lists are sorted here — two yanks of the
    /// same selection are byte-identical. `None` when nothing is selected.
    pub fn impact_report(&self) -> Option<String> {
        let uid = self.selected_unique_id()?;
        let node = self.dag.get(&uid)?;
        let names = |uids: std::collections::HashSet<String>| -> Vec<String> {
            let mut v: Vec<String> = uids
                .into_iter()
                .filter_map(|u| self.dag.get(&u).map(|n| n.name.clone()))
                .collect();
            v.sort();
            v
        };
        let down = names(self.dag.downstream(&uid));
        let up = names(self.dag.upstream(&uid));
        let mut out = format!("# Impact: {}\n\n", node.name);
        out.push_str(&format!("- unique_id: `{uid}`\n"));
        out.push_str(&format!(
            "- direct: {} upstream / {} downstream\n",
            node.direct_up, node.direct_down
        ));
        out.push_str(&format!(
            "- transitive: {} upstream / {} downstream\n",
            up.len(),
            down.len()
        ));
        for (title, list) in [("Downstream (blast radius)", &down), ("Upstream", &up)] {
            out.push_str(&format!("\n## {title} ({})\n", list.len()));
            for name in list {
                out.push_str(&format!("- {name}\n"));
            }
        }
        Some(out)
    }

    /// The first node in the current lineage subgraph whose name matches `query`
    /// (subsequence). The subgraph is `dag.subgraph(selected)`, whose `nodes` are
    /// sorted by `unique_id`, so the match is deterministic. Used by the lineage
    /// node-jump search (size-unaware: just the match; the loop does the anchor).
    pub fn lineage_matches(&self, query: &str) -> Vec<String> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        self.lineage_subgraph()
            .nodes
            .iter()
            .filter(|n| crate::name_matches_query(&n.name, query))
            .map(|n| n.unique_id.clone())
            .collect()
    }

    /// The lineage node currently targeted by an active lineage-search: the
    /// `match_idx`-th match (cycled by Up/Down). `None` unless a lineage search
    /// with a non-empty query is open and has at least one match — so an empty
    /// query never jolts the viewport off the rooted node.
    pub fn current_lineage_match(&self) -> Option<String> {
        if let Mode::Search(s) = &self.mode {
            if s.target == SearchTarget::Lineage {
                let matches = self.lineage_matches(&s.query);
                if matches.is_empty() {
                    return None;
                }
                let idx = s.match_idx % matches.len();
                return matches.get(idx).cloned();
            }
        }
        None
    }

    /// A one-line summary of the selected node for the status bar:
    /// `"<materialized>, tests: N"` (materialization omitted for sources).
    /// `None` when nothing is selected.
    pub fn selected_status_note(&self) -> Option<String> {
        let uid = &self.selected_node()?.unique_id;
        let mat = self.dag.detail(uid).and_then(|d| d.materialized.clone());
        let tests = self.dag.tests(uid).len();
        let mut parts: Vec<String> = Vec::new();
        if let Some(m) = mat {
            parts.push(m);
        }
        parts.push(format!("tests: {tests}"));
        let sep = match self.glyph_mode {
            crate::GlyphMode::Unicode => " · ",
            crate::GlyphMode::Ascii => ", ",
        };
        Some(parts.join(sep))
    }

    /// Compute the stats-dashboard payload from the `Dag` at open time, so the
    /// render layer never needs a `Dag` (mirrors [`DetailView`]). Deterministic:
    /// counts accumulate into `BTreeMap`s and hubs are fully sorted (degree desc,
    /// then `unique_id` asc for a stable tie-break), so the result is bit-stable
    /// across runs despite the `HashMap` node iteration order.
    pub fn compute_stats_view(&self) -> StatsView {
        let mut by_rt: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_mat: BTreeMap<String, usize> = BTreeMap::new();
        let mut zero_down = 0;
        let mut no_desc = 0;
        let mut hubs: Vec<(String, String, usize)> = Vec::new();
        // Transitive blast-radius hubs over ALL nodes (same base as `hubs`), and
        // orphan models (zero kept parents AND zero kept children).
        let mut transitive: Vec<(String, String, usize)> = Vec::new();
        let mut orphans: Vec<String> = Vec::new();
        // Coverage comes from the ONE shared source (`coverage_summary`, the
        // `coverage_gap` base) so the dashboard, the `t` lens, and the status
        // `cov` segment always quote the same numbers.
        let (testable_tested, testable_total) = self.coverage_summary();
        for (uid, n) in self.dag.nodes() {
            *by_rt.entry(n.resource_type.clone()).or_default() += 1;
            let degree = n.direct_up + n.direct_down;
            hubs.push((uid.clone(), n.name.clone(), degree));
            transitive.push((uid.clone(), n.name.clone(), self.dag.downstream(uid).len()));
            if n.resource_type == "model" {
                let mat = n.materialized.clone().unwrap_or_else(|| "(none)".into());
                *by_mat.entry(mat).or_default() += 1;
                if n.direct_down == 0 {
                    zero_down += 1;
                }
                if n.direct_up == 0 && n.direct_down == 0 {
                    orphans.push(n.name.clone());
                }
                let has_desc = self
                    .dag
                    .detail(uid)
                    .and_then(|d| d.description.as_ref())
                    .is_some();
                if !has_desc {
                    no_desc += 1;
                }
            }
        }
        // Top-5 by degree desc, then unique_id asc for a deterministic tie-break.
        hubs.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        hubs.truncate(5);
        // Top-5 by transitive-downstream closure desc, unique_id asc tie-break;
        // project to (name, count) AFTER sorting (the tie-break key is the uid).
        transitive.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        transitive.truncate(5);
        let transitive_hubs: Vec<(String, usize)> =
            transitive.into_iter().map(|(_, name, c)| (name, c)).collect();
        orphans.sort();
        // Reuse the single layer-violation predicate; map uids → names, sort.
        let mut layer_violations: Vec<(String, String)> = layer_violation_edges(&self.dag)
            .into_iter()
            .map(|(p, c)| {
                let pn = self.dag.get(&p).map_or(p, |n| n.name.clone());
                let cn = self.dag.get(&c).map_or(c, |n| n.name.clone());
                (pn, cn)
            })
            .collect();
        layer_violations.sort();
        let critical_path = longest_chain(&self.dag);
        StatsView {
            project: self.stats.project.clone(),
            by_resource_type: by_rt.into_iter().collect(),
            by_materialization: by_mat.into_iter().collect(),
            testable_total,
            testable_tested,
            top_hubs: hubs,
            transitive_hubs,
            orphan_models: orphans,
            layer_violations,
            critical_path,
            untested_testable: testable_total - testable_tested,
            zero_downstream_models: zero_down,
            no_description_models: no_desc,
            scroll: 0,
        }
    }

    /// The set of node `unique_id`s on the path from the rooted selection to the
    /// lineage cursor, over the CURRENT [`lineage_subgraph`](App::lineage_subgraph)
    /// treated as UNDIRECTED (the cursor can be up- OR downstream of the root).
    /// Empty when the cursor is home (`== selection`) or nothing is selected — so
    /// a home cursor highlights nothing.
    ///
    /// Deterministic: `lineage_subgraph().edges` are already sorted, the
    /// undirected adjacency is built in that order, and the BFS records the
    /// first-found predecessor — so two calls produce the identical set. The set
    /// is only ever read via `.contains(uid)` (in [`lineage_styles`]), never
    /// iterated into output, so no `HashSet` order can leak.
    ///
    /// [`lineage_styles`]: App::lineage_styles
    pub fn lineage_path_set(&self) -> std::collections::HashSet<String> {
        use std::collections::{HashMap, HashSet, VecDeque};
        let (Some(root), Some(cursor)) = (self.selected_unique_id(), self.lineage_cursor_uid())
        else {
            return HashSet::new();
        };
        if root == cursor {
            return HashSet::new();
        }
        // Root semantics (NOT the display copy): same node/edge set, but keyed off
        // the rooted selection independent of which node the display "selects".
        let sg = self.lineage_subgraph();
        // Undirected adjacency, built in the (already-sorted) edge order.
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &sg.edges {
            adj.entry(&e.parent).or_default().push(&e.child);
            adj.entry(&e.child).or_default().push(&e.parent);
        }
        // BFS root -> cursor, recording the first-found predecessor (deterministic).
        let mut prev: HashMap<&str, &str> = HashMap::new();
        let mut seen: HashSet<&str> = HashSet::from([root.as_str()]);
        let mut q: VecDeque<&str> = VecDeque::from([root.as_str()]);
        let mut found = false;
        while let Some(cur) = q.pop_front() {
            if cur == cursor.as_str() {
                found = true;
                break;
            }
            if let Some(ns) = adj.get(cur) {
                for &nb in ns {
                    if seen.insert(nb) {
                        prev.insert(nb, cur);
                        q.push_back(nb);
                    }
                }
            }
        }
        let mut path = HashSet::new();
        if found {
            // Walk predecessors back from the cursor to the root, collecting both.
            let mut node = cursor.as_str();
            loop {
                path.insert(node.to_string());
                if node == root.as_str() {
                    break;
                }
                match prev.get(node) {
                    Some(&p) => node = p,
                    None => break,
                }
            }
        }
        path
    }

    /// The blast radius of the current ROOT (selected) node as
    /// `(downstream_count, upstream_count)`: the sizes of its transitive
    /// descendant / ancestor closures. Both come from the `Dag` closure methods,
    /// which already EXCLUDE the node itself, so the node is never double-counted.
    /// Order-independent (just `.len()` over a `HashSet`), hence deterministic.
    /// `None` when nothing is selected.
    pub fn impact_counts(&self) -> Option<(usize, usize)> {
        self.selected_unique_id()
            .map(|uid| self.impact_counts_for(&uid))
    }

    /// The blast radius of an arbitrary node as `(downstream_count,
    /// upstream_count)`. Shared by [`impact_counts`](App::impact_counts) (root) and
    /// the structure modal (which targets the focus uid, NOT necessarily the root —
    /// the lineage cursor can sit on a non-root source/seed/snapshot).
    pub fn impact_counts_for(&self, uid: &str) -> (usize, usize) {
        (self.dag.downstream(uid).len(), self.dag.upstream(uid).len())
    }

    /// The "impact ↓D ↑U" status segment for the selected node, using the active
    /// glyph mode's [`Chrome`](crate::ui::Chrome) down/up badges (Unicode `↓`/`↑`,
    /// ASCII `v`/`^`) so the arrows are never hardcoded (the ascii-guard contract).
    /// `None` when nothing is selected. Mirrors
    /// [`lineage_view_label`](App::lineage_view_label)'s role: the one place this
    /// string is built, reused by the loop and the ascii-guard render test.
    pub fn impact_status(&self) -> Option<String> {
        let (down, up) = self.impact_counts()?;
        let chrome = crate::ui::chrome(self.glyph_mode);
        Some(format!(
            "impact {d}{down} {u}{up}",
            d = chrome.badge_down,
            u = chrome.badge_up
        ))
    }

    /// The lineage re-root breadcrumb for the pane title: the last up-to-3
    /// back-history node NAMES followed by the current root name, joined with
    /// `" > "` (ASCII). `None` when the back-history is empty (so a fresh title is
    /// unchanged). Uids that no longer resolve to a node are skipped.
    ///
    /// If the joined trail exceeds `max_width`, whole entries are dropped from the
    /// LEFT (oldest first) until it fits, then a `".."` prefix is added (ASCII —
    /// never the `…` ellipsis, which is ambiguous-width). Size-unaware:
    /// `max_width` is a parameter, chosen at the width-aware render seam.
    /// Deterministic: it reads the ordered `back` stack and the Dag.
    ///
    /// This producer is *lenient*: when even `".. > root"` overflows it returns it
    /// anyway (the title block clips it). The render seam re-truncates STRICTLY via
    /// [`fit_breadcrumb`] so the trailing `[{v}]{lens}` title suffixes always
    /// survive — so the loop should hand the FULL trail (`usize::MAX`) here and let
    /// the draw seam be the single width authority.
    pub fn breadcrumb(&self, max_width: usize) -> Option<String> {
        if self.back.is_empty() {
            return None;
        }
        let root = self.selected_unique_id()?;
        // The last up-to-3 history uids (oldest→newest), then the current root;
        // uids that no longer resolve are skipped (so a vanished node drops out).
        let mut entries: Vec<String> = self
            .back
            .iter()
            .rev()
            .take(3)
            .rev()
            .filter_map(|uid| self.dag.get(uid).map(|n| n.name.clone()))
            .collect();
        entries.push(self.dag.get(&root).map_or(root, |n| n.name.clone()));

        let join = |parts: &[String]| parts.join(" > ");
        let full = join(&entries);
        // Lenient: the strict fit can drop to empty, but a producer never returns
        // an empty trail — fall back to ".. > root" (the title block clips it).
        Some(fit_breadcrumb(&full, max_width).unwrap_or_else(|| {
            format!(".. > {}", entries[entries.len() - 1])
        }))
    }

    /// `(tested, total)` over the project's testable resources (the
    /// [`coverage_gap`](crate::coverage_gap) base: model / snapshot / seed). Used
    /// for the "cov NN%" status segment when the lens is on. Deterministic: an
    /// order-independent sum over the node map.
    pub fn coverage_summary(&self) -> (usize, usize) {
        let mut total = 0;
        let mut tested = 0;
        for n in self.dag.nodes().values() {
            if matches!(n.resource_type.as_str(), "model" | "snapshot" | "seed") {
                total += 1;
                if n.test_count > 0 {
                    tested += 1;
                }
            }
        }
        (tested, total)
    }

    /// Build the per-node render attributes for a lineage [`Layout`], folding in
    /// the materialization class (→ colour) always, plus three render-only
    /// overlays that share the foreground/background channels:
    /// - `lens`: the ACTIVE lineage lens's [`LensTint`] for this node (computed by
    ///   [`lens_tint`](App::lens_tint)); `None` when the lens is `Off`.
    /// - `dimmed`: focus dim — set on every node NOT on the root↔cursor path
    ///   whenever the path is non-empty (i.e. the cursor is off-root). Lens-
    ///   independent; the render layer dims its foreground so the path nodes pop.
    /// - `on_path`: the node is on the root↔cursor path (the orthogonal bg band).
    ///
    /// Keyed by `unique_id` over the layout's nodes; fed to
    /// [`Layout::apply_node_styles`] by the event loop after `layout()`.
    ///
    /// Scope: path highlight + dim are NODE-BOXES-ONLY — `Layout` exposes no
    /// edge→cell map, and `apply_node_styles` stamps `rects` only, so the
    /// connector cells between path nodes stay Plain. (Tests show up as the
    /// `tests:N` bottom-border label, drawn by `layout()` itself from
    /// `NodeInfo::test_count` — no style needed here.)
    pub fn lineage_styles(&self, lay: &Layout) -> BTreeMap<String, CellAttr> {
        let lens = self.ui_state.lens();
        let path = self.lineage_path_set(); // empty unless the cursor is off-root
        let dimming = !path.is_empty();
        // The LayerViolation lens needs the project-wide incident-node set; compute
        // it ONCE here (not per node) so it is O(edges) regardless of subgraph size.
        let violation_nodes: std::collections::HashSet<String> =
            if lens == LineageLens::LayerViolation {
                layer_violation_edges(&self.dag)
                    .into_iter()
                    .flat_map(|(p, c)| [p, c])
                    .collect()
            } else {
                std::collections::HashSet::new()
            };
        lay.rects
            .keys()
            .map(|uid| {
                let node = self.dag.get(uid);
                let rt = node.map(|n| n.resource_type.as_str()).unwrap_or("");
                let mat = self.dag.detail(uid).and_then(|d| d.materialized.as_deref());
                let attr = CellAttr {
                    class: MaterializationClass::classify(rt, mat),
                    lens: self.lens_tint(lens, uid, node, &violation_nodes),
                    // Off-path nodes dim only while a path exists (cursor off-root).
                    dimmed: dimming && !path.contains(uid),
                    on_path: path.contains(uid),
                };
                (uid.clone(), attr)
            })
            .collect()
    }

    /// The [`LensTint`] for one node under the ACTIVE `lens`. The single place each
    /// lens's per-node metric is mapped to a semantic tint (the render layer turns
    /// the tint into a `Color`); `violation_nodes` is the precomputed project-wide
    /// incident set (empty unless the lens is `LayerViolation`). Returns
    /// `LensTint::None` for `Off` and for nodes a lens doesn't tint.
    fn lens_tint(
        &self,
        lens: LineageLens,
        uid: &str,
        node: Option<&NodeInfo>,
        violation_nodes: &std::collections::HashSet<String>,
    ) -> LensTint {
        match lens {
            LineageLens::Off => LensTint::None,
            // Coverage: exactly the old `untested` rule (a testable resource with
            // zero tests). Same `coverage_gap` base as the status cov% + stats.
            LineageLens::Coverage => {
                if node.is_some_and(coverage_gap) {
                    LensTint::Warn
                } else {
                    LensTint::None
                }
            }
            // Degree heat: transitive-downstream "blast radius". Buckets:
            //   n == 0     → None     (a leaf taints nothing downstream)
            //   1..=2      → HeatLow
            //   3..=6      → HeatMid
            //   >= 7       → HeatHigh
            LineageLens::DegreeHeat => match self.dag.downstream(uid).len() {
                0 => LensTint::None,
                1..=2 => LensTint::HeatLow,
                3..=6 => LensTint::HeatMid,
                _ => LensTint::HeatHigh,
            },
            // Layer: tint a MODEL by its dbt logical layer (the SAME `first_dir`
            // classification the list groups by). Sources/seeds/snapshots get None
            // so their distinctive materialization-class colour keeps showing.
            LineageLens::Layer => match node {
                Some(n) if n.resource_type == "model" => {
                    match crate::model_list::first_dir(n).unwrap_or("") {
                        "staging" => LensTint::LayerStaging,
                        "intermediate" => LensTint::LayerIntermediate,
                        "marts" => LensTint::LayerMarts,
                        "utilities" => LensTint::LayerUtilities,
                        _ => LensTint::LayerOther,
                    }
                }
                _ => LensTint::None,
            },
            // Layer violations: tint any node incident to a backward edge.
            LineageLens::LayerViolation => {
                if violation_nodes.contains(uid) {
                    LensTint::Violation
                } else {
                    LensTint::None
                }
            }
        }
    }


    /// Re-read the manifest from disk and rebuild, preserving the current
    /// selection by `unique_id` when possible. On a load error the app is left
    /// unchanged (the `?` returns before any mutation), so reload never corrupts
    /// state — the caller can keep running on the old data.
    pub fn reload(&mut self) -> Result<()> {
        let current = self.selected_node().map(|n| n.unique_id.clone());
        // Re-read from the same source; returns before mutating on error.
        let dag = match &self.source {
            DataSource::Manifest(path) => load_dag(path)?,
            DataSource::Project(dir) => load_dag_from_source(dir)?,
        };
        self.model_list = build_model_list(&dag, self.sort);
        self.stats = compute_stats(&dag);
        self.dag = dag;
        // Drop bookmarks whose model vanished; surviving ids persist (ids stable).
        self.bookmarks.retain(|id| self.dag.get(id).is_some());
        self.filter = None;
        self.mode = Mode::Selection;
        // Cursor home: selection is restored BY ID, so the loop's
        // selection-change chokepoint won't fire — reset here or a pre-reload
        // cursor would survive into the rebuilt graph.
        self.lineage_cursor = None;
        // Recompute the persistent-filter view over the rebuilt list (it also
        // sets the model count to the active list's length).
        self.apply_list_filter();
        if let Some(uid) = current {
            self.select_by_unique_id(&uid);
        }
        Ok(())
    }

    /// Rebuild the filtered list view for the current list-search query and
    /// re-resolve the selection **by unique_id** (prefer the current selection if
    /// it still matches, else the search origin, else the top). No-op unless a
    /// list search is active. This is the only place that touches `filter`, so
    /// the active list, its `model_count`, and the selection stay coherent — the
    /// guard against the two-coordinate-space desync.
    pub fn refilter(&mut self) {
        let (query, origin) = match &self.mode {
            Mode::Search(s) if s.target == SearchTarget::List => {
                (s.query.clone(), s.origin_uid.clone())
            }
            _ => return,
        };
        let current = self.selected_node().map(|n| n.unique_id.clone());
        let filtered = build_filtered_model_list(self.base_list(), &query);
        self.ui_state.set_model_count(filtered.len());
        self.filter = Some(filtered);
        // Prefer keeping the current node if it survived the filter, else the
        // origin, else fall back to the top of the (possibly empty) list.
        let mut resolved = false;
        if let Some(uid) = &current {
            resolved = self.select_by_unique_id(uid);
        }
        if !resolved {
            if let Some(uid) = &origin {
                resolved = self.select_by_unique_id(uid);
            }
        }
        if !resolved {
            self.ui_state.set_selected(0);
        }
    }

    /// Leave search mode, dropping the filter and restoring the full list, then
    /// select `restore_uid` (the search origin on cancel, or the chosen match on
    /// confirm) in the full list.
    fn close_search(&mut self, restore_uid: Option<String>) {
        self.filter = None;
        self.mode = Mode::Selection;
        // Back to the search's BASE (the persistent-filter view when active).
        self.ui_state.set_model_count(self.base_list().len());
        if let Some(uid) = restore_uid {
            self.select_by_unique_id(&uid);
        }
    }
}

/// Fit a `" > "`-joined breadcrumb `trail` into `max_width` DISPLAY CELLS by
/// dropping whole entries from the LEFT (oldest first) and prefixing `".."`
/// (ASCII — never the `…` ellipsis, which is ambiguous-width). The current root
/// (the last entry) is always kept.
///
/// STRICT: returns `None` when even `".. > root"` cannot fit — the render seam
/// uses that to drop the crumb to empty rather than let it overrun and evict the
/// trailing title suffixes (the bug being fixed). The size-unaware producer
/// [`App::breadcrumb`] handles `None` with its own lenient fallback.
///
/// Measures with [`UnicodeWidthStr`] so a CJK node name (which `chars().count()`
/// undercounts) is budgeted honestly. Pure + deterministic; shared by the
/// producer and the draw seam so the left-drop logic lives in ONE place.
pub(crate) fn fit_breadcrumb(trail: &str, max_width: usize) -> Option<String> {
    use unicode_width::UnicodeWidthStr;
    if trail.width() <= max_width {
        return Some(trail.to_string());
    }
    // `entries[last]` is the current root; never dropped.
    let entries: Vec<&str> = trail.split(" > ").collect();
    for start in 1..entries.len() {
        let candidate = format!(".. > {}", entries[start..].join(" > "));
        if candidate.width() <= max_width {
            return Some(candidate);
        }
    }
    None
}

/// The shared yank arm: record the intent toast and request the clipboard
/// effect, or no-op when there is nothing to copy (no selection / empty SQL).
/// `run_effect` overwrites the notice if the clipboard write fails, so the
/// optimistic text never survives a failed copy.
fn yank_with_notice(app: &mut App, text: Option<String>, notice: &str) -> Outcome {
    match text {
        Some(text) => {
            app.set_notice(notice);
            Outcome::effect(Effect::Yank(text))
        }
        None => Outcome::cont(),
    }
}

/// Apply an [`Action`] to the app, returning the [`Outcome`] (quit + effects).
///
/// Size-unaware: it never reads pane dimensions. UiState-only actions are
/// forwarded to [`reduce_selection`]; domain actions and mode transitions are
/// handled here. The match is exhaustive over [`Action`], so a new variant is a
/// compile error until it is given behaviour.
pub fn apply_action(app: &mut App, action: Action) -> Outcome {
    match action {
        Action::Quit => Outcome::quit(),
        // Movement keys are focus-routed: lineage-pane focus moves the lineage
        // CURSOR (Dag-aware, so it lives here, not in the UiState sub-reducer);
        // list focus forwards to the sub-reducer's list selection. Neither pans
        // the viewport — the loop follows the cursor with a minimal
        // ensure-visible.
        Action::MoveDown | Action::MoveUp | Action::MoveLeft | Action::MoveRight => {
            if app.ui_state.focus() == Focus::RightPane {
                let dir = match action {
                    Action::MoveDown => Direction::Down,
                    Action::MoveUp => Direction::Up,
                    Action::MoveLeft => Direction::Left,
                    _ => Direction::Right,
                };
                app.move_lineage_cursor(dir);
            } else {
                reduce_selection(&mut app.ui_state, action);
            }
            Outcome::cont()
        }
        // UiState-only arms: forward to the sub-reducer.
        Action::JumpTop
        | Action::JumpBottom
        | Action::PageDown
        | Action::PageUp
        | Action::ToggleFocus
        | Action::ToggleListPane => {
            reduce_selection(&mut app.ui_state, action);
            Outcome::cont()
        }
        // Column-extreme cursor jumps act on the lineage regardless of focus
        // (they have no list meaning, unlike the focus-routed h/l).
        Action::LineageLeftmost | Action::LineageRightmost => {
            app.move_lineage_cursor_extreme(matches!(action, Action::LineageRightmost));
            Outcome::cont()
        }
        // ---- overlays ----
        Action::HelpToggle => {
            app.mode = if matches!(app.mode, Mode::Help { .. }) {
                Mode::Selection
            } else {
                Mode::Help { scroll: 0 }
            };
            Outcome::cont()
        }
        Action::DetailOpen => {
            // Enter acts on the lineage CURSOR when the lineage pane is focused
            // (else the selection): an off-root cursor on a model RE-ROOTS to it
            // (committing the cursor, same as a mouse click); anything else —
            // the root itself, or a non-selectable source/seed/snapshot — opens
            // the structure modal, with detail + tests snapshotted into the mode
            // payload (cloned from the Dag side maps), so the render layer needs
            // no Dag.
            let root = app.selected_unique_id();
            let Some(uid) = app.focus_target() else {
                return Outcome::cont();
            };
            if Some(&uid) != root.as_ref()
                && app
                    .dag
                    .get(&uid)
                    .is_some_and(|n| n.resource_type == "model")
            {
                app.jump_to(&uid);
                return Outcome::cont();
            }
            let name = app.node_name_or_uid(&uid);
            let detail = app.dag.detail(&uid).cloned().unwrap_or_default();
            let tests = app.dag.tests(&uid).to_vec();
            // Blast radius of THIS node (the focus target — may be a non-root
            // source/seed/snapshot the lineage cursor sits on, NOT the root).
            let (downstream_count, upstream_count) = app.impact_counts_for(&uid);
            app.mode = Mode::Detail(DetailView {
                model_id: uid,
                name,
                detail,
                tests,
                downstream_count,
                upstream_count,
                scroll: 0,
            });
            Outcome::cont()
        }
        Action::DetailClose => {
            app.mode = Mode::Selection;
            Outcome::cont()
        }
        // Overlay scroll: acts inside whichever scrollable overlay is open. The
        // size-aware upper clamp is the loop's.
        Action::DetailScroll(dir) | Action::DetailScrollPage(dir) => {
            // Line vs page step; the loop's per-frame clamp bounds the result.
            let amount = if matches!(action, Action::DetailScroll(_)) {
                1
            } else {
                10
            };
            if let Some(s) = modal_scroll_mut(&mut app.mode) {
                *s = if matches!(dir, crate::action::Direction::Down) {
                    s.saturating_add(amount)
                } else {
                    s.saturating_sub(amount)
                };
            }
            Outcome::cont()
        }
        Action::DetailScrollHome | Action::DetailScrollEnd => {
            // End records MAX as "as far as it goes"; the loop's per-frame
            // clamp (the same one every scroll passes through) bounds it to
            // the content height, so the reducer stays size-unaware.
            if let Some(s) = modal_scroll_mut(&mut app.mode) {
                *s = if matches!(action, Action::DetailScrollHome) {
                    0
                } else {
                    usize::MAX
                };
            }
            Outcome::cont()
        }
        // ---- command palette ----
        // Open the fuzzy command finder. Its editing keys reuse the Search* arms,
        // routed by `Mode::Palette` below.
        Action::PaletteOpen => {
            app.mode = Mode::Palette(PaletteState::default());
            Outcome::cont()
        }
        // ---- search ----
        Action::SearchOpen => {
            let target = if app.ui_state.focus() == Focus::List {
                SearchTarget::List
            } else {
                SearchTarget::Lineage
            };
            let origin = app.selected_node().map(|n| n.unique_id.clone());
            app.mode = Mode::Search(SearchState {
                target,
                query: String::new(),
                origin_uid: origin,
                match_idx: 0,
            });
            app.refilter(); // no-op for a lineage-target search
            Outcome::cont()
        }
        Action::SearchType(c) => {
            // Route by mode: the palette and the search share the action, never
            // the state. In the palette, typing only ever NARROWS the candidate
            // list, so reset `selected` to 0 (which also satisfies the
            // "selection clamps when the filter shrinks" contract).
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.push(c);
                    s.match_idx = 0; // query changed → matches changed
                    app.refilter();
                }
                Mode::Palette(p) => {
                    p.query.push(c);
                    p.selected = 0;
                }
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchBackspace => {
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.pop();
                    s.match_idx = 0;
                    app.refilter();
                }
                Mode::Palette(p) => {
                    p.query.pop();
                    // Reset to the top EXACTLY like the SearchType arm, so the
                    // highlighted row is a pure function of the query — the same
                    // query always lands the same row regardless of arrival path
                    // (typing vs. backspacing). (Previously kept-and-clamped, which
                    // left ~1/3 of backspace transitions on an unrelated command.)
                    p.selected = 0;
                }
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchMove(dir) => {
            // Command palette: move `selected` over the current candidates,
            // clamped at both ends (no wrap), mirroring a list cursor.
            if let Mode::Palette(p) = &mut app.mode {
                let count = palette_candidates(&p.query).len();
                if count > 0 {
                    p.selected = match dir {
                        Direction::Down => (p.selected + 1).min(count - 1),
                        Direction::Up => p.selected.saturating_sub(1),
                        _ => p.selected,
                    };
                }
                return Outcome::cont();
            }
            // List search: move within the filtered list (loop's ensure_visible
            // follows; move_* require List focus, which a list search has).
            // Lineage search: cycle the match cursor (the loop anchors to it).
            let target = match &app.mode {
                Mode::Search(s) => Some((s.target, s.query.clone())),
                _ => None,
            };
            match target {
                Some((SearchTarget::List, _)) => match dir {
                    Direction::Down => reduce_selection(&mut app.ui_state, Action::MoveDown),
                    Direction::Up => reduce_selection(&mut app.ui_state, Action::MoveUp),
                    _ => {}
                },
                Some((SearchTarget::Lineage, query)) => {
                    let count = app.lineage_matches(&query).len();
                    if count > 0 {
                        if let Mode::Search(s) = &mut app.mode {
                            s.match_idx = match dir {
                                Direction::Down => (s.match_idx + 1) % count,
                                Direction::Up => (s.match_idx + count - 1) % count,
                                _ => s.match_idx,
                            };
                        }
                    }
                }
                None => {}
            }
            Outcome::cont()
        }
        Action::SearchCancel => {
            // Palette cancel just closes the overlay — no filter/list to restore.
            if matches!(app.mode, Mode::Palette(_)) {
                app.mode = Mode::Selection;
                return Outcome::cont();
            }
            let origin = match &app.mode {
                Mode::Search(s) => s.origin_uid.clone(),
                _ => None,
            };
            app.close_search(origin);
            Outcome::cont()
        }
        Action::SearchConfirm => {
            // Palette confirm: resolve the selected candidate to its Action,
            // close the palette FIRST, then recursively apply the chosen action
            // and propagate its WHOLE Outcome (so choosing "quit" quits, and
            // choosing "open SQL in $EDITOR" yields the effect). `Action` is
            // `Copy` and the candidate refs are `'static`, so the borrow ends
            // before the mutation.
            if let Mode::Palette(p) = &app.mode {
                let chosen = palette_candidates(&p.query)
                    .get(p.selected)
                    .map(|b| b.action);
                app.mode = Mode::Selection;
                return match chosen {
                    Some(a) => apply_action(app, a),
                    None => Outcome::cont(),
                };
            }
            let target = match &app.mode {
                Mode::Search(s) => Some((s.target, s.query.clone())),
                _ => None,
            };
            match target {
                Some((SearchTarget::Lineage, _)) => {
                    // Jump to the currently-cycled match: if it is a selectable
                    // model, re-root to it (recording history); else just leave.
                    let hit = app.current_lineage_match();
                    app.mode = Mode::Selection;
                    if let Some(uid) = hit {
                        app.jump_to(&uid);
                    }
                }
                _ => {
                    // List search: keep the highlighted match (resolve by id in
                    // the full list).
                    let chosen = app.selected_node().map(|n| n.unique_id.clone());
                    app.close_search(chosen);
                }
            }
            Outcome::cont()
        }
        // ---- effects (performed by the loop, never here) ----
        Action::OpenEditor => match app.selected_file_path() {
            Some(path) => Outcome::effect(Effect::OpenEditor(path)),
            None => Outcome::cont(),
        },
        Action::YankId => {
            let text = app.selected_unique_id();
            yank_with_notice(app, text, "Copied unique_id")
        }
        Action::YankName => {
            let text = app.selected_name();
            yank_with_notice(app, text, "Copied model name")
        }
        Action::YankMermaid => {
            let text = app.lineage_mermaid();
            yank_with_notice(app, text, "Copied Mermaid lineage")
        }
        Action::YankDot => {
            let text = app.lineage_dot();
            yank_with_notice(app, text, "Copied DOT lineage")
        }
        Action::YankAscii => {
            let text = app.lineage_ascii();
            yank_with_notice(app, text, "Copied ASCII lineage")
        }
        Action::YankSql => {
            let text = app.selected_raw_sql();
            yank_with_notice(app, text, "Copied raw SQL")
        }
        Action::YankImpact => {
            let text = app.impact_report();
            yank_with_notice(app, text, "Copied impact report")
        }
        Action::ExportLineage => match (app.selected_name(), app.lineage_ascii()) {
            (Some(name), Some(contents)) => {
                let path = format!("{name}_lineage.txt");
                // Optimistic intent; `run_effect` overwrites it if the write fails.
                app.set_notice(format!("Exported {path}"));
                Outcome::effect(Effect::WriteFile { path, contents })
            }
            _ => Outcome::cont(),
        },
        Action::Reload => Outcome::effect(Effect::ReloadManifest),
        // Recenter and the lineage-view actions re-anchor the lineage; that is
        // applied size-aware in the loop, so the reducer just records intent.
        // Recenter additionally sends the cursor home, so `z` always means
        // "back to the rooted node".
        Action::Recenter => {
            app.reset_lineage_cursor();
            Outcome::cont()
        }
        Action::ToggleUpstream => {
            app.lineage_view.upstream = !app.lineage_view.upstream;
            Outcome::cont()
        }
        Action::ToggleDownstream => {
            app.lineage_view.downstream = !app.lineage_view.downstream;
            Outcome::cont()
        }
        Action::DepthDecrease => {
            app.lineage_view.depth = match app.lineage_view.depth {
                None => Some(3),
                Some(n) => Some(n.saturating_sub(1).max(1)),
            };
            Outcome::cont()
        }
        Action::DepthIncrease => {
            app.lineage_view.depth = match app.lineage_view.depth {
                None => None,
                Some(n) if n >= 8 => None, // widen past 8 hops → unlimited
                Some(n) => Some(n + 1),
            };
            Outcome::cont()
        }
        Action::ResetView => {
            app.lineage_view = LineageView::default();
            Outcome::cont()
        }
        Action::HistoryBack => {
            app.history_back();
            Outcome::cont()
        }
        Action::HistoryForward => {
            app.history_forward();
            Outcome::cont()
        }
        // ---- SQL preview / stats dashboard modals ----
        Action::SqlOpen => {
            // Focus-aware target (`s` previews whatever the lineage cursor is on
            // when the lineage pane is focused, else the list selection); unlike
            // `Enter`, `s` only PREVIEWS — it never re-roots.
            let Some(uid) = app.focus_target() else {
                return Outcome::cont();
            };
            let name = app.node_name_or_uid(&uid);
            // Sources/seeds (and manifests omitting raw_code) get a placeholder —
            // there is no transient-status channel in this app.
            let sql = match app.dag.raw_code(&uid) {
                Some(code) => code.to_string(),
                None => {
                    let rt = app
                        .dag
                        .get(&uid)
                        .map(|n| n.resource_type.as_str())
                        .unwrap_or("node");
                    format!("(no SQL for this {rt})")
                }
            };
            let path = app
                .dag
                .detail(&uid)
                .and_then(|d| d.original_file_path.clone());
            app.mode = Mode::Sql(SqlView {
                model_id: uid,
                name,
                sql,
                path,
                scroll: 0,
            });
            Outcome::cont()
        }
        Action::StatsOpen => {
            app.mode = Mode::Stats(app.compute_stats_view());
            Outcome::cont()
        }
        // Lineage lens cycle: a pure view-pref mutation. Routed DIRECTLY here (not
        // through reduce_selection, which is only the legacy list-movement arms)
        // per the two-level-reducer contract.
        Action::CycleLens => {
            app.ui_state.cycle_lens();
            Outcome::cont()
        }
        // ---- bookmarks + list sort (Step C) — App-level data, so handled here
        // (never reduce_selection, which is UiState-only and can't reach them). ----
        // Toggle a bookmark on the SELECTED model, regardless of focus: the list
        // holds only models, so the selection always has a row to draw the badge
        // on (a lineage-cursor source/seed/snapshot would have no list-row home).
        Action::BookmarkToggle => {
            if let Some(uid) = app.selected_unique_id() {
                let name = app.node_name_or_uid(&uid);
                if app.bookmarks.insert(uid.clone()) {
                    app.set_notice(format!("Bookmarked: {name}"));
                } else {
                    app.bookmarks.remove(&uid);
                    app.set_notice(format!("Removed bookmark: {name}"));
                }
                // Under the Bookmarked filter the toggle changes the view's
                // membership — rebuild it so an un-bookmarked row leaves it.
                if app.list_filter == ListFilter::Bookmarked {
                    app.apply_list_filter();
                }
            }
            Outcome::cont()
        }
        Action::ToggleUntestedFilter | Action::ToggleBookmarkFilter => {
            let target = if matches!(action, Action::ToggleUntestedFilter) {
                ListFilter::Untested
            } else {
                ListFilter::Bookmarked
            };
            // Toggling the active filter turns it off; a different one replaces it.
            app.list_filter = if app.list_filter == target {
                ListFilter::All
            } else {
                target
            };
            app.apply_list_filter();
            Outcome::cont()
        }
        // Jump to the next bookmarked model in the ACTIVE list's visible order
        // (filtered order during search), wrapping from selected+1. A selection
        // jump like history, not a focus-routed move; no-op when no bookmark is
        // visible. The target index is computed under the immutable `active_list`
        // borrow, then applied — so the borrow is released before the mutation.
        Action::BookmarkCycle | Action::BookmarkCycleBack => {
            let forward = matches!(action, Action::BookmarkCycle);
            let target = cycle_to(app.active_list(), app.ui_state.selected(), forward, |m| {
                app.bookmarks.contains(&m.unique_id)
            });
            if let Some(i) = target {
                app.ui_state.set_selected(i);
            }
            Outcome::cont()
        }
        Action::GapNext | Action::GapPrev => {
            let forward = matches!(action, Action::GapNext);
            let target = cycle_to(
                app.active_list(),
                app.ui_state.selected(),
                forward,
                crate::coverage_gap,
            );
            if let Some(i) = target {
                app.ui_state.set_selected(i);
            }
            Outcome::cont()
        }
        // Cycle the within-group sort, rebuild the list in the new order, and
        // re-resolve the selection BY unique_id (never a raw index across the
        // rebuild). `refilter` re-derives the filtered view if a search is active.
        Action::SortCycle => {
            app.sort = app.sort.next();
            let current = app.selected_unique_id();
            app.model_list = build_model_list(&app.dag, app.sort);
            // Re-derive the persistent-filter view from the re-sorted list (it
            // refilters the open search itself, so the search view inherits
            // the new order too).
            app.apply_list_filter();
            if let Some(uid) = &current {
                app.select_by_unique_id(uid);
            }
            Outcome::cont()
        }
        // Toggle the lineage minimap (a pure UiState view-pref). Routed DIRECTLY
        // here, mirroring `CycleLens` — NOT through `reduce_selection`, which is
        // the UiState-only legacy list-movement arms (spec D5).
        Action::ToggleMinimap => {
            app.ui_state.toggle_minimap();
            Outcome::cont()
        }
    }
}

/// A node's display kind in an export: its materialization if recorded, else its
/// resource type. Shared by the Mermaid and DOT exporters so the policy is single.
fn node_kind(n: &NodeInfo) -> &str {
    n.materialized.as_deref().unwrap_or(&n.resource_type)
}

/// The scroll slot of whichever overlay modal is open (`None` in non-modal
/// modes). The ONE place the four scrollable modals are enumerated, shared by
/// the line / page / home / end scroll arms so a new modal can't be wired into
/// some arms and missed in others.
fn modal_scroll_mut(mode: &mut Mode) -> Option<&mut usize> {
    match mode {
        Mode::Help { scroll } => Some(scroll),
        Mode::Detail(dv) => Some(&mut dv.scroll),
        Mode::Sql(sv) => Some(&mut sv.scroll),
        Mode::Stats(sv) => Some(&mut sv.scroll),
        _ => None,
    }
}

/// The next selection index (scanning from `start`, exclusive, wrapping) whose
/// model satisfies `pred`, or `None` when no model does. Shared by the
/// bookmark and coverage-gap cycles; `start` itself is checked LAST, so with a
/// single matching model the cycle still lands on it.
fn cycle_to(
    list: &crate::model_list::ModelList,
    start: usize,
    forward: bool,
    pred: impl Fn(&crate::NodeInfo) -> bool,
) -> Option<usize> {
    let n = list.len();
    if n == 0 {
        return None;
    }
    (1..=n).find_map(|off| {
        let i = if forward {
            (start + off) % n
        } else {
            (start + n - off % n) % n
        };
        list.model_at(i).filter(|m| pred(m)).map(|_| i)
    })
}

/// A Mermaid-safe node id: non-alphanumeric chars (the `.` in a `unique_id`)
/// become `_`, so `model.p.x` → `model_p_x`.
fn mermaid_id(uid: &str) -> String {
    uid.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Escape a label for a Mermaid `["..."]` node (double quotes would close it).
fn mermaid_label(name: &str) -> String {
    name.replace('"', "'")
}

/// Escape a string for a Graphviz DOT quoted id/label (`\` and `"`).
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// All kept 1-hop `(parent, child)` edges where BOTH endpoints are `model` nodes
/// and the parent sits in a LATER logical layer than the child — a backward
/// dependency (e.g. a `marts` model feeding a `staging` model), which is almost
/// always a modelling smell. "Later" is by [`layer_rank`](crate::model_list::layer_rank)
/// over each model's `first_dir` layer (staging < intermediate < marts <
/// utilities; an unknown layer ranks last). Returns the edges sorted (the `Dag`
/// edge order is already sorted) for determinism.
///
/// Single source of truth for the `LayerViolation` lineage lens AND the stats
/// dashboard's violation readout (implemented after this); keep the name exactly
/// `layer_violation_edges` so that reuse is verbatim.
pub fn layer_violation_edges(dag: &Dag) -> Vec<(String, String)> {
    use crate::model_list::{first_dir, layer_rank};
    dag.edges()
        .into_iter()
        .filter(|(parent, child)| {
            let (Some(p), Some(c)) = (dag.get(parent), dag.get(child)) else {
                return false;
            };
            if p.resource_type != "model" || c.resource_type != "model" {
                return false;
            }
            let pr = layer_rank(first_dir(p).unwrap_or(""));
            let cr = layer_rank(first_dir(c).unwrap_or(""));
            pr > cr
        })
        .collect()
}

/// The longest dependency chain in the DAG as node NAMES, upstream →
/// downstream. Memoized DFS over `Dag::edges()` (sorted, so children are
/// visited in `unique_id` order); ties at every level keep the first (smallest
/// uid) candidate, making the result fully deterministic. dbt manifests are
/// acyclic by construction, which the recursion relies on (the same assumption
/// `layout()`'s longest-path columns already make).
fn longest_chain(dag: &Dag) -> Vec<String> {
    use std::collections::HashMap;
    let edges = dag.edges();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (p, c) in &edges {
        children.entry(p.as_str()).or_default().push(c.as_str());
    }

    // (chain length from uid, the next hop on that chain).
    fn chain<'a>(
        uid: &'a str,
        children: &HashMap<&'a str, Vec<&'a str>>,
        memo: &mut HashMap<&'a str, (usize, Option<&'a str>)>,
    ) -> (usize, Option<&'a str>) {
        if let Some(&hit) = memo.get(uid) {
            return hit;
        }
        let mut best = (1, None);
        for &kid in children.get(uid).into_iter().flatten() {
            let (len, _) = chain(kid, children, memo);
            // Strictly greater: kids come sorted, so ties keep the first.
            if len + 1 > best.0 {
                best = (len + 1, Some(kid));
            }
        }
        memo.insert(uid, best);
        best
    }

    let mut memo = HashMap::new();
    let mut starts: Vec<&str> = dag.nodes().keys().map(String::as_str).collect();
    starts.sort_unstable();
    let Some(&start) = starts
        .iter()
        .max_by_key(|uid| chain(uid, &children, &mut memo).0)
    else {
        return Vec::new();
    };
    // max_by_key keeps the LAST maximum; re-scan for the FIRST (smallest uid).
    let best_len = memo[start].0;
    let start = *starts
        .iter()
        .find(|uid| memo[**uid].0 == best_len)
        .expect("a maximum exists");

    let mut path = Vec::new();
    let mut cur = Some(start);
    while let Some(uid) = cur {
        path.push(dag.get(uid).map_or_else(|| uid.to_string(), |n| n.name.clone()));
        cur = memo[uid].1;
    }
    path
}

/// Compute the title-bar stats from a `Dag`. The project name is the second
/// segment of any node's `unique_id` (`model.<project>.<name>`), which is the
/// same across a single dbt project.
fn compute_stats(dag: &Dag) -> AppStats {
    // Deterministic across runs: take the lexicographically-smallest project
    // segment (stable for a single-project manifest; well-defined for multi).
    let project = dag
        .nodes()
        .keys()
        .filter_map(|uid| uid.split('.').nth(1))
        .min()
        .unwrap_or("")
        .to_string();
    AppStats {
        project,
        models: dag.count_by_resource_type("model"),
        sources: dag.count_by_resource_type("source"),
        seeds: dag.count_by_resource_type("seed"),
        snapshots: dag.count_by_resource_type("snapshot"),
    }
}

/// Derive the dbt project root from the manifest path: `<root>/target/manifest.json`
/// → `<root>`. Falls back to the current directory if there aren't two parents.
pub fn derive_project_root(manifest: &Path) -> PathBuf {
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Direction;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

    fn app() -> App {
        let dag = load_dag(FIXTURE).expect("fixture loads");
        App::new(dag, PathBuf::from(FIXTURE))
    }

    #[test]
    fn yank_and_bookmark_record_oneshot_notices() {
        let mut a = app();
        assert_eq!(a.take_notice(), None, "fresh app has no pending notice");
        let out = apply_action(&mut a, Action::YankId);
        assert!(
            matches!(out.effects.as_slice(), [Effect::Yank(_)]),
            "yank still requests the clipboard effect"
        );
        assert_eq!(a.take_notice().as_deref(), Some("Copied unique_id"));
        assert_eq!(a.take_notice(), None, "take_notice drains (one-shot)");

        apply_action(&mut a, Action::BookmarkToggle);
        let note = a.take_notice().expect("bookmark toggle notices");
        assert!(note.starts_with("Bookmarked: "), "got {note:?}");
        apply_action(&mut a, Action::BookmarkToggle);
        let note = a.take_notice().expect("un-bookmark notices");
        assert!(note.starts_with("Removed bookmark: "), "got {note:?}");
    }

    #[test]
    fn export_notice_names_the_written_path() {
        let mut a = app();
        let out = apply_action(&mut a, Action::ExportLineage);
        let Some(Effect::WriteFile { path, .. }) = out.effects.first() else {
            panic!("export must request the write effect");
        };
        assert_eq!(a.take_notice(), Some(format!("Exported {path}")));
    }

    #[test]
    fn apply_action_forwards_uistate_arms_like_handle_key() {
        // For the legacy keys UNDER LIST FOCUS, apply_action must mutate
        // ui_state identically to driving reduce_selection directly (the
        // wrapping relationship is exact). Under lineage focus the movement
        // keys intentionally diverge: they drive the App-level lineage cursor,
        // which the bare UiState cannot represent (see the cursor tests below).
        let mut a = app();
        let mut bare = UiState::new(a.model_list.len());
        for action in [
            Action::MoveDown,
            Action::MoveDown,
            Action::JumpBottom,
            Action::MoveUp,
        ] {
            let out = apply_action(&mut a, action);
            assert!(!out.quit && out.effects.is_empty());
            reduce_selection(&mut bare, action);
            assert_eq!(
                a.ui_state.selected(),
                bare.selected(),
                "{action:?} divergent"
            );
        }
    }

    #[test]
    fn quit_action_sets_quit() {
        let mut a = app();
        assert!(apply_action(&mut a, Action::Quit).quit);
    }

    #[test]
    fn unimplemented_domain_actions_are_safe_noops() {
        let mut a = app();
        let before = a.ui_state.selected();
        for action in [
            Action::HelpToggle,
            Action::DetailOpen,
            Action::SearchOpen,
            Action::DetailScroll(Direction::Down),
        ] {
            let out = apply_action(&mut a, action);
            assert!(!out.quit && out.effects.is_empty());
        }
        assert_eq!(
            a.ui_state.selected(),
            before,
            "no-op domain actions don't move selection"
        );
    }

    // ---- blast radius (impact_counts) ----

    #[test]
    fn impact_counts_match_frozen_fixture_closures() {
        // Anchored to the SAME closures the fixture closure tests freeze:
        // fct_subscription_process → 2 downstream / 27 upstream (manifest_fixture
        // `closure_deep_multihop_fct_subscription_process`).
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        assert_eq!(a.impact_counts(), Some((2, 27)), "fct blast radius");
        // stg_payment__shoppers → 1 downstream / 2 upstream (closure_both…).
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        assert_eq!(a.impact_counts(), Some((1, 2)), "stg shoppers blast radius");
    }

    #[test]
    fn impact_counts_for_targets_the_given_node_not_the_root() {
        // The detail modal targets the FOCUS uid (a non-root source/seed/snapshot
        // under the lineage cursor), so the helper must count THAT node.
        let a = app();
        // A source with two downstream models (closure_downstream_only_shoppers…).
        let src = "source.jaffle_finance.dev_lake_jaffle_payment.shoppers";
        assert_eq!(a.impact_counts_for(src), (2, 0), "source down/up");
    }

    #[test]
    fn impact_status_uses_chrome_badges_per_glyph_mode() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        a.glyph_mode = crate::GlyphMode::Unicode;
        assert_eq!(a.impact_status().as_deref(), Some("impact ↓1 ↑2"));
        a.glyph_mode = crate::GlyphMode::Ascii;
        assert_eq!(
            a.impact_status().as_deref(),
            Some("impact v1 ^2"),
            "ascii mode uses v/^ badges, never the unicode arrows"
        );
    }

    // ---- lineage breadcrumb ----

    #[test]
    fn breadcrumb_is_none_with_empty_history() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        assert!(a.back.is_empty());
        assert_eq!(a.breadcrumb(200), None, "no history → no breadcrumb");
    }

    #[test]
    fn breadcrumb_shows_last_three_history_names_then_root() {
        // Drive the public API: each jump_to pushes the prior root onto `back`.
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        // fct → wt_delivery_base_metrics → fct_delivery_monthly_snapshot
        a.jump_to("model.jaffle_finance.wt_delivery_base_metrics");
        a.jump_to("model.jaffle_finance.fct_delivery_monthly_snapshot");
        assert_eq!(
            a.breadcrumb(200).as_deref(),
            Some("fct_subscription_process > wt_delivery_base_metrics > fct_delivery_monthly_snapshot"),
            "breadcrumb = back history names then the current root"
        );
    }

    #[test]
    fn breadcrumb_caps_at_three_history_entries_plus_root() {
        // A four-deep back stack: only the LAST 3 history entries show + the root,
        // so the oldest (a > ) drops even before truncation.
        let mut a = app();
        a.back = vec![
            "model.jaffle_finance.pos_txn".into(),
            "model.jaffle_finance.pos_pay".into(),
            "model.jaffle_finance.stg_payment__shoppers".into(),
            "model.jaffle_finance.int_shoppers__enriched".into(),
        ];
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        // 3 newest history (pos_pay, stg…, int…) + root; pos_txn is dropped.
        assert_eq!(
            a.breadcrumb(200).as_deref(),
            Some("pos_pay > stg_payment__shoppers > int_shoppers__enriched > fct_subscription_process"),
        );
    }

    #[test]
    fn fit_breadcrumb_is_strict_and_drops_to_none() {
        // The pure helper shared by the producer and the draw seam: it fits by
        // dropping whole LEFT entries then a ".." prefix, and returns None (strict)
        // when even ".. > root" cannot fit — the draw seam needs that to drop the
        // crumb to empty rather than overrun the title suffixes.
        let full = "alpha > beta > gamma";
        assert_eq!(fit_breadcrumb(full, 100).as_deref(), Some(full), "fits whole");
        // Width holds ".. > beta > gamma" (17) but not the full 20.
        assert_eq!(
            fit_breadcrumb(full, 17).as_deref(),
            Some(".. > beta > gamma"),
            "drop oldest, prefix .."
        );
        // Width holds only ".. > gamma" (10).
        assert_eq!(fit_breadcrumb(full, 10).as_deref(), Some(".. > gamma"));
        // Too narrow even for ".. > gamma": strict None (never an ellipsis char).
        assert_eq!(fit_breadcrumb(full, 5), None, "strict: None when nothing fits");
    }

    #[test]
    fn breadcrumb_truncates_whole_entries_from_the_left_with_dotdot() {
        let mut a = app();
        a.back = vec!["model.jaffle_finance.stg_payment__shoppers".into()];
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        let full = "stg_payment__shoppers > fct_subscription_process";
        // A width that cannot hold the full trail but can hold ".. > root".
        let max = ".. > fct_subscription_process".chars().count();
        assert!(max < full.chars().count());
        assert_eq!(
            a.breadcrumb(max).as_deref(),
            Some(".. > fct_subscription_process"),
            "overflow drops whole entries from the left, ASCII '..' prefix"
        );
        // The ".." prefix is ASCII, never the ellipsis char.
        assert!(!a.breadcrumb(max).unwrap().contains('…'));
    }

    #[test]
    fn breadcrumb_skips_uids_that_no_longer_resolve() {
        // A vanished history uid is unconstructable via jump_to (it refuses an
        // unresolvable id), so set `back` directly — the breadcrumb must skip it.
        let mut a = app();
        a.back = vec![
            "model.jaffle_finance.ghost_does_not_exist".into(),
            "model.jaffle_finance.stg_payment__shoppers".into(),
        ];
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        assert_eq!(
            a.breadcrumb(200).as_deref(),
            Some("stg_payment__shoppers > fct_subscription_process"),
            "the unresolvable ghost uid is dropped, not rendered"
        );
    }

    #[test]
    fn select_by_unique_id_moves_selection_and_reports_missing() {
        let mut a = app();
        let target = "model.jaffle_finance.fct_subscription_process";
        assert!(a.select_by_unique_id(target));
        assert_eq!(
            a.active_list()
                .model_at(a.ui_state.selected())
                .unwrap()
                .unique_id,
            target
        );
        assert!(!a.select_by_unique_id("model.does.not.exist"));
    }

    #[test]
    fn reload_preserves_selection_by_unique_id() {
        let mut a = app();
        let target = "model.jaffle_finance.pos_pay";
        a.select_by_unique_id(target);
        a.reload().expect("reload ok");
        assert_eq!(
            a.active_list()
                .model_at(a.ui_state.selected())
                .unwrap()
                .unique_id,
            target,
            "selection survives reload"
        );
        assert_eq!(a.model_list.len(), 45, "reload rebuilt the full list");
    }

    #[test]
    fn yank_mermaid_emits_a_graph_lr_diagram() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let effects = apply_action(&mut a, Action::YankMermaid).effects;
        let text = match &effects[..] {
            [Effect::Yank(t)] => t.clone(),
            other => panic!("expected one Yank effect, got {other:?}"),
        };
        // Fenced so a Markdown paste renders a diagram, not plain text.
        assert!(
            text.starts_with("```mermaid\ngraph LR\n"),
            "fenced Mermaid header: {text}"
        );
        assert!(text.trim_end().ends_with("```"), "closing fence: {text}");
        // The selected node, an upstream, and a downstream all appear as nodes,
        // with sanitized ids and a materialization-tagged label.
        assert!(text.contains("model_jaffle_finance_stg_payment__shoppers"));
        assert!(
            text.contains("\"stg_payment__shoppers (view) *\""),
            "selected node tagged + marked"
        );
        assert!(
            text.contains("\"int_shoppers__enriched (view)\""),
            "downstream node present"
        );
        assert!(
            text.contains("\"shoppers (source)\""),
            "upstream source present"
        );
        // An edge line uses sanitized ids.
        assert!(
            text.contains("model_jaffle_finance_stg_payment__shoppers --> model_jaffle_finance_int_shoppers__enriched"),
            "downstream edge present:\n{text}"
        );
    }

    #[test]
    fn yank_dot_emits_a_digraph() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let text = match &apply_action(&mut a, Action::YankDot).effects[..] {
            [Effect::Yank(t)] => t.clone(),
            other => panic!("expected one Yank effect, got {other:?}"),
        };
        assert!(text.starts_with("digraph lineage {"), "DOT header: {text}");
        assert!(text.contains("rankdir=LR;"));
        assert!(text.contains(
            "\"model.jaffle_finance.stg_payment__shoppers\" [label=\"stg_payment__shoppers\\n(view)\"];"
        ));
        assert!(text.contains(" -> "), "has at least one edge");
        assert!(text.trim_end().ends_with('}'));
    }

    #[test]
    fn yank_ascii_emits_the_lineage_text_diagram() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let text = match &apply_action(&mut a, Action::YankAscii).effects[..] {
            [Effect::Yank(t)] => t.clone(),
            other => panic!("expected one Yank effect, got {other:?}"),
        };
        // The yank IS the pane's diagram: same layout, same glyphs.
        let expected = crate::layout_mode(&a.lineage_subgraph(), a.glyph_mode)
            .grid
            .to_text();
        assert_eq!(text, expected, "yank matches the rendered grid");
        assert!(
            text.contains("stg_payment__shoppers"),
            "selected name in the diagram"
        );
        // ASCII mode swaps the glyph repertoire in the yank too.
        a.glyph_mode = crate::GlyphMode::Ascii;
        let ascii = match &apply_action(&mut a, Action::YankAscii).effects[..] {
            [Effect::Yank(t)] => t.clone(),
            other => panic!("expected one Yank effect, got {other:?}"),
        };
        assert!(ascii.is_ascii(), "ASCII-mode yank is pure ASCII");
    }

    #[test]
    fn yank_sql_emits_the_raw_code_and_noops_without_sql() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let text = match &apply_action(&mut a, Action::YankSql).effects[..] {
            [Effect::Yank(t)] => t.clone(),
            other => panic!("expected one Yank effect, got {other:?}"),
        };
        assert_eq!(
            text.as_str(),
            a.dag
                .raw_code("model.jaffle_finance.stg_payment__shoppers")
                .unwrap(),
            "yank is the side-map raw SQL verbatim"
        );
        // Manifests record seeds with `raw_code: ""`; the selection-side filter
        // treats blank SQL as absent so a yank never clobbers the clipboard
        // with nothing. (Seeds are not list-selectable, so assert the side-map
        // shape the filter exists for.)
        assert_eq!(
            a.dag.raw_code("seed.jaffle_finance.fiscal_years"),
            Some(""),
            "seed raw_code is the empty string the filter guards against"
        );
    }

    #[test]
    fn yank_impact_emits_a_markdown_blast_radius_report() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let text = match &apply_action(&mut a, Action::YankImpact).effects[..] {
            [Effect::Yank(t)] => t.clone(),
            other => panic!("expected one Yank effect, got {other:?}"),
        };
        assert!(
            text.starts_with("# Impact: stg_payment__shoppers\n"),
            "title: {text}"
        );
        // Counts agree with the frozen impact_counts fixture values (1 down, 2 up).
        assert!(text.contains("- transitive: 2 upstream / 1 downstream\n"));
        assert!(text.contains("## Downstream (blast radius) (1)\n"));
        assert!(text.contains("## Upstream (2)\n"));
        assert!(
            text.contains("- int_shoppers__enriched\n"),
            "downstream member listed: {text}"
        );
        // Deterministic: a second yank is byte-identical.
        let again = match &apply_action(&mut a, Action::YankImpact).effects[..] {
            [Effect::Yank(t)] => t.clone(),
            other => panic!("expected one Yank effect, got {other:?}"),
        };
        assert_eq!(text, again, "two yanks are byte-identical");
    }

    #[test]
    fn export_lineage_emits_a_write_file_effect_with_the_diagram() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let effects = apply_action(&mut a, Action::ExportLineage).effects;
        let (path, contents) = match &effects[..] {
            [Effect::WriteFile { path, contents }] => (path.clone(), contents.clone()),
            other => panic!("expected one WriteFile effect, got {other:?}"),
        };
        assert_eq!(path, "stg_payment__shoppers_lineage.txt");
        assert_eq!(
            contents,
            a.lineage_ascii().unwrap(),
            "exported contents are the lineage text diagram"
        );
    }

    #[test]
    fn page_down_and_up_move_the_list_selection_by_ten_clamped() {
        let mut a = app();
        assert_eq!(a.ui_state.selected(), 0);
        apply_action(&mut a, Action::PageDown);
        assert_eq!(a.ui_state.selected(), 10, "page down = +10");
        apply_action(&mut a, Action::PageUp);
        assert_eq!(a.ui_state.selected(), 0, "page up = -10");
        apply_action(&mut a, Action::PageUp);
        assert_eq!(a.ui_state.selected(), 0, "clamped at the top");
        for _ in 0..20 {
            apply_action(&mut a, Action::PageDown);
        }
        assert_eq!(
            a.ui_state.selected(),
            a.model_list.len() - 1,
            "clamped at the bottom"
        );
    }

    #[test]
    fn modal_page_scroll_and_home_end_record_intent() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        apply_action(&mut a, Action::SqlOpen);
        assert!(matches!(a.mode, Mode::Sql(_)), "SQL modal open");
        apply_action(&mut a, Action::DetailScrollPage(Direction::Down));
        let scroll = |m: &Mode| match m {
            Mode::Sql(sv) => sv.scroll,
            other => panic!("expected Sql mode, got {other:?}"),
        };
        assert_eq!(scroll(&a.mode), 10, "page = 10 lines");
        apply_action(&mut a, Action::DetailScroll(Direction::Down));
        assert_eq!(scroll(&a.mode), 11, "line scroll still 1");
        apply_action(&mut a, Action::DetailScrollPage(Direction::Up));
        assert_eq!(scroll(&a.mode), 1, "page up = -10 saturating");
        apply_action(&mut a, Action::DetailScrollEnd);
        assert_eq!(scroll(&a.mode), usize::MAX, "End records MAX for the loop clamp");
        apply_action(&mut a, Action::DetailScrollHome);
        assert_eq!(scroll(&a.mode), 0, "Home rewinds to the top");
        // The same arms drive the help overlay.
        apply_action(&mut a, Action::DetailClose);
        apply_action(&mut a, Action::HelpToggle);
        apply_action(&mut a, Action::DetailScrollPage(Direction::Down));
        assert!(matches!(a.mode, Mode::Help { scroll: 10 }), "help pages too");
    }

    #[test]
    fn gap_next_and_prev_cycle_through_untested_models() {
        // The big fixture has zero untested MODELS, so it pins the no-op side;
        // the sample manifest (dim_customers / agg_country_orders untested)
        // exercises the cycle itself.
        let mut full = app();
        apply_action(&mut full, Action::GapNext);
        assert_eq!(full.ui_state.selected(), 0, "no gaps -> selection unmoved");

        let sample = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample_manifest.json"
        );
        let mut a = App::new(load_dag(sample).expect("sample loads"), PathBuf::from(sample));
        let gaps: Vec<usize> = a
            .model_list
            .models
            .iter()
            .enumerate()
            .filter(|(_, m)| crate::coverage_gap(m))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(gaps.len(), 2, "sample manifest has two untested models");
        apply_action(&mut a, Action::GapNext);
        let first = a.ui_state.selected();
        assert!(gaps.contains(&first), "n lands on a coverage gap");
        apply_action(&mut a, Action::GapNext);
        let second = a.ui_state.selected();
        assert!(gaps.contains(&second) && second != first, "n cycles onward");
        apply_action(&mut a, Action::GapPrev);
        assert_eq!(a.ui_state.selected(), first, "N walks back");
    }

    #[test]
    fn bookmark_cycle_back_walks_bookmarks_in_reverse() {
        let mut a = app();
        // Bookmark models at indices 3 and 7, then cycle backward from 0.
        for i in [3usize, 7] {
            a.ui_state.set_selected(i);
            apply_action(&mut a, Action::BookmarkToggle);
        }
        a.ui_state.set_selected(0);
        apply_action(&mut a, Action::BookmarkCycleBack);
        assert_eq!(a.ui_state.selected(), 7, "backward wraps to the last bookmark");
        apply_action(&mut a, Action::BookmarkCycleBack);
        assert_eq!(a.ui_state.selected(), 3, "backward again hits the earlier one");
        apply_action(&mut a, Action::BookmarkCycle);
        assert_eq!(a.ui_state.selected(), 7, "forward cycles onward");
    }

    #[test]
    fn lineage_extreme_jumps_send_the_cursor_to_first_and_last_columns() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let lay = crate::layout(&a.lineage_subgraph());
        let max_col = lay.columns.values().max().copied().unwrap();
        assert!(max_col >= 2, "fixture lineage spans 3+ columns");
        apply_action(&mut a, Action::LineageRightmost);
        let cur = a.lineage_cursor_uid().unwrap();
        assert_eq!(lay.columns[&cur], max_col, "L lands in the last column");
        apply_action(&mut a, Action::LineageLeftmost);
        let cur = a.lineage_cursor_uid().unwrap();
        assert_eq!(lay.columns[&cur], 0, "H lands in the first column");
    }

    #[test]
    fn untested_filter_narrows_the_list_and_toggles_off() {
        let sample = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample_manifest.json"
        );
        let mut a = App::new(load_dag(sample).expect("sample loads"), PathBuf::from(sample));
        let full = a.model_list.len();
        apply_action(&mut a, Action::ToggleUntestedFilter);
        assert_eq!(a.list_filter, ListFilter::Untested);
        assert_eq!(a.active_list().len(), 2, "only the two untested models");
        assert!(
            a.active_list().models.iter().all(crate::coverage_gap),
            "every visible model is untested"
        );
        assert_eq!(
            a.ui_state.model_count(),
            2,
            "selection space follows the filtered view"
        );
        assert_eq!(a.list_filter_label(), Some("untested"));
        apply_action(&mut a, Action::ToggleUntestedFilter);
        assert_eq!(a.list_filter, ListFilter::All, "second press toggles off");
        assert_eq!(a.active_list().len(), full, "full list restored");
        assert_eq!(a.list_filter_label(), None);
    }

    #[test]
    fn bookmark_filter_tracks_toggles_and_search_narrows_on_top() {
        let mut a = app();
        let kept = "model.jaffle_finance.stg_payment__shoppers";
        a.select_by_unique_id(kept);
        apply_action(&mut a, Action::BookmarkToggle);
        apply_action(&mut a, Action::ToggleBookmarkFilter);
        assert_eq!(a.active_list().len(), 1, "only the bookmarked model");
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(kept),
            "selection re-resolved by id into the filtered view"
        );
        // A list search narrows FROM the bookmarked view: a query matching
        // many full-list models still shows only the bookmarked one.
        apply_action(&mut a, Action::SearchOpen);
        for c in "stg".chars() {
            apply_action(&mut a, Action::SearchType(c));
        }
        assert_eq!(a.active_list().len(), 1, "search composes with the filter");
        apply_action(&mut a, Action::SearchCancel);
        // Un-bookmarking the row under the live Bookmarked filter empties it.
        apply_action(&mut a, Action::BookmarkToggle);
        assert_eq!(a.active_list().len(), 0, "un-bookmark leaves the view");
        // Toggling the OTHER filter replaces, not stacks.
        apply_action(&mut a, Action::ToggleUntestedFilter);
        assert_eq!(a.list_filter, ListFilter::Untested);
        apply_action(&mut a, Action::ToggleBookmarkFilter);
        assert_eq!(a.list_filter, ListFilter::Bookmarked);
    }

    #[test]
    fn critical_path_is_a_deterministic_source_rooted_chain() {
        let a = app();
        let sv = a.compute_stats_view();
        assert!(
            sv.critical_path.len() >= 4,
            "the fixture graph is at least 4 deep, got {:?}",
            sv.critical_path
        );
        // The chain start must have no parents (else its parent's chain would
        // be longer) and the end no children — both checked by name lookup.
        let by_name = |name: &str| {
            a.dag
                .nodes()
                .values()
                .filter(|n| n.name == name)
                .collect::<Vec<_>>()
        };
        let first = sv.critical_path.first().unwrap();
        assert!(
            by_name(first).iter().any(|n| n.direct_up == 0),
            "chain starts at a root: {first}"
        );
        let last = sv.critical_path.last().unwrap();
        assert!(
            by_name(last).iter().any(|n| n.direct_down == 0),
            "chain ends at a leaf: {last}"
        );
        // Deterministic: a second computation is identical.
        assert_eq!(sv.critical_path, a.compute_stats_view().critical_path);
    }

    #[test]
    fn sql_modal_payload_carries_the_file_path() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        apply_action(&mut a, Action::SqlOpen);
        match &a.mode {
            Mode::Sql(sv) => assert_eq!(
                sv.path.as_deref(),
                Some("models/utilities/pos_prep/pos_txn.sql"),
                "path snapshotted at open"
            ),
            other => panic!("expected Sql mode, got {other:?}"),
        }
    }

    #[test]
    fn select_by_name_resolves_models_and_rejects_unknowns() {
        let mut a = app();
        assert!(a.select_by_name("pos_txn"), "known model name selects");
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.pos_txn")
        );
        let before = a.ui_state.selected();
        assert!(!a.select_by_name("no_such_model"), "unknown name rejected");
        assert_eq!(a.ui_state.selected(), before, "selection untouched");
        // The watch root in manifest mode is the manifest FILE.
        let (root, recursive) = a.watch_root();
        assert!(root.ends_with("manifest.json"));
        assert!(!recursive);
    }

    #[test]
    fn jump_to_records_history_and_back_forward_navigate() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
        let start = a.selected_unique_id().unwrap();
        a.jump_to("model.jaffle_finance.int_shoppers__enriched");
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.int_shoppers__enriched")
        );
        a.history_back();
        assert_eq!(
            a.selected_unique_id(),
            Some(start),
            "back returns to the origin"
        );
        a.history_forward();
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.int_shoppers__enriched"),
            "forward re-applies the jump"
        );
        // A new jump clears the forward stack.
        a.history_back();
        a.jump_to("model.jaffle_finance.pos_txn");
        a.history_forward();
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.pos_txn"),
            "forward is a no-op after a new jump cleared it"
        );
    }

    #[test]
    fn lineage_view_toggles_and_depth_filter_the_subgraph() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        let full = a.lineage_subgraph().nodes.len();
        assert_eq!(full, 30, "full lineage = 27 up + selected + 2 down");

        // Upstream-only: drop the downstream side.
        apply_action(&mut a, Action::ToggleDownstream);
        let up_only = a.lineage_subgraph();
        assert_eq!(up_only.nodes.len(), 28, "27 upstream + selected");
        assert!(
            !up_only
                .nodes
                .iter()
                .any(|n| n.unique_id == "model.jaffle_finance.fct_delivery_monthly_snapshot"),
            "a downstream node is excluded"
        );

        // Reset restores the full view.
        apply_action(&mut a, Action::ResetView);
        assert_eq!(a.lineage_subgraph().nodes.len(), full);

        // Depth limit: None → 3 → 2 → 1, shrinking the neighbourhood.
        apply_action(&mut a, Action::DepthDecrease);
        apply_action(&mut a, Action::DepthDecrease);
        apply_action(&mut a, Action::DepthDecrease);
        assert_eq!(a.lineage_view.depth, Some(1));
        let d1 = a.lineage_subgraph().nodes.len();
        assert!(
            d1 > 1 && d1 < full,
            "1-hop neighbourhood is a strict subset: {d1}"
        );
        // Widening past the cap returns to unlimited.
        for _ in 0..10 {
            apply_action(&mut a, Action::DepthIncrease);
        }
        assert_eq!(
            a.lineage_view.depth, None,
            "widening past 8 hops → unlimited"
        );
        assert_eq!(a.lineage_subgraph().nodes.len(), full);
    }

    #[test]
    fn reload_error_leaves_state_unchanged() {
        // A missing manifest path: reload returns Err BEFORE mutating, so the app
        // keeps running on the old data (run_effect swallows the Err).
        let dag = load_dag(FIXTURE).expect("fixture loads");
        let mut a = App::new(dag, PathBuf::from("/no/such/manifest.json"));
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        let before_len = a.model_list.len();
        let before_sel = a.selected_unique_id();
        assert!(a.reload().is_err(), "missing manifest → Err");
        assert_eq!(
            a.model_list.len(),
            before_len,
            "list unchanged on reload error"
        );
        assert_eq!(
            a.selected_unique_id(),
            before_sel,
            "selection unchanged on reload error"
        );
    }

    #[test]
    fn effect_actions_emit_effects_without_mutating_state() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        let before = a.ui_state.selected();

        // Yank id / name produce a clipboard effect carrying the right text.
        assert_eq!(
            apply_action(&mut a, Action::YankId).effects,
            vec![Effect::Yank("model.jaffle_finance.pos_txn".into())]
        );
        assert_eq!(
            apply_action(&mut a, Action::YankName).effects,
            vec![Effect::Yank("pos_txn".into())]
        );
        // Reload always requests a reload effect.
        assert_eq!(
            apply_action(&mut a, Action::Reload).effects,
            vec![Effect::ReloadManifest]
        );
        // OpenEditor resolves the SQL path under the project root.
        match &apply_action(&mut a, Action::OpenEditor).effects[..] {
            [Effect::OpenEditor(path)] => {
                assert!(
                    path.ends_with("models/utilities/pos_prep/pos_txn.sql"),
                    "got {path}"
                );
            }
            other => panic!("expected one OpenEditor effect, got {other:?}"),
        }
        // The pure reducer never mutated selection while emitting effects.
        assert_eq!(a.ui_state.selected(), before);
    }

    #[test]
    fn derive_project_root_strips_target() {
        let root = derive_project_root(Path::new("/x/proj/target/manifest.json"));
        assert_eq!(root, PathBuf::from("/x/proj"));
    }

    #[test]
    fn lineage_styles_maps_materialization_and_tests() {
        let a = app();
        let lay = crate::layout(&a.dag.subgraph("model.jaffle_finance.pos_txn"));
        let styles = a.lineage_styles(&lay);
        // pos_txn is a table model.
        assert_eq!(
            styles.get("model.jaffle_finance.pos_txn").unwrap().class,
            MaterializationClass::Table
        );
        // Its upstream subgraph contains source/seed nodes, classed accordingly.
        assert!(
            styles
                .values()
                .any(|c| c.class == MaterializationClass::Source),
            "a source node is classed Source"
        );
        assert!(
            styles
                .values()
                .any(|c| c.class == MaterializationClass::Seed),
            "a seed node is classed Seed"
        );
    }

    #[test]
    fn help_toggle_opens_and_closes() {
        let mut a = app();
        apply_action(&mut a, Action::HelpToggle);
        assert!(matches!(a.mode, Mode::Help { .. }), "? opens help");
        apply_action(&mut a, Action::HelpToggle);
        assert!(matches!(a.mode, Mode::Selection), "? again closes help");
    }

    #[test]
    fn lineage_search_jumps_to_a_matching_upstream_node() {
        // Open a lineage-target search from the RIGHT pane and confirm: a matching
        // UPSTREAM model becomes the new selection (re-root).
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
        a.ui_state.set_focus(Focus::RightPane);
        apply_action(&mut a, Action::SearchOpen);
        assert!(
            matches!(&a.mode, Mode::Search(s) if s.target == SearchTarget::Lineage),
            "search from the lineage pane targets the lineage"
        );
        for c in "dimsupp".chars() {
            apply_action(&mut a, Action::SearchType(c));
        }
        // "dimsupp" matches a dim_supplier* model — an upstream of fct.
        let fct = "model.jaffle_finance.fct_subscription_process";
        let hit = a.current_lineage_match().expect("a lineage node matches");
        assert!(
            a.dag.upstream(fct).contains(&hit),
            "the match is an upstream of fct: {hit}"
        );
        assert_ne!(
            hit, fct,
            "the match is a different node than the current root"
        );
        apply_action(&mut a, Action::SearchConfirm);
        assert!(matches!(a.mode, Mode::Selection), "confirm leaves search");
        assert_eq!(
            a.active_list()
                .model_at(a.ui_state.selected())
                .unwrap()
                .unique_id,
            hit,
            "the matched upstream model is now selected (re-rooted)"
        );
    }

    // ---- lineage cursor: spatial movement, focus routing, Enter commit ----

    const FCT: &str = "model.jaffle_finance.fct_subscription_process";

    #[test]
    fn lineage_cursor_moves_spatially_under_right_pane_focus() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        let lay = crate::layout(&a.lineage_subgraph());
        let root_col = lay.columns[FCT];
        assert!(root_col > 0, "fct has upstream columns");

        // h: exactly one column upstream per keypress, down to column 0.
        apply_action(&mut a, Action::MoveLeft);
        let cur = a.lineage_cursor_uid().unwrap();
        assert_eq!(lay.columns[&cur], root_col - 1, "h = one column left");
        for expect in (0..root_col - 1).rev() {
            apply_action(&mut a, Action::MoveLeft);
            let c = a.lineage_cursor_uid().unwrap();
            assert_eq!(lay.columns[&c], expect, "each h steps one column");
        }
        let edge = a.lineage_cursor_uid().unwrap();
        apply_action(&mut a, Action::MoveLeft);
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(edge.as_str()),
            "h at the most-upstream column is a no-op"
        );

        // j/k: step through column 0's stack (sources/seeds — always ≥2 for
        // fct) and round-trip.
        let mut col: Vec<(String, usize)> = lay
            .rects
            .iter()
            .filter(|(uid, _)| lay.columns[uid.as_str()] == 0)
            .map(|(uid, r)| (uid.clone(), r.y))
            .collect();
        col.sort_by_key(|(_, y)| *y);
        assert!(col.len() >= 2, "fixture: column 0 stacks nodes");
        let i = col.iter().position(|(uid, _)| *uid == edge).unwrap();
        if i + 1 < col.len() {
            apply_action(&mut a, Action::MoveDown);
            assert_eq!(
                a.lineage_cursor_uid().as_deref(),
                Some(col[i + 1].0.as_str()),
                "j = next node down the column"
            );
            apply_action(&mut a, Action::MoveUp);
        } else {
            apply_action(&mut a, Action::MoveUp);
            assert_eq!(
                a.lineage_cursor_uid().as_deref(),
                Some(col[i - 1].0.as_str()),
                "k = previous node up the column"
            );
            apply_action(&mut a, Action::MoveDown);
        }
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(edge.as_str()),
            "j/k round-trips"
        );

        // l: one column back toward downstream.
        apply_action(&mut a, Action::MoveRight);
        let back = a.lineage_cursor_uid().unwrap();
        assert_eq!(lay.columns[&back], 1, "l = one column right");

        // The rooted selection never moved while the cursor walked.
        assert_eq!(a.selected_unique_id().as_deref(), Some(FCT));
    }

    #[test]
    fn lineage_cursor_routing_display_subgraph_and_reset() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        // List focus: movement keys drive the LIST selection; the cursor stays home.
        let before = a.ui_state.selected();
        apply_action(&mut a, Action::MoveDown);
        assert_eq!(
            a.ui_state.selected(),
            before + 1,
            "list focus moves the list"
        );
        assert_eq!(
            a.lineage_cursor_uid(),
            a.selected_unique_id(),
            "cursor home = the selection"
        );

        // Lineage focus: the cursor walks and the DISPLAY subgraph re-selects it…
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        apply_action(&mut a, Action::MoveLeft);
        let cur = a.lineage_cursor_uid().unwrap();
        assert_ne!(cur.as_str(), FCT, "the cursor left the root");
        assert_eq!(
            a.lineage_display_subgraph().selected,
            cur,
            "display subgraph selects (→ emphasizes, anchors) the cursor"
        );
        // …while ROOT semantics (exports, matches, title) stay on the selection.
        assert_eq!(a.lineage_subgraph().selected, FCT);

        // z (recenter) sends the cursor home.
        apply_action(&mut a, Action::Recenter);
        assert_eq!(a.lineage_cursor_uid().as_deref(), Some(FCT));
    }

    #[test]
    fn toggle_list_pane_pins_focus_and_movement_drives_the_lineage_cursor() {
        let mut a = app();
        a.select_by_unique_id(FCT);

        // Hide the list: focus is pinned to the lineage pane…
        apply_action(&mut a, Action::ToggleListPane);
        assert!(!a.ui_state.list_visible());
        assert_eq!(a.ui_state.focus(), Focus::RightPane);
        // …so the movement keys route to the lineage CURSOR, never the hidden
        // list (the selection — the lineage root — must not move).
        apply_action(&mut a, Action::MoveLeft);
        assert_ne!(
            a.lineage_cursor_uid().as_deref(),
            Some(FCT),
            "h walks the cursor upstream while the list is hidden"
        );
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(FCT),
            "the rooted selection stays put"
        );

        // Show it again: the list pane takes focus back.
        apply_action(&mut a, Action::ToggleListPane);
        assert!(a.ui_state.list_visible());
        assert_eq!(a.ui_state.focus(), Focus::List);
    }

    #[test]
    fn stale_lineage_cursor_falls_back_to_the_root() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        apply_action(&mut a, Action::MoveLeft); // cursor = an upstream node
        let cur = a.lineage_cursor_uid().unwrap();
        assert_ne!(cur.as_str(), FCT);
        // Dropping the upstream side removes the cursor's node from the subgraph.
        apply_action(&mut a, Action::ToggleUpstream);
        assert!(
            !a.lineage_subgraph().contains(&cur),
            "precondition: the cursor's node was dropped from the view"
        );
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(FCT),
            "a stale cursor falls back to the root"
        );
        assert_eq!(a.lineage_display_subgraph().selected, FCT);
    }

    #[test]
    fn reload_sends_the_cursor_home() {
        // Reload restores the selection BY ID, so the loop's selection-change
        // chokepoint never fires — reload itself must re-home the cursor or a
        // pre-reload cursor would survive into the rebuilt graph.
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        apply_action(&mut a, Action::MoveLeft);
        assert_ne!(a.lineage_cursor_uid().as_deref(), Some(FCT));
        a.reload().expect("reload ok");
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(FCT),
            "reload re-homes the cursor to the (restored) root"
        );
    }

    #[test]
    fn click_reroots_models_moves_cursor_for_sources_and_rehomes_on_root() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        // Click an upstream MODEL: re-root + cursor home.
        let up_model = a
            .dag
            .upstream(FCT)
            .iter()
            .filter(|u| u.starts_with("model."))
            .min()
            .cloned()
            .unwrap();
        a.click_lineage_node(&up_model);
        assert_eq!(a.selected_unique_id().as_deref(), Some(up_model.as_str()));
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(up_model.as_str()),
            "cursor is home on the new root"
        );

        // Click a SOURCE: no re-root, the CURSOR moves there.
        a.select_by_unique_id(FCT);
        let src = a
            .dag
            .upstream(FCT)
            .iter()
            .filter(|u| u.starts_with("source."))
            .min()
            .cloned()
            .unwrap();
        a.click_lineage_node(&src);
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(FCT),
            "clicking a source never re-roots"
        );
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(src.as_str()),
            "the cursor moved to the clicked source"
        );

        // Click the CURRENT ROOT: id-preserving, so the loop's chokepoint won't
        // fire — the click itself must send the cursor home.
        a.click_lineage_node(FCT);
        assert_eq!(a.selected_unique_id().as_deref(), Some(FCT));
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(FCT),
            "clicking the root re-homes the cursor"
        );
    }

    #[test]
    fn enter_commits_cursor_reroot_for_models_structure_for_sources() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);

        // (a) cursor == root: Enter opens the root's structure (unchanged).
        apply_action(&mut a, Action::DetailOpen);
        match &a.mode {
            Mode::Detail(dv) => assert_eq!(dv.model_id, FCT),
            m => panic!("expected Detail for the root, got {m:?}"),
        }
        apply_action(&mut a, Action::DetailClose);

        // (b) cursor on an upstream MODEL: Enter re-roots (same as a click),
        // recording history.
        let up_model = a
            .dag
            .upstream(FCT)
            .iter()
            .filter(|u| u.starts_with("model."))
            .min()
            .cloned()
            .expect("fct has an upstream model");
        a.lineage_cursor = Some(up_model.clone());
        apply_action(&mut a, Action::DetailOpen);
        assert!(
            matches!(a.mode, Mode::Selection),
            "a re-root stays in Selection mode"
        );
        assert_eq!(a.selected_unique_id().as_deref(), Some(up_model.as_str()));
        a.history_back();
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(FCT),
            "the Enter re-root recorded history (b returns)"
        );

        // (c) cursor on a SOURCE (not list-selectable): Enter opens ITS
        // structure modal instead, and the root stays put.
        a.select_by_unique_id(FCT);
        let src = a
            .dag
            .upstream(FCT)
            .iter()
            .filter(|u| u.starts_with("source."))
            .min()
            .cloned()
            .expect("fct has an upstream source");
        a.lineage_cursor = Some(src.clone());
        apply_action(&mut a, Action::DetailOpen);
        match &a.mode {
            Mode::Detail(dv) => {
                assert_eq!(dv.model_id, src);
                assert_eq!(
                    Some(dv.name.as_str()),
                    a.dag.get(&src).map(|n| n.name.as_str()),
                    "the modal is titled with the source's name"
                );
            }
            m => panic!("expected Detail for the source, got {m:?}"),
        }
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(FCT),
            "a non-model cursor never re-roots"
        );
    }

    #[test]
    fn detail_open_snapshots_payload_from_side_maps() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        apply_action(&mut a, Action::DetailOpen);
        match &a.mode {
            Mode::Detail(dv) => {
                assert_eq!(dv.model_id, "model.jaffle_finance.pos_txn");
                assert_eq!(dv.name, "pos_txn");
                assert_eq!(dv.detail.materialized.as_deref(), Some("table"));
                assert!(
                    !dv.detail.columns.is_empty(),
                    "columns came from the Dag side map"
                );
            }
            _ => panic!("expected Detail mode"),
        }
        apply_action(&mut a, Action::DetailClose);
        assert!(
            matches!(a.mode, Mode::Selection),
            "Esc/close returns to Selection"
        );
    }

    #[test]
    fn selected_status_note_reports_materialization_and_tests() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        let note = a.selected_status_note().expect("a model is selected");
        assert!(
            note.contains("table"),
            "status note names the materialization: {note}"
        );
        assert!(
            note.contains("tests:"),
            "status note includes a tests count: {note}"
        );
    }

    // ---- SQL preview / stats dashboard modals ----

    #[test]
    fn sql_open_snapshots_raw_code() {
        // A manifest-loaded model carries raw_code; SqlOpen snapshots it into the
        // Mode payload (no Dag in the render layer). pos_txn has clean ASCII SQL.
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        apply_action(&mut a, Action::SqlOpen);
        match &a.mode {
            Mode::Sql(sv) => {
                assert_eq!(sv.model_id, "model.jaffle_finance.pos_txn");
                assert_eq!(sv.name, "pos_txn");
                assert!(
                    !sv.sql.is_empty() && !sv.sql.starts_with("(no SQL"),
                    "real raw_code was snapshotted: {:?}",
                    &sv.sql[..sv.sql.len().min(40)]
                );
                assert_eq!(sv.scroll, 0);
            }
            m => panic!("expected Mode::Sql, got {m:?}"),
        }
        apply_action(&mut a, Action::DetailClose);
        assert!(matches!(a.mode, Mode::Selection), "DetailClose closes Sql");
    }

    #[test]
    fn sql_open_source_shows_placeholder() {
        // A source has no raw_code → the modal shows a placeholder, not a panic.
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        // Move the cursor onto an upstream source.
        let src = a
            .dag
            .upstream(FCT)
            .iter()
            .filter(|u| u.starts_with("source."))
            .min()
            .cloned()
            .expect("fct has an upstream source");
        a.lineage_cursor = Some(src.clone());
        apply_action(&mut a, Action::SqlOpen);
        match &a.mode {
            Mode::Sql(sv) => {
                assert_eq!(sv.model_id, src);
                assert!(
                    sv.sql.starts_with("(no SQL"),
                    "source SQL is a placeholder: {}",
                    sv.sql
                );
            }
            m => panic!("expected Mode::Sql, got {m:?}"),
        }
    }

    #[test]
    fn sql_open_respects_focus() {
        // Lineage focus previews the CURSOR; list focus previews the selection.
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        let up_model = a
            .dag
            .upstream(FCT)
            .iter()
            .filter(|u| u.starts_with("model."))
            .min()
            .cloned()
            .expect("fct has an upstream model");
        a.lineage_cursor = Some(up_model.clone());
        apply_action(&mut a, Action::SqlOpen);
        match &a.mode {
            Mode::Sql(sv) => assert_eq!(sv.model_id, up_model, "lineage focus previews the cursor"),
            m => panic!("expected Mode::Sql, got {m:?}"),
        }
        apply_action(&mut a, Action::DetailClose);

        // List focus: the cursor is ignored; the selection (the root) is previewed.
        a.ui_state.set_focus(Focus::List);
        apply_action(&mut a, Action::SqlOpen);
        match &a.mode {
            Mode::Sql(sv) => assert_eq!(sv.model_id, FCT, "list focus previews the selection"),
            m => panic!("expected Mode::Sql, got {m:?}"),
        }
    }

    #[test]
    fn stats_open_computes_dashboard() {
        let mut a = app();
        apply_action(&mut a, Action::StatsOpen);
        let sv = match &a.mode {
            Mode::Stats(sv) => sv.clone(),
            m => panic!("expected Mode::Stats, got {m:?}"),
        };
        // Coverage base == coverage_summary's (model|seed|snapshot): 45+7+1 = 53,
        // and the sole gap in the fixture is the untested snapshot.
        assert_eq!(sv.testable_total, 53, "fixture: 45 models + 7 seeds + 1 snapshot");
        assert_eq!(sv.untested_testable, 1, "fixture: only the snapshot is untested");
        assert_eq!(
            (sv.testable_tested, sv.testable_total),
            a.coverage_summary(),
            "dashboard coverage == the lens/status coverage_summary (single source)"
        );
        let rt = |k: &str| {
            sv.by_resource_type
                .iter()
                .find(|(name, _)| name == k)
                .map(|(_, n)| *n)
        };
        assert_eq!(rt("source"), Some(38), "fixture: 38 sources");
        assert_eq!(rt("seed"), Some(7), "fixture: 7 seeds");
        assert_eq!(rt("snapshot"), Some(1), "fixture: 1 snapshot");
        assert_eq!(
            sv.testable_tested + sv.untested_testable,
            sv.testable_total,
            "tested + untested == testable_total"
        );
        assert!(sv.top_hubs.len() <= 5, "at most 5 hubs");
        // Hubs are degree-desc, then unique_id-asc — verify the ordering.
        for w in sv.top_hubs.windows(2) {
            assert!(
                w[0].2 > w[1].2 || (w[0].2 == w[1].2 && w[0].0 <= w[1].0),
                "hubs sorted by degree desc then unique_id asc"
            );
        }

        // --- transitive_hubs: top-5 by downstream-closure size over ALL nodes,
        // count desc then unique_id asc (the seed/source siblings tie at 17, so
        // `seed.* < source.*` and the four pos_* payment sources order by name;
        // pos_shp @17 drops off the cap). Derived by running against the fixture
        // and verified against `Dag::downstream`, not guessed.
        assert_eq!(
            sv.transitive_hubs,
            vec![
                ("source_datetime_policy".to_string(), 20),
                ("pos_prod_aws_store_master".to_string(), 17),
                ("pos_cat".to_string(), 17),
                ("pos_pay".to_string(), 17),
                ("pos_rcv".to_string(), 17),
            ],
            "top-5 transitive blast-radius hubs (count desc, unique_id tie-break)"
        );
        // Cross-check the leader against the live closure (reuse, not a magic number).
        assert_eq!(
            a.dag
                .downstream("seed.jaffle_finance.source_datetime_policy")
                .len(),
            20,
            "leader's transitive downstream closure size"
        );

        // --- orphans + violations: the committed fixture is clean on both (no
        // disconnected model, no backward layer edge). The cap/listing render is
        // exercised with hand-built StatsView literals in the overlay tests.
        assert_eq!(sv.orphan_models, Vec::<String>::new(), "fixture has no orphans");
        assert_eq!(
            sv.layer_violations,
            Vec::<(String, String)>::new(),
            "fixture has no layer violations (matches layer_violation_edges)"
        );

        // Deterministic: a second compute is bit-identical.
        assert_eq!(a.compute_stats_view(), sv, "compute_stats_view is stable");
    }

    #[test]
    fn sql_and_stats_scroll_through_detail_scroll() {
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        // SQL modal: DetailScroll steps sv.scroll; up saturates at 0.
        apply_action(&mut a, Action::SqlOpen);
        apply_action(&mut a, Action::DetailScroll(Direction::Down));
        apply_action(&mut a, Action::DetailScroll(Direction::Down));
        match &a.mode {
            Mode::Sql(sv) => assert_eq!(sv.scroll, 2),
            m => panic!("expected Sql, got {m:?}"),
        }
        for _ in 0..5 {
            apply_action(&mut a, Action::DetailScroll(Direction::Up));
        }
        match &a.mode {
            Mode::Sql(sv) => assert_eq!(sv.scroll, 0, "scroll up saturates at 0"),
            m => panic!("expected Sql, got {m:?}"),
        }
        apply_action(&mut a, Action::DetailClose);

        // Stats modal: same plumbing.
        apply_action(&mut a, Action::StatsOpen);
        apply_action(&mut a, Action::DetailScroll(Direction::Down));
        match &a.mode {
            Mode::Stats(sv) => assert_eq!(sv.scroll, 1),
            m => panic!("expected Stats, got {m:?}"),
        }
    }

    #[test]
    fn feature_toggle_stubs_are_safe_noops() {
        // The 5 scaffolded-but-unimplemented toggles must not mutate selection or
        // change the mode (filled by later steps).
        let mut a = app();
        a.select_by_unique_id(FCT);
        let before = a.ui_state.selected();
        for action in [
            Action::CycleLens,
            Action::BookmarkToggle,
            Action::BookmarkCycle,
            Action::SortCycle,
            Action::ToggleMinimap,
        ] {
            let out = apply_action(&mut a, action);
            assert!(!out.quit && out.effects.is_empty(), "{action:?} is a no-op");
            assert!(matches!(a.mode, Mode::Selection), "{action:?} keeps Selection");
        }
        assert_eq!(a.ui_state.selected(), before, "no toggle moved selection");
    }

    #[test]
    fn overlay_scroll_acts_only_inside_an_overlay() {
        let mut a = app();
        // In Selection mode, an overlay-scroll action is a no-op (no panic).
        apply_action(&mut a, Action::DetailScroll(Direction::Down));
        assert!(matches!(a.mode, Mode::Selection));
        // In Help mode it advances the help scroll.
        apply_action(&mut a, Action::HelpToggle);
        apply_action(&mut a, Action::DetailScroll(Direction::Down));
        apply_action(&mut a, Action::DetailScroll(Direction::Down));
        match a.mode {
            Mode::Help { scroll } => assert_eq!(scroll, 2),
            _ => panic!("expected Help mode"),
        }
        // Up saturates at 0.
        for _ in 0..5 {
            apply_action(&mut a, Action::DetailScroll(Direction::Up));
        }
        match a.mode {
            Mode::Help { scroll } => assert_eq!(scroll, 0, "scroll up saturates at 0"),
            _ => panic!("expected Help mode"),
        }
    }

    // ---- Step B: test-coverage lens ('t') + automatic root↔cursor path ----

    /// The one fixture node that is a coverage gap (testable, zero tests): the
    /// delivery-classifications snapshot. All 45 models and all 7 seeds carry
    /// tests, so the snapshot is the sole gap (verified against the fixture).
    const GAP_SNAPSHOT: &str = "snapshot.jaffle_finance.delivery_classifications_snapshot";

    #[test]
    fn coverage_gap_predicate_over_resource_type_and_tests() {
        let a = app();
        // A tested model is NOT a gap; the snapshot with zero tests IS.
        let txn = a.dag.get("model.jaffle_finance.pos_txn").unwrap();
        assert!(txn.test_count > 0 && !coverage_gap(txn), "tested model: no gap");
        let snap = a.dag.get(GAP_SNAPSHOT).unwrap();
        assert!(
            snap.test_count == 0 && coverage_gap(snap),
            "untested snapshot is a coverage gap"
        );
        // A source is NEVER a gap even with zero tests (excluded by type).
        let src = a
            .dag
            .nodes()
            .values()
            .find(|n| n.resource_type == "source" && n.test_count == 0)
            .expect("fixture has an untested source");
        assert!(!coverage_gap(src), "a source is never a coverage gap");
        // A synthetic untested model IS a gap (predicate is pure over NodeInfo).
        let m = NodeInfo {
            resource_type: "model".into(),
            test_count: 0,
            ..Default::default()
        };
        assert!(coverage_gap(&m), "an untested model is a gap");
    }

    #[test]
    fn coverage_summary_counts_testable_resources() {
        // Fixture: 45 models + 7 seeds + 1 snapshot = 53 testable; only the
        // snapshot is untested → 52 tested.
        let a = app();
        assert_eq!(
            a.coverage_summary(),
            (52, 53),
            "coverage over model/snapshot/seed: 52 of 53 carry tests"
        );
    }

    #[test]
    fn lineage_path_set_empty_when_cursor_home() {
        // Cursor home (== selection): the set is empty so a home cursor
        // highlights nothing.
        let mut a = app();
        a.select_by_unique_id(FCT);
        assert_eq!(a.lineage_cursor_uid().as_deref(), Some(FCT), "cursor home");
        assert!(
            a.lineage_path_set().is_empty(),
            "a home cursor produces an empty path set"
        );
    }

    #[test]
    fn lineage_path_set_connects_root_to_an_upstream_cursor() {
        // Off-root (upstream) cursor: the path contains both endpoints, every
        // member is adjacent to another in the (undirected) subgraph edges, and
        // the result is deterministic across two calls.
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        apply_action(&mut a, Action::MoveLeft); // cursor → one column upstream
        let cur = a.lineage_cursor_uid().unwrap();
        assert_ne!(cur, FCT, "cursor left the root");

        let path = a.lineage_path_set();
        assert!(path.contains(FCT), "path includes the root");
        assert!(path.contains(&cur), "path includes the cursor");
        assert!(path.len() >= 2);

        // Connectivity: every path node touches another path node via some edge
        // (treated undirected), so the highlighted set is one connected chain.
        let sg = a.lineage_subgraph();
        for uid in &path {
            let touches = sg.edges.iter().any(|e| {
                (e.parent == *uid && path.contains(&e.child))
                    || (e.child == *uid && path.contains(&e.parent))
            });
            assert!(touches, "path node {uid} is connected to another path node");
        }
        // Deterministic.
        assert_eq!(path, a.lineage_path_set(), "two calls are identical");
    }

    #[test]
    fn lineage_path_set_is_undirected_for_a_downstream_cursor() {
        // The cursor can sit DOWNSTREAM of the root (not just upstream): the
        // undirected BFS must still connect them. Root a node and place the
        // cursor on a known downstream model.
        let root = "model.jaffle_finance.int_delivery_classifications__enriched";
        let down_model = "model.jaffle_finance.fct_delivery_monthly_snapshot";
        let mut a = app();
        a.select_by_unique_id(root);
        assert!(
            a.dag.downstream(root).contains(down_model),
            "precondition: the model is downstream of the root"
        );
        assert!(
            a.lineage_subgraph().contains(down_model),
            "the downstream model is in the rooted subgraph"
        );
        a.lineage_cursor = Some(down_model.to_string());
        let path = a.lineage_path_set();
        assert!(path.contains(root), "undirected path includes the root");
        assert!(
            path.contains(down_model),
            "undirected path includes the downstream cursor"
        );
        // Connected chain (undirected) and deterministic.
        let sg = a.lineage_subgraph();
        for uid in &path {
            let touches = sg.edges.iter().any(|e| {
                (e.parent == *uid && path.contains(&e.child))
                    || (e.child == *uid && path.contains(&e.parent))
            });
            assert!(touches, "downstream path node {uid} stays connected");
        }
        assert_eq!(path, a.lineage_path_set(), "deterministic");
    }

    #[test]
    fn coverage_lens_sets_warn_tint_only_when_active() {
        // Root on the gap snapshot so its own box is in the layout. With the lens
        // Off no attr carries a tint; cycling to Coverage marks the gap snapshot
        // Warn while a tested upstream model and a source stay None.
        let mut a = app();
        a.select_by_unique_id(FCT); // any selectable model; we layout the snapshot subgraph below
        let lay = crate::layout(&a.dag.subgraph(GAP_SNAPSHOT));

        let styles_off = a.lineage_styles(&lay);
        assert!(
            styles_off.values().all(|c| c.lens == LensTint::None),
            "lens off: nothing is tinted"
        );

        // One `t` press from default == Coverage (the old behaviour).
        a.ui_state.cycle_lens();
        assert_eq!(a.ui_state.lens(), LineageLens::Coverage);
        let styles_on = a.lineage_styles(&lay);
        assert_eq!(
            styles_on.get(GAP_SNAPSHOT).unwrap().lens,
            LensTint::Warn,
            "Coverage lens: the gap snapshot is Warn-tinted"
        );
        // A tested upstream model is NOT tinted.
        let tested_up = "model.jaffle_finance.stg_masterdata__companies";
        assert!(
            styles_on.contains_key(tested_up),
            "precondition: tested model is in the snapshot subgraph"
        );
        assert_eq!(
            styles_on.get(tested_up).unwrap().lens,
            LensTint::None,
            "a tested model is not tinted under the coverage lens"
        );
        // A source upstream is never tinted (excluded by coverage_gap).
        let src = lay
            .rects
            .keys()
            .find(|uid| uid.starts_with("source."))
            .expect("the snapshot subgraph has an upstream source");
        assert_eq!(
            styles_on.get(src).unwrap().lens,
            LensTint::None,
            "a source is never tinted under the coverage lens"
        );
    }

    /// Style one node's own box by rooting the lineage on it and reading its
    /// `lens` tint — the node is always present in its own subgraph, so this is a
    /// deterministic way to assert a lens's per-node tint on the real fixture.
    fn tint_of(a: &mut App, lens: LineageLens, uid: &str) -> LensTint {
        a.select_by_unique_id(uid);
        while a.ui_state.lens() != lens {
            a.ui_state.cycle_lens();
        }
        let lay = crate::layout(&a.lineage_subgraph());
        a.lineage_styles(&lay).get(uid).copied().unwrap().lens
    }

    #[test]
    fn degree_heat_lens_buckets_by_transitive_downstream() {
        // Fixture-anchored buckets (computed, not guessed): pos_files__assignment
        // has 16 transitive downstream (HeatHigh), stg_payment__suppliers has 5
        // (HeatMid), fct_subscription_process has 2 (HeatLow), and the leaf
        // int_shoppers__enriched has 0 (None).
        let mut a = app();
        let hub = "model.jaffle_finance.pos_files__assignment";
        let mid = "model.jaffle_finance.stg_payment__suppliers";
        let leaf = "model.jaffle_finance.int_shoppers__enriched";
        assert_eq!(a.dag.downstream(hub).len(), 16, "fixture hub blast radius");
        assert_eq!(a.dag.downstream(mid).len(), 5, "fixture mid blast radius");
        assert_eq!(a.dag.downstream(FCT).len(), 2, "fixture FCT blast radius");
        assert_eq!(a.dag.downstream(leaf).len(), 0, "fixture leaf");
        assert_eq!(tint_of(&mut a, LineageLens::DegreeHeat, hub), LensTint::HeatHigh);
        assert_eq!(tint_of(&mut a, LineageLens::DegreeHeat, mid), LensTint::HeatMid);
        assert_eq!(tint_of(&mut a, LineageLens::DegreeHeat, FCT), LensTint::HeatLow);
        assert_eq!(tint_of(&mut a, LineageLens::DegreeHeat, leaf), LensTint::None);
    }

    #[test]
    fn layer_lens_tints_models_by_layer_and_leaves_non_models_untinted() {
        // Each layer maps to its own tint; a source keeps its class colour (None).
        let mut a = app();
        let staging = "model.jaffle_finance.stg_payment__shoppers";
        let inter = "model.jaffle_finance.int_shoppers__enriched";
        let marts = FCT; // fct_subscription_process is a marts model
        assert_eq!(
            tint_of(&mut a, LineageLens::Layer, staging),
            LensTint::LayerStaging
        );
        assert_eq!(
            tint_of(&mut a, LineageLens::Layer, inter),
            LensTint::LayerIntermediate
        );
        assert_eq!(
            tint_of(&mut a, LineageLens::Layer, marts),
            LensTint::LayerMarts
        );
        // A source upstream of stg_payment__shoppers gets None (its class colour).
        a.select_by_unique_id(staging);
        while a.ui_state.lens() != LineageLens::Layer {
            a.ui_state.cycle_lens();
        }
        let lay = crate::layout(&a.lineage_subgraph());
        let styles = a.lineage_styles(&lay);
        let src = lay
            .rects
            .keys()
            .find(|uid| uid.starts_with("source."))
            .expect("staging subgraph has an upstream source");
        assert_eq!(
            styles.get(src).unwrap().lens,
            LensTint::None,
            "a source is untinted under the layer lens (keeps its class colour)"
        );
    }

    #[test]
    fn layer_violation_edges_empty_on_a_clean_fixture_and_no_violation_tint() {
        // The committed fixture is a clean dbt project: no marts→staging-style
        // backward edge, so the violation set is empty and no node is tinted.
        let mut a = app();
        assert!(
            layer_violation_edges(&a.dag).is_empty(),
            "the clean fixture has no layer-violation edges"
        );
        assert_eq!(
            tint_of(&mut a, LineageLens::LayerViolation, FCT),
            LensTint::None,
            "no node is violation-tinted on a clean project"
        );
    }

    /// A tiny synthetic Dag with a deliberate layer violation: a `marts` model
    /// (`mt`) feeds a `staging` model (`st`) — a backward edge — plus a clean
    /// `staging → intermediate` edge (`st → it`) that is NOT a violation.
    fn violation_dag() -> Dag {
        use crate::{RawManifest, RawNode};
        use std::collections::HashMap;
        let mut nodes = HashMap::new();
        let mut add = |id: &str, name: &str, path: &str| {
            nodes.insert(
                id.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    path: Some(path.into()),
                    ..Default::default()
                },
            );
        };
        add("model.p.mt", "mt", "marts/mt.sql");
        add("model.p.st", "st", "staging/st.sql");
        add("model.p.it", "it", "intermediate/it.sql");
        // child_map: mt → st (BACKWARD), st → it (clean). parent_map mirrors it.
        let mut child_map = HashMap::new();
        child_map.insert("model.p.mt".to_string(), vec!["model.p.st".to_string()]);
        child_map.insert("model.p.st".to_string(), vec!["model.p.it".to_string()]);
        let mut parent_map = HashMap::new();
        parent_map.insert("model.p.st".to_string(), vec!["model.p.mt".to_string()]);
        parent_map.insert("model.p.it".to_string(), vec!["model.p.st".to_string()]);
        Dag::build(&RawManifest {
            nodes,
            sources: HashMap::new(),
            parent_map,
            child_map,
        })
    }

    #[test]
    fn layer_violation_edges_finds_the_backward_edge_only() {
        // Exactly the marts→staging edge is a violation; the clean staging→inter
        // edge is not. Deterministic (sorted) output.
        let dag = violation_dag();
        assert_eq!(
            layer_violation_edges(&dag),
            vec![("model.p.mt".to_string(), "model.p.st".to_string())],
            "only the marts→staging backward edge is a violation"
        );
    }

    #[test]
    fn violation_lens_tints_both_incident_nodes_and_spares_the_rest() {
        // Root on the marts model so the whole synthetic graph is laid out; under
        // the violation lens the two incident nodes (mt, st) are Violation-tinted,
        // the clean downstream `it` is not.
        let mut a = App::new(violation_dag(), PathBuf::from("/tmp/x/target/manifest.json"));
        a.select_by_unique_id("model.p.mt");
        a.ui_state.cycle_lens(); // Coverage
        a.ui_state.cycle_lens(); // DegreeHeat
        a.ui_state.cycle_lens(); // Layer
        a.ui_state.cycle_lens(); // LayerViolation
        assert_eq!(a.ui_state.lens(), LineageLens::LayerViolation);
        let lay = crate::layout(&a.lineage_subgraph());
        let styles = a.lineage_styles(&lay);
        assert_eq!(
            styles.get("model.p.mt").unwrap().lens,
            LensTint::Violation,
            "the marts parent is violation-tinted"
        );
        assert_eq!(
            styles.get("model.p.st").unwrap().lens,
            LensTint::Violation,
            "the staging child is violation-tinted"
        );
        assert_eq!(
            styles.get("model.p.it").unwrap().lens,
            LensTint::None,
            "the clean downstream model is not violation-tinted"
        );
    }

    #[test]
    fn lineage_styles_sets_on_path_for_an_off_root_cursor() {
        // With an off-root cursor, exactly the root↔cursor path nodes carry
        // on_path; nodes off the path do not.
        let mut a = app();
        a.select_by_unique_id(FCT);
        a.ui_state.set_focus(Focus::RightPane);
        apply_action(&mut a, Action::MoveLeft);
        let cur = a.lineage_cursor_uid().unwrap();
        assert_ne!(cur, FCT);

        let path = a.lineage_path_set();
        // Build the layout the loop draws (display subgraph), then style it.
        let lay = crate::layout(&a.lineage_display_subgraph());
        let styles = a.lineage_styles(&lay);
        for (uid, attr) in &styles {
            assert_eq!(
                attr.on_path,
                path.contains(uid),
                "on_path matches the path set for {uid}"
            );
        }
        // The two endpoints are on the path.
        assert!(styles.get(FCT).unwrap().on_path, "root is on the path");
        assert!(styles.get(&cur).unwrap().on_path, "cursor is on the path");

        // Cursor home → no on_path anywhere.
        a.reset_lineage_cursor();
        let styles_home = a.lineage_styles(&lay);
        assert!(
            styles_home.values().all(|c| !c.on_path),
            "a home cursor highlights no path"
        );
    }

    #[test]
    fn focus_dim_is_set_only_off_root_and_only_for_off_path_nodes() {
        // Focus dim is lens-INDEPENDENT (no lens active here): with the cursor at
        // home NOTHING is dimmed; with an off-root cursor, every node NOT on the
        // root↔cursor path is dimmed and the path nodes are not.
        let mut a = app();
        a.select_by_unique_id(FCT);

        // Home cursor → empty path → no dim anywhere (the diagram reads normally).
        let lay_home = crate::layout(&a.lineage_display_subgraph());
        let styles_home = a.lineage_styles(&lay_home);
        assert!(
            styles_home.values().all(|c| !c.dimmed),
            "a home cursor dims nothing"
        );

        // Off-root cursor → dim partitions exactly on the path set.
        a.ui_state.set_focus(Focus::RightPane);
        apply_action(&mut a, Action::MoveLeft);
        let cur = a.lineage_cursor_uid().unwrap();
        assert_ne!(cur, FCT, "precondition: cursor walked off the root");
        let path = a.lineage_path_set();
        assert!(!path.is_empty(), "off-root path is non-empty");
        let lay = crate::layout(&a.lineage_display_subgraph());
        let styles = a.lineage_styles(&lay);
        for (uid, attr) in &styles {
            assert_eq!(
                attr.dimmed,
                !path.contains(uid),
                "off-path nodes are dimmed, path nodes are not ({uid})"
            );
        }
        // The two endpoints are on the path, so never dimmed.
        assert!(!styles.get(FCT).unwrap().dimmed, "root never dims");
        assert!(!styles.get(&cur).unwrap().dimmed, "cursor never dims");
        // At least one off-path node exists in fct's subgraph and IS dimmed.
        assert!(
            styles.values().any(|c| c.dimmed),
            "fct's subgraph has off-path nodes that dim"
        );
    }

    #[test]
    fn cycle_lens_advances_the_lens_as_a_noop_action() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        let before = a.ui_state.selected();
        assert_eq!(a.ui_state.lens(), LineageLens::Off, "lens starts Off");
        let out = apply_action(&mut a, Action::CycleLens);
        assert!(!out.quit && out.effects.is_empty(), "cycle is a no-op action");
        assert!(matches!(a.mode, Mode::Selection), "stays in Selection");
        assert_eq!(a.ui_state.lens(), LineageLens::Coverage, "advanced to Coverage");
        // Cycle through the rest and back to Off.
        for expected in [
            LineageLens::DegreeHeat,
            LineageLens::Layer,
            LineageLens::LayerViolation,
            LineageLens::Off,
        ] {
            apply_action(&mut a, Action::CycleLens);
            assert_eq!(a.ui_state.lens(), expected);
        }
        assert_eq!(a.ui_state.selected(), before, "selection never moved");
    }

    #[test]
    fn toggle_minimap_flips_the_flag_as_a_noop_action() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        let before = a.ui_state.selected();
        assert!(!a.ui_state.minimap_visible(), "minimap starts off (default)");
        let out = apply_action(&mut a, Action::ToggleMinimap);
        assert!(!out.quit && out.effects.is_empty(), "toggle is a no-op action");
        assert!(matches!(a.mode, Mode::Selection), "stays in Selection");
        assert!(a.ui_state.minimap_visible(), "the flag flipped on");
        apply_action(&mut a, Action::ToggleMinimap);
        assert!(!a.ui_state.minimap_visible(), "and flips back off");
        assert_eq!(a.ui_state.selected(), before, "selection never moved");
    }

    #[test]
    fn bookmark_toggle_inserts_then_removes_on_selected_uid() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        assert!(a.bookmarks.is_empty(), "no bookmarks initially");
        apply_action(&mut a, Action::BookmarkToggle);
        assert!(a.bookmarks.contains(FCT), "first toggle adds the bookmark");
        apply_action(&mut a, Action::BookmarkToggle);
        assert!(!a.bookmarks.contains(FCT), "second toggle removes it");
        assert!(a.bookmarks.is_empty());
    }

    #[test]
    fn bookmark_toggle_is_a_noop_when_nothing_selected() {
        // An empty filter leaves no selectable model, so selected_unique_id is
        // None and the toggle does nothing (no panic, no insert).
        let mut a = app();
        a.mode = Mode::Search(crate::action::SearchState {
            target: SearchTarget::List,
            query: "zzzzzz".into(), // matches no model
            origin_uid: None,
            match_idx: 0,
        });
        a.refilter();
        assert!(a.selected_node().is_none(), "filter matched nothing");
        apply_action(&mut a, Action::BookmarkToggle);
        assert!(a.bookmarks.is_empty(), "toggle with no selection is a no-op");
    }

    #[test]
    fn bookmark_cycle_wraps_to_next_bookmarked_model() {
        let mut a = app();
        // Bookmark two models; cycling from one lands on the other, and again
        // wraps back. Use the flat models list to pick deterministic neighbours.
        let ids: Vec<String> = a
            .model_list
            .models
            .iter()
            .map(|m| m.unique_id.clone())
            .collect();
        let first = ids[1].clone(); // not index 0, so the wrap is exercised
        let second = ids[5].clone();
        a.bookmarks.insert(first.clone());
        a.bookmarks.insert(second.clone());
        a.select_by_unique_id(&first);
        apply_action(&mut a, Action::BookmarkCycle);
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(second.as_str()),
            "cycle moves to the next bookmarked model"
        );
        // From the later one, cycling wraps forward back to the earlier one.
        apply_action(&mut a, Action::BookmarkCycle);
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(first.as_str()),
            "cycle wraps around to the first bookmark"
        );
    }

    #[test]
    fn bookmark_cycle_is_a_noop_when_set_empty() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        let before = a.selected_unique_id();
        apply_action(&mut a, Action::BookmarkCycle);
        assert_eq!(a.selected_unique_id(), before, "no bookmarks ⇒ no move");
    }

    #[test]
    fn reload_prunes_vanished_bookmarks_and_keeps_survivors() {
        let mut a = app();
        // One real id (survives reload) and one stale id (pruned).
        a.bookmarks.insert(FCT.to_string());
        a.bookmarks.insert("model.jaffle_finance.gone_forever".to_string());
        a.reload().expect("reload ok");
        assert!(a.bookmarks.contains(FCT), "surviving id kept");
        assert!(
            !a.bookmarks.contains("model.jaffle_finance.gone_forever"),
            "vanished id pruned"
        );
        assert_eq!(a.bookmarks.len(), 1);
    }

    #[test]
    fn sort_cycle_advances_mode_and_preserves_selection_by_uid() {
        let mut a = app();
        a.select_by_unique_id(FCT);
        assert_eq!(a.sort, SortMode::Layer, "starts in Layer");
        apply_action(&mut a, Action::SortCycle);
        assert_eq!(a.sort, SortMode::Downstream, "cycles Layer→Downstream");
        // Selection is re-resolved BY uid across the rebuild — same node, even if
        // its row moved within its layer group.
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(FCT),
            "selection survives the sort rebuild by unique_id"
        );
        apply_action(&mut a, Action::SortCycle);
        assert_eq!(a.sort, SortMode::Tests);
        apply_action(&mut a, Action::SortCycle);
        assert_eq!(a.sort, SortMode::Layer, "wraps back to Layer");
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(FCT),
            "still the same node after a full cycle"
        );
    }

    #[test]
    fn sort_cycle_refilters_in_the_new_order_during_search() {
        // With a list search active, SortCycle rebuilds the full list AND the
        // filtered view, keeping the selection by uid. The active list reorders.
        let mut a = app();
        a.mode = Mode::Search(crate::action::SearchState {
            target: SearchTarget::List,
            query: "pos".into(),
            origin_uid: None,
            match_idx: 0,
        });
        a.refilter();
        let before: Vec<String> = a
            .active_list()
            .models
            .iter()
            .map(|m| m.unique_id.clone())
            .collect();
        assert!(before.len() > 1, "search matched several models");
        let sel = a.selected_unique_id();
        apply_action(&mut a, Action::SortCycle);
        // Still filtered (search mode preserved) and selection survives by uid.
        assert!(a.filter.is_some(), "filter rebuilt, search still active");
        assert_eq!(a.selected_unique_id(), sel, "selection unchanged by uid");
        let after: Vec<String> = a
            .active_list()
            .models
            .iter()
            .map(|m| m.unique_id.clone())
            .collect();
        // Same membership (a multiset), regardless of order.
        let mut b = before.clone();
        let mut af = after.clone();
        b.sort();
        af.sort();
        assert_eq!(b, af, "filtered membership is unchanged by the sort");
    }

    // ---- command palette round-trips ----

    /// Open the palette, type `query`, then return the resulting `App` so a test
    /// can confirm the filter/selection state before Enter.
    fn open_palette_and_type(query: &str) -> App {
        let mut a = app();
        apply_action(&mut a, Action::PaletteOpen);
        assert!(matches!(a.mode, Mode::Palette(_)), "PaletteOpen → Palette");
        for c in query.chars() {
            apply_action(&mut a, Action::SearchType(c));
        }
        a
    }

    #[test]
    fn palette_open_type_enter_runs_the_resolved_action() {
        // Filter to the minimap toggle and run it: the minimap pref flips and the
        // mode returns to Selection (the recursively-applied action's effect lands).
        let mut a = open_palette_and_type("minimap");
        let before = a.ui_state.minimap_visible();
        let out = apply_action(&mut a, Action::SearchConfirm);
        assert!(!out.quit && out.effects.is_empty());
        assert_eq!(a.mode, Mode::Selection, "palette closes on confirm");
        assert_ne!(
            a.ui_state.minimap_visible(),
            before,
            "the resolved ToggleMinimap action ran"
        );
    }

    #[test]
    fn palette_confirm_propagates_quit_outcome() {
        // Choosing the quit command must quit (the whole Outcome propagates).
        let mut a = open_palette_and_type("quit");
        // "quit" is the only command whose help contains that subsequence.
        assert!(
            palette_candidates("quit")
                .iter()
                .any(|b| b.action == Action::Quit),
            "the quit command is a candidate"
        );
        let out = apply_action(&mut a, Action::SearchConfirm);
        assert!(out.quit, "confirming the quit command quits");
        assert_eq!(a.mode, Mode::Selection, "mode set to Selection before apply");
    }

    #[test]
    fn palette_confirm_propagates_editor_effect() {
        // Choosing "open SQL in $EDITOR" must yield the editor Effect. A model
        // with a resolvable file path is selected first so the effect is produced.
        let mut a = app();
        a.select_by_unique_id("model.jaffle_finance.pos_txn");
        apply_action(&mut a, Action::PaletteOpen);
        for c in "$EDITOR".chars() {
            apply_action(&mut a, Action::SearchType(c));
        }
        // The OpenEditor row's help is "open SQL in $EDITOR".
        assert!(
            palette_candidates("$EDITOR")
                .iter()
                .any(|b| b.action == Action::OpenEditor),
            "the editor command is a candidate"
        );
        let out = apply_action(&mut a, Action::SearchConfirm);
        assert!(
            matches!(out.effects.as_slice(), [Effect::OpenEditor(_)]),
            "confirming the editor command emits its Effect: {:?}",
            out.effects
        );
        assert_eq!(a.mode, Mode::Selection);
    }

    #[test]
    fn palette_selected_clamps_when_the_filter_shrinks() {
        // Move the cursor down on the full list, then narrow the query so fewer
        // candidates remain: `selected` must clamp into range (never out of bounds).
        let mut a = open_palette_and_type("");
        // Step down a few times over the full candidate list.
        for _ in 0..5 {
            apply_action(&mut a, Action::SearchMove(Direction::Down));
        }
        let moved = match &a.mode {
            Mode::Palette(p) => p.selected,
            _ => unreachable!(),
        };
        assert!(moved > 0, "the palette cursor moved down");
        // Now type a narrowing query; `selected` resets to 0 on each keystroke and
        // can never exceed the (smaller) candidate count.
        for c in "lens".chars() {
            apply_action(&mut a, Action::SearchType(c));
        }
        if let Mode::Palette(p) = &a.mode {
            let count = palette_candidates(&p.query).len();
            assert!(count > 0, "'lens' still has candidates");
            assert!(p.selected < count, "selected stays in range after shrink");
        } else {
            unreachable!();
        }
    }

    #[test]
    fn palette_cancel_returns_to_selection_without_touching_the_filter() {
        // Esc closes the palette and leaves the list filter untouched (the palette
        // shares the SearchCancel action but never drives the list filter).
        let mut a = open_palette_and_type("lens");
        assert!(a.filter.is_none(), "the palette never builds a list filter");
        let out = apply_action(&mut a, Action::SearchCancel);
        assert!(!out.quit && out.effects.is_empty());
        assert_eq!(a.mode, Mode::Selection, "Esc closes the palette");
        assert!(a.filter.is_none(), "filter still untouched after cancel");
    }

    #[test]
    fn palette_backspace_resets_selected_to_top() {
        // Backspace resets `selected` to 0 EXACTLY like typing, so the highlighted
        // row is a pure function of the query (no arrival-path dependence). Force a
        // non-zero selection, backspace, and assert it snapped back to the top.
        let mut a = open_palette_and_type("lens");
        if let Mode::Palette(p) = &mut a.mode {
            p.selected = palette_candidates(&p.query).len() - 1;
            assert!(p.selected > 0, "fixture has >1 'lens' candidate to move off 0");
        }
        apply_action(&mut a, Action::SearchBackspace); // "len"
        if let Mode::Palette(p) = &a.mode {
            assert_eq!(p.selected, 0, "backspace resets the highlight to the top");
        } else {
            unreachable!();
        }
    }
}
