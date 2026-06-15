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
//!
//! Module layout (one owner, several concerns — every submodule is an
//! `impl App` extension, so the privacy boundary stays the `app` module):
//! - [`mod.rs`](self): the `App` struct + core state management (selection,
//!   lists, filters, search, reload, history, data source).
//! - `lineage`: the lineage subgraph, cursor, path set, and breadcrumb.
//! - `styles`: per-node render attributes (materialization class + lens tints).
//! - `analysis`: graph analytics (impact, coverage, stats dashboard, layer
//!   violations, critical path).
//! - `export`: the yank/export text producers (Mermaid / DOT / ASCII / report).
//! - `reducer`: [`apply_action`] — the `Action` → state transition table.
//! - `cache`: keyed memoization of the per-frame lineage pipeline (subgraph →
//!   layout → styles) and impact counts; pure-function results only, so
//!   determinism is unaffected.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;

use crate::action::{Mode, SearchTarget};
use crate::effect::Effect;
use crate::ui::theme::{self, Theme};
use crate::{
    build_filtered_model_list, build_model_list, compute_diff, coverage_gap, load_dag,
    load_dag_from_source, Dag, DagDiff, ModelList, NodeInfo, SortMode, UiState,
};

mod analysis;
mod cache;
mod export;
mod lineage;
mod reducer;
mod styles;
#[cfg(test)]
mod tests;

pub use analysis::layer_violation_edges;
pub(crate) use lineage::fit_breadcrumb;
pub use reducer::apply_action;

