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

use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;

use crate::action::{
    menu_slot_label, menu_slot_mark, MenuAnchor, MenuEntry, MenuSlot, MenuState, Mode, NoticeLevel,
    NotificationEntry, NotificationsView, SearchTarget, NODE_MENU_LAYOUT,
};
use crate::effect::Effect;
use crate::ui::theme::{self, Theme};
use crate::{
    build_filtered_model_list, build_model_list, build_model_list_grouped, compute_diff_with,
    coverage_gap, default_catalog_path, default_run_results_path,
    load_dag_from_source_with_warnings, load_dag_with_warnings, ColumnRef, Dag, DagDiff, ModelList,
    NodeDetail, NodeInfo, RunResults, SortMode, SqlDialect, SqlParseResolver, UiState,
};

mod analysis;
mod cache;
mod column;
mod export;
mod lineage;
// `pub(crate)`: the persisted-state layer reads its cap constants; the
// palette surface itself reaches `App` through the `impl App` methods.
pub(crate) mod palette;
mod reducer;
mod styles;
#[cfg(test)]
mod tests;

// The single violation predicate lives in `dbtl_core::impact` (shared by the
// lens, the stats worklist, the PR impact pack, and `dbtl check`); re-export
// it here so `dbtl::app::layer_violation_edges` keeps resolving.
pub use crate::layer_violation_edges;
// The subgraph→fenced-Mermaid core lives in `dbtl_core::mermaid` so the static
// docs generator (`dbtl-docs`) shares the identical id-assignment/escaping
// without depending on this crate.
pub(crate) use lineage::fit_breadcrumb;
pub use reducer::apply_action;

use analysis::compute_stats;
use cache::LineageCaches;
use column::ColumnFocus;

