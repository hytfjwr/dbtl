//! The single source of truth for user intent and key bindings.
//!
//! Every interaction is reified as an [`Action`]. A mode-aware [`dispatch`]
//! maps `(Mode, KeyEvent) -> Option<Action>`, and the SAME binding table feeds
//! [`help_lines`], so the `?` overlay can never drift from what the keys
//! actually do. `dispatch` is PURE and SIZE-UNAWARE: size-aware work
//! (scroll-follow, mouse hit-test) stays in the event loop.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{ColumnChange, Focus, LineageLens, NodeDetail, PackageScope, PrImpact, TestInfo};

/// A movement / scroll direction, reused across actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Every user intent. `Copy` (all payloads are `char`/`u16`/[`Direction`]), so
/// it can live in the static [`BINDINGS`] table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // --- movement keys: focus-routed in `app::apply_action` (list move via
    // `ui::reduce_selection` when the list is focused, lineage CURSOR move when
    // the lineage pane is focused — never a viewport pan).
    MoveDown,
    MoveUp,
    MoveLeft,
    MoveRight,
    JumpTop,
    JumpBottom,
    ToggleFocus,
    /// Show / hide the left model-list pane (the lineage pane takes the full
    /// width while hidden; focus is forced to the lineage).
    ToggleListPane,
    Quit,
    // --- Overlays / domain (handled by `app::apply_action`).
    DetailOpen,
    DetailClose,
    DetailScroll(Direction),
    SearchOpen,
    SearchType(char),
    SearchBackspace,
    SearchCancel,
    SearchConfirm,
    SearchMove(Direction),
    /// Open the command palette (a fuzzy finder over every Selection-mode
    /// command). Its editing keys REUSE the `Search*` actions — only the opener
    /// is its own action, so the keymap stays the single source of truth.
    PaletteOpen,
    HelpToggle,
    Recenter,
    OpenEditor,
    YankId,
    YankName,
    /// Copy the current lineage subgraph to the clipboard as Mermaid `graph LR`.
    YankMermaid,
    /// Copy the current lineage subgraph to the clipboard as Graphviz DOT.
    YankDot,
    Reload,
    // --- lineage view (direction / depth) ---
    /// Toggle showing the upstream side of the lineage.
    ToggleUpstream,
    /// Toggle showing the downstream side of the lineage.
    ToggleDownstream,
    /// Decrease / increase the lineage hop-depth limit.
    DepthDecrease,
    DepthIncrease,
    /// Reset the lineage view to the default (both directions, unlimited depth).
    ResetView,
    // --- selection (re-root) history ---
    /// Go back / forward through the re-root history.
    HistoryBack,
    HistoryForward,
    // --- new feature commands (Step A wires ALL of them; only the two modal
    // openers are implemented here — the five toggles get no-op stubs filled by
    // their own later steps, so the keymap is never edited again). ---
    /// Open the SQL preview modal for the selected / cursored node.
    SqlOpen,
    /// Inside the SQL preview: switch the displayed text between the raw
    /// (uncompiled) SQL and the compiled (Jinja-expanded) one — when the node
    /// has compiled SQL at all (`target/compiled/` or manifest `compiled_code`).
    SqlToggleCompiled,
    /// Open the project stats dashboard modal.
    StatsOpen,
    /// Cycle the lineage colour lens (Off → Coverage → DegreeHeat → Layer →
    /// LayerViolation → Off).
    CycleLens,
    /// Set the lineage lens directly — the status chips' click path names a
    /// lens (the `run …` chip jumps to RunStatus rather than pedalling the
    /// `t` cycle). Never key-bound: only hit targets produce it.
    SetLens(LineageLens),
    /// Toggle a bookmark on the selected model.
    BookmarkToggle,
    /// Cycle the selection through the bookmarked models.
    BookmarkCycle,
    /// Cycle the left-pane sort order.
    SortCycle,
    /// Toggle the lineage minimap.
    ToggleMinimap,
    /// Toggle the lineage density (Comfortable 3-row boxes <-> Compact 1-row).
    ToggleDensity,
    /// Toggle the whole-graph overview (the lineage pane shows the ENTIRE DAG
    /// instead of the rooted subgraph; always Compact density).
    ToggleOverview,
    /// Two-node path mode: first `p` anchors a path start at the selection;
    /// the root↔cursor path highlight then runs anchor↔cursor (dbt's
    /// `A+,+B` answer to "why does X reach Y"). `p` again clears the anchor.
    PathToggle,
    /// Copy the current lineage diagram to the clipboard as plain text (the
    /// same glyphs the pane draws).
    YankAscii,
    /// Copy the selected node's raw (uncompiled) SQL to the clipboard.
    YankSql,
    /// Copy a Markdown blast-radius report for the selected node.
    YankImpact,
    /// Copy a runnable `dbt build --select …` command for the current lineage
    /// view (root + direction toggles + depth limit as graph operators).
    YankSelector,
    /// Copy the selected node's compiled (Jinja-expanded) SQL — the
    /// `target/compiled/` / `compiled_code` counterpart of [`YankSql`], which
    /// stays raw-only. Bound as `ys` inside the yank-prefix menu.
    YankCompiledSql,
    /// Inside the yank menu (`yl`): copy the selected column's DOWNSTREAM
    /// column-level lineage closure as a deterministic Markdown table (the
    /// same [`ColumnReport`](crate::ColumnReport) `dbtl columns` prints). The
    /// "selected column" is the structure modal's column cursor, stashed on
    /// the App when `y` opened the menu; without it the action explains how
    /// to pick one instead of writing noise to the clipboard.
    YankColumnLineage,
    /// Open the yank prefix menu (a which-key over `Mode::Yank`): every yank
    /// action is also reachable as `y` + the same letter it always used
    /// (`ym` = Mermaid, `yi` = impact, `yy` = unique_id). The single-key
    /// bindings stay bound for the migration period.
    YankPrefix,
    /// Write the current lineage diagram to `<name>_lineage.txt` in the cwd.
    ExportLineage,
    /// Move the list selection a fixed page (10 models) down / up. List-side
    /// only (forwarded to `reduce_selection`); fixed-step so the reducer stays
    /// size-unaware.
    PageDown,
    PageUp,
    /// Scroll an overlay modal a fixed page (10 lines); the loop clamps.
    DetailScrollPage(Direction),
    /// Jump an overlay modal's scroll to the top / bottom (bottom records
    /// `usize::MAX`; the loop's per-frame clamp bounds it to the content).
    DetailScrollHome,
    DetailScrollEnd,
    /// Move the structure modal's column cursor one row (`Up` = previous
    /// column, `Down` = next) — the `yl` yank targets the column under it.
    /// Distinct from `DetailScroll`, which pans the viewport: this is a
    /// SELECTION, and the loop's ensure-visible follows it. While the tests
    /// section owns the cursor (`Tab`), the same move walks the jumpable
    /// (relationships-target) test rows.
    DetailColumnMove(Direction),
    /// Toggle the structure modal's h/l cursor between the columns section and
    /// the tests section (`Tab`) — the tests side only offers rows Enter can
    /// jump to (relationships `to:` targets).
    DetailSectionToggle,
    /// Cycle the selection to the next / previous untested model (the
    /// `coverage_gap` set — same predicate as the Coverage lens).
    GapNext,
    GapPrev,
    /// Cycle the selection backward through the bookmarked models.
    BookmarkCycleBack,
    /// Send the lineage cursor to the leftmost (most-upstream) / rightmost
    /// (most-downstream) column, nearest row.
    LineageLeftmost,
    LineageRightmost,
    /// Toggle the persistent untested-only / bookmarked-only list filter.
    ToggleUntestedFilter,
    ToggleBookmarkFilter,
    /// Fold/unfold the list group the cursor sits in (`-`; the group-header
    /// click's keyboard twin — screen-spec §10).
    ToggleGroupFold,
    /// Run `dbt parse` in the project root and adopt the regenerated
    /// `target/manifest.json` (upgrade source-mode's inferred lineage to the
    /// compiled manifest's full fidelity; in manifest mode it refreshes).
    DbtParse,
    /// Cycle the colour theme through the App's loaded list (built-in presets
    /// plus any user themes); the toast names the one that landed.
    ThemeCycle,
    /// Open the baseline-diff summary modal (`--diff`); a toast explains how to
    /// load a baseline when none is.
    DiffOpen,
    /// Export the PR impact report (the diff modal's full content) as Markdown to
    /// `<project>_pr_impact.md`. Bound INSIDE the Diff modal (the `e`-style
    /// export); reads the open [`DiffView`] snapshot so the file matches screen.
    ExportPrImpact,
    /// Copy the suggested `dbt build --select …` command for the changed-node set
    /// to the clipboard. Bound INSIDE the Diff modal; reads the open
    /// [`DiffView`]'s [`PrImpact`]. Copies the column-narrowed
    /// [`PrImpact::ci_command_columns`] when the analysis produced one —
    /// the most precise buildable set — falling back to the wide
    /// [`PrImpact::ci_command`]. `Y` ([`YankPrSelectorWide`]) takes the wide
    /// command unconditionally.
    YankPrSelector,
    /// The wide counterpart of [`YankPrSelector`]: always copies
    /// [`PrImpact::ci_command`] (every changed node with `+` fan-out),
    /// ignoring the column narrowing.
    YankPrSelectorWide,
    // --- column-level lineage focus (Issue 09) ---
    /// Enter column focus on the structure modal's cursored column: the
    /// lineage pane filters to the column's related models and draws the
    /// related columns inside each box. Falls back to `DetailClose`
    /// behaviour when no column is under the cursor (so Enter still closes
    /// a column-less modal).
    ColumnFocusEnter,
    /// Leave column focus (Esc in Selection mode) — the lineage returns to
    /// the normal rooted view. No-op while unfocused.
    ColumnFocusExit,
    /// Open the column-lineage modal (`Mode::Columns`): the traced /
    /// cursored column's upstream+downstream column tree, scrollable and
    /// jumpable (Enter re-roots to the row's model). Toasts a hint when no
    /// column is in scope.
    ColumnsOpen,
    /// Move the column modal's row cursor one selectable row (`Up`/`Left` =
    /// previous, `Down`/`Right` = next) — a SELECTION like
    /// `DetailColumnMove`, not a viewport scroll; the loop's
    /// `clamp_columns_scroll` follows it.
    ColumnRowMove(Direction),
    /// Re-root to the model of the column-modal row under the cursor
    /// (records re-root history, like a lineage click). A non-model row
    /// notices instead of jumping.
    ColumnJump,
    // --- pane widths (Issue 02; screen-spec §5) ---
    /// `<`: shrink the focused pane's saved width by §5's 4-column step —
    /// the list, until Issue 04's inspector gains a focus target.
    PaneNarrow,
    /// `>`: grow the focused pane's saved width by the same step.
    PaneWiden,
    /// `=`: every resizable pane's saved width back to its §1 default.
    PaneWidthsReset,
    /// Open the notification centre (`Ctrl-n`): the scrollable log of recent
    /// notifications (`HH:MM:SS  level  text`, newest first — Issue 08).
    /// Opening it is the read gesture that zeroes the `!N` unread-warning
    /// count.
    NotificationsOpen,
    // --- node context menu (Issue 06) ---
    /// Open the node context menu (`a`, or a right-click on a node): a
    /// BINDINGS-derived action list over the node under the cursor/pointer.
    /// Over the structure / SQL modal the menu targets the modal's node.
    MenuOpen,
    /// Move the node menu's selection cursor (j/k, ↑/↓).
    MenuMove(Direction),
    /// Execute the cursored menu item.
    MenuConfirm,
    /// Execute the menu item at `items` index `i` — produced by the
    /// dispatcher's in-menu shortcut lookup (an item's own key fires it) and
    /// by a click on that row; never a fixed BINDINGS row.
    MenuInvoke(usize),
    /// Highlight the menu item at `items` index `i` (pointer hover) — pure
    /// cursor move, no execution.
    MenuSelect(usize),
    /// Highlight the `y` which-key row `i` (a row click selects, per the
    /// row-bearing-modal rule) — the index into [`yank_bindings`].
    YankPick(usize),
    /// Execute the `y` which-key row `i` (a double-click), resolving through
    /// the same [`yank_bindings`] table the renderer draws.
    YankInvoke(usize),
    /// Highlight palette candidate `i` (a row click; double-click then fires
    /// the existing [`SearchConfirm`](Action::SearchConfirm)).
    PalettePick(usize),
    /// Move the columns modal's row cursor to `i` (a row click; double-click
    /// then fires the existing [`ColumnJump`](Action::ColumnJump)).
    ColumnPick(usize),
    // --- tabs (Issue 03; screen-spec §2) ---
    /// `O`: open the focus target's model in a NEW tab right of the active
    /// one (already-open roots just activate their tab — T2 dedupe).
    TabOpen,
    /// `X` / `Ctrl-w`: close the ACTIVE tab — the right neighbour (else the
    /// left) takes over; the last tab refuses with a toast.
    TabClose,
    /// `{` / `}`: previous / next tab, wrapping at both ends.
    TabPrev,
    TabNext,
    /// `1`..=`9`: activate the tab at that 1-based position (tabs from
    /// position 10 carry no number and are unreachable by digit). Position
    /// counts MODEL tabs only — the home tab sits left of position 1 and is
    /// reached via `~`/`{`/`}` (Issue 10).
    TabActivate(u8),
    // --- home tab (Issue 10; screen-spec §3) ---
    /// `~`: activate the home tab (no-op while already on it).
    HomeGo,
    /// `j`/`k`/`↑`/`↓` on the home screen: move the item cursor over the
    /// flattened selectable rows (resume → recent → bookmarks → attention;
    /// the search-results list while `/` search is open).
    HomeMove(Direction),
    /// `Enter` on the home screen: open the item under the cursor — already
    /// open activates its tab, else a new tab (§3's "開く" rule).
    HomeOpen,
    /// A click on home item `i` (its flattened selectable index): select it
    /// and open it in one gesture (§6 — items open on click).
    HomePick(usize),
    // --- inspector pane (Issue 04; screen-spec §4) ---
    /// `I`: cycle the inspector visibility pref `auto → on → off` (the
    /// persisted value; the width-aware auto rule resolves at draw time).
    ToggleInspector,
    /// Move the inspector's column-row cursor to `i` (a row click in the
    /// pane — the event loop maps the cell to the column index). `Enter`
    /// then traces the row's column, exactly like the structure modal's.
    InspectorColumnPick(usize),
}

/// Which pane a search targets (decided from focus when search opens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTarget {
    /// Filter the left model list.
    List,
    /// Jump to a matching node within the current lineage subgraph.
    Lineage,
    /// The home screen's `/` field (§3): same syntax as `List` (`!` selector /
    /// `?` full-text), results replace the home sections. Enter opens the
    /// picked model in a tab rather than re-rooting.
    Home,
}

