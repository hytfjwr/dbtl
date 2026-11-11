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
mod home;
mod lineage;
mod list;
mod menu;
mod notices;
// `pub(crate)`: the persisted-state layer reads its cap constants; the
// palette surface itself reaches `App` through the `impl App` methods.
pub(crate) mod palette;
mod prefs;
mod reducer;
mod select;
mod source;
mod status;
mod styles;
mod tabs;
#[cfg(test)]
mod tests;

// The single violation predicate lives in `dbtl_core::impact` (shared by the
// lens, the stats worklist, the PR impact pack, and `dbtl check`); re-export
// it here so `dbtl::app::layer_violation_edges` keeps resolving.
pub use crate::layer_violation_edges;
// The subgraph→fenced-Mermaid core lives in `dbtl_core::mermaid` so the static
// docs generator (`dbtl-docs`) shares the identical id-assignment/escaping
// without depending on this crate.
pub use home::{HomeData, HomeInfo, HomeItem, HomeRow, HomeSearch, HomeSection, HomeTone};
pub(crate) use lineage::fit_breadcrumb;
pub use reducer::apply_action;
pub use tabs::TabState;

use analysis::compute_stats;
use cache::LineageCaches;

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
/// limit. Default = the full lineage (both directions, unlimited). Persisted
/// per tab: `#[serde(default)]` keeps an old/partial state file loading.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// `unfolded_base`).
    pub list_filter: ListFilter,
    /// The `list_filter` view of `model_list` (`None` when `list_filter` is
    /// `All`). Rebuilt ONLY by [`apply_list_filter`](App::apply_list_filter).
    filtered_base: Option<ModelList>,
    /// The folded group keys — the `-` toggle / header click's persistent
    /// model (persisted via `PersistedState::folded_groups`).
    /// App-level state like `list_filter` — shared across tabs, not a
    /// per-tab pref. A pipeline INPUT — the fold is materialized into
    /// `folded_base` by `apply_list_filter`, never faked at draw time, so a
    /// folded group's models leave selection space entirely.
    pub folded_groups: BTreeSet<String>,
    /// The `folded_groups` view of the unfolded base (`None` while no group
    /// is folded). Same single-writer rule as `filtered_base`: only
    /// `apply_list_filter` rebuilds it. Sits BETWEEN the persistent filter
    /// and a live search — search narrows `unfolded_base` (folding is
    /// ignored while searching and restored when the search closes).
    folded_base: Option<ModelList>,
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
    /// Bookmarked model `unique_id`s (Space toggles, `'` cycles). A `BTreeSet`
    /// for deterministic iteration; lives on `App` (not the list) so toggling a
    /// bookmark never rebuilds the list and it survives a refilter untouched.
    /// `reload` prunes ids that no longer resolve.
    pub bookmarks: BTreeSet<String>,
    pub ui_state: UiState,
    pub mode: Mode,
    /// Project name + resource counts, for the title bar.
    pub stats: AppStats,
    /// Which glyph repertoire the lineage grid + chrome draw with. Defaults to
    /// Unicode; `main` overwrites it from the CLI flags / the startup
    /// ambiguous-width terminal probe.
    pub glyph_mode: crate::GlyphMode,
    /// The transient-notice channel: a one-shot (level, message) describing the
    /// latest user-visible outcome (copied / bookmarked / exported / reloaded).
    /// Writers: the reducer arms (intent) and `main::run_effect` (which
    /// OVERWRITES the intent on effect failure — it runs after `apply_action`
    /// in the same loop iteration, so the failure always wins). The event loop
    /// drains it via [`take_notice_leveled`](App::take_notice_leveled), stamps
    /// the wall-clock, and owns the on-screen toast lifetime — `App` stores no
    /// clock, so the reducer stays pure.
    notice: Option<(NoticeLevel, String)>,
    /// The notification log behind `Ctrl-n`: stamped entries,
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
    /// The open tabs (≥1 — [`close_tab`](App::close_tab) refuses the last).
    /// Each keeps its own root + per-root view state (see [`TabState`]); every
    /// access goes through [`tab`](App::tab)/[`tab_mut`](App::tab_mut).
    tabs: Vec<TabState>,
    /// The active tab's index into `tabs`.
    active: usize,
    /// One-shot flag the event loop's selection-change chokepoint drains via
    /// [`take_tab_switch`](App::take_tab_switch): set by `activate_tab` /
    /// `close_tab` / restore so the loop knows an id-changing selection is a
    /// TAB SWITCH (skip the re-home/collapse-clear; restore the tab's own).
    tab_switched: bool,
    /// The tab strip's first-visible index — the scroll intent the wheel and
    /// the `‹`/`›` cells write; resolved (clamped + active-ensured) by
    /// [`tab_bar_layout`](crate::ui::tab_bar_layout) per frame. Session-only.
    tab_scroll: usize,
    /// Recently-rooted models (newest first, max 10) — the home reads
    /// it; persisted per project like the tab list itself.
    pub recent: Vec<String>,
    /// The home screen's item cursor — an index into the flattened
    /// selectable rows ([`HomeData::selectable`]), or into the search-result
    /// list while a home-target `/` search is open. Lives on `App` (not
    /// `Mode::Home`) so modal round-trips keep it.
    pub home_sel: usize,
    /// The home screen's `Resume` row: the last active MODEL tab's root —
    /// captured by `activate_tab`/`close_tab` when they land on home, or
    /// seeded from the persisted `last_selected` on a home-landing restore.
    pub home_resume: Option<String>,
    /// The home body's scroll offset (lines scrolled off the top). Written
    /// ONLY by the event loop's ensure-visible clamp — the size-aware seam
    /// that owns every other viewport bound.
    pub home_scroll: usize,
    /// Stamp covering every `HomeData` input `generation` does NOT already
    /// key: `recent` writes, bookmark toggles, `home_resume` writes. Bumped
    /// at each mutation site — the home cache keys on
    /// `(generation, home_stamp)`.
    pub(crate) home_stamp: u64,
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
    /// The column-level-lineage resolver (the `sqlparser`-backed
    /// path, falling back to the name-match approximation per
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
    /// The armed overlay target (`unique_id`), stashed the same way — what
    /// every OTHER `y`-menu yank (`yy`/`yY`/`yC`/`ym`/…) targets instead of
    /// the list selection, AND what a node-menu invoke targets while its
    /// action dispatch runs (`MenuInvoke` arms it for just that window).
    /// Without it a yank dispatched over the modal — or a menu item fired
    /// on a node the selection can't land on — would silently copy the
    /// *selected* node's data, not the target's. See
    /// [`pending_or_selection`](App::pending_or_selection).
    pub(super) pending_node: Option<String>,
    /// Confirmed list-search queries, most-recent-first, deduped, capped at
    /// `palette::SEARCH_HISTORY_MAX` — what the `/`-mode Up/Down recall
    /// browses. Persisted as `search_history` (recorded by
    /// [`record_search_query`](App::record_search_query) on `SearchConfirm`).
    pub search_history: Vec<String>,
    /// Recently-run palette commands as `Action` Debug names,
    /// most-recent-first, capped at `palette::PALETTE_RECENT_MAX` — the
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
        // The first tab roots on the list head — the same model `selected` 0
        // pointed at before tabs existed.
        let first_root = model_list.models.first().map(|n| n.unique_id.clone());
        App {
            dag,
            model_list,
            filter: None,
            list_filter: ListFilter::default(),
            filtered_base: None,
            folded_groups: BTreeSet::new(),
            folded_base: None,
            sort,
            group_by_meta: None,
            owner_meta_key: "owner".to_string(),
            collapse_threshold: crate::collapse::DEFAULT_COLLAPSE_THRESHOLD,
            bookmarks: BTreeSet::new(),
            ui_state,
            mode: Mode::Selection,
            stats,
            glyph_mode: crate::GlyphMode::default(),
            // A malformed `.dbtl.yml` surfaces as a startup toast (one-shot
            // WARNING notice) — the same channel main.rs uses for the
            // source-mode hint (an info append joins it without dropping the
            // warning level).
            notice: lint_notice.map(|t| (NoticeLevel::Warning, t)),
            notifications: VecDeque::new(),
            unread_from: 0,
            // The strip always opens `[home, seeded]` — home is the fixed
            // left tab. `active` starts on the seeded model tab so
            // a pre-home state file keeps its old landing behaviour; the
            // no-state-file path calls `enter_home` explicitly.
            tabs: vec![
                TabState::home(),
                TabState {
                    root: first_root.clone(),
                    ..TabState::default()
                },
            ],
            active: 1,
            tab_switched: false,
            tab_scroll: 0,
            recent: first_root.into_iter().collect(),
            home_sel: 0,
            home_resume: None,
            home_scroll: 0,
            home_stamp: 0,
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