/// What the event loop should do after an action: keep running (and run any
/// recorded effects) or quit. Effects are data; the loop performs them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub quit: bool,
    pub effects: Vec<Effect>,
    /// "The applied action reshapes or recentres the lineage" — the event loop
    /// ORs this into its anchor decision (its own dispatched-key check covers
    /// the direct path; the flag exists for actions applied INDIRECTLY — a
    /// palette-confirmed command the loop only ever sees as `SearchConfirm`).
    /// Set from [`crate::action::reanchors_lineage`], the single list.
    pub reanchor: bool,
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
            ..Default::default()
        }
    }
    /// Keep running and request one effect.
    pub fn effect(e: Effect) -> Self {
        Outcome {
            effects: vec![e],
            ..Default::default()
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
    /// How the left pane orders models within each group (the `.`-cycle).
    /// Preserved across reload; the full + filtered lists are rebuilt with it.
    pub sort: SortMode,
    /// `config.yml`'s `group_by_meta`: `Some(key)` groups the model list by
    /// that `meta` key's VALUE instead of the layer dirs — replacing the
    /// layer grouping outright (`key=value` groups, `(no <key>)` last). `sort`
    /// still orders within each group; every list rebuild reads this.
    pub group_by_meta: Option<String>,
    /// The `meta` key the PR-impact pack collects as "affected owners"
    /// (`config.yml` `owner_meta_key`, default `owner`). Feeds the `D` modal
    /// snapshot + `e` export through [`crate::diff_report`].
    pub owner_meta_key: String,
    /// `config.yml`'s `lineage_collapse`: the sibling-group size over which a
    /// lineage column contracts the tail into one `+M more` marker node
    /// (`0` = never collapse). Display-only — the rooted subgraph, the
    /// Mermaid/DOT exports, and lineage search keep every real node; only the
    /// pane (and the screen-equivalent ASCII export) sees the markers.
    pub collapse_threshold: usize,
    /// The sibling-collapse groups the user expanded, keyed to the CURRENT
    /// root — the issue's "kept per root, initialised on re-root". A plain
    /// `BTreeSet` of group keys (hub parent uids), cleared by every path that
    /// lands on a different selected model:
    /// [`select_by_unique_id`](App::select_by_unique_id)'s change check, the
    /// event loop's selection-change chokepoint, and
    /// [`reload`](App::reload) (a new graph re-initialises everything). Never
    /// read for a stale root — it is empty by then.
    collapse_expanded: BTreeSet<String>,
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
    /// The transient-notice channel: a one-shot (level, message) describing the
    /// latest user-visible outcome (copied / bookmarked / exported / reloaded).
    /// Writers: the reducer arms (intent) and `main::run_effect` (which
    /// OVERWRITES the intent on effect failure — it runs after `apply_action`
    /// in the same loop iteration, so the failure always wins). The event loop
    /// drains it via [`take_notice_leveled`](App::take_notice_leveled), stamps
    /// the wall-clock, and owns the on-screen toast lifetime — `App` stores no
    /// clock, so the reducer stays pure.
    notice: Option<(NoticeLevel, String)>,
    /// The notification log behind `Ctrl-n` (Issue 08): stamped entries,
    /// newest LAST, capped at [`NOTIFICATION_CAP`](App::NOTIFICATION_CAP).
    /// Written by the event loop (every drained notice) and the CLI's
    /// `build_app` (startup load warnings) — never by the reducer, so
    /// timestamps always come from the outside. Memory-only: never persisted.
    notifications: VecDeque<NotificationEntry>,
    /// Read watermark into `notifications`: entries before this index (the
    /// oldest end) are read; everything at or after is unread. Set to
    /// `notifications.len()` by
    /// [`mark_notifications_read`](App::mark_notifications_read) (opening the
    /// centre is the read gesture) and shifted down when a front evicts, so
    /// the `!N` chip always counts warnings still in the log — a raw counter
    /// could not, since evicting an unread entry must decrement it while
    /// evicting a read one must not underflow it.
    unread_from: usize,
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
    /// The baseline side's CLL resolver — what the diff's column analysis
    /// reads the baseline's columns/provenance/expressions through (its own
    /// adapter dialect, or the pinned `--sql-dialect`; no catalog — the
    /// baseline's warehouse record isn't loaded). Built once in
    /// [`set_diff_base`](App::set_diff_base) since the baseline never
    /// changes: its per-uid memos stay valid across every `reload`.
    diff_cll: Option<SqlParseResolver>,
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
    /// The last dbt run's outcomes (`run_results.json`), keyed by `unique_id`
    /// — the RunStatus/RunTime lenses' and the runtime-weighted critical
    /// path's data source. `None` = no results found (those lenses are
    /// skipped, like Diff without a baseline). Reloaded alongside the Dag.
    run_results: Option<RunResults>,
    /// Where `run_results` was loaded from (explicit `--run-results` or the
    /// auto-detected sibling), so [`reload`](App::reload) re-reads the same
    /// file — `--watch` picks up a fresh `dbt run`'s results for free.
    run_results_path: Option<PathBuf>,
    /// The `dbt docs generate` warehouse record (`catalog.json`), keyed by
    /// `unique_id` — real column types, row counts, byte sizes, and the
    /// schema.yml↔warehouse drift check for the detail modal. `Rc`-shared
    /// with the CLL resolver (its top-priority column source) so both read
    /// the ONE loaded copy.
    catalog: Option<Rc<crate::Catalog>>,
    /// Where `catalog` was auto-detected — `reload` re-reads it.
    catalog_path: Option<PathBuf>,
    /// The column-level-lineage resolver (Issue 08's `sqlparser`-backed
    /// resolver, falling back to Issue 07's name-match approximation per
    /// model). Owns its own per-uid memos, so it must be rebuilt whenever the
    /// Dag/catalog can change — `reload` does that. The `ColumnResolver`
    /// trait is the consumer-facing boundary, so callers never see which
    /// resolver ran.
    pub(super) cll: SqlParseResolver,
    /// An explicit `--sql-dialect` override; `None` = auto from the Dag's
    /// adapter_type (recomputed on `reload` in case the manifest changed).
    sql_dialect: Option<SqlDialect>,
    /// The project-local lint configuration (`.dbtl.yml`'s `lint:` section;
    /// [`crate::LintConfig::default`] when absent). Every surface reads the
    /// SAME computed findings — the `Lint` lens, the stats worklist, and
    /// `dbtl check --lint` can never disagree.
    lint_config: crate::LintConfig,
    /// The static-lint findings under `lint_config`, computed once per graph
    /// (in `with_source` / `reload` / [`set_lint_config`](App::set_lint_config))
    /// by the shared [`crate::run_rules`] — a style input, so changes ride the
    /// `generation` cache key.
    lint_findings: Vec<crate::Finding>,
    /// The `.dbtl.yml` in effect (auto-detected beside the data source like
    /// `run_results.json` is), so [`reload`](App::reload) re-reads the same
    /// file — a config edit shows up without a restart. `None` when no
    /// `.dbtl.yml` exists (defaults still lint).
    lint_config_path: Option<PathBuf>,
    /// The column target `y` stashed while opening the yank menu over the
    /// structure modal — what `yl` copies. Always rewritten on
    /// `Action::YankPrefix` (the menu's entry point) and cleared the moment
    /// the menu closes, so it can never be consumed stale.
    pub(super) pending_col: Option<ColumnRef>,
    /// The structure modal's `model_id`, stashed the same way — what every
    /// OTHER `y`-menu yank (`yy`/`yY`/`yC`/`ym`/…) targets instead of the
    /// list selection. Without it a yank dispatched over the modal would
    /// silently copy the *selected* node's data, not the modal's.
    pub(super) pending_node: Option<String>,
    /// The `p`-anchored path start (`unique_id`): while set, the lineage path
    /// highlight runs anchor↔cursor instead of root↔cursor — the two-node
    /// "why does X reach Y" answer. `None` = normal root↔cursor pathing.
    pub(super) path_anchor: Option<String>,
    /// The traced-column state (Issue 09's column focus — see [`column`]).
    /// `Some` filters the lineage subgraph to the column's related models and
    /// draws the related columns inside each box; `None` = the normal rooted
    /// view. Every reader goes through [`active_column_focus`]
    /// (App::active_column_focus), which suppresses it while the overview is
    /// up. Cleared by `Esc` (`ColumnFocusExit`), by [`jump_to`](App::jump_to)
    /// (a re-root means "leave the trace"), and by `reload` (the rebuilt Dag
    /// may not carry the column).
    pub(super) column_focus: Option<ColumnFocus>,
    /// Confirmed list-search queries, most-recent-first, deduped, capped at
    /// [`palette::SEARCH_HISTORY_MAX`] — what the `/`-mode Up/Down recall
    /// browses. Persisted as `search_history` (recorded by
    /// [`record_search_query`](App::record_search_query) on `SearchConfirm`).
    pub search_history: Vec<String>,
    /// Recently-run palette commands as `Action` Debug names,
    /// most-recent-first, capped at [`palette::PALETTE_RECENT_MAX`] — the
    /// Recent section's source. Persisted as `palette_recent`; names whose
    /// action is gone from `BINDINGS` drop on apply and on render (see
    /// [`palette_binding_named`](crate::action::palette_binding_named)).
    pub palette_recent: Vec<String>,
    /// The package scope the Dag was loaded under (`--include-packages` ⇒
    /// `All`): [`reload`](App::reload) re-applies it to the freshly loaded
    /// graph, the status bar prints its tag, and the diff/impact report
    /// carries it onto the page. The graph itself is already scoped when it
    /// arrives (the load path calls [`PackageScope::apply`] before `App`
    /// sees it), so this field is a *record* of the loaded scope + the
    /// reload-time re-filter — the in-scope test itself always goes through
    /// [`Dag::is_root_project`], never this flag.
    scope: crate::PackageScope,
}

/// Where the `Dag` was loaded from, so `reload` re-reads the same source.
enum DataSource {
    /// A compiled `manifest.json` at this path.
    Manifest(PathBuf),
    /// A dbt project directory parsed from source (no `dbt compile`).
    Project(PathBuf),
}

impl DataSource {
    /// The run_results.json dbt would have written next to this source
    /// (the auto-detect target [`crate::default_run_results_path`] encodes).
    fn default_run_results_path(&self) -> Option<PathBuf> {
        match self {
            DataSource::Manifest(m) => default_run_results_path(Some(m), None),
            DataSource::Project(d) => default_run_results_path(None, Some(d)),
        }
    }

    /// The catalog.json `dbt docs generate` would write next to this source
    /// (the auto-detect target [`crate::default_catalog_path`] encodes).
    fn default_catalog_path(&self) -> Option<PathBuf> {
        match self {
            DataSource::Manifest(m) => default_catalog_path(Some(m), None),
            DataSource::Project(d) => default_catalog_path(None, Some(d)),
        }
    }

    /// The `.dbtl.yml` lint config for this source (the auto-detect target
    /// [`crate::lint_config_path`] encodes): the project dir in source mode,
    /// the project root (parent of `target/`) in manifest mode.
    fn lint_config_path(&self) -> Option<PathBuf> {
        match self {
            DataSource::Manifest(m) => crate::lint_config_path(Some(m), None),
            DataSource::Project(d) => crate::lint_config_path(None, Some(d)),
        }
    }
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
        // Auto-detect the sibling run_results.json (target/run_results.json
        // next to the manifest, or under the project dir in source mode).
        // Missing/unreadable → simply no run data; an explicit --run-results
        // failure IS an error and is reported by the caller.
        let run_results_path = source.default_run_results_path();
        let run_results = run_results_path
            .as_ref()
            .filter(|p| p.is_file())
            .and_then(|p| RunResults::load(p).ok());
        // Same auto-detect for target/catalog.json — warehouse truth for the
        // detail modal (real types, row counts, schema.yml drift) AND the
        // CLL resolver's top-priority column list (the Rc shares one copy).
        let catalog_path = source.default_catalog_path();
        let catalog = catalog_path
            .as_ref()
            .filter(|p| p.is_file())
            .and_then(|p| crate::Catalog::load(p).ok())
            .map(Rc::new);
        let cll = SqlParseResolver::for_dag(catalog.clone(), &dag);
        // Same auto-detect for `.dbtl.yml`: the project-local lint config.
        // A malformed file is a warning + shipped defaults (the TUI's policy —
        // `check`/`docs` make the same error fatal; the screen never blocks on
        // it). The notice is a one-shot toast, like the other load warnings.
        let lint_config_path = source.lint_config_path();
        let (lint_config, lint_notice) = match &lint_config_path {
            Some(path) => match crate::LintConfig::load(path) {
                Ok(c) => (c, None),
                Err(e) => (
                    crate::LintConfig::default(),
                    Some(format!("warning: ignoring .dbtl.yml: {e}")),
                ),
            },
            None => (crate::LintConfig::default(), None),
        };
        let lint_findings = crate::run_rules(&dag, &lint_config);
        App {
            dag,
            model_list,
            filter: None,
            list_filter: ListFilter::default(),
            filtered_base: None,
            sort,
            group_by_meta: None,
            owner_meta_key: "owner".to_string(),
            collapse_threshold: crate::collapse::DEFAULT_COLLAPSE_THRESHOLD,
            collapse_expanded: BTreeSet::new(),
            bookmarks: BTreeSet::new(),
            ui_state,
            mode: Mode::Selection,
            stats,
            lineage_view: LineageView::default(),
            glyph_mode: crate::GlyphMode::default(),
            lineage_cursor: None,
            // A malformed `.dbtl.yml` surfaces as a startup toast (one-shot
            // WARNING notice) — the same channel main.rs uses for the
            // source-mode hint (an info append joins it without dropping the
            // warning level).
            notice: lint_notice.map(|t| (NoticeLevel::Warning, t)),
            notifications: VecDeque::new(),
            unread_from: 0,
            back: Vec::new(),
            forward: Vec::new(),
            source,
            project_root,
            generation: 0,
            diff_base: None,
            diff_cll: None,
            diff_label: String::new(),
            diff: None,
            caches: LineageCaches::default(),
            themes: theme::presets()
                .iter()
                .map(|(n, t)| (n.to_string(), *t))
                .collect(),
            theme_index: 0,
            run_results,
            run_results_path,
            catalog,
            catalog_path,
            cll,
            sql_dialect: None,
            lint_config,
            lint_findings,
            lint_config_path,
            pending_col: None,
            pending_node: None,
            path_anchor: None,
            column_focus: None,
            search_history: Vec::new(),
            palette_recent: Vec::new(),
            // CONTRACT: every App starts recorded as `Root` — the scope is a
            // record of how the Dag was LOADED, and App can never recover it
            // from the already-pruned graph. The load path (`cli::build_app`)
            // MUST call `set_scope` with the real CLI scope right after
            // construction, or `reload` will silently re-scope to Root.
            scope: crate::PackageScope::default(),
        }
    }

    /// The static-lint findings under the active [`LintConfig`](crate::LintConfig)
    /// — the shared [`crate::run_rules`] output every surface reads (Lint lens,
    /// stats worklist). `dbtl check --lint` gates on the same vector.
    pub fn lint_findings(&self) -> &[crate::Finding] {
        &self.lint_findings
    }

    /// The active lint configuration (`.dbtl.yml` or the shipped defaults).
    pub fn lint_config(&self) -> &crate::LintConfig {
        &self.lint_config
    }

    /// Replace the lint configuration and recompute findings (test/CLI seam —
    /// production gets its config from the auto-detected `.dbtl.yml`). Findings
    /// are a style input, so the generation bumps like a reload.
    pub fn set_lint_config(&mut self, config: crate::LintConfig) {
        self.lint_config = config;
        self.lint_findings = crate::run_rules(&self.dag, &self.lint_config);
        self.generation += 1;
    }

    /// The loaded `run_results.json` side-map, if any (auto-detected sibling or
    /// an explicit `--run-results`). The RunStatus/RunTime lenses and the
    /// runtime-weighted critical path read it; `None` skips them all.
    pub fn run_results(&self) -> Option<&RunResults> {
        self.run_results.as_ref()
    }

    /// Point the app at a specific `run_results.json` (`--run-results`). A load
    /// failure is the caller's error to report — the App only ever stores a
    /// successfully parsed file. Re-read by [`reload`](App::reload).
    pub fn set_run_results(&mut self, path: PathBuf) -> Result<()> {
        self.run_results = Some(RunResults::load(&path)?);
        self.run_results_path = Some(path);
        Ok(())
    }

    /// The loaded `catalog.json` side-map, if any — the detail modal reads it
    /// for real column types, row counts, byte sizes, and doc↔warehouse drift.
    pub fn catalog(&self) -> Option<&crate::Catalog> {
        self.catalog.as_deref()
    }

    /// The detail-modal catalog block for a node: `catalog:` stats line (rows,
    /// size), a per-column real type when the manifest only documents a name,
    /// and a `drift:` line when documented/actual column sets disagree. Empty
    /// when the catalog doesn't cover this uid.
    pub fn catalog_note(&self, uid: &str, detail: &NodeDetail) -> Vec<String> {
        let Some(node) = self.catalog.as_ref().and_then(|c| c.get(uid)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stats = Vec::new();
        if let Some(rows) = node.row_count() {
            stats.push(format!("{rows} rows"));
        }
        if let Some(bytes) = node.size_bytes() {
            stats.push(format!("{bytes} bytes"));
        }
        if !stats.is_empty() {
            out.push(format!("catalog: {}", stats.join(", ")));
        }
        // Real warehouse type per documented column (the manifest's data_type
        // is usually None — schema.yml rarely declares it).
        for col in &detail.columns {
            if let Some(ty) = node.column_type(&col.name) {
                if col.data_type.as_deref() != Some(ty) {
                    out.push(format!("  {}: {}", col.name, ty));
                }
            }
        }
        let (missing_in_wh, missing_in_docs) = node.drift(
            &detail
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
        );
        if !missing_in_wh.is_empty() || !missing_in_docs.is_empty() {
            out.push(format!(
                "drift:   {} documented-but-missing, {} undocumented",
                missing_in_wh.len(),
                missing_in_docs.len()
            ));
        }
        out
    }

    /// The status-bar readout for the loaded run results:
    /// `run 12/34 @ 2024-01-02` — covered/dag-nodes plus the artifact's date —
    /// so a stale or partial run is visible at a glance. `None` when unloaded.
    pub fn run_status_label(&self) -> Option<String> {
        let rr = self.run_results.as_ref()?;
        let covered = rr.covered_in(&self.dag);
        let total = self.dag.nodes().len();
        let date = rr
            .generated_at
            .as_deref()
            .map(|ts| ts.get(..10).unwrap_or(ts)) // ISO date part only
            .unwrap_or("?");
        Some(format!("run {covered}/{total} @{date}"))
    }

    /// Load a `--diff` baseline: store it (with its display label AND the
    /// baseline-side CLL resolver the column analysis reads through — one
    /// dialect shared with the current side per
    /// [`SqlDialect::for_diff_baseline`], or the pinned `--sql-dialect`),
    /// compute the diff against the current Dag, and bump the cache
    /// generation — the diff is a style input (the Diff lens reads it), so
    /// any cached styled layout from before the baseline landed must miss.
    /// Recomputed on every reload; the baseline itself never changes after
    /// startup.
    pub fn set_diff_base(&mut self, base: Dag, label: String) {
        let dialect = self
            .sql_dialect
            .unwrap_or_else(|| SqlDialect::for_diff_baseline(&self.dag, &base));
        let base_res = SqlParseResolver::new(None, dialect);
        self.diff = Some(compute_diff_with(&base, &self.dag, &base_res, &self.cll));
        self.diff_cll = Some(base_res);
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

    /// Set the `meta` key the model list groups by (`config.yml`
    /// `group_by_meta`; `None` = the default layer grouping) and rebuild the
    /// list like a sort change — selection re-resolves by `unique_id`. An
    /// empty or whitespace-only key is `None` — it would otherwise group
    /// every model under a literal `(no  )` bucket.
    pub fn set_group_by_meta(&mut self, key: Option<String>) {
        let key = key.filter(|k| !k.trim().is_empty());
        if self.group_by_meta == key {
            return;
        }
        self.group_by_meta = key;
        let current = self.selected_unique_id();
        self.model_list =
            build_model_list_grouped(&self.dag, self.sort, self.group_by_meta.as_deref());
        self.apply_list_filter();
        if let Some(uid) = &current {
            self.select_by_unique_id(uid);
        }
    }

    /// Set the `meta` key the impact reports read as "affected owners"
    /// (`config.yml` `owner_meta_key`). A plain assignment — it feeds the
    /// next `diff_report` snapshot, not any cached list.
    pub fn set_owner_meta_key(&mut self, key: impl Into<String>) {
        self.owner_meta_key = key.into();
    }

    /// Set the sibling-collapse threshold (`config.yml` `lineage_collapse`;
    /// `0` disables the `+M more` markers entirely). The threshold is a cache
    /// key input, so no explicit invalidation is needed — the next layout
    /// read simply misses and rebuilds.
    pub fn set_collapse_threshold(&mut self, threshold: usize) {
        self.collapse_threshold = threshold;
    }

    /// Drop every expanded sibling-collapse group — the re-root
    /// re-initialisation. Called at the event loop's selection-change
    /// chokepoint (which also re-homes the cursor) and internally wherever a
    /// selection-identity change is detected; `reload` calls it on the new
    /// graph's behalf.
    pub fn clear_collapse_expansions(&mut self) {
        self.collapse_expanded.clear();
    }

    /// The package scope the Dag was loaded under (`--include-packages` ⇒
    /// [`PackageScope::All`]). [`reload`](App::reload) re-applies it to the
    /// re-loaded graph and the status bar prints its `scope:` tag.
    pub fn scope(&self) -> crate::PackageScope {
        self.scope
    }

    /// Record the package scope the load path already applied to `dag`
    /// (`build_app` calls this once, right after `App::new`/`from_source`):
    /// `reload` re-scopes the freshly parsed graph with it and the status
    /// bar shows its tag. It does NOT re-filter the live Dag — the loaded
    /// graph is already scoped by then.
    pub fn set_scope(&mut self, scope: crate::PackageScope) {
        self.scope = scope;
    }

    /// Pin the column-lineage SQL dialect (`--sql-dialect`); rebuilds the
    /// resolver on the spot — and the baseline side's, then the diff, since
    /// the pinned dialect applies to both parse surfaces. `reload` re-derives
    /// the auto dialect when none was pinned. Bumps the cache generation:
    /// resolved columns and the recomputed diff are style inputs (column
    /// trace, Diff lens), so layouts built under the old dialect must miss.
    pub fn set_sql_dialect(&mut self, dialect: SqlDialect) {
        self.sql_dialect = Some(dialect);
        self.cll = SqlParseResolver::new(self.catalog.clone(), dialect);
        if let Some(base) = &self.diff_base {
            let base_res = SqlParseResolver::new(None, dialect);
            self.diff = Some(compute_diff_with(base, &self.dag, &base_res, &self.cll));
            self.diff_cll = Some(base_res);
        }
        self.generation += 1;
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

    /// The notification log's capacity (Issue 08): the centre keeps the
    /// newest `NOTIFICATION_CAP` entries; the oldest evicts beyond it.
    /// Memory-only — the log is never persisted.
    pub const NOTIFICATION_CAP: usize = 100;

    /// Record a transient notice (the toast text) at the default `Info` level.
    /// Last write wins: a later writer in the same loop iteration (e.g.
    /// `run_effect` reporting a failure) deliberately replaces the reducer's
    /// optimistic intent.
    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.set_notice_level(NoticeLevel::Info, text);
    }

    /// `set_notice` with an explicit level: warnings carry a longer on-screen
    /// TTL and bump the `!N` unread count once the loop logs them. Same
    /// last-write-wins rule — an effect failure still replaces the optimistic
    /// intent notice outright.
    pub fn set_notice_level(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notice = Some((level, text.into()));
    }

    /// Append to a PENDING notice instead of replacing it (`" | "` joined) —
    /// for startup-time writers that must not clobber a warning already
    /// queued by `with_source` (e.g. a malformed `.dbtl.yml` notice followed
    /// by the source-mode hint: both show in the one toast). The pending
    /// notice's level is preserved (an info append never downgrades a queued
    /// warning).
    pub fn append_notice(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.notice = Some(match self.notice.take() {
            Some((level, prev)) => (level, format!("{prev} | {text}")),
            None => (NoticeLevel::Info, text),
        });
    }

    /// Drain the pending notice's text (one-shot). The event loop calls
    /// [`take_notice_leveled`](App::take_notice_leveled) so the level drives
    /// the toast TTL and the log entry; this text-only view stays for tests
    /// and callers that don't care about severity.
    pub fn take_notice(&mut self) -> Option<String> {
        self.take_notice_leveled().map(|(_, t)| t)
    }

    /// Drain the pending notice WITH its level — the event loop's per-frame
    /// drain (it stamps the wall-clock and pushes the toast + the log entry,
    /// keeping time out of the reducer).
    pub fn take_notice_leveled(&mut self) -> Option<(NoticeLevel, String)> {
        self.notice.take()
    }

    /// Append `entry` to the notification log (newest last; the oldest drops
    /// at [`NOTIFICATION_CAP`](App::NOTIFICATION_CAP)). Warning entries count
    /// toward the unread `!N` chip until the centre is opened. The `stamp` is
    /// the caller's — the event loop / CLI format the wall-clock; `App` keeps
    /// no clock.
    pub fn record_notification(&mut self, entry: NotificationEntry) {
        if self.notifications.len() >= Self::NOTIFICATION_CAP {
            self.notifications.pop_front();
            // The evicted front shifts every index down — the read watermark
            // shifts with it, whether that entry was read or unread.
            self.unread_from = self.unread_from.saturating_sub(1);
        }
        self.notifications.push_back(entry);
    }

    /// Record every startup load warning in the notification log (warning
    /// level, unread) — `build_app` feeds it the `ResolvedSource.warnings`
    /// vec after printing them to stderr as before. `stamp` is the caller's
    /// wall-clock reading, taken ONCE per batch: stamping per line would
    /// spawn one `date` per warning (and could split a batch across a
    /// second boundary anyway).
    pub fn record_startup_warnings(&mut self, warnings: &[String], stamp: &str) {
        for w in warnings {
            self.record_notification(NotificationEntry {
                stamp: stamp.to_string(),
                level: NoticeLevel::Warning,
                text: w.clone(),
            });
        }
    }

    /// The number of unread WARNING notifications (drives the `!N` chip) —
    /// counted from the log itself each call, so the chip can never count a
    /// warning the cap has already evicted.
    pub fn unread_warnings(&self) -> usize {
        self.notifications
            .iter()
            .skip(self.unread_from)
            .filter(|n| n.level == NoticeLevel::Warning)
            .count()
    }

    /// The notification log's rows newest-first, snapshotted for the
    /// `Ctrl-n` centre (the render layer needs no `Dag`, like every modal).
    pub fn notifications_snapshot(&self) -> NotificationsView {
        NotificationsView {
            rows: self.notifications.iter().rev().cloned().collect(),
            scroll: 0,
        }
    }

    /// Mark every logged entry read — opening the notification centre is the
    /// read gesture that zeroes the `!N` chip. Later entries stay unread.
    pub fn mark_notifications_read(&mut self) {
        self.unread_from = self.notifications.len();
    }

    /// Re-root the lineage to `unique_id`, recording the move in the back stack
    /// (and clearing the forward stack) — the browser-style history for clicks /
    /// lineage-search jumps. No-op if the target isn't a selectable model. A
    /// re-root also ends any column focus: "jump to that model" means land on
    /// the model's own lineage, not keep the trace's filtered view.
    pub fn jump_to(&mut self, unique_id: &str) {
        let current = self.selected_unique_id();
        if self.reroot_select(unique_id) {
            // A successful re-root is the overview's drill-in gesture: leave the
            // whole-graph view and land on the rooted lineage of the target.
            if self.ui_state.overview() {
                self.ui_state.toggle_overview();
            }
            if let Some(cur) = current {
                if cur != unique_id {
                    self.back.push(cur);
                    self.forward.clear();
                }
            }
        }
    }

    /// The shared "land on this model" select: `select_by_unique_id` plus the
    /// re-root contract — a successful move ends the column trace, UNLESS the
    /// target IS the traced column's node (clicking/Entering the trace's own
    /// root box is interacting WITH the trace, not leaving it). A failed
    /// select (a non-model lineage node) leaves the focus alone — the same
    /// rule the columns modal's Enter applies. [`jump_to`], history, search
    /// close, the bookmark/gap cycles, and the mouse's list click all go
    /// through here so the contract can't drift between paths.
    ///
    /// [`jump_to`]: App::jump_to
    pub fn reroot_select(&mut self, unique_id: &str) -> bool {
        let trace_root = self.column_focus.as_ref().map(|f| f.col.node.clone());
        if !self.select_by_unique_id(unique_id) {
            return false;
        }
        if trace_root.as_deref() != Some(unique_id) {
            self.column_focus = None;
        }
        true
    }

    /// Go back one step in the re-root history (pushes the current onto forward).
    pub fn history_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            if let Some(cur) = self.selected_unique_id() {
                self.forward.push(cur);
            }
            self.reroot_select(&prev);
        }
    }

    /// Go forward one step in the re-root history (pushes the current onto back).
    pub fn history_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            if let Some(cur) = self.selected_unique_id() {
                self.back.push(cur);
            }
            self.reroot_select(&next);
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

    /// The `unique_id` a `y`-menu yank should act on: the structure modal's
    /// node (`pending_node`, stashed when `y` opened the menu over the modal)
    /// preferred over the list selection — ONLY while `Mode::Yank` is live;
    /// the fields are cleared the moment the menu closes, so a single-key
    /// yank outside the menu keeps the selection semantics.
    pub(super) fn yank_target_uid(&self) -> Option<String> {
        if matches!(self.mode, Mode::Yank { .. }) {
            self.pending_node
                .clone()
                .or_else(|| self.selected_unique_id())
        } else {
            self.selected_unique_id()
        }
    }

    // ---- node context menu (Issue 06) -------------------------------------

    /// The `unique_id` the `a` key's node menu acts on: over the structure /
    /// SQL modal the modal's own node (same rule `y` uses); in Selection the
    /// focus target (the lineage cursor under right-pane focus, else the
    /// list selection).
    fn menu_uid(&self) -> Option<String> {
        match &self.mode {
            Mode::Detail(dv) => Some(dv.model_id.clone()),
            Mode::Sql(sv) => Some(sv.model_id.clone()),
            _ => self.focus_target(),
        }
    }

    /// Which menu items are usable for `uid`: SQL preview only when the node
    /// actually has SQL (a source has none — screen-spec §9's "unsupported
    /// actions are omitted"; a seed's EMPTY `raw_code` preview would open a
    /// blank modal, so blank counts as unavailable too), and the editor row
    /// only when the node has a file path. Everything else is universal.
    fn menu_item_available(&self, uid: &str, action: crate::Action) -> bool {
        match action {
            crate::Action::SqlOpen | crate::Action::YankSql => self
                .dag
                .raw_code(uid)
                .is_some_and(|code| !code.trim().is_empty()),
            crate::Action::OpenEditor => self
                .dag
                .detail(uid)
                .and_then(|d| d.original_file_path.as_deref())
                .is_some(),
            _ => true,
        }
    }

    /// The menu rows for `uid`, generated from [`NODE_MENU_LAYOUT`]: every
    /// item slot resolves to the `(mode, action)` [`BINDINGS`] row it names
    /// (the action + key label + triggers all live there), separators are
    /// kept only between surviving groups (no leading/trailing/double rules),
    /// and unavailable items drop out first. An `uid` outside the Dag (a
    /// `+N more` marker, a stale target) yields an empty list — `open_menu`
    /// turns that into a notice instead of an empty box.
    pub fn menu_entries(&self, uid: &str) -> Vec<MenuEntry> {
        if !self.dag.contains(uid) {
            return Vec::new();
        }
        // `Re-root here` is honest only when the target isn't already the
        // re-rooted node — Enter on the modal's own node just re-opens the
        // structure view, so the label says that instead.
        let re_roots = self.selected_unique_id().as_deref() != Some(uid);
        let mut entries: Vec<MenuEntry> = Vec::new();
        for slot in NODE_MENU_LAYOUT {
            match slot {
                MenuSlot::Separator => {
                    if matches!(entries.last(), Some(MenuEntry::Item { .. })) {
                        entries.push(MenuEntry::Separator);
                    }
                }
                MenuSlot::Item(mode, action) => {
                    if !self.menu_item_available(uid, *action) {
                        continue;
                    }
                    let Some(binding) = crate::action::BINDINGS
                        .iter()
                        .position(|b| b.mode == *mode && b.action == *action)
                    else {
                        continue; // layout slot names a missing row — see tests
                    };
                    let label = menu_slot_label(menu_slot_mark(*action), re_roots);
                    entries.push(MenuEntry::Item { binding, label });
                }
            }
        }
        // A trailing separator left by a fully-filtered final group.
        if matches!(entries.last(), Some(MenuEntry::Separator)) {
            entries.pop();
        }
        entries
    }

    /// Open the node context menu over `uid` at `anchor`: the rows come from
    /// [`menu_entries`](Self::menu_entries) and the box's Esc/execute returns
    /// to whatever mode was live (a modal stays its own `back` — the same
    /// nested-restore mechanism `Mode::Yank` carries).
    pub fn open_menu(&mut self, uid: &str, anchor: MenuAnchor) {
        let items = self.menu_entries(uid);
        if items.is_empty() {
            self.set_notice("no actions for this node");
            return;
        }
        let name = self.node_name_or_uid(uid).to_string();
        let selected = items
            .iter()
            .position(|e| matches!(e, MenuEntry::Item { .. }))
            .unwrap_or(0);
        let back = match std::mem::replace(&mut self.mode, Mode::Selection) {
            Mode::Yank { back, .. } => back, // re-open keeps the outer destination
            Mode::Selection | Mode::Search(_) | Mode::Menu(_) => None,
            m => Some(Box::new(m)),
        };
        self.mode = Mode::Menu(MenuState {
            target: uid.to_string(),
            name,
            items,
            selected,
            back,
            anchor,
        });
    }

    /// A right-click opens the node menu for the clicked node — the same
    /// open path `a` uses, with the pointer cell as the anchor.
    fn open_menu_for(&mut self, uid: &str, col: u16, row: u16) {
        self.open_menu(uid, MenuAnchor::Cell { col, row });
    }

    /// Move the selection/cursor to `uid` (the menu's execution contract:
    /// act on the item's node, not wherever the selection happened to be).
    /// Right-pane focus means "the lineage cursor" — a node absent from the
    /// current subgraph can't take the cursor, so it re-roots instead; list
    /// focus means the list selection (`reroot_select`, the shared land-on
    /// path).
    pub(crate) fn move_selection_to(&mut self, uid: &str) {
        if self.ui_state.focus() == crate::ui::Focus::RightPane
            && self.lineage_display_subgraph().contains(uid)
        {
            self.click_lineage_node(uid);
        } else {
            self.reroot_select(uid);
        }
    }

    /// The selected node's display name, if any.
    pub fn selected_name(&self) -> Option<String> {
        self.selected_node().map(|n| n.name.clone())
    }

    /// The density the lineage pipeline actually renders at: the overview
    /// forces Compact (a whole graph at 3-row boxes is unusably tall), else
    /// the user's `v` toggle. Shared by the cached layout AND `lineage_ascii`
    /// so the yank stays byte-identical to the pane.
    pub(crate) fn effective_density(&self) -> crate::Density {
        if self.ui_state.overview() {
            crate::Density::Compact
        } else {
            self.ui_state.density()
        }
    }

    /// The lineage pane's title body while the whole-graph overview is on
    /// (`"Overview: N nodes"`), `None` otherwise. Precomputed Dag-aware at the
    /// loop seam so `RenderCtx` stays Dag-free.
    pub fn overview_title(&self) -> Option<String> {
        self.ui_state
            .overview()
            .then(|| format!("Overview: {} nodes", self.dag.len()))
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
    /// Returns the manifest's load warnings (unhandled `resource_type`s /
    /// `functions`, plus the package-scope warnings from re-applying
    /// `self.scope`) like [`reload`](App::reload) does, so the caller can
    /// toast them.
    pub fn adopt_manifest(&mut self, path: PathBuf) -> Result<Vec<String>> {
        let new_root = derive_project_root(&path);
        let prev = std::mem::replace(&mut self.source, DataSource::Manifest(path));
        match self.reload() {
            Err(err) => {
                self.source = prev;
                Err(err)
            }
            Ok(warnings) => {
                self.project_root = new_root;
                Ok(warnings)
            }
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
                // Landing on a different uid is a re-root: the sibling-collapse
                // expansions are per-root state, so they re-initialise here.
                // A same-uid re-resolve (filter toggle, sort rebuild) keeps
                // them — it is not a re-root.
                if self.selected_unique_id().as_deref() != Some(unique_id) {
                    self.collapse_expanded.clear();
                }
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
    /// state — the caller can keep running on the old data. Returns the load
    /// warnings for whichever source is active: source mode's parse warnings
    /// (unparsable schema.yml, duplicate resource names), or manifest mode's
    /// unhandled `resource_type`/`functions` warnings (a synthesized source
    /// manifest never produces the latter — that channel is manifest-only),
    /// plus the package-scope warnings from re-applying `self.scope`
    /// ([`PackageScope::apply_with_warnings`] — e.g. an ambiguously-rooted or
    /// emptied graph); an unremarkable reload returns an empty vec.
    pub fn reload(&mut self) -> Result<Vec<String>> {
        let current = self.selected_node().map(|n| n.unique_id.clone());
        // Re-read from the same source; returns before mutating on error.
        // The package scope the load path applied at startup is re-applied
        // here — a recompiled manifest can gain/drop `dbt_packages/` contents
        // between reloads, and the default `Root` scope must keep hiding them.
        let (dag, mut warnings) = match &self.source {
            DataSource::Manifest(path) => load_dag_with_warnings(path)?,
            DataSource::Project(dir) => load_dag_from_source_with_warnings(dir)?,
        };
        // The SAME scope application the CLI load path runs, warnings included:
        // an emptied or ambiguously-rooted re-load is reported exactly like a
        // first load (the toast quotes the returned vec).
        let (dag, scope_warnings) = self.scope.apply_with_warnings(dag);
        warnings.extend(scope_warnings);
        self.model_list = build_model_list_grouped(&dag, self.sort, self.group_by_meta.as_deref());
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
        // Same source, fresh outcomes: re-read the run_results the lenses and
        // the weighted critical path read (a `dbt run` between reloads shows up
        // without a restart). A vanished file → no run data, never an error.
        self.run_results = self
            .run_results_path
            .as_ref()
            .filter(|p| p.is_file())
            .and_then(|p| RunResults::load(p).ok());
        self.catalog = self
            .catalog_path
            .as_ref()
            .filter(|p| p.is_file())
            .and_then(|p| crate::Catalog::load(p).ok())
            .map(Rc::new);
        // The CLL resolver memoizes column lists/SQL parses PER UID — over a
        // replaced Dag those memos are poison, so rebuild (memos inside the
        // engine are per-call and die with it anyway).
        let dialect = self
            .sql_dialect
            .unwrap_or_else(|| SqlDialect::from_dag(&self.dag));
        // Same generic-dialect notice as first load — a recompiled manifest
        // can change adapter_type between reloads (an explicit
        // --sql-dialect suppresses it).
        if self.sql_dialect.is_none() {
            if let Some(a) = SqlDialect::unrecognized_adapter(&self.dag) {
                warnings.push(format!(
                    "column lineage: unknown adapter '{a}' — using the generic SQL dialect"
                ));
            }
        }
        self.cll = SqlParseResolver::new(self.catalog.clone(), dialect);
        // `.dbtl.yml` reloads alongside the graph like the other side maps —
        // an edit mid-session applies on the next reload, and a file created
        // or deleted mid-session is picked up/dropped too (the path is
        // re-detected from the data source, not just replayed). A malformed
        // file falls back to shipped defaults + a warning (same policy as
        // startup).
        self.lint_config_path = self.source.lint_config_path();
        match &self.lint_config_path {
            Some(path) => match crate::LintConfig::load(path) {
                Ok(c) => self.lint_config = c,
                Err(e) => {
                    warnings.push(format!("ignoring .dbtl.yml: {e}"));
                    self.lint_config = crate::LintConfig::default();
                }
            },
            None => self.lint_config = crate::LintConfig::default(),
        }
        self.lint_findings = crate::run_rules(&self.dag, &self.lint_config);
        self.filter = None;
        self.mode = Mode::Selection;
        // Any yank-menu context stashed against the OLD graph dies here too.
        self.pending_col = None;
        self.pending_node = None;
        // And so does a column trace: the rebuilt Dag may not carry the
        // column (or resolve different edges for it) — drop the focus rather
        // than render a stale trace over a new graph.
        self.column_focus = None;
        // Cursor home: selection is restored BY ID, so the loop's
        // selection-change chokepoint won't fire — reset here or a pre-reload
        // cursor would survive into the rebuilt graph.
        self.lineage_cursor = None;
        // Same for the sibling-collapse expansions: the group keys index into
        // the OLD graph's columns, so a fresh graph re-initialises them.
        self.collapse_expanded.clear();
        // Re-diff the rebuilt Dag against the (unchanged) --diff baseline, so
        // the Diff lens / chip / modal always describe the CURRENT graph.
        // The baseline resolver's per-uid memos stay valid (the baseline never
        // changes); the CURRENT side reads the just-rebuilt `self.cll`. The
        // baseline resolver is rebuilt only when its dialect drifted off the
        // shared one-dialect-per-diff rule (`for_diff_baseline` follows the
        // CURRENT dag's adapter, which a reload can move) — a pinned
        // --sql-dialect never drifts.
        self.diff = match &self.diff_base {
            Some(b) => {
                let want = self
                    .sql_dialect
                    .unwrap_or_else(|| SqlDialect::for_diff_baseline(&self.dag, b));
                if self.diff_cll.as_ref().is_none_or(|r| r.dialect() != want) {
                    self.diff_cll = Some(SqlParseResolver::new(None, want));
                }
                let res = self.diff_cll.as_ref().expect("baseline resolver set");
                Some(compute_diff_with(b, &self.dag, res, &self.cll))
            }
            None => None,
        };
        // Recompute the persistent-filter view over the rebuilt list (it also
        // sets the model count to the active list's length).
        self.apply_list_filter();
        if let Some(uid) = current {
            self.select_by_unique_id(&uid);
        }
        Ok(warnings)
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
        // `!`-prefixed queries are dbt selectors, not fuzzy name matches:
        // `!tag:nightly`, `!+fct_orders`, `!state:modified+` (with --diff).
        // `?`-prefixed queries are FULL-TEXT: match description, column names,
        // and raw SQL — "who references this table/column" (`/?raw_events`).
        let filtered = if let Some(spec) = query.strip_prefix('!') {
            if spec.trim().is_empty() {
                // A bare `!` is the selector PREFIX, not a query — keep the
                // full list while the user is still typing (`select("")`
                // would be a zero-term empty set and flash a wrong
                // "no match" reason at the first keystroke).
                build_filtered_model_list(self.base_list(), "")
            } else {
                let set = crate::select::select(&self.dag, self.diff(), spec);
                crate::model_list::filter_model_list(self.base_list(), |m| {
                    set.contains(&m.unique_id)
                })
            }
        } else if let Some(needle) = query.strip_prefix('?') {
            let needle = needle.to_lowercase();
            crate::model_list::filter_model_list(self.base_list(), |m| {
                self.full_text_match(&m.unique_id, &needle)
            })
        } else {
            build_filtered_model_list(self.base_list(), &query)
        };
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

    /// Full-text predicate behind `/<query>`: case-insensitive substring over
    /// the node's description, column names, and raw (uncompiled) SQL — the
    /// surfaces the name-only fuzzy match can't reach ("who selects from
    /// `raw_events`", "who writes `customer_id`").
    fn full_text_match(&self, uid: &str, needle: &str) -> bool {
        let hit = |s: &str| s.to_lowercase().contains(needle);
        if let Some(d) = self.dag.detail(uid) {
            if d.description.as_deref().is_some_and(hit) {
                return true;
            }
            if d.columns.iter().any(|c| hit(&c.name)) {
                return true;
            }
        }
        self.dag.raw_code(uid).is_some_and(hit)
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
            self.reroot_select(&uid);
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