/// Live search state (query text + which pane it drives + the selection to
/// restore on cancel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub target: SearchTarget,
    pub query: String,
    /// The `unique_id` that was selected when search opened, restored on cancel.
    pub origin_uid: Option<String>,
    /// For a lineage-target search: index into the (ordered) match list, advanced
    /// by Up/Down to cycle matches. Reset to 0 whenever the query changes.
    pub match_idx: usize,
    /// For a LIST-target search: while `Some(i)`, the query is a recalled
    /// `App::search_history` entry — the browse cursor Up/Down walk (entered
    /// only from an EMPTY query). `None` = the live query the user is typing;
    /// any edit drops the browse back to `None`.
    pub hist: Option<usize>,
}

/// The structure-modal payload: cloned from the [`Dag`](crate::Dag) side maps
/// when the modal opens, so the render layer never needs a `Dag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailView {
    pub model_id: String,
    pub name: String,
    pub detail: NodeDetail,
    pub tests: Vec<TestInfo>,
    /// The blast-radius counts for THIS node (the focus target, not necessarily
    /// the root), snapshotted from the `Dag` closures at open time so the render
    /// layer stays Dag-free.
    pub downstream_count: usize,
    pub upstream_count: usize,
    /// Pre-rendered `catalog:`/`drift:` lines from `catalog.json` (real column
    /// types, row count, byte size, doc↔warehouse drift) — snapshotted at open
    /// like every other field; empty without catalog coverage.
    pub catalog: Vec<String>,
    /// The cursor-addressable column list: the declared column names in
    /// declared order when `detail.columns` is non-empty, else the CLL
    /// resolver's inferred list (so a model with no documented columns still
    /// gets a cursor). `col_index` indexes this; when declared columns exist
    /// they render with type/description rows, otherwise this list IS the
    /// rows. Declared names render verbatim; the resolver's inferred names are
    /// already normalized + deduped.
    pub col_names: Vec<String>,
    /// The column cursor (h/l inside the modal) — an index into `col_names`,
    /// or `usize::MAX` when the list is empty.
    pub col_index: usize,
    /// The tests-section cursor — an index into `tests`, or `usize::MAX` when
    /// the COLUMNS section owns the h/l cursor (the default). `Tab` toggles
    /// between the two; the cursor only lands on rows with a `ref_uid`
    /// (relationships targets — the rows Enter can jump to), mirroring
    /// [`ColumnView`]'s uid-gated cursor.
    pub test_index: usize,
    /// `normalized column -> (one-hop upstream, one-hop downstream)` DIRECT
    /// column-edge counts, snapshotted from `ColumnLineage::column_counts` at
    /// open — the `↑u ↓d` numbers beside each column row (direct parents in /
    /// direct children out, per the Issue 07 spec — NOT the transitive
    /// closure). `BTreeMap` keeps the render order deterministic; empty when
    /// the resolver sees no columns. Keys are the resolver's normalized names,
    /// so a declared `col_names` entry looks itself up through
    /// `norm_column` — an un-normalizable name (rare: declared names come
    /// from manifest keys, which dbt already lowers) just gets no count.
    pub col_counts: std::collections::BTreeMap<String, (usize, usize)>,
    /// Any of this node's column edges was resolved against compiled SQL
    /// older than its source — the columns header gets a `[stale]` tag.
    pub stale: bool,
    /// This node's column edges fell back to the Issue 07 name-match
    /// approximation (unparsed or missing SQL) — the columns header gets a
    /// `[name-match]` tag.
    pub name_match: bool,
    pub scroll: usize,
}

/// The right inspector pane's snapshot for the node under the lineage cursor
/// (or the rooted selection): `detail` reuses the structure modal's
/// `detail_view_for` assembly verbatim (screen-spec §4's "share the DetailView
/// construction" rule — one domain pass, never two), and the pane-specific
/// extras (tag / exposures / catalog types / last-run / lint / diff) join
/// here. Plain comparable data, built at `(uid, generation)` cache time so
/// the render layer stays `Dag`-free.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectorView {
    /// The structure-modal data product for this node (`detail_view_for`'s
    /// output — tests / col_names / col_counts / catalog lines / impact
    /// counts / `name` / `model_id` all ride inside). The modal-only fields
    /// (`scroll`, `col_index`, `test_index`) sit at their defaults; the
    /// inspector's own scroll + column cursor live on `UiState`.
    pub detail: DetailView,
    /// The materialization tag (`MaterializationClass::tag`) for the head
    /// line — `Table` / `View` / `Source` / …
    pub tag: &'static str,
    /// Downstream EXPOSURE count — the third component of
    /// `impact_breakdown_for` the two-number `DetailView` drops. §4 shows it
    /// alongside the up/down counts.
    pub exposures: usize,
    /// The owner from `App::owner_meta_key`'s meta lookup (the config's key,
    /// default `"owner"`), resolved at build so the render layer never reads
    /// the config. `None` when the node carries no such meta entry.
    pub owner: Option<String>,
    /// `catalog.json`'s real warehouse type per cursor-addressable column
    /// (keyed by the declared/resolved name verbatim — the row lookup needs
    /// no normalization). Empty when the catalog doesn't cover this node.
    pub col_types: std::collections::BTreeMap<String, String>,
    /// The `run_results.json` row for this node as a display pair
    /// `(status word, optional seconds)` — `("success", Some(3.2))` renders
    /// `✓ success  3.2s`. `None` when the artifact doesn't cover the node.
    pub run: Option<(String, Option<f64>)>,
    /// This node's lint findings as `"{rule}: {message}"` lines (only
    /// findings whose `node_uid` IS this node — `related` uids are context
    /// for the OTHER node's violation, not this node's).
    pub lint: Vec<String>,
    /// The diff readout when a `--diff` baseline flags this node:
    /// `"+ added"` / `"~ modified: {reasons}"`. `None` when unchanged or no
    /// baseline is loaded.
    pub diff: Option<String>,
}

/// The SQL-preview modal payload, snapshotted when the modal opens so the render
/// layer never needs a [`Dag`](crate::Dag). MUST be `Eq` (`Mode` derives `Eq`),
/// so it holds only plain data — no f64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlView {
    pub model_id: String,
    pub name: String,
    /// The raw SQL, or a one-line placeholder for sources/seeds/missing.
    pub sql: String,
    /// The compiled (Jinja-expanded) SQL when the project was compiled — from
    /// the manifest's `compiled_code` or `target/compiled/`. `c` toggles the
    /// display between this and `sql`; `None` = nothing to toggle to.
    pub compiled: Option<String>,
    /// The compiled text was expanded before the source `.sql` last changed —
    /// the title shows `[compiled, stale]` instead of `[compiled]`.
    pub compiled_stale: bool,
    /// Which variant is displayed (`false` = raw). Opens on raw, except when
    /// the raw text is a placeholder and compiled exists — show something real.
    pub show_compiled: bool,
    /// The node's project-relative file path, echoed in the modal title (so
    /// the preview says WHICH file `o` would open). `None` when unknown.
    pub path: Option<String>,
    pub scroll: usize,
}

impl SqlView {
    /// The text currently displayed: compiled when `show_compiled` (and a
    /// compiled payload exists — the toggle arm guards this, the fallback is
    /// belt-and-braces), raw otherwise.
    pub fn active_sql(&self) -> &str {
        if self.show_compiled {
            self.compiled.as_deref().unwrap_or(&self.sql)
        } else {
            &self.sql
        }
    }

    /// The title tag naming the displayed variant: `[raw]`, `[compiled]`, or
    /// `[compiled, stale]`.
    pub fn variant_label(&self) -> &'static str {
        if self.show_compiled && self.compiled.is_some() {
            if self.compiled_stale {
                "compiled, stale"
            } else {
                "compiled"
            }
        } else {
            "raw"
        }
    }
}

/// The stats-dashboard modal payload, computed when the modal opens so the
/// render layer never needs a [`Dag`](crate::Dag). MUST be `Eq` (`Mode` derives
/// `Eq`): coverage is stored as integers (covered/total) and the percentage is
/// computed by integer math at render time — never an f64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsView {
    pub project: String,
    /// (resource_type, count), sorted — deterministic.
    pub by_resource_type: Vec<(String, usize)>,
    /// (materialization, count) over models, sorted.
    pub by_materialization: Vec<(String, usize)>,
    /// Test coverage as integers; render computes `pct = tested*100/total`.
    /// Base = the [`coverage_gap`](crate::coverage_gap) testable set
    /// (model | seed | snapshot), the SAME base the `t` lens and the status-bar
    /// `cov` segment use — the three readouts can never disagree.
    pub testable_total: usize,
    pub testable_tested: usize,
    /// Column-level test coverage (Issue 11): declared columns carrying ≥1
    /// attached test / declared columns, over the same root-project testable
    /// base — the SHARED [`column_coverage_counts`](crate::column_coverage_counts)
    /// numbers `dbtl docs`' README quotes, so the dashboard and the generated
    /// docs can never disagree.
    pub col_declared: usize,
    pub col_tested: usize,
    /// Top-5 hubs: (unique_id, name, parent+child degree), sorted desc. Answers
    /// "most directly-connected node"; distinct from `transitive_hubs`.
    pub top_hubs: Vec<(String, String, usize)>,
    /// Top-5 hubs by TRANSITIVE downstream-closure size (the
    /// [`Dag::downstream`](crate::Dag::downstream) blast radius, over ALL nodes —
    /// the same all-node base as `top_hubs`), sorted desc by count then
    /// `unique_id` asc; stored as (name, count). Answers "biggest blast radius"
    /// — a different question from `top_hubs`'s direct degree.
    pub transitive_hubs: Vec<(String, usize)>,
    /// EVERY orphan model name (model with zero kept parents AND zero kept
    /// children), sorted. Stored in full; the render caps the listing.
    pub orphan_models: Vec<String>,
    /// EVERY layer-violation edge as (parent name, child name) from
    /// [`layer_violation_edges`](crate::layer_violation_edges), sorted. Stored in
    /// full; the render caps the listing and shows the total `(N)`.
    pub layer_violations: Vec<(String, String)>,
    /// The static-lint worklist: `(rule name, sorted violating node names)` per
    /// rule that has ≥1 finding, in [`LintRule::ALL`](crate::LintRule) order —
    /// the SAME [`run_rules`](crate::run_rules) findings `dbtl check --lint`
    /// gates on, so the dashboard can never disagree with the CI gate. Empty
    /// when nothing violates.
    pub lint_findings: Vec<(String, Vec<String>)>,
    /// The longest dependency chain in the whole DAG (node names, upstream →
    /// downstream; deterministic tie-breaks by `unique_id`). Its `len()` is the
    /// graph's depth — the "how many hops can a change ripple" readout.
    pub critical_path: Vec<String>,
    /// The runtime-heaviest dependency chain under the loaded run_results
    /// (node names + a `"12.3s"` total): the critical path that actually costs
    /// wall-clock time, not just hops. `None` without run_results.
    pub critical_path_runtime: Option<(Vec<String>, String)>,
    /// `testable_total - testable_tested` (same coverage_gap base as above).
    pub untested_testable: usize,
    pub zero_downstream_models: usize,
    pub no_description_models: usize,
    /// Root-project models whose column lineage resolved through the Issue 07
    /// name-match fallback (SQL missing or unparseable) — the aggregate of the
    /// detail modal's per-model `[name-match]` tag. Computed by resolving every
    /// model once when the dashboard opens; the resolutions stay memoized, so
    /// later column browsing reuses them for free.
    pub cll_fallbacks: usize,
    pub scroll: usize,
}

/// The reviewer-shaped "PR Impact Pack" analysis — [`crate::PrImpact`], defined
/// in `dbtl-core` so `dbtl impact`/`dbtl check` share the same computation as
/// this modal. Snapshotted into [`DiffView`] at modal-open time (so the render
/// layer stays Dag-free) and re-read verbatim by the Markdown export — both
/// from the single `dbtl_core::diff_report`, so the modal, the file, the
/// yanked command, and the CI output can never disagree.
///
/// The baseline-diff modal payload, snapshotted from the App's
/// [`DagDiff`](crate::DagDiff) when the modal opens (names resolved, reasons
/// joined), so the render layer never needs a `Dag`. MUST be `Eq` (`Mode`
/// derives `Eq`) — plain strings and counts only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffView {
    /// Where the baseline came from (the `--diff` argument), for the title.
    pub baseline: String,
    /// The package scope both graphs were loaded under — the modal prints it
    /// as the `scope:` line so the screen matches the `D`→`e` export and the
    /// `dbtl impact` report (whose [`DiffReport::scope`](crate::DiffReport)
    /// field carries the same value).
    pub scope: PackageScope,
    /// Added nodes as `(name, resource_type)`, sorted by name.
    pub added: Vec<(String, String)>,
    /// Removed (baseline-only) nodes as `(name, resource_type)`, sorted.
    pub removed: Vec<(String, String)>,
    /// Modified nodes as `(name, joined reasons)`, sorted by name.
    pub modified: Vec<(String, String)>,
    /// Dependency edges present only in the current Dag, `(parent, child)` names.
    pub edges_added: Vec<(String, String)>,
    /// Dependency edges present only in the baseline, same shape.
    pub edges_removed: Vec<(String, String)>,
    /// The column-level findings (the "Column changes" section — what moved
    /// and what it breaks downstream), the same [`DiffReport::column_changes`]
    /// data the Markdown/JSON surfaces emit.
    pub column_changes: Vec<ColumnChange>,
    /// The reviewer-shaped PR impact analysis over the changed set (blast
    /// radius / exposures / CI command / risk flags), snapshotted at open.
    pub pr: PrImpact,
    pub scroll: usize,
}

/// The command-palette state: the live query and the selected candidate index.
/// The candidate LIST is derived on demand ([`App::palette_rows`], keyed off
/// [`BINDINGS`] — or the model list when the query is `@`-prefixed, §7), never
/// stored — so the palette can't drift from the keymap. No scroll field: the
/// render layer derives the scroll window from `selected`, so there is nothing
/// for the event loop to clamp (it falls through unchanged).
///
/// `selected` indexes the SELECTABLE rows only — section headers are
/// display-only chrome and never a target (the same rule the list pane's
/// hint/empty rows follow).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    /// The `+` cell's variant: Enter on an `@`-query match opens the model as
    /// a NEW tab instead of re-rooting the active one (§2 — a plain Ctrl-p
    /// palette's `@` search keeps the re-root semantics).
    pub new_tab: bool,
}

/// One row of the command palette's candidate list, computed by
/// `App::palette_rows` (the Dag-aware producer — plain data, so the modal and
/// the `RenderCtx` it travels through stay Dag-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteRow {
    /// A non-selectable section header (`Recent` / `Commands`) — display-only,
    /// never a selection target.
    Header(&'static str),
    /// A Selection-mode [`BINDINGS`] command. `help`/`keys` are the rendered
    /// text (the SAME formatting `help_lines` produces); `unavailable` is the
    /// shared `App::unavailable_reason` — `Some` greys the row, appends the
    /// reason, and turns Enter into a toast instead of a run.
    Command {
        action: Action,
        help: &'static str,
        keys: String,
        unavailable: Option<String>,
    },
    /// An `@`-search model result: Enter re-roots to `uid` (the "active tab
    /// root" the spec names — the current selection until tabs exist).
    Model { name: String, uid: String },
}