use analysis::compute_stats;
use cache::LineageCaches;

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
    pub exposures: usize,
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
    /// Monotonic identity of the style-relevant graph state: bumped by every
    /// [`reload`](App::reload) (the only place the `Dag` is replaced) AND by
    /// [`set_diff_base`](App::set_diff_base) (the diff is a style input via the
    /// Diff lens), so the [`cache`] keys can name "the current graph + diff"
    /// without hashing either.
    generation: u64,
    /// Keyed memoization of the per-frame lineage pipeline (subgraph → layout →
    /// styles) and the impact closures. Pure-function results only; invalidated
    /// by key comparison, never explicitly — see [`cache`].
    caches: LineageCaches,
    /// The `--diff` BASELINE Dag (another manifest / checkout), kept so every
    /// [`reload`](App::reload) can re-diff the rebuilt current Dag against it.
    /// `None` = no baseline (the Diff lens is skipped, `D` toasts a hint).
    diff_base: Option<Dag>,
    /// Where the baseline came from (the `--diff` argument), for titles/toasts.
    diff_label: String,
    /// The computed baseline↔current diff. Derived state: recomputed by
    /// [`set_diff_base`](App::set_diff_base) and every `reload` — never edited.
    diff: Option<DagDiff>,
    /// The loaded colour themes, in Ctrl-t cycle order: the built-in presets by
    /// default; `main` replaces the list (presets + user theme files) and the
    /// start index via [`set_themes`](App::set_themes) (`--theme`). Never empty.
    themes: Vec<(String, Theme)>,
    /// Index of the ACTIVE theme in `themes`; the loop hands
    /// [`active_theme`](App::active_theme) to `RenderCtx` each frame, so a
    /// cycle repaints without touching any cache (the cached layout's attrs
    /// are semantic — colours resolve at draw time).
    theme_index: usize,
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
            generation: 0,
            diff_base: None,
            diff_label: String::new(),
            diff: None,
            caches: LineageCaches::default(),
            themes: theme::presets()
                .iter()
                .map(|(n, t)| (n.to_string(), *t))
                .collect(),
            theme_index: 0,
        }
    }

    /// Load a `--diff` baseline: store it (with its display label), compute the
    /// diff against the current Dag, and bump the cache generation — the diff is
    /// a style input (the Diff lens reads it), so any cached styled layout from
    /// before the baseline landed must miss. Recomputed on every reload; the
    /// baseline itself never changes after startup.
    pub fn set_diff_base(&mut self, base: Dag, label: String) {
        self.diff = Some(compute_diff(&base, &self.dag));
        self.diff_base = Some(base);
        self.diff_label = label;
        self.generation += 1;
    }

    /// The computed baseline↔current diff (`None` without a `--diff` baseline).
    pub fn diff(&self) -> Option<&DagDiff> {
        self.diff.as_ref()
    }

    /// The `--diff` baseline's display label (empty without a baseline).
    pub fn diff_label(&self) -> &str {
        &self.diff_label
    }

    /// The status-bar diff chip: `"diff +A ~M -R"` (added / modified / removed
    /// node counts vs the baseline), or `"diff clean"` when the Dags match.
    /// `None` without a baseline. ASCII by construction.
    pub fn diff_status_label(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        if diff.is_empty() {
            return Some("diff clean".to_string());
        }
        let (a, m, r) = diff.counts();
        Some(format!("diff +{a} ~{m} -{r}"))
    }

    /// The active colour theme (what the loop hands to `RenderCtx`).
    pub fn active_theme(&self) -> &Theme {
        &self.themes[self.theme_index].1
    }

    /// The active theme's name (the toast / status text).
    pub fn theme_name(&self) -> &str {
        &self.themes[self.theme_index].0
    }

    /// Replace the loaded theme list and the active index (the `--theme` CLI
    /// seam). An empty list is ignored and an out-of-range index clamps — the
    /// App must always have an active theme.
    pub fn set_themes(&mut self, themes: Vec<(String, Theme)>, index: usize) {
        if !themes.is_empty() {
            self.themes = themes;
        }
        self.theme_index = index.min(self.themes.len().saturating_sub(1));
    }

    /// Step the active theme to the next loaded one (wrapping) and record the
    /// landing name as the toast notice.
    pub fn cycle_theme(&mut self) {
        self.theme_index = (self.theme_index + 1) % self.themes.len();
        let name = self.themes[self.theme_index].0.clone();
        self.set_notice(format!("theme: {name}"));
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

    /// A node's display name, falling back to the `unique_id` itself when the node
    /// is unknown to the Dag — the repeated overlay-title idiom.
    pub fn node_name_or_uid(&self, uid: &str) -> String {
        self.dag
            .get(uid)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| uid.to_string())
    }

    /// The selected model's SQL file path, resolved against the project root
    /// (`<root>/<original_file_path>`). `None` if nothing is selected, the node
    /// has no recorded file path, or the recorded path would escape the
    /// project root.
    pub fn selected_file_path(&self) -> Option<String> {
        let uid = self.selected_node()?.unique_id.clone();
        let ofp = self.dag.detail(&uid)?.original_file_path.clone()?;
        // `original_file_path` comes from an untrusted manifest.json: a crafted
        // value (absolute, or with a `..` component) would point the `o`
        // editor jump at an arbitrary file outside the project. Lexical check
        // on purpose — `canonicalize` fails on not-yet-existing files, and a
        // plain relative path must keep resolving exactly as before.
        let rel = Path::new(&ofp);
        if rel
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return None;
        }
        Some(self.project_root.join(rel).to_string_lossy().into_owned())
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

    /// Whether the data was inferred from project source (no compiled manifest)
    /// — the approximate-lineage mode the startup notice recommends upgrading
    /// from via `P` (`dbt parse`).
    pub fn is_source_mode(&self) -> bool {
        matches!(self.source, DataSource::Project(_))
    }

    /// The dbt project root (where `dbt_project.yml` is expected): the project
    /// dir in source mode, or the manifest's `target/..` parent in manifest
    /// mode. The `dbt parse` effect runs here.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Switch the data source to a compiled manifest at `path` and reload from
    /// it. On a load error the previous source (and all data, including the
    /// project root) is restored, so a truncated/corrupt manifest never strands
    /// the app — same never-corrupt contract as [`reload`](App::reload). On
    /// success the project root is re-derived from the manifest path, keeping
    /// the `$EDITOR` jump correct even for a manifest under a different root.
    pub fn adopt_manifest(&mut self, path: PathBuf) -> Result<()> {
        let new_root = derive_project_root(&path);
        let prev = std::mem::replace(&mut self.source, DataSource::Manifest(path));
        if let Err(err) = self.reload() {
            self.source = prev;
            return Err(err);
        }
        self.project_root = new_root;
        Ok(())
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
        // New graph identity: every cache key embedding the generation misses
        // from here on, so no pre-reload subgraph/layout/impact can survive.
        self.generation += 1;
        // Drop bookmarks whose model vanished; surviving ids persist (ids stable).
        self.bookmarks.retain(|id| self.dag.get(id).is_some());
        // Same prune for the re-root history: a stale id would make
        // history_back/forward pop the entry, push the current node onto the
        // opposite stack, and then fail to select — consuming the step and
        // accumulating bogus duplicates instead of landing on a survivor.
        self.back.retain(|id| self.dag.get(id).is_some());
        self.forward.retain(|id| self.dag.get(id).is_some());
        // Pruning can leave adjacent equal entries (A → vanished → A);
        // collapse them so a history step never "moves" to the same node.
        self.back.dedup();
        self.forward.dedup();
        self.filter = None;
        self.mode = Mode::Selection;
        // Cursor home: selection is restored BY ID, so the loop's
        // selection-change chokepoint won't fire — reset here or a pre-reload
        // cursor would survive into the rebuilt graph.
        self.lineage_cursor = None;
        // Re-diff the rebuilt Dag against the (unchanged) --diff baseline, so
        // the Diff lens / chip / modal always describe the CURRENT graph.
        self.diff = self.diff_base.as_ref().map(|b| compute_diff(b, &self.dag));
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

/// Derive the dbt project root from the manifest path: `<root>/target/manifest.json`
/// → `<root>`. Falls back to the current directory if there aren't two parents.
pub fn derive_project_root(manifest: &Path) -> PathBuf {
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}