/// The `?` help overlay's state: the scroll offset, the `/`-filter, and the
/// mode it was opened OVER (`back` — `?` works inside the modals too, and
/// Esc/`q`/`?` peel back to exactly that mode, payload and all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpView {
    pub scroll: usize,
    /// The `/`-filter text; `Some` once `/` has been pressed (an empty string
    /// is a live filter that simply matches everything). `None` = no filter.
    pub filter: Option<String>,
    /// `true` while keys type INTO the filter — the palette/search rule:
    /// printable chars are text (`j`/`q`/`?` edit, they do not scroll/close).
    /// Enter commits (`editing=false` — the filter stays applied and plain
    /// keys return to scrolling); Esc clears the filter outright.
    pub editing: bool,
    /// `back.kind()` — the mode the help was opened over; its section lists
    /// FIRST in the layout (the spec's "開いた時点のモードの節を先頭に").
    pub from: ModeKind,
    /// The mode to restore when help closes — the overlay is a LAYER over
    /// whatever was up (a modal keeps its snapshot and reappears on Esc).
    pub back: Box<Mode>,
}

impl HelpView {
    pub fn new(back: Mode) -> Self {
        HelpView {
            scroll: 0,
            filter: None,
            editing: false,
            from: back.kind(),
            back: Box::new(back),
        }
    }
}

impl Default for HelpView {
    fn default() -> Self {
        Self::new(Mode::Selection)
    }
}

/// One row of the column-lineage modal (`Mode::Columns`, Issue 09 C): either a
/// section header/separator (`uid == None`, non-selectable) or a selectable
/// `model.column` entry — `Enter` on it re-roots to the entry's model. The
/// text is pre-rendered at open time so the render layer stays Dag-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRow {
    /// The row's display text (indent + `name.column` + an optional edge-kind
    /// tag), already formatted. Section headers use plain caps text.
    pub label: String,
    /// The node's `unique_id` this row's column lives on; `None` for
    /// headers/separators — the cursor (j/k) only lands on `Some` rows, and
    /// [`Action::ColumnJump`] re-roots to it when it names a model.
    pub uid: Option<String>,
    /// This row IS the traced column — accent-rendered when not the cursor row.
    pub traced: bool,
    /// The edge that brought this column onto the trace is inference-only
    /// (never saw [`crate::Confidence::Exact`]) — rendered faint.
    pub faint: bool,
}

impl ColumnRow {
    /// A non-selectable section header / separator row.
    pub fn header(label: String) -> Self {
        ColumnRow {
            label,
            uid: None,
            traced: false,
            faint: false,
        }
    }
}

/// The column-lineage modal payload (Issue 09 C), snapshotted when the modal
/// opens so the render layer never needs a [`Dag`](crate::Dag): the traced
/// column's upstream and downstream columns as a hop-indented tree, one
/// selectable row per column. MUST be `Eq` (`Mode` derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnView {
    /// The traced column's `model.column` label, echoed in the modal title.
    pub title: String,
    /// Header + entry rows in display order. `rows[i].uid.is_some()` = a
    /// selectable entry (the cursor lands only on those).
    pub rows: Vec<ColumnRow>,
    /// The selection cursor — an INDEX into `rows` (lands only on entry rows;
    /// opens on the traced column's own row).
    pub cursor: usize,
    pub scroll: usize,
}

impl ColumnView {
    /// Move the selection cursor one row in `dir` over SELECTABLE rows
    /// (`uid.is_some()`); headers and separators are skipped. Clamped at both
    /// ends, never wraps — the loop's `clamp_columns_scroll` follows it with a
    /// minimal pan.
    pub fn move_cursor(&mut self, dir: Direction) {
        if self.cursor >= self.rows.len() {
            return;
        }
        let step: isize = match dir {
            Direction::Up | Direction::Left => -1,
            Direction::Down | Direction::Right => 1,
        };
        let n = self.rows.len() as isize;
        let mut i = self.cursor as isize + step;
        while (0..n).contains(&i) {
            if self.rows[i as usize].uid.is_some() {
                self.cursor = i as usize;
                return;
            }
            i += step;
        }
    }
}

/// A notification's severity (Issue 08): drives the toast's TTL/border
/// colour, the centre's `level` column, and the `!N` unread-warning count
/// (only warnings count). `Info` is the default channel level; `Warning`
/// marks failures and load warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
}

impl NoticeLevel {
    /// The level column's label in the notification centre.
    pub fn label(self) -> &'static str {
        match self {
            NoticeLevel::Info => "info",
            NoticeLevel::Warning => "warn",
        }
    }
}

/// A `menu` annotation resolved per node-menu item: the label the row shows.
/// The menu never invents actions — every item resolves through the
/// [`BINDINGS`] row its [`MenuSlot`] names, so the action, its key label, and
/// its triggers stay the single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuMark {
    /// Show the row with this display label.
    Label(&'static str),
    /// The `Enter` row, whose honest label depends on the target: "Re-root
    /// here" when `Enter` would re-root to a different node, plain
    /// "Structure view" when the target is already the re-rooted node (the
    /// re-root is a no-op — the Enter really just opens the structure view).
    EnterContext,
}

/// One row inside the node context menu: either a separator rule or an
/// actionable item. Items carry NO action — `binding` indexes [`BINDINGS`]
/// (the row's action, trigger, and `key_label` are all read from there), so
/// the menu can never drift from the keymap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A rule between item groups.
    Separator,
    /// A selectable row: `binding` is the [`BINDINGS`] index the row was
    /// resolved from; `label` is the [`MenuMark`] text resolved against the
    /// target at open.
    Item { binding: usize, label: String },
}

/// Where the menu box anchors (screen-spec §9): the pointer cell it was
/// right-clicked on, or the target node's drawn box for the `a` keyboard
/// path. The box grows down-right of the anchor and flips on overflow —
/// the actual cells are computed by the size-aware layer (draw / hit-test).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAnchor {
    /// The screen cell the menu opened on (the right-click point).
    Cell { col: u16, row: u16 },
    /// The target node's lineage box — resolved to cells at draw/hit time
    /// (the event loop owns screen geometry).
    Node,
}

/// The node context menu's state (the `Mode::Menu` payload). Everything the
/// renderer needs is snapshotted here — the render layer never consults a
/// `Dag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    /// The `unique_id` every item acts on (the clicked / cursored node).
    pub target: String,
    /// The node's display name — the box's title.
    pub name: String,
    /// The rows (items + separators), already filtered for availability —
    /// e.g. a source node omits "SQL preview" because it has no SQL.
    pub items: Vec<MenuEntry>,
    /// The selection cursor: an index into `items`, always lands on `Item`
    /// rows.
    pub selected: usize,
    /// The mode restored on Esc/execute (`None` = Selection) — the same
    /// return mechanism `Mode::Yank` carries for nested overlays.
    pub back: Option<Box<Mode>>,
    /// Where the box anchors on screen.
    pub anchor: MenuAnchor,
}

/// One slot in the node menu's layout table ([`NODE_MENU_LAYOUT`]): the
/// ORDER and grouping only. Each item slot names the `(mode, action)` pair
/// its [`BINDINGS`] row carries — the action, key label, and triggers all
/// come from that row, so a layout reorder can never fork the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSlot {
    /// An actionable item resolved from the `(ModeKind, Action)` row.
    Item(ModeKind, Action),
    /// A group separator.
    Separator,
}

/// The node context menu's layout — the screen-spec §9 shape: open/view
/// actions, a rule, the yanks, a rule, the bookmarks/path toggles. This table
/// carries NO item data; `menu_layout_resolves` asserts every item slot names
/// a real `(mode, action)` [`BINDINGS`] row.
///
/// `Copy unique_id` resolves the `y` row from `ModeKind::Yank` (its trigger
/// is plain `y` — the second-key of the yank prefix); every other item names
/// its `ModeKind::Selection` row.
pub static NODE_MENU_LAYOUT: &[MenuSlot] = &[
    MenuSlot::Item(ModeKind::Selection, Action::DetailOpen),
    MenuSlot::Item(ModeKind::Selection, Action::SqlOpen),
    MenuSlot::Item(ModeKind::Selection, Action::OpenEditor),
    MenuSlot::Separator,
    MenuSlot::Item(ModeKind::Selection, Action::YankName),
    MenuSlot::Item(ModeKind::Yank, Action::YankId),
    MenuSlot::Item(ModeKind::Selection, Action::YankSql),
    MenuSlot::Item(ModeKind::Yank, Action::YankSelector),
    MenuSlot::Separator,
    MenuSlot::Item(ModeKind::Selection, Action::BookmarkToggle),
    MenuSlot::Item(ModeKind::Selection, Action::PathToggle),
];

/// The display label a menu slot resolves to — `re_roots` is whether the
/// target differs from the current re-rooted node (see
/// [`MenuMark::EnterContext`]).
pub(crate) fn menu_slot_label(mark: MenuMark, re_roots: bool) -> String {
    match mark {
        MenuMark::Label(l) => l.to_string(),
        MenuMark::EnterContext => {
            if re_roots {
                "Re-root here".to_string()
            } else {
                "Structure view".to_string()
            }
        }
    }
}

/// One logged notification (Issue 08): the wall-clock `HH:MM:SS` stamp applied
/// by the event loop — `App` and the reducer keep no clock — the severity, and
/// the text. Logged newest-last in `App::notifications` (capped at
/// [`App::NOTIFICATION_CAP`](crate::App::NOTIFICATION_CAP)) and snapshotted
/// into [`NotificationsView`] when the centre opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationEntry {
    pub stamp: String,
    pub level: NoticeLevel,
    pub text: String,
}

/// The notification-centre modal payload (Issue 08), snapshotted at open like
/// every other modal so the render layer never needs a [`Dag`](crate::Dag).
/// MUST be `Eq` (`Mode` derives `Eq`). `rows` are newest-first (the centre's
/// display order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationsView {
    pub rows: Vec<NotificationEntry>,
    pub scroll: usize,
}

/// The mark each layout slot displays — kept next to the layout so the label
/// choice lives in one place (the `MenuSlot` stays a pure `(mode, action)`
/// key into [`BINDINGS`]).
pub(crate) fn menu_slot_mark(action: Action) -> MenuMark {
    match action {
        Action::DetailOpen => MenuMark::EnterContext,
        Action::SqlOpen => MenuMark::Label("SQL preview"),
        Action::OpenEditor => MenuMark::Label("Open in editor"),
        Action::YankName => MenuMark::Label("Copy name"),
        Action::YankId => MenuMark::Label("Copy unique_id"),
        Action::YankSql => MenuMark::Label("Copy raw SQL"),
        Action::YankSelector => MenuMark::Label("Copy dbt selector"),
        Action::BookmarkToggle => MenuMark::Label("Bookmark"),
        Action::PathToggle => MenuMark::Label("Path anchor here"),
        other => panic!("menu_slot_mark: no label for {other:?}"),
    }
}

/// The interaction mode. Overlays are plain enum variants (not boxed trait
/// objects): adding one is a variant + a render arm.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Selection,
    Search(SearchState),
    Detail(DetailView),
    /// SQL source preview of a node (scrollable, syntax-coloured plain text).
    Sql(SqlView),
    /// Project stats dashboard (scrollable sections).
    Stats(StatsView),
    /// Baseline-diff summary (scrollable sections; needs a `--diff` baseline).
    Diff(DiffView),
    /// Column-lineage tree for one column (scrollable, Enter jumps to the
    /// row's model — Issue 09 C).
    Columns(ColumnView),
    /// Notification centre (`Ctrl-n`, Issue 08): the recent-notification log,
    /// scrollable, newest first.
    Notifications(NotificationsView),
    /// Command palette: a fuzzy finder over every Selection-mode command.
    Palette(PaletteState),
    /// The `y` yank-prefix which-key menu: any key dispatched here runs its
    /// yank action and closes the menu (one-shot); `Esc`/`q` cancel. `back`
    /// restores the overlay it was opened over (structure modal → `y` → Esc
    /// returns to the structure modal, not the panes); `sel` is the
    /// pointer-selectable row index (a click selects, a double-click runs).
    Yank {
        back: Option<Box<Mode>>,
        sel: usize,
    },
    /// The node context menu (Issue 06): right-click on a node or `a`.
    Menu(MenuState),
    /// The `?` keybindings overlay (scrollable, `/`-filterable; `back`
    /// restores the mode it was opened over — Issue 09).
    Help(HelpView),
    /// The home tab is ACTIVE (Issue 10, §3): the body draws the home screen
    /// instead of the three panes. NOT a modal — it is the idle mode while
    /// `tabs[0]` (the unclosable home tab) is active; overlays (help, stats,
    /// search) still stack on top and close back to it. The item cursor
    /// lives on `App::home_sel` so modal round-trips keep it.
    Home,
}

/// The mode discriminant, used for keymap lookup (the binding table keys on this,
/// not on the payload-carrying [`Mode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Selection,
    Search,
    Detail,
    Sql,
    Stats,
    Diff,
    Columns,
    Notifications,
    Palette,
    Yank,
    Menu,
    Help,
    Home,
}

impl Mode {
    pub fn kind(&self) -> ModeKind {
        match self {
            Mode::Selection => ModeKind::Selection,
            Mode::Search(_) => ModeKind::Search,
            Mode::Detail(_) => ModeKind::Detail,
            Mode::Sql(_) => ModeKind::Sql,
            Mode::Stats(_) => ModeKind::Stats,
            Mode::Diff(_) => ModeKind::Diff,
            Mode::Columns(_) => ModeKind::Columns,
            Mode::Notifications(_) => ModeKind::Notifications,
            Mode::Palette(_) => ModeKind::Palette,
            Mode::Yank { .. } => ModeKind::Yank,
            Mode::Menu(_) => ModeKind::Menu,
            Mode::Help(_) => ModeKind::Help,
            Mode::Home => ModeKind::Home,
        }
    }

    /// Whether pointer input is live in this mode: clicks route to the
    /// panes (Selection) or the modal/menu hit-test (everything else with a
    /// surface). `Search` keeps mouse inert — its query renders inline in
    /// the pane titles and typing owns the keys, so a stray click must not
    /// hijack the search.
    pub fn mouse_live(&self) -> bool {
        !matches!(self, Mode::Search(_))
    }
}

/// A single key that fires a binding. `Key` matches the `KeyCode` ignoring
/// modifiers (so `G` = Shift+g still matches); `Ctrl` matches Ctrl+char;
/// `Super` matches Cmd+char (delivered only by terminals speaking the kitty
/// keyboard protocol — `main` opts in at startup; a `Ctrl` fallback should
/// accompany every `Super` binding).
#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    Key(KeyCode),
    Ctrl(char),
    Super(char),
}

impl Trigger {
    fn matches(self, key: KeyEvent) -> bool {
        match self {
            // A plain-key trigger must NOT fire on a chorded press: without
            // this, Ctrl-d would match the `d` binding (table order decided
            // the winner, so Ctrl-d / Ctrl-u could never mean paging). SHIFT
            // stays allowed — capitals arrive as Char('G') + SHIFT.
            Trigger::Key(code) => {
                key.code == code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
            }
            Trigger::Ctrl(c) => {
                key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(c)
            }
            Trigger::Super(c) => {
                key.modifiers.contains(KeyModifiers::SUPER) && key.code == KeyCode::Char(c)
            }
        }
    }

    /// Short human label for the `?` help.
    fn label(self) -> String {
        match self {
            Trigger::Ctrl(c) => format!("Ctrl-{c}"),
            Trigger::Super(c) => format!("Cmd-{c}"),
            Trigger::Key(code) => match code {
                // Space prints as an invisible glyph; name it so `?` is legible.
                KeyCode::Char(' ') => "Space".into(),
                KeyCode::Char(c) => c.to_string(),
                KeyCode::Enter => "Enter".into(),
                KeyCode::Esc => "Esc".into(),
                KeyCode::Tab => "Tab".into(),
                KeyCode::BackTab => "S-Tab".into(),
                KeyCode::Backspace => "Bksp".into(),
                // ASCII names (arrow glyphs are ambiguous-width on CJK terminals).
                KeyCode::Up => "Up".into(),
                KeyCode::Down => "Down".into(),
                KeyCode::Left => "Left".into(),
                KeyCode::Right => "Right".into(),
                KeyCode::PageDown => "PgDn".into(),
                KeyCode::PageUp => "PgUp".into(),
                other => format!("{other:?}"),
            },
        }
    }
}

/// One keyboard binding: which mode it applies in, the keys that trigger it, the
/// resulting action, and the help text. The `?` overlay is generated from this.
pub struct KeyBinding {
    pub mode: ModeKind,
    pub triggers: &'static [Trigger],
    pub action: Action,
    pub help: &'static str,
    /// The home screen's Actions row (§3): `true` marks this row eligible for
    /// the home "Actions" line — the row's `key_label()`/`help` render there
    /// verbatim, and [`App::unavailable_reason`] still filters the unusable
    /// ones out (a baseline-less `D` never shows).
    pub home: bool,
    /// Status-line hint annotations (screen-spec §8): offer this row's action
    /// as a `{key} {desc}` hint while `ctx` is the on-screen context. Rows
    /// sharing `(ctx, prio, desc)` merge into ONE hint (`j` + `k` →
    /// `j/k move`); `prio` is the drop-priority — higher values shed first on
    /// a narrow line. An empty slice opts the row out of hints entirely.
    pub hints: &'static [Hint],
}

/// The on-screen context a status hint is offered in — the §8 table's "状況"
/// column. `Any` emits in every context (the `?` escape hatch every screen
/// keeps); the pane contexts are Selection-mode only and supersede each
/// other: a live column trace or the whole-graph overview replaces the pane
/// key story with its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HintCtx {
    /// Every context (modal modes only ever emit these).
    Any,
    /// The model list owns the focus.
    List,
    /// The lineage pane owns the focus.
    Lineage,
    /// A column trace is live (`h/l` step columns there).
    Traced,
    /// The whole-graph overview (`w`) — the browser-like home page.
    Overview,
    /// The inspector pane owns the focus (Issue 04's third pane).
    Inspector,
    /// The home tab is active (Issue 10; §8's `j/k move ⏎ open / search
    /// 1-9 tabs q quit` row).
    Home,
}

/// One hint annotation on a [`KeyBinding`] row (see [`KeyBinding::hints`]).
/// `desc` is the dim label after the accent key; `prio` orders the emitted
/// row left-to-right and decides the drop order (higher sheds first).
#[derive(Debug, Clone, Copy)]
pub struct Hint {
    pub prio: u8,
    pub ctx: HintCtx,
    pub desc: &'static str,
}

impl Hint {
    /// Shorthand constructors keep the one-line [`BINDINGS`] rows readable.
    pub const fn any(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Any,
            desc,
        }
    }
    /// Offer in the list-focus context.
    pub const fn list(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::List,
            desc,
        }
    }
    /// Offer in the lineage-focus context.
    pub const fn lineage(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Lineage,
            desc,
        }
    }
    /// Offer while a column trace is live.
    pub const fn traced(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Traced,
            desc,
        }
    }
    /// Offer in the whole-graph overview.
    pub const fn overview(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Overview,
            desc,
        }
    }
    /// Offer while the inspector pane owns the focus.
    pub const fn inspector(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Inspector,
            desc,
        }
    }
    /// Offer while the home tab is active.
    pub const fn home(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Home,
            desc,
        }
    }
}

/// One status-line hint's data (its cell range lives in
/// [`StatusSpan`](crate::ui::StatusSpan)): the accent key part, the dim
/// description, and the [`Action`] a click dispatches. `action: None` marks
/// the loop's transient readouts (splitter hover/drag — the pointer IS the
/// gesture, so the text teaches rather than buttons). `prio` is the layout's
/// drop rank: higher sheds first on a narrow line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusHint {
    /// The accent-coloured key label (or the whole text for a transient).
    pub key: String,
    /// The dim description after the key (empty for transients).
    pub desc: String,
    /// The action a left click dispatches (`None` = a readout, not a button).
    pub action: Option<Action>,
    /// The drop rank — the row's right-to-left shed order is by this field.
    pub prio: u8,
}

/// The contextual status-line hints for `mode`/`focus`/`traced`/`overview` —
/// generated from [`BINDINGS`] (the single source: key labels come from
/// [`KeyBinding::key_label`]'s primary trigger, actions are the rows' own),
/// so the status line can never teach a key that doesn't work. Emission is
/// priority-sorted; bindings sharing `(ctx, prio, desc)` merge into one hint
/// (the `j`/`k` pair → `j/k move`) whose click action survives only while
/// every member agrees on it (a direction family dispatches nothing). Only
/// the hint's OWN row is listed — the event loop may REPLACE this list with
/// a transient readout (splitter hover/drag).
pub fn status_hints(mode: ModeKind, focus: Focus, traced: bool, overview: bool) -> Vec<StatusHint> {
    // Which contexts emit. Modal modes are context-free (`Any` only). In
    // Selection the focused pane's ctx emits — except a live column trace or
    // the overview, which supersede the pane story with their own (traced
    // wins over overview: a trace pins a rooted graph).
    let ctxs: &[HintCtx] = match mode {
        ModeKind::Selection => {
            if traced {
                // A live trace supersedes even the `?` escape hatch — §8's
                // traced row is a self-contained story (`h/l`, `K`, `Esc`).
                &[HintCtx::Traced]
            } else if overview {
                &[HintCtx::Overview]
            } else {
                match focus {
                    Focus::List => &[HintCtx::Any, HintCtx::List],
                    Focus::RightPane => &[HintCtx::Any, HintCtx::Lineage],
                    Focus::Inspector => &[HintCtx::Any, HintCtx::Inspector],
                }
            }
        }
        // §8's home row: `j/k move  ⏎ open  / search  1-9 tabs  q quit`.
        ModeKind::Home => &[HintCtx::Any, HintCtx::Home],
        _ => &[HintCtx::Any],
    };
    // Group rows by (ctx, prio, desc). `BTreeMap` iteration IS the priority
    // sort. A group's click action survives only while every member agrees
    // on it — a merged family (`j/k move`, `h/l step column`, `1-9 tabs`)
    // names no single command, so it goes inert rather than firing an
    // arbitrary member (`1-9` jumping to tab 1 would be a quiet surprise).
    type Groups = std::collections::BTreeMap<
        (u8, HintCtx, &'static str),
        (usize, Vec<String>, Option<Action>),
    >;
    let mut groups: Groups = Groups::new();
    for (i, b) in BINDINGS.iter().enumerate() {
        if b.mode != mode {
            continue;
        }
        for h in b.hints {
            if !ctxs.contains(&h.ctx) {
                continue;
            }
            let key = b.triggers.first().map(|t| t.label()).unwrap_or_default();
            let e = groups
                .entry((h.prio, h.ctx, h.desc))
                .or_insert_with(|| (i, Vec::new(), Some(b.action)));
            e.0 = e.0.min(i);
            e.1.push(key);
            if e.2 != Some(b.action) {
                e.2 = None;
            }
        }
    }
    groups
        .into_iter()
        .map(|((prio, _ctx, desc), (_first, mut keys, action))| {
            keys.sort();
            keys.dedup();
            StatusHint {
                // Compact the merged key part: single-char members form a
                // contiguous run (`1`-`9` → `1-9`) or join bare (`hjkl`),
                // pairs keep the `/` convention, mixed shapes join `/`.
                key: compact_keys(&keys),
                desc: desc.to_string(),
                action,
                prio,
            }
        })
        .collect()
}

/// Join a merged hint's member key labels the §8 way: 3+ single-char labels
/// that form a contiguous ASCII range collapse to `a-z` (`1..9` → `1-9`), 3+
/// single chars join bare (`hjkl`), exactly two join `/` (`j/k`), and
/// anything else joins `/` (`Down/Up`). Deterministic — labels arrive sorted.
fn compact_keys(keys: &[String]) -> String {
    let singles: Option<Vec<u8>> = keys
        .iter()
        .map(|k| (k.len() == 1).then(|| k.as_bytes()[0]))
        .collect();
    match singles {
        Some(cs) if cs.len() >= 3 && cs.windows(2).all(|w| w[1] == w[0] + 1) => {
            format!("{}-{}", cs[0] as char, cs[cs.len() - 1] as char)
        }
        Some(cs) if cs.len() >= 3 => cs.iter().map(|&c| c as char).collect(),
        _ => keys.join("/"),
    }
}

impl KeyBinding {
    /// The binding's trigger labels joined with `/` (e.g. `"Ctrl-p/Cmd-p"`) — the
    /// SAME formatting [`help_lines`] uses, reused by the command palette's
    /// right-aligned key column. `Trigger::label` stays private; this is the one
    /// public accessor for it.
    pub fn key_label(&self) -> String {
        self.triggers
            .iter()
            .map(|t| t.label())
            .collect::<Vec<_>>()
            .join("/")
    }
}

use Action as A;
use KeyCode::{BackTab, Backspace, Char, Down, Enter, Esc, Left, PageDown, PageUp, Right, Tab, Up};
use ModeKind as M;
use Trigger::{Ctrl, Key, Super};

/// THE binding table — the single source of truth for dispatch AND help.
/// Kept as a one-row-per-binding table (rustfmt would explode it to ~6 lines each).
#[rustfmt::skip]
pub static BINDINGS: &[KeyBinding] = &[
    // ---- Selection mode ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('j')), Key(Down)], action: A::MoveDown, help: "down (list / lineage cursor)", hints: &[Hint::list(10, "move"), Hint::lineage(10, "move"), Hint::overview(10, "move"), Hint::inspector(10, "scroll")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('k')), Key(Up)], action: A::MoveUp, help: "up (list / lineage cursor)", hints: &[Hint::list(10, "move"), Hint::lineage(10, "move"), Hint::overview(10, "move"), Hint::inspector(10, "scroll")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('h')), Key(Left)], action: A::MoveLeft, help: "lineage cursor left (upstream)", hints: &[Hint::lineage(10, "move"), Hint::traced(10, "step column")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('l')), Key(Right)], action: A::MoveRight, help: "lineage cursor right (downstream)", hints: &[Hint::lineage(10, "move"), Hint::traced(10, "step column")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('g'))], action: A::JumpTop, help: "jump to first model (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('G'))], action: A::JumpBottom, help: "jump to last model (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Tab)], action: A::ToggleFocus, help: "switch list / lineage focus", hints: &[Hint::list(50, "lineage"), Hint::inspector(30, "list")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Super('b'), Ctrl('b')], action: A::ToggleListPane, help: "show / hide the model list pane", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Enter)], action: A::DetailOpen, help: "open structure / re-root to cursor / toggle +more", hints: &[Hint::list(20, "open"), Hint::lineage(20, "re-root"), Hint::overview(20, "open"), Hint::inspector(20, "trace column")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('/'))], action: A::SearchOpen, help: "search (list filter / lineage jump)", hints: &[Hint::list(30, "search"), Hint::overview(30, "search")], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "toggle this help", hints: &[Hint::any(90, "help")], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('o'))], action: A::OpenEditor, help: "open SQL in $EDITOR", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('y'))], action: A::YankPrefix, help: "yank prefix menu", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('Y'))], action: A::YankName, help: "copy model name", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('m'))], action: A::YankMermaid, help: "copy lineage as Mermaid", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('x'))], action: A::YankDot, help: "copy lineage as Graphviz DOT", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('r'))], action: A::Reload, help: "reload manifest", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('z'))], action: A::Recenter, help: "cursor home + re-center lineage", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('u'))], action: A::ToggleUpstream, help: "toggle upstream side", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('d'))], action: A::ToggleDownstream, help: "toggle downstream side", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('['))], action: A::DepthDecrease, help: "limit lineage depth (-)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char(']'))], action: A::DepthIncrease, help: "widen lineage depth (+)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('0'))], action: A::ResetView, help: "reset lineage view", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('b'))], action: A::HistoryBack, help: "jump back (re-root history)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('f'))], action: A::HistoryForward, help: "jump forward (re-root history)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('q')), Ctrl('c')], action: A::Quit, help: "quit", hints: &[Hint::overview(80, "quit")], home: false },
    // ---- new feature commands: placed AFTER `quit` so `quit` stays inside the
    // first help page at 100x40 (help_overlay_renders_keybindings_from_keymap).
    // Reorder these past `quit` only if that test's TestBackend height grows. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('s'))], action: A::SqlOpen, help: "preview SQL", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('S'))], action: A::StatsOpen, help: "stats dashboard", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('t'))], action: A::CycleLens, help: "cycle lineage lens", hints: &[Hint::lineage(50, "lens")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char(' '))], action: A::BookmarkToggle, help: "toggle bookmark", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('\''))], action: A::BookmarkCycle, help: "cycle bookmarks", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('.'))], action: A::SortCycle, help: "cycle list sort", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('M'))], action: A::ToggleMinimap, help: "toggle minimap", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('v'))], action: A::ToggleDensity, help: "toggle compact lineage", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('p'), Super('p')], action: A::PaletteOpen, help: "command palette", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('c'))], action: A::YankAscii, help: "copy lineage as text diagram", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('C'))], action: A::YankSql, help: "copy raw SQL", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('i'))], action: A::YankImpact, help: "copy impact report (Markdown)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('e'))], action: A::ExportLineage, help: "export lineage to <name>_lineage.txt", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(PageDown), Ctrl('d')], action: A::PageDown, help: "page down (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(PageUp), Ctrl('u')], action: A::PageUp, help: "page up (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('n'))], action: A::GapNext, help: "next untested model", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('N'))], action: A::GapPrev, help: "previous untested model", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('"'))], action: A::BookmarkCycleBack, help: "cycle bookmarks backward", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('H'))], action: A::LineageLeftmost, help: "lineage cursor to first column", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('L'))], action: A::LineageRightmost, help: "lineage cursor to last column", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('T'))], action: A::ToggleUntestedFilter, help: "filter list: untested only", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('*'))], action: A::ToggleBookmarkFilter, help: "filter list: bookmarked only", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('-'))], action: A::ToggleGroupFold, help: "fold/unfold model group", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('P'))], action: A::DbtParse, help: "run dbt parse (build manifest)", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('t')], action: A::ThemeCycle, help: "cycle color theme", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('D'))], action: A::DiffOpen, help: "diff vs baseline (--diff)", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('!'))], action: A::YankSelector, help: "copy dbt --select command", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('w'))], action: A::ToggleOverview, help: "toggle whole-graph overview", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('p'))], action: A::PathToggle, help: "anchor/clear two-node path", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('K'))], action: A::ColumnsOpen, help: "column lineage modal (traced column)", hints: &[Hint::traced(20, "column modal")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('a'))], action: A::MenuOpen, help: "node actions menu", hints: &[Hint::lineage(40, "actions")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Esc)], action: A::ColumnFocusExit, help: "exit column focus", hints: &[Hint::traced(30, "exit trace")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('<'))], action: A::PaneNarrow, help: "narrow pane 4 cols", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('>'))], action: A::PaneWiden, help: "widen pane 4 cols", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('='))], action: A::PaneWidthsReset, help: "reset pane widths", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('n')], action: A::NotificationsOpen, help: "notification center", hints: &[], home: false },
    // ---- tabs (Issue 03; §2): `O` opens the cursor's model in a new tab,
    // `X`/Ctrl-w closes, `{`/`}` cycle, `1`-`9` jump straight to a tab. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('O'))], action: A::TabOpen, help: "open node in new tab", hints: &[Hint::list(40, "new tab"), Hint::lineage(30, "new tab")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('X')), Ctrl('w')], action: A::TabClose, help: "close tab", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('{'))], action: A::TabPrev, help: "previous tab", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('}'))], action: A::TabNext, help: "next tab", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('1'))], action: A::TabActivate(1), help: "go to tab 1", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('2'))], action: A::TabActivate(2), help: "go to tab 2", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('3'))], action: A::TabActivate(3), help: "go to tab 3", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('4'))], action: A::TabActivate(4), help: "go to tab 4", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('5'))], action: A::TabActivate(5), help: "go to tab 5", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('6'))], action: A::TabActivate(6), help: "go to tab 6", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('7'))], action: A::TabActivate(7), help: "go to tab 7", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('8'))], action: A::TabActivate(8), help: "go to tab 8", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('9'))], action: A::TabActivate(9), help: "go to tab 9", hints: &[Hint::overview(40, "tabs")], home: false },
    // ---- inspector (Issue 04; §4): `I` cycles the pane's visibility pref. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('I'))], action: A::ToggleInspector, help: "inspector pane auto / on / off", hints: &[], home: false },
    // ---- home tab (Issue 10; §3): `~` jumps to the fixed left tab. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('~'))], action: A::HomeGo, help: "go to home tab", hints: &[], home: false },
    // ---- Yank prefix mode (`y` + one key): the second key is the SAME letter
    // the single-key binding always used, so muscle memory carries over as
    // `ym`/`yC`/…; `yy` copies the unique_id (what bare `y` used to do). ----
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('y'))], action: A::YankId, help: "copy unique_id", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('Y'))], action: A::YankName, help: "copy model name", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('m'))], action: A::YankMermaid, help: "copy lineage as Mermaid", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('x'))], action: A::YankDot, help: "copy lineage as Graphviz DOT", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('c'))], action: A::YankAscii, help: "copy lineage as text diagram", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('C'))], action: A::YankSql, help: "copy raw SQL", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('s'))], action: A::YankCompiledSql, help: "copy compiled SQL", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('i'))], action: A::YankImpact, help: "copy impact report (Markdown)", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('l'))], action: A::YankColumnLineage, help: "copy column lineage (Markdown)", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('!'))], action: A::YankSelector, help: "copy dbt --select command", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "cancel", hints: &[Hint::any(80, "cancel")], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    // ---- Search mode (printable chars are handled dynamically -> SearchType) ----
    KeyBinding { mode: M::Search, triggers: &[Key(Esc)], action: A::SearchCancel, help: "cancel search", hints: &[Hint::any(80, "cancel")], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Enter)], action: A::SearchConfirm, help: "confirm", hints: &[Hint::any(20, "confirm")], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Backspace)], action: A::SearchBackspace, help: "delete char", hints: &[], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Up)], action: A::SearchMove(Direction::Up), help: "previous match", hints: &[Hint::any(10, "match")], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Down)], action: A::SearchMove(Direction::Down), help: "next match", hints: &[Hint::any(10, "match")], home: false },
    // ---- Command palette (printable chars are handled dynamically -> SearchType;
    // the editing actions REUSE the Search* arms, routed by mode in apply_action) ----
    KeyBinding { mode: M::Palette, triggers: &[Key(Esc)], action: A::SearchCancel, help: "close palette", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Enter)], action: A::SearchConfirm, help: "run command", hints: &[Hint::any(20, "run")], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Backspace)], action: A::SearchBackspace, help: "delete char", hints: &[], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Up)], action: A::SearchMove(Direction::Up), help: "prev command", hints: &[Hint::any(10, "pick")], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Down)], action: A::SearchMove(Direction::Down), help: "next command", hints: &[Hint::any(10, "pick")], home: false },
    // ---- Detail modal ----
    KeyBinding { mode: M::Detail, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    // `?` opens help over the modal (HelpView.back restores it) — the
    // structure-modal section then leads the layout.
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    // Enter/`c` commits the column under the h/l cursor to column focus (the
    // arm closes the modal instead when the node has no columns — the classic
    // Enter-to-close survives there). While the tests section owns the cursor
    // (Tab), Enter re-roots to the relationships test's `to:` target instead.
    KeyBinding { mode: M::Detail, triggers: &[Key(Enter), Key(Char('c'))], action: A::ColumnFocusEnter, help: "trace column / jump to test target", hints: &[Hint::any(50, "trace")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Tab), Key(BackTab)], action: A::DetailSectionToggle, help: "columns / tests section", hints: &[Hint::any(40, "sections")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('K'))], action: A::ColumnsOpen, help: "column lineage modal (this column)", hints: &[Hint::any(60, "column modal")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('h')), Key(Char('H'))], action: A::DetailColumnMove(Direction::Up), help: "previous column / test", hints: &[Hint::any(30, "column")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('l')), Key(Char('L'))], action: A::DetailColumnMove(Direction::Down), help: "next column / test", hints: &[Hint::any(30, "column")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('y'))], action: A::YankPrefix, help: "yank prefix menu", hints: &[Hint::any(70, "yank")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('a'))], action: A::MenuOpen, help: "node actions menu", hints: &[Hint::any(75, "actions")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- SQL preview modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Sql, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('c'))], action: A::SqlToggleCompiled, help: "toggle raw / compiled SQL", hints: &[Hint::any(30, "compiled")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('a'))], action: A::MenuOpen, help: "node actions menu", hints: &[Hint::any(40, "actions")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Stats dashboard modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Stats, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Diff summary modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Diff, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('e'))], action: A::ExportPrImpact, help: "export PR impact report (Markdown)", hints: &[Hint::any(30, "export")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('y'))], action: A::YankPrSelector, help: "copy dbt build --select (column-narrowed)", hints: &[Hint::any(40, "copy selector")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('Y'))], action: A::YankPrSelectorWide, help: "copy dbt build --select (wide)", hints: &[], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Column lineage modal (Issue 09): j/k move the ROW cursor (a
    // selection the loop scroll-follows), d/u/g/G page the viewport like every
    // other modal, Enter re-roots to the row's model. ----
    KeyBinding { mode: M::Columns, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Enter)], action: A::ColumnJump, help: "jump to this model", hints: &[Hint::any(30, "open")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('j')), Key(Down)], action: A::ColumnRowMove(Direction::Down), help: "next column row", hints: &[Hint::any(10, "row")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('k')), Key(Up)], action: A::ColumnRowMove(Direction::Up), help: "previous column row", hints: &[Hint::any(10, "row")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Notification centre (Issue 08; reuses DetailClose + DetailScroll) ----
    // `Ctrl-n` opens it from Selection and toggles it closed while open.
    KeyBinding { mode: M::Notifications, triggers: &[Key(Esc), Key(Enter), Key(Char('q')), Ctrl('n')], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Node context menu (Issue 06): only nav / run / close live here —
    // an item's OWN shortcut (the trigger of the BINDINGS row its layout
    // slot names) is resolved by the dispatcher's item lookup, never a
    // fixed row. ----
    KeyBinding { mode: M::Menu, triggers: &[Key(Char('j')), Key(Down)], action: A::MenuMove(Direction::Down), help: "next item", hints: &[Hint::any(10, "move")], home: false },
    KeyBinding { mode: M::Menu, triggers: &[Key(Char('k')), Key(Up)], action: A::MenuMove(Direction::Up), help: "previous item", hints: &[Hint::any(10, "move")], home: false },
    KeyBinding { mode: M::Menu, triggers: &[Key(Enter)], action: A::MenuConfirm, help: "run the selected action", hints: &[Hint::any(20, "run")], home: false },
    KeyBinding { mode: M::Menu, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close menu", hints: &[Hint::any(80, "close")], home: false },
    // ---- Help overlay ----
    KeyBinding { mode: M::Help, triggers: &[Key(Esc), Key(Char('?')), Key(Char('q'))], action: A::HelpToggle, help: "close help", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // `/` opens the line filter. While the filter is being edited, printable
    // keys are TEXT (they route through the Search* editing actions — the same
    // reuse the palette/search rows use), so `?`/`q` close is suspended until
    // the filter is committed (Enter) or cleared (Esc); see dispatch().
    KeyBinding { mode: M::Help, triggers: &[Key(Char('/'))], action: A::SearchOpen, help: "filter help lines", hints: &[Hint::any(30, "filter")], home: false },
    // ---- Home tab (Issue 10; §3 操作): j/k move over model items, Enter
    // opens (activate-or-new-tab), `/` types into the search field, `Esc`
    // lives in the Search rows. The Actions-row commands rebind here so the
    // same keys work while the home tab is active. ----
    KeyBinding { mode: M::Home, triggers: &[Key(Char('j')), Key(Down)], action: A::HomeMove(Direction::Down), help: "next item", hints: &[Hint::home(10, "move")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('k')), Key(Up)], action: A::HomeMove(Direction::Up), help: "previous item", hints: &[Hint::home(10, "move")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Enter)], action: A::HomeOpen, help: "open item in a tab", hints: &[Hint::home(20, "open")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('/'))], action: A::SearchOpen, help: "search models", hints: &[Hint::home(30, "search")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('1'))], action: A::TabActivate(1), help: "go to tab 1", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('2'))], action: A::TabActivate(2), help: "go to tab 2", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('3'))], action: A::TabActivate(3), help: "go to tab 3", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('4'))], action: A::TabActivate(4), help: "go to tab 4", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('5'))], action: A::TabActivate(5), help: "go to tab 5", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('6'))], action: A::TabActivate(6), help: "go to tab 6", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('7'))], action: A::TabActivate(7), help: "go to tab 7", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('8'))], action: A::TabActivate(8), help: "go to tab 8", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('9'))], action: A::TabActivate(9), help: "go to tab 9", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('{'))], action: A::TabPrev, help: "previous tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('}'))], action: A::TabNext, help: "next tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('~'))], action: A::HomeGo, help: "go to home tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('X')), Ctrl('w')], action: A::TabClose, help: "close tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('w'))], action: A::ToggleOverview, help: "toggle whole-graph overview", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('S'))], action: A::StatsOpen, help: "stats dashboard", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('D'))], action: A::DiffOpen, help: "diff vs baseline (--diff)", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('P'))], action: A::DbtParse, help: "run dbt parse (build manifest)", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Ctrl('n')], action: A::NotificationsOpen, help: "notification center", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "toggle this help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('q')), Ctrl('c')], action: A::Quit, help: "quit", hints: &[Hint::home(80, "quit")], home: false },
];

/// Map `(Mode, KeyEvent)` to an [`Action`], or `None` if the key is unbound.
///
/// Ctrl-C always quits (any mode). While the help `/`-filter is being edited,
/// printable/Backspace/Enter keys route to the `Search*` editing actions BEFORE
/// the table. Then [`BINDINGS`] is consulted for the mode; finally, in Search
/// or Palette mode, any leftover plain printable char becomes
/// [`Action::SearchType`].
pub fn dispatch(mode: &Mode, key: KeyEvent) -> Option<Action> {
    // Global: Ctrl-C quits regardless of mode (never a printable char).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    // While the help `/`-filter is being edited, printable text types INTO it
    // and Backspace/Enter edit/commit it — the same "printable is text"
    // contract Search/Palette live by, reusing the Search* actions. This runs
    // BEFORE the BINDINGS table so `j`/`q`/`?`/`g`/`/` edit the filter instead
    // of scrolling or closing; Esc isn't printable, so it still reaches
    // HelpToggle (which the reducer reads as "clear the filter first").
    if let Mode::Help(h) = mode {
        if h.editing {
            match key.code {
                KeyCode::Char(c) => {
                    let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::SUPER);
                    if plain {
                        return Some(Action::SearchType(c));
                    }
                }
                KeyCode::Backspace => return Some(Action::SearchBackspace),
                KeyCode::Enter => return Some(Action::SearchConfirm),
                _ => {}
            }
        }
    }
    let mk = mode.kind();
    for b in BINDINGS {
        if b.mode == mk && b.triggers.iter().any(|t| t.matches(key)) {
            return Some(b.action);
        }
    }
    // Dynamic: in search OR the command palette, printable text (no Ctrl/Alt/
    // Super) edits the query — so a printable like 'q' filters and never quits.
    // Super-modified chars are commands, not text — they can actually arrive
    // once the kitty keyboard protocol is active.
    if mk == ModeKind::Search || mk == ModeKind::Palette {
        if let KeyCode::Char(c) = key.code {
            let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && !key.modifiers.contains(KeyModifiers::SUPER);
            if plain {
                return Some(Action::SearchType(c));
            }
        }
    }
    // Dynamic: inside the node menu, an item's OWN shortcut invokes it
    // directly (the trigger of the BINDINGS row its layout slot resolved to —
    // `s` runs SQL preview, `Space` bookmarks, without moving the cursor).
    // The nav/close keys all matched M::Menu rows above, so only item keys
    // reach here.
    if let Mode::Menu(ms) = mode {
        for (i, entry) in ms.items.iter().enumerate() {
            if let MenuEntry::Item { binding, .. } = entry {
                if BINDINGS[*binding].triggers.iter().any(|t| t.matches(key)) {
                    return Some(Action::MenuInvoke(i));
                }
            }
        }
    }
    None
}

/// The [`Action`] `Esc` produces in `mode` — resolved through [`BINDINGS`], so
/// an outside click sends literally the same action the key sends ("Esc と同
/// じ", screen-spec §6). `None` for modes with no `Esc` trigger.
pub fn esc_action(mode: &Mode) -> Option<Action> {
    let mk = mode.kind();
    BINDINGS
        .iter()
        .find(|b| {
            b.mode == mk
                && b.triggers
                    .iter()
                    .any(|t| matches!(t, Trigger::Key(KeyCode::Esc)))
        })
        .map(|b| b.action)
}

/// The which-key menu's BINDINGS rows (`ModeKind::Yank`, in table order) —
/// the single enumeration the which-key renderer, the event loop's click→row
/// map, and the tests all share, so a new `y<key>` binding rows every surface
/// at once.
pub fn yank_bindings() -> impl Iterator<Item = &'static KeyBinding> {
    BINDINGS.iter().filter(|b| b.mode == ModeKind::Yank)
}

/// One line of the `?` help overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpLine {
    pub mode: ModeKind,
    pub keys: String,
    pub desc: String,
}

/// Synthetic help for actions NOT bound to a fixed key (text input, mouse).
/// These are hand-maintained — add a row whenever a mode gains key-free
/// input behaviour so the `?` overlay documents it.
static DYNAMIC_HELP: &[(ModeKind, &str, &str)] = &[
    (ModeKind::Search, "a-z..", "type to filter / search"),
    (ModeKind::Palette, "a-z..", "type to filter commands"),
    (ModeKind::Palette, "@name", "search models / re-root"),
    (ModeKind::Help, "a-z..", "type to filter help lines"),
    (
        ModeKind::Selection,
        "left click",
        "re-root / move cursor to a node (release confirms)",
    ),
    (ModeKind::Selection, "right click", "node actions menu"),
    (
        ModeKind::Selection,
        "wheel",
        "scroll the pane under the cursor",
    ),
    (
        ModeKind::Menu,
        "item key",
        "an item's own key runs it directly",
    ),
    (
        ModeKind::Menu,
        "click item",
        "run the action (outside click closes)",
    ),
];

/// All help lines for the `?` overlay, derived from [`BINDINGS`] plus the
/// synthetic dynamic entries. Single source of truth → cannot drift from
/// [`dispatch`].
pub fn help_lines() -> Vec<HelpLine> {
    let mut lines: Vec<HelpLine> = BINDINGS
        .iter()
        .map(|b| HelpLine {
            mode: b.mode,
            keys: b.key_label(),
            desc: b.help.to_string(),
        })
        .collect();
    for (mode, keys, desc) in DYNAMIC_HELP {
        lines.push(HelpLine {
            mode: *mode,
            keys: (*keys).to_string(),
            desc: (*desc).to_string(),
        });
    }
    lines
}

/// The command-palette candidates for `query`: every Selection-mode binding
/// (excluding [`Action::PaletteOpen`] itself), filtered by a fuzzy subsequence
/// match. The match runs over the binding's `help` text first, falling back to
/// its key label ([`KeyBinding::key_label`]) — so both "minimap" and "Ctrl-p"
/// can find a row. An empty query lists ALL candidates.
///
/// Order is [`BINDINGS`] table order, preserved by the single-pass filter (no
/// sort) — so the palette is deterministic and mirrors the help layout. The
/// single source of palette candidates: it reads [`BINDINGS`], so the palette can
/// never drift from the keymap.
pub fn palette_candidates(query: &str) -> Vec<&'static KeyBinding> {
    // `match_indices` returns empty for BOTH an empty query AND a non-match, so
    // the empty-query case must be handled explicitly or the palette opens blank.
    let empty = query.trim().is_empty();
    BINDINGS
        .iter()
        .filter(|b| b.mode == ModeKind::Selection && b.action != Action::PaletteOpen)
        .filter(|b| {
            empty
                || !crate::model_list::match_indices(b.help, query).is_empty()
                || !crate::model_list::match_indices(&b.key_label(), query).is_empty()
        })
        .collect()
}

/// "This action reshapes or recentres the lineage without moving the
/// selection" — the event loop forces the anchor branch after it (the loop's
/// dispatched-key check AND the `Outcome.reanchor` flag a palette-confirmed
/// command carries: one list, no drift between the key and palette paths).
pub fn reanchors_lineage(action: Action) -> bool {
    matches!(
        action,
        Action::Recenter
            | Action::ToggleUpstream
            | Action::ToggleDownstream
            | Action::DepthDecrease
            | Action::DepthIncrease
            | Action::ResetView
            | Action::Reload
            | Action::ToggleListPane
            | Action::ToggleInspector
            | Action::DbtParse
            | Action::ToggleDensity
            | Action::ToggleOverview
            | Action::ColumnFocusEnter
            | Action::ColumnFocusExit
    )
}

/// Resolve a persisted palette-recent NAME back to its live [`BINDINGS`] row.
/// Recents are stored as `Action`'s `Debug` name (`"ToggleMinimap"`); a name no
/// longer present in the Selection-mode keymap (renamed/removed action) drops
/// rather than pointing at a dead row — the Recent section mirrors [`BINDINGS`],
/// it never becomes a second command source.
pub fn palette_binding_named(name: &str) -> Option<&'static KeyBinding> {
    BINDINGS.iter().find(|b| {
        b.mode == ModeKind::Selection
            && b.action != Action::PaletteOpen
            && format!("{:?}", b.action) == name
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn cmd(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SUPER)
    }
    fn search_mode() -> Mode {
        Mode::Search(SearchState {
            target: SearchTarget::List,
            query: String::new(),
            origin_uid: None,
            match_idx: 0,
            hist: None,
        })
    }
    fn help_mode() -> Mode {
        Mode::Help(HelpView::new(Mode::Selection))
    }
    fn detail_mode() -> Mode {
        Mode::Detail(DetailView {
            model_id: "m".into(),
            name: "m".into(),
            detail: NodeDetail::default(),
            tests: vec![],
            downstream_count: 0,
            upstream_count: 0,
            catalog: vec![],
            col_names: vec![],
            col_index: usize::MAX,
            test_index: usize::MAX,
            col_counts: std::collections::BTreeMap::new(),
            stale: false,
            name_match: false,
            scroll: 0,
        })
    }
    fn sql_mode() -> Mode {
        Mode::Sql(SqlView {
            model_id: "m".into(),
            name: "m".into(),
            sql: String::new(),
            compiled: None,
            compiled_stale: false,
            show_compiled: false,
            path: None,
            scroll: 0,
        })
    }
    fn palette_mode() -> Mode {
        Mode::Palette(PaletteState::default())
    }
    fn yank_mode() -> Mode {
        Mode::Yank { back: None, sel: 0 }
    }
    fn menu_mode() -> Mode {
        Mode::Menu(MenuState {
            target: "m".into(),
            name: "m".into(),
            items: vec![MenuEntry::Item {
                binding: 0,
                label: "item".into(),
            }],
            selected: 0,
            back: None,
            anchor: MenuAnchor::Cell { col: 0, row: 0 },
        })
    }
    fn diff_mode() -> Mode {
        Mode::Diff(DiffView::default())
    }
    fn columns_mode() -> Mode {
        Mode::Columns(ColumnView {
            title: "m.c".into(),
            rows: vec![],
            cursor: usize::MAX,
            scroll: 0,
        })
    }
    fn notifications_mode() -> Mode {
        Mode::Notifications(NotificationsView {
            rows: vec![],
            scroll: 0,
        })
    }
    fn stats_mode() -> Mode {
        Mode::Stats(StatsView {
            project: String::new(),
            by_resource_type: vec![],
            by_materialization: vec![],
            testable_total: 0,
            testable_tested: 0,
            col_declared: 0,
            col_tested: 0,
            top_hubs: vec![],
            transitive_hubs: vec![],
            orphan_models: vec![],
            layer_violations: vec![],
            lint_findings: vec![],
            critical_path: vec![],
            critical_path_runtime: None,
            untested_testable: 0,
            zero_downstream_models: 0,
            no_description_models: 0,
            cll_fallbacks: 0,
            scroll: 0,
        })
    }

    // ---- Selection dispatch maps the core keys to their actions ----

    #[test]
    fn selection_legacy_keys_map_to_expected_actions() {
        let sel = Mode::Selection;
        assert_eq!(dispatch(&sel, press(Char('j'))), Some(Action::MoveDown));
        assert_eq!(dispatch(&sel, press(KeyCode::Down)), Some(Action::MoveDown));
        assert_eq!(dispatch(&sel, press(Char('k'))), Some(Action::MoveUp));
        assert_eq!(dispatch(&sel, press(Char('h'))), Some(Action::MoveLeft));
        assert_eq!(dispatch(&sel, press(Char('l'))), Some(Action::MoveRight));
        assert_eq!(dispatch(&sel, press(Char('g'))), Some(Action::JumpTop));
        assert_eq!(dispatch(&sel, press(Char('G'))), Some(Action::JumpBottom));
        assert_eq!(
            dispatch(&sel, press(KeyCode::Tab)),
            Some(Action::ToggleFocus)
        );
        assert_eq!(dispatch(&sel, press(Char('q'))), Some(Action::Quit));
        assert_eq!(dispatch(&sel, ctrl('c')), Some(Action::Quit));
        assert_eq!(dispatch(&sel, press(Char('\\'))), None, "unbound key");
    }

    #[test]
    fn t_cycles_the_lineage_lens_in_selection_but_is_text_in_search() {
        // 't' in Selection cycles the lineage lens; in Search it is query text
        // (the modal text-input footgun: a printable must never command).
        assert_eq!(
            dispatch(&Mode::Selection, press(Char('t'))),
            Some(Action::CycleLens)
        );
        assert_eq!(
            dispatch(&search_mode(), press(Char('t'))),
            Some(Action::SearchType('t')),
            "'t' in search is text, not the lens cycle"
        );
    }

    #[test]
    fn p_runs_dbt_parse_in_selection_but_is_text_in_search() {
        // 'P' (capital — arrives as Char('P'), SHIFT allowed by Trigger::Key)
        // requests a dbt parse; in Search it must stay query text.
        assert_eq!(
            dispatch(&Mode::Selection, press(Char('P'))),
            Some(Action::DbtParse)
        );
        assert_eq!(
            dispatch(&search_mode(), press(Char('P'))),
            Some(Action::SearchType('P')),
            "'P' in search is text, not a dbt parse"
        );
    }

    #[test]
    fn bookmark_and_sort_keys_dispatch_in_selection() {
        let sel = Mode::Selection;
        assert_eq!(
            dispatch(&sel, press(Char(' '))),
            Some(Action::BookmarkToggle),
            "Space toggles a bookmark in Selection"
        );
        assert_eq!(
            dispatch(&sel, press(Char('\''))),
            Some(Action::BookmarkCycle),
            "' cycles bookmarks"
        );
        assert_eq!(
            dispatch(&sel, press(Char('.'))),
            Some(Action::SortCycle),
            ". cycles the list sort"
        );
        // In Search mode, Space (and ' / .) are query text, never commands.
        let m = search_mode();
        assert_eq!(
            dispatch(&m, press(Char(' '))),
            Some(Action::SearchType(' ')),
            "Space in search is text, not a bookmark toggle"
        );
        assert_eq!(
            dispatch(&m, press(Char('.'))),
            Some(Action::SearchType('.'))
        );
    }

    #[test]
    fn space_label_is_named_not_blank() {
        // The Space trigger must render a legible key in `?` help, not an empty
        // string (it would print as an invisible glyph).
        assert_eq!(Trigger::Key(Char(' ')).label(), "Space");
        assert_eq!(Trigger::Key(Char('\'')).label(), "'");
        assert_eq!(Trigger::Key(Char('.')).label(), ".");
    }

    #[test]
    fn cmd_b_and_ctrl_b_toggle_the_list_pane() {
        let sel = Mode::Selection;
        assert_eq!(dispatch(&sel, cmd('b')), Some(Action::ToggleListPane));
        assert_eq!(dispatch(&sel, ctrl('b')), Some(Action::ToggleListPane));
        // Plain 'b' stays the history-back binding — modifiers matter.
        assert_eq!(dispatch(&sel, press(Char('b'))), Some(Action::HistoryBack));
    }

    // ---- modal text-input footgun: printable in Search is TEXT, not commands ----

    #[test]
    fn search_printable_keys_are_text_not_commands() {
        let m = search_mode();
        for c in ['q', 'g', 'j', 'G', 'a', '/'] {
            assert_eq!(
                dispatch(&m, press(Char(c))),
                Some(Action::SearchType(c)),
                "'{c}' in search must be text, not a command"
            );
        }
        assert_eq!(dispatch(&m, press(Esc)), Some(Action::SearchCancel));
        assert_eq!(dispatch(&m, press(Enter)), Some(Action::SearchConfirm));
        assert_eq!(
            dispatch(&m, press(Backspace)),
            Some(Action::SearchBackspace)
        );
        // Ctrl-c still quits even in search (never a printable char).
        assert_eq!(dispatch(&m, ctrl('c')), Some(Action::Quit));
        // A Super-modified char is a command chord, not query text.
        assert_eq!(dispatch(&m, cmd('b')), None);
    }

    #[test]
    fn overlay_q_closes_overlay_not_app() {
        // 'q' must CLOSE the overlay, not quit the application.
        assert_eq!(
            dispatch(&detail_mode(), press(Char('q'))),
            Some(Action::DetailClose)
        );
        assert_eq!(
            dispatch(&help_mode(), press(Char('q'))),
            Some(Action::HelpToggle)
        );
        assert_eq!(
            dispatch(&detail_mode(), press(Esc)),
            Some(Action::DetailClose)
        );
    }

    #[test]
    fn help_editing_printable_keys_are_text_not_commands() {
        // While the `/`-filter is being edited, printable chars are TEXT —
        // they must never reach the Help-mode BINDINGS rows (`q`/`?` close,
        // `j`/`g` scroll, `/` re-arms). The editing pre-emption in dispatch
        // is what stands between the filter and those commands.
        let editing = Mode::Help(HelpView {
            editing: true,
            filter: Some("co".to_string()),
            ..HelpView::new(Mode::Selection)
        });
        for c in ['q', '?', 'j', 'g', '/', 'G', 'k'] {
            assert_eq!(
                dispatch(&editing, press(Char(c))),
                Some(Action::SearchType(c)),
                "'{c}' while editing the help filter must be text, not a command"
            );
        }
        assert_eq!(
            dispatch(&editing, press(Backspace)),
            Some(Action::SearchBackspace)
        );
        assert_eq!(
            dispatch(&editing, press(Enter)),
            Some(Action::SearchConfirm)
        );
        // Esc is not printable: it still reaches the close binding — the
        // reducer reads it as "clear the filter first".
        assert_eq!(dispatch(&editing, press(Esc)), Some(Action::HelpToggle));
        // And NOT editing: the printable chars ARE commands again.
        let idle = help_mode();
        assert_eq!(dispatch(&idle, press(Char('q'))), Some(Action::HelpToggle));
        assert_eq!(
            dispatch(&idle, press(Char('j'))),
            Some(Action::DetailScroll(Direction::Down))
        );
    }

    // ---- drift guard: help and dispatch can never diverge ----

    /// Exhaustive categorization. Adding an `Action` variant without an arm here
    /// is a COMPILE error, forcing the author to declare whether it is key-bound
    /// (must appear in BINDINGS) or dynamic (text/mouse).
    fn is_key_bound(a: &Action) -> bool {
        match a {
            Action::MoveDown
            | Action::MoveUp
            | Action::MoveLeft
            | Action::MoveRight
            | Action::JumpTop
            | Action::JumpBottom
            | Action::ToggleFocus
            | Action::ToggleListPane
            | Action::Quit
            | Action::DetailOpen
            | Action::DetailClose
            | Action::DetailScroll(_)
            | Action::SearchOpen
            | Action::SearchBackspace
            | Action::SearchCancel
            | Action::SearchConfirm
            | Action::SearchMove(_)
            | Action::PaletteOpen
            | Action::HelpToggle
            | Action::Recenter
            | Action::OpenEditor
            | Action::YankId
            | Action::YankName
            | Action::YankMermaid
            | Action::YankDot
            | Action::Reload
            | Action::ToggleUpstream
            | Action::ToggleDownstream
            | Action::DepthDecrease
            | Action::DepthIncrease
            | Action::ResetView
            | Action::HistoryBack
            | Action::HistoryForward
            | Action::SqlOpen
            | Action::SqlToggleCompiled
            | Action::StatsOpen
            | Action::CycleLens
            | Action::BookmarkToggle
            | Action::BookmarkCycle
            | Action::SortCycle
            | Action::ToggleMinimap
            | Action::ToggleDensity
            | Action::ToggleOverview
            | Action::PathToggle
            | Action::YankAscii
            | Action::YankSql
            | Action::YankCompiledSql
            | Action::YankColumnLineage
            | Action::YankImpact
            | Action::YankSelector
            | Action::YankPrefix
            | Action::ExportLineage
            | Action::PageDown
            | Action::PageUp
            | Action::DetailScrollPage(_)
            | Action::DetailScrollHome
            | Action::DetailScrollEnd
            | Action::DetailColumnMove(_)
            | Action::GapNext
            | Action::GapPrev
            | Action::BookmarkCycleBack
            | Action::LineageLeftmost
            | Action::LineageRightmost
            | Action::ToggleUntestedFilter
            | Action::ToggleBookmarkFilter
            | Action::ToggleGroupFold
            | Action::DbtParse
            | Action::ThemeCycle
            | Action::DiffOpen
            | Action::ExportPrImpact
            | Action::YankPrSelector
            | Action::YankPrSelectorWide
            | Action::ColumnFocusEnter
            | Action::ColumnFocusExit
            | Action::ColumnsOpen
            | Action::ColumnRowMove(_)
            | Action::ColumnJump
            | Action::PaneNarrow
            | Action::PaneWiden
            | Action::PaneWidthsReset
            | Action::DetailSectionToggle
            | Action::NotificationsOpen
            | Action::MenuOpen
            | Action::MenuMove(_)
            | Action::MenuConfirm
            | Action::TabOpen
            | Action::TabClose
            | Action::TabPrev
            | Action::TabNext
            | Action::TabActivate(_)
            | Action::HomeGo
            | Action::HomeMove(_)
            | Action::HomeOpen
            | Action::ToggleInspector => true,
            // Dynamic: produced without a fixed key binding (text input, a
            // resolved menu item, mouse row picks/invokes). Mouse is handled
            // in the event loop (size-aware), not via the keymap.
            Action::SearchType(_)
            | Action::MenuInvoke(_)
            | Action::MenuSelect(_)
            | Action::YankPick(_)
            | Action::YankInvoke(_)
            | Action::PalettePick(_)
            | Action::ColumnPick(_)
            | Action::InspectorColumnPick(_)
            | Action::HomePick(_)
            | Action::SetLens(_) => false,
        }
    }

    #[test]
    fn every_binding_has_help_and_round_trips() {
        for b in BINDINGS {
            assert!(
                !b.help.is_empty(),
                "binding for {:?} has empty help",
                b.action
            );
            // Each trigger dispatches to this binding's action in its mode.
            let mode = match b.mode {
                ModeKind::Selection => Mode::Selection,
                ModeKind::Search => search_mode(),
                ModeKind::Detail => detail_mode(),
                ModeKind::Sql => sql_mode(),
                ModeKind::Stats => stats_mode(),
                ModeKind::Diff => diff_mode(),
                ModeKind::Columns => columns_mode(),
                ModeKind::Notifications => notifications_mode(),
                ModeKind::Palette => palette_mode(),
                ModeKind::Yank => yank_mode(),
                ModeKind::Menu => menu_mode(),
                ModeKind::Help => help_mode(),
                ModeKind::Home => Mode::Home,
            };
            for t in b.triggers {
                let key = match t {
                    Trigger::Key(c) => press(*c),
                    Trigger::Ctrl(c) => ctrl(*c),
                    Trigger::Super(c) => cmd(*c),
                };
                assert_eq!(
                    dispatch(&mode, key),
                    Some(b.action),
                    "trigger {:?} should dispatch to {:?}",
                    t,
                    b.action
                );
            }
        }
    }

    #[test]
    fn key_bound_actions_appear_in_bindings() {
        // Every action the categorizer calls "key bound" must actually be in the
        // table — so a new bound action without a BINDINGS row fails here.
        let samples = [
            Action::MoveDown,
            Action::MoveUp,
            Action::MoveLeft,
            Action::MoveRight,
            Action::JumpTop,
            Action::JumpBottom,
            Action::ToggleFocus,
            Action::ToggleListPane,
            Action::Quit,
            Action::DetailOpen,
            Action::DetailClose,
            Action::DetailScroll(Direction::Down),
            Action::SearchOpen,
            Action::SearchBackspace,
            Action::SearchCancel,
            Action::SearchConfirm,
            Action::SearchMove(Direction::Down),
            Action::PaletteOpen,
            Action::HelpToggle,
            Action::Recenter,
            Action::OpenEditor,
            Action::YankId,
            Action::YankName,
            Action::YankMermaid,
            Action::YankDot,
            Action::Reload,
            Action::ToggleUpstream,
            Action::ToggleDownstream,
            Action::DepthDecrease,
            Action::DepthIncrease,
            Action::ResetView,
            Action::HistoryBack,
            Action::HistoryForward,
            Action::SqlOpen,
            Action::SqlToggleCompiled,
            Action::StatsOpen,
            Action::CycleLens,
            Action::BookmarkToggle,
            Action::BookmarkCycle,
            Action::SortCycle,
            Action::ToggleMinimap,
            Action::ToggleDensity,
            Action::ToggleOverview,
            Action::PathToggle,
            Action::YankAscii,
            Action::YankSql,
            Action::YankCompiledSql,
            Action::YankColumnLineage,
            Action::YankImpact,
            Action::YankSelector,
            Action::YankPrefix,
            Action::ExportLineage,
            Action::PageDown,
            Action::PageUp,
            Action::DetailScrollPage(Direction::Down),
            Action::DetailScrollHome,
            Action::DetailScrollEnd,
            Action::DetailColumnMove(Direction::Down),
            Action::GapNext,
            Action::GapPrev,
            Action::BookmarkCycleBack,
            Action::LineageLeftmost,
            Action::LineageRightmost,
            Action::ToggleUntestedFilter,
            Action::ToggleBookmarkFilter,
            Action::ToggleGroupFold,
            Action::DbtParse,
            Action::ThemeCycle,
            Action::DiffOpen,
            Action::ExportPrImpact,
            Action::YankPrSelector,
            Action::YankPrSelectorWide,
            Action::ColumnFocusEnter,
            Action::ColumnFocusExit,
            Action::ColumnsOpen,
            Action::ColumnRowMove(Direction::Down),
            Action::ColumnJump,
            Action::NotificationsOpen,
            Action::MenuOpen,
            Action::MenuMove(Direction::Down),
            Action::MenuConfirm,
        ];
        for a in samples {
            assert!(
                is_key_bound(&a),
                "{a:?} categorized as dynamic but listed as bound"
            );
            assert!(
                BINDINGS.iter().any(|b| b.action == a),
                "{a:?} is key-bound but missing from BINDINGS"
            );
        }
        // The dynamic text-input action must NOT be in the table.
        let dynamic = Action::SearchType('x');
        assert!(!is_key_bound(&dynamic));
        assert!(
            BINDINGS.iter().all(|b| b.action != dynamic),
            "SearchType is dynamic, not a fixed binding"
        );
        // The dynamic menu-invoke action must NOT be in the table either —
        // it is produced by the dispatcher's item lookup and mouse clicks.
        let dynamic = Action::MenuInvoke(0);
        assert!(!is_key_bound(&dynamic));
        assert!(
            BINDINGS.iter().all(|b| b.action != dynamic),
            "MenuInvoke is dynamic, not a fixed binding"
        );
    }

    // ---- command palette ----

    #[test]
    fn column_focus_keys_dispatch() {
        // Issue 09's keymap: `K` opens the column modal from Selection or the
        // structure modal, Enter/`c` on a Detail column cursor commits it to
        // the trace, Esc exits the trace in Selection (and closes the modal
        // like every other overlay), and the modal rides ColumnRowMove/
        // ColumnJump + the shared scroll chords.
        assert_eq!(
            dispatch(&Mode::Selection, press(KeyCode::Char('K'))),
            Some(Action::ColumnsOpen)
        );
        assert_eq!(
            dispatch(&Mode::Selection, press(KeyCode::Esc)),
            Some(Action::ColumnFocusExit)
        );
        let dv = detail_mode();
        assert_eq!(
            dispatch(&dv, press(KeyCode::Enter)),
            Some(Action::ColumnFocusEnter)
        );
        assert_eq!(
            dispatch(&dv, press(KeyCode::Char('c'))),
            Some(Action::ColumnFocusEnter)
        );
        assert_eq!(
            dispatch(&dv, press(KeyCode::Char('K'))),
            Some(Action::ColumnsOpen)
        );
        let cv = columns_mode();
        assert_eq!(
            dispatch(&cv, press(KeyCode::Char('j'))),
            Some(Action::ColumnRowMove(Direction::Down))
        );
        assert_eq!(
            dispatch(&cv, press(KeyCode::Char('k'))),
            Some(Action::ColumnRowMove(Direction::Up))
        );
        assert_eq!(
            dispatch(&cv, press(KeyCode::Enter)),
            Some(Action::ColumnJump)
        );
        assert_eq!(
            dispatch(&cv, ctrl('d')),
            Some(Action::DetailScrollPage(Direction::Down))
        );
        assert_eq!(
            dispatch(&cv, press(KeyCode::Esc)),
            Some(Action::DetailClose)
        );
    }

    #[test]
    fn ctrl_p_and_cmd_p_open_the_palette() {
        let sel = Mode::Selection;
        assert_eq!(dispatch(&sel, ctrl('p')), Some(Action::PaletteOpen));
        assert_eq!(dispatch(&sel, cmd('p')), Some(Action::PaletteOpen));
    }

    #[test]
    fn tab_keys_dispatch_in_selection_mode() {
        // O/X/Ctrl-w/{/}/1-9 — the §3 tab keymap (uppercase for open/close so
        // the lowercase row stays free; digits are direct tab activation).
        let sel = Mode::Selection;
        assert_eq!(dispatch(&sel, press(Char('O'))), Some(Action::TabOpen));
        assert_eq!(dispatch(&sel, press(Char('X'))), Some(Action::TabClose));
        assert_eq!(dispatch(&sel, ctrl('w')), Some(Action::TabClose));
        assert_eq!(dispatch(&sel, press(Char('{'))), Some(Action::TabPrev));
        assert_eq!(dispatch(&sel, press(Char('}'))), Some(Action::TabNext));
        for n in 1..=9u8 {
            let c = char::from(b'0' + n);
            assert_eq!(
                dispatch(&sel, press(Char(c))),
                Some(Action::TabActivate(n)),
                "'{c}' must activate tab {n}"
            );
        }
        // `0` is not a tab key — it keeps its ResetView binding.
        assert_eq!(dispatch(&sel, press(Char('0'))), Some(Action::ResetView));
        // Tab keys are Selection-mode commands, not text — inside the palette
        // they filter the query instead.
        let m = palette_mode();
        assert_eq!(
            dispatch(&m, press(Char('O'))),
            Some(Action::SearchType('O'))
        );
        assert_eq!(
            dispatch(&m, press(Char('1'))),
            Some(Action::SearchType('1'))
        );
    }

    #[test]
    fn ctrl_t_cycles_the_theme_and_stays_distinct_from_plain_t() {
        // Ctrl-t is the theme cycle; plain `t` stays the lens cycle (the
        // `Trigger::Key`-requires-no-Ctrl rule is what keeps them apart).
        let sel = Mode::Selection;
        assert_eq!(dispatch(&sel, ctrl('t')), Some(Action::ThemeCycle));
        assert_eq!(dispatch(&sel, press(Char('t'))), Some(Action::CycleLens));
        // In text-input modes Ctrl-t is unbound (never typed text).
        assert_eq!(dispatch(&search_mode(), ctrl('t')), None);
        assert_eq!(dispatch(&palette_mode(), ctrl('t')), None);
    }

    #[test]
    fn palette_printable_keys_are_text_not_commands() {
        // The same modal-text-input footgun as Search: a printable in the palette
        // FILTERS, never commands. 'q' must not quit; 't' must not cycle the lens;
        // Space must not toggle a bookmark.
        let m = palette_mode();
        for c in ['q', 't', ' ', 'a', '/'] {
            assert_eq!(
                dispatch(&m, press(Char(c))),
                Some(Action::SearchType(c)),
                "'{c}' in the palette must be text, not a command"
            );
        }
        // Esc cancels (closes the palette); Enter runs; Backspace edits.
        assert_eq!(dispatch(&m, press(Esc)), Some(Action::SearchCancel));
        assert_eq!(dispatch(&m, press(Enter)), Some(Action::SearchConfirm));
        assert_eq!(
            dispatch(&m, press(Backspace)),
            Some(Action::SearchBackspace)
        );
        // Ctrl-c still quits even in the palette (never a printable char).
        assert_eq!(dispatch(&m, ctrl('c')), Some(Action::Quit));
        // A Super-modified char is a command chord, not query text.
        assert_eq!(dispatch(&m, cmd('p')), None);
    }

    #[test]
    fn palette_candidates_empty_query_lists_all_selection_commands_minus_open() {
        let all = palette_candidates("");
        // Every Selection-mode binding except PaletteOpen itself.
        let expected = BINDINGS
            .iter()
            .filter(|b| b.mode == ModeKind::Selection && b.action != Action::PaletteOpen)
            .count();
        assert_eq!(
            all.len(),
            expected,
            "empty query lists every Selection command"
        );
        assert!(
            all.iter().all(|b| b.mode == ModeKind::Selection),
            "only Selection-mode bindings are candidates"
        );
        assert!(
            all.iter().all(|b| b.action != Action::PaletteOpen),
            "the palette opener excludes itself"
        );
    }

    #[test]
    fn palette_candidates_fuzzy_filters_by_help_text() {
        // "lens" is a subsequence of "cycle lineage lens" → the CycleLens row.
        let hits = palette_candidates("lens");
        assert!(
            hits.iter().any(|b| b.action == Action::CycleLens),
            "'lens' finds the cycle-lens command"
        );
        assert!(
            hits.iter()
                .all(|b| crate::model_list::name_matches_query(b.help, "lens")
                    || crate::model_list::name_matches_query(&b.key_label(), "lens")),
            "every hit matches the query"
        );
        // A query no command satisfies yields nothing (no panic).
        assert!(palette_candidates("zzzqqq").is_empty());
    }

    #[test]
    fn palette_candidates_preserve_bindings_table_order() {
        // Deterministic: candidates appear in BINDINGS order (no sort).
        let all = palette_candidates("");
        let positions: Vec<usize> = all
            .iter()
            .map(|c| {
                BINDINGS
                    .iter()
                    .position(|b| std::ptr::eq(b, *c))
                    .expect("candidate is a BINDINGS row")
            })
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "candidates keep BINDINGS table order");
    }

    #[test]
    fn help_lines_cover_every_binding() {
        let lines = help_lines();
        assert!(!lines.is_empty());
        for b in BINDINGS {
            assert!(
                lines.iter().any(|l| l.mode == b.mode && l.desc == b.help),
                "no help line for {:?}",
                b.action
            );
        }
    }

    // ---- node context menu (Issue 06) ----

    fn menu_mode_with(items: Vec<MenuEntry>) -> Mode {
        Mode::Menu(MenuState {
            target: "m".into(),
            name: "m".into(),
            items,
            selected: 0,
            back: None,
            anchor: MenuAnchor::Cell { col: 0, row: 0 },
        })
    }

    /// Resolve every [`NODE_MENU_LAYOUT`] slot to its `(mode, action)`
    /// BINDINGS row index — the same lookup `App::menu_entries` performs.
    fn slot_binding(mode: ModeKind, action: Action) -> Option<usize> {
        BINDINGS
            .iter()
            .position(|b| b.mode == mode && b.action == action)
    }

    #[test]
    fn menu_layout_slots_resolve_to_real_bindings() {
        // The layout↔keymap drift guard: every item slot names a `(mode,
        // action)` pair that exists in BINDINGS — a renamed action or a
        // re-keyed row fails here, never silently drops a menu item.
        for slot in NODE_MENU_LAYOUT {
            let MenuSlot::Item(mode, action) = slot else {
                continue;
            };
            let idx = slot_binding(*mode, *action)
                .unwrap_or_else(|| panic!("no BINDINGS row for {mode:?}/{action:?}"));
            assert_eq!(
                BINDINGS[idx].action, *action,
                "the resolved row carries the slot's action"
            );
            // An unmapped slot panics in menu_slot_mark — surface it here.
            let _ = menu_slot_mark(*action);
        }
    }

    #[test]
    fn menu_layout_has_no_orphaned_separators() {
        // A menu that starts/ends on a rule or carries two adjacent rules
        // would draw a degenerate box — the layout is screened at open time,
        // so keep the shape clean statically.
        assert!(!matches!(
            NODE_MENU_LAYOUT.first(),
            Some(MenuSlot::Separator)
        ));
        assert!(!matches!(
            NODE_MENU_LAYOUT.last(),
            Some(MenuSlot::Separator)
        ));
        for w in NODE_MENU_LAYOUT.windows(2) {
            assert!(
                !(matches!(w[0], MenuSlot::Separator) && matches!(w[1], MenuSlot::Separator)),
                "adjacent separators"
            );
        }
    }

    #[test]
    fn a_opens_the_menu_in_selection_detail_and_sql() {
        assert_eq!(
            dispatch(&Mode::Selection, press(Char('a'))),
            Some(Action::MenuOpen)
        );
        assert_eq!(
            dispatch(&detail_mode(), press(Char('a'))),
            Some(Action::MenuOpen)
        );
        assert_eq!(
            dispatch(&sql_mode(), press(Char('a'))),
            Some(Action::MenuOpen)
        );
    }

    #[test]
    fn menu_nav_and_close_dispatch() {
        let m = menu_mode_with(vec![]);
        assert_eq!(
            dispatch(&m, press(Char('j'))),
            Some(Action::MenuMove(Direction::Down))
        );
        assert_eq!(
            dispatch(&m, press(KeyCode::Up)),
            Some(Action::MenuMove(Direction::Up))
        );
        assert_eq!(dispatch(&m, press(Enter)), Some(Action::MenuConfirm));
        assert_eq!(dispatch(&m, press(Esc)), Some(Action::DetailClose));
        assert_eq!(dispatch(&m, press(Char('q'))), Some(Action::DetailClose));
    }

    #[test]
    fn an_items_own_key_invokes_it_inside_the_menu() {
        // A menu with two items built from REAL bindings rows: `Space`
        // (BookmarkToggle) and `p` (PathToggle). Pressing either key inside
        // the menu invokes that item directly — the same key the row labels.
        let space = slot_binding(ModeKind::Selection, Action::BookmarkToggle).unwrap();
        let path = slot_binding(ModeKind::Selection, Action::PathToggle).unwrap();
        let m = menu_mode_with(vec![
            MenuEntry::Item {
                binding: space,
                label: "Bookmark".into(),
            },
            MenuEntry::Item {
                binding: path,
                label: "Path anchor here".into(),
            },
        ]);
        assert_eq!(
            dispatch(&m, press(Char(' '))),
            Some(Action::MenuInvoke(0)),
            "Space invokes the bookmark item"
        );
        assert_eq!(
            dispatch(&m, press(Char('p'))),
            Some(Action::MenuInvoke(1)),
            "p invokes the path-anchor item"
        );
        // An unrelated key stays unbound (items don't leak each other's keys).
        assert_eq!(dispatch(&m, press(Char('x'))), None);
        // Separators are skipped by the lookup.
        let m = menu_mode_with(vec![
            MenuEntry::Separator,
            MenuEntry::Item {
                binding: path,
                label: "Path anchor here".into(),
            },
        ]);
        assert_eq!(
            dispatch(&m, press(Char('p'))),
            Some(Action::MenuInvoke(1)),
            "the items index (not the item ordinal) is returned"
        );
    }

    #[test]
    fn esc_action_mirrors_each_modes_esc_binding() {
        assert_eq!(
            esc_action(&detail_mode()),
            Some(Action::DetailClose),
            "the structure modal's Esc is DetailClose"
        );
        assert_eq!(
            esc_action(&help_mode()),
            Some(Action::HelpToggle),
            "help's Esc is HelpToggle, not DetailClose"
        );
        assert_eq!(
            esc_action(&palette_mode()),
            Some(Action::SearchCancel),
            "the palette's Esc is SearchCancel"
        );
        assert_eq!(
            esc_action(&yank_mode()),
            Some(Action::DetailClose),
            "the which-key menu's Esc cancels it"
        );
        assert_eq!(
            esc_action(&menu_mode_with(vec![])),
            Some(Action::DetailClose),
            "the node menu's Esc closes it"
        );
        assert_eq!(
            esc_action(&search_mode()),
            Some(Action::SearchCancel),
            "search's Esc cancels the query"
        );
    }

    #[test]
    fn yank_bindings_lists_only_the_prefix_rows() {
        let rows: Vec<_> = yank_bindings().collect();
        assert!(
            !rows.is_empty() && rows.iter().all(|b| b.mode == ModeKind::Yank),
            "only ModeKind::Yank rows, in table order"
        );
        // The first row is `yy` (copy unique_id); the cancel row, then `?`
        // (Issue 09's help layer) close the table.
        assert_eq!(rows[0].action, Action::YankId);
        assert_eq!(rows[rows.len() - 2].action, Action::DetailClose);
        assert_eq!(rows.last().unwrap().action, Action::HelpToggle);
    }

    // ---- Issue 07: the §8 contextual status hints (BINDINGS-generated) ----

    /// The generated hints as `(key, desc)` pairs — the shape `draw_status`
    /// renders left-anchored.
    fn hint_pairs(
        mode: ModeKind,
        focus: Focus,
        traced: bool,
        overview: bool,
    ) -> Vec<(String, String)> {
        status_hints(mode, focus, traced, overview)
            .iter()
            .map(|h| (h.key.clone(), h.desc.clone()))
            .collect()
    }

    #[test]
    fn status_hints_emit_the_screen_spec_rows() {
        // §8's per-context hint tables — generated from BINDINGS, so a key
        // the status line teaches always dispatches.
        assert_eq!(
            hint_pairs(ModeKind::Selection, Focus::List, false, false),
            vec![
                ("j/k".into(), "move".into()),
                ("Enter".into(), "open".into()),
                ("/".into(), "search".into()),
                ("O".into(), "new tab".into()),
                ("Tab".into(), "lineage".into()),
                ("?".into(), "help".into()),
            ],
            "the list-focus row"
        );
        assert_eq!(
            hint_pairs(ModeKind::Selection, Focus::RightPane, false, false),
            vec![
                ("hjkl".into(), "move".into()),
                ("Enter".into(), "re-root".into()),
                ("O".into(), "new tab".into()),
                ("a".into(), "actions".into()),
                ("t".into(), "lens".into()),
                ("?".into(), "help".into()),
            ],
            "the lineage-focus row"
        );
        assert_eq!(
            hint_pairs(ModeKind::Selection, Focus::List, true, false),
            vec![
                ("h/l".into(), "step column".into()),
                ("K".into(), "column modal".into()),
                ("Esc".into(), "exit trace".into()),
            ],
            "a live column trace supersedes the pane row (no Any hints)"
        );
        assert_eq!(
            hint_pairs(ModeKind::Selection, Focus::List, false, true),
            vec![
                ("j/k".into(), "move".into()),
                ("Enter".into(), "open".into()),
                ("/".into(), "search".into()),
                ("1-9".into(), "tabs".into()),
                ("q".into(), "quit".into()),
            ],
            "the overview row (no Any hints either)"
        );
        assert_eq!(
            hint_pairs(ModeKind::Selection, Focus::Inspector, false, false),
            vec![
                ("j/k".into(), "scroll".into()),
                ("Enter".into(), "trace column".into()),
                ("Tab".into(), "list".into()),
                ("?".into(), "help".into()),
            ],
            "the inspector-focus row"
        );
        // A modal mode emits its Any-context rows — Detail is the
        // representative scrollable modal.
        assert_eq!(
            hint_pairs(ModeKind::Detail, Focus::List, false, false),
            vec![
                ("j/k".into(), "scroll".into()),
                ("d/u".into(), "page".into()),
                ("G/g".into(), "jump".into()),
                ("h/l".into(), "column".into()),
                ("Tab".into(), "sections".into()),
                ("Enter".into(), "trace".into()),
                ("K".into(), "column modal".into()),
                ("y".into(), "yank".into()),
                ("a".into(), "actions".into()),
                ("Esc".into(), "close".into()),
                ("?".into(), "help".into()),
            ],
            "the Detail-modal Any row"
        );
    }

    #[test]
    fn status_hints_sort_by_prio_and_carry_bound_actions() {
        // Every generated hint's action is bound in the mode it was emitted
        // for — the drift guard: a status-hint click can never name an action
        // the keymap doesn't offer there. A merged family (`j/k move`) names
        // no single command and is click-inert (`action: None`).
        for (mode, focus, traced, overview) in [
            (ModeKind::Selection, Focus::List, false, false),
            (ModeKind::Selection, Focus::RightPane, false, false),
            (ModeKind::Selection, Focus::Inspector, false, false),
            (ModeKind::Selection, Focus::List, true, false),
            (ModeKind::Selection, Focus::List, false, true),
            (ModeKind::Detail, Focus::List, false, false),
            (ModeKind::Search, Focus::List, false, false),
            (ModeKind::Palette, Focus::List, false, false),
            (ModeKind::Yank, Focus::List, false, false),
            (ModeKind::Menu, Focus::List, false, false),
            (ModeKind::Help, Focus::List, false, false),
            (ModeKind::Sql, Focus::List, false, false),
            (ModeKind::Stats, Focus::List, false, false),
            (ModeKind::Diff, Focus::List, false, false),
            (ModeKind::Columns, Focus::List, false, false),
            (ModeKind::Notifications, Focus::List, false, false),
        ] {
            let hints = status_hints(mode, focus, traced, overview);
            assert!(
                hints.windows(2).all(|w| w[0].prio <= w[1].prio),
                "hints arrive priority-sorted in {mode:?}"
            );
            for h in &hints {
                if let Some(a) = h.action {
                    assert!(
                        BINDINGS.iter().any(|b| b.mode == mode && b.action == a),
                        "hint action {a:?} must be bound in {mode:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn merged_hint_families_are_click_inert() {
        // A group whose members disagree on the action (a direction pair, a
        // tab-number range) dispatches nothing on click — firing an
        // arbitrary member would be a quiet surprise.
        let list = status_hints(ModeKind::Selection, Focus::List, false, false);
        let by_desc =
            |hints: &[StatusHint], d: &str| hints.iter().find(|h| h.desc == d).unwrap().action;
        assert_eq!(
            by_desc(&list, "move"),
            None,
            "j/k is a family, not a command"
        );
        assert_eq!(
            by_desc(&list, "open"),
            Some(Action::DetailOpen),
            "single-command hints keep their dispatch"
        );
        let overview = status_hints(ModeKind::Selection, Focus::List, false, true);
        assert_eq!(
            by_desc(&overview, "tabs"),
            None,
            "1-9 must not pick a tab number"
        );
        let detail = status_hints(ModeKind::Detail, Focus::List, false, false);
        assert_eq!(by_desc(&detail, "scroll"), None, "j/k scroll is a family");
        assert_eq!(
            by_desc(&detail, "close"),
            Some(Action::DetailClose),
            "Esc/q share one action — the hint stays live"
        );
    }

    #[test]
    fn compact_keys_collapses_ranges_runs_and_pairs() {
        let k = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(compact_keys(&k(&["j", "k"])), "j/k");
        assert_eq!(compact_keys(&k(&["h", "j", "k", "l"])), "hjkl");
        assert_eq!(
            compact_keys(&k(&["1", "2", "3", "4", "5", "6", "7", "8", "9"])),
            "1-9"
        );
        // Two members keep the `/` convention even when they form a run.
        assert_eq!(compact_keys(&k(&["h", "l"])), "h/l");
        // Multi-char labels never merge into a run.
        assert_eq!(compact_keys(&k(&["Down", "Up"])), "Down/Up");
        assert_eq!(compact_keys(&k(&["Enter"])), "Enter");
    }
}
