//! The single source of truth for user intent and key bindings.
//!
//! Every interaction is reified as an [`Action`]. A mode-aware [`dispatch`]
//! maps `(Mode, KeyEvent) -> Option<Action>`, and the SAME binding table feeds
//! [`help_lines`], so the `?` overlay can never drift from what the keys
//! actually do. `dispatch` is PURE and SIZE-UNAWARE: size-aware work
//! (scroll-follow, mouse hit-test) stays in the event loop.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{NodeDetail, PackageScope, PrImpact, TestInfo};

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
    /// SELECTION, and the loop's ensure-visible follows it.
    DetailColumnMove(Direction),
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
    /// [`DiffView`]'s [`PrImpact::ci_command`].
    YankPrSelector,
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
}

/// Which pane a search targets (decided from focus when search opens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTarget {
    /// Filter the left model list.
    List,
    /// Jump to a matching node within the current lineage subgraph.
    Lineage,
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
    pub scroll: usize,
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
    /// The reviewer-shaped PR impact analysis over the changed set (blast
    /// radius / exposures / CI command / risk flags), snapshotted at open.
    pub pr: PrImpact,
    pub scroll: usize,
}

/// The command-palette state: the live query and the selected candidate index.
/// The candidate LIST is derived on demand from [`palette_candidates`] (keyed off
/// [`BINDINGS`]), never stored — so the palette can't drift from the keymap. No
/// scroll field: the render layer derives the scroll window from `selected`, so
/// there is nothing for the event loop to clamp (it falls through unchanged).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
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
    /// Command palette: a fuzzy finder over every Selection-mode command.
    Palette(PaletteState),
    /// The `y` yank-prefix which-key menu: any key dispatched here runs its
    /// yank action and closes the menu (one-shot); `Esc`/`q` cancel.
    Yank,
    Help {
        scroll: usize,
    },
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
    Palette,
    Yank,
    Help,
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
            Mode::Palette(_) => ModeKind::Palette,
            Mode::Yank => ModeKind::Yank,
            Mode::Help { .. } => ModeKind::Help,
        }
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
use KeyCode::{Backspace, Char, Down, Enter, Esc, Left, PageDown, PageUp, Right, Tab, Up};
use ModeKind as M;
use Trigger::{Ctrl, Key, Super};

/// THE binding table — the single source of truth for dispatch AND help.
/// Kept as a one-row-per-binding table (rustfmt would explode it to ~6 lines each).
#[rustfmt::skip]
pub static BINDINGS: &[KeyBinding] = &[
    // ---- Selection mode ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('j')), Key(Down)], action: A::MoveDown, help: "down (list / lineage cursor)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('k')), Key(Up)], action: A::MoveUp, help: "up (list / lineage cursor)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('h')), Key(Left)], action: A::MoveLeft, help: "lineage cursor left (upstream)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('l')), Key(Right)], action: A::MoveRight, help: "lineage cursor right (downstream)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('g'))], action: A::JumpTop, help: "jump to first model (list)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('G'))], action: A::JumpBottom, help: "jump to last model (list)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Tab)], action: A::ToggleFocus, help: "switch list / lineage focus" },
    KeyBinding { mode: M::Selection, triggers: &[Super('b'), Ctrl('b')], action: A::ToggleListPane, help: "show / hide the model list pane" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Enter)], action: A::DetailOpen, help: "open structure / re-root to cursor / toggle +more" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('/'))], action: A::SearchOpen, help: "search (list filter / lineage jump)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "toggle this help" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('o'))], action: A::OpenEditor, help: "open SQL in $EDITOR" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('y'))], action: A::YankPrefix, help: "yank prefix menu" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('Y'))], action: A::YankName, help: "copy model name" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('m'))], action: A::YankMermaid, help: "copy lineage as Mermaid" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('x'))], action: A::YankDot, help: "copy lineage as Graphviz DOT" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('r'))], action: A::Reload, help: "reload manifest" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('z'))], action: A::Recenter, help: "cursor home + re-center lineage" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('u'))], action: A::ToggleUpstream, help: "toggle upstream side" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('d'))], action: A::ToggleDownstream, help: "toggle downstream side" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('['))], action: A::DepthDecrease, help: "limit lineage depth (-)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char(']'))], action: A::DepthIncrease, help: "widen lineage depth (+)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('0'))], action: A::ResetView, help: "reset lineage view" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('b'))], action: A::HistoryBack, help: "jump back (re-root history)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('f'))], action: A::HistoryForward, help: "jump forward (re-root history)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('q')), Ctrl('c')], action: A::Quit, help: "quit" },
    // ---- new feature commands: placed AFTER `quit` so `quit` stays inside the
    // first help page at 100x40 (help_overlay_renders_keybindings_from_keymap).
    // Reorder these past `quit` only if that test's TestBackend height grows. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('s'))], action: A::SqlOpen, help: "preview SQL" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('S'))], action: A::StatsOpen, help: "stats dashboard" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('t'))], action: A::CycleLens, help: "cycle lineage lens" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char(' '))], action: A::BookmarkToggle, help: "toggle bookmark" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('\''))], action: A::BookmarkCycle, help: "cycle bookmarks" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('.'))], action: A::SortCycle, help: "cycle list sort" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('M'))], action: A::ToggleMinimap, help: "toggle minimap" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('v'))], action: A::ToggleDensity, help: "toggle compact lineage" },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('p'), Super('p')], action: A::PaletteOpen, help: "command palette" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('c'))], action: A::YankAscii, help: "copy lineage as text diagram" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('C'))], action: A::YankSql, help: "copy raw SQL" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('i'))], action: A::YankImpact, help: "copy impact report (Markdown)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('e'))], action: A::ExportLineage, help: "export lineage to <name>_lineage.txt" },
    KeyBinding { mode: M::Selection, triggers: &[Key(PageDown), Ctrl('d')], action: A::PageDown, help: "page down (list)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(PageUp), Ctrl('u')], action: A::PageUp, help: "page up (list)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('n'))], action: A::GapNext, help: "next untested model" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('N'))], action: A::GapPrev, help: "previous untested model" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('"'))], action: A::BookmarkCycleBack, help: "cycle bookmarks backward" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('H'))], action: A::LineageLeftmost, help: "lineage cursor to first column" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('L'))], action: A::LineageRightmost, help: "lineage cursor to last column" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('T'))], action: A::ToggleUntestedFilter, help: "filter list: untested only" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('*'))], action: A::ToggleBookmarkFilter, help: "filter list: bookmarked only" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('P'))], action: A::DbtParse, help: "run dbt parse (build manifest)" },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('t')], action: A::ThemeCycle, help: "cycle color theme" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('D'))], action: A::DiffOpen, help: "diff vs baseline (--diff)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('!'))], action: A::YankSelector, help: "copy dbt --select command" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('w'))], action: A::ToggleOverview, help: "toggle whole-graph overview" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('p'))], action: A::PathToggle, help: "anchor/clear two-node path" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('K'))], action: A::ColumnsOpen, help: "column lineage modal (traced column)" },
    KeyBinding { mode: M::Selection, triggers: &[Key(Esc)], action: A::ColumnFocusExit, help: "exit column focus" },
    // ---- Yank prefix mode (`y` + one key): the second key is the SAME letter
    // the single-key binding always used, so muscle memory carries over as
    // `ym`/`yC`/…; `yy` copies the unique_id (what bare `y` used to do). ----
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('y'))], action: A::YankId, help: "copy unique_id" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('Y'))], action: A::YankName, help: "copy model name" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('m'))], action: A::YankMermaid, help: "copy lineage as Mermaid" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('x'))], action: A::YankDot, help: "copy lineage as Graphviz DOT" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('c'))], action: A::YankAscii, help: "copy lineage as text diagram" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('C'))], action: A::YankSql, help: "copy raw SQL" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('s'))], action: A::YankCompiledSql, help: "copy compiled SQL" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('i'))], action: A::YankImpact, help: "copy impact report (Markdown)" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('l'))], action: A::YankColumnLineage, help: "copy column lineage (Markdown)" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('!'))], action: A::YankSelector, help: "copy dbt --select command" },
    KeyBinding { mode: M::Yank, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "cancel" },
    // ---- Search mode (printable chars are handled dynamically -> SearchType) ----
    KeyBinding { mode: M::Search, triggers: &[Key(Esc)], action: A::SearchCancel, help: "cancel search" },
    KeyBinding { mode: M::Search, triggers: &[Key(Enter)], action: A::SearchConfirm, help: "confirm" },
    KeyBinding { mode: M::Search, triggers: &[Key(Backspace)], action: A::SearchBackspace, help: "delete char" },
    KeyBinding { mode: M::Search, triggers: &[Key(Up)], action: A::SearchMove(Direction::Up), help: "previous match" },
    KeyBinding { mode: M::Search, triggers: &[Key(Down)], action: A::SearchMove(Direction::Down), help: "next match" },
    // ---- Command palette (printable chars are handled dynamically -> SearchType;
    // the editing actions REUSE the Search* arms, routed by mode in apply_action) ----
    KeyBinding { mode: M::Palette, triggers: &[Key(Esc)], action: A::SearchCancel, help: "close palette" },
    KeyBinding { mode: M::Palette, triggers: &[Key(Enter)], action: A::SearchConfirm, help: "run command" },
    KeyBinding { mode: M::Palette, triggers: &[Key(Backspace)], action: A::SearchBackspace, help: "delete char" },
    KeyBinding { mode: M::Palette, triggers: &[Key(Up)], action: A::SearchMove(Direction::Up), help: "prev command" },
    KeyBinding { mode: M::Palette, triggers: &[Key(Down)], action: A::SearchMove(Direction::Down), help: "next command" },
    // ---- Detail modal ----
    KeyBinding { mode: M::Detail, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close" },
    // Enter/`c` commits the column under the h/l cursor to column focus (the
    // arm closes the modal instead when the node has no columns — the classic
    // Enter-to-close survives there).
    KeyBinding { mode: M::Detail, triggers: &[Key(Enter), Key(Char('c'))], action: A::ColumnFocusEnter, help: "trace this column's lineage" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('K'))], action: A::ColumnsOpen, help: "column lineage modal (this column)" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('h')), Key(Char('H'))], action: A::DetailColumnMove(Direction::Up), help: "previous column" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('l')), Key(Char('L'))], action: A::DetailColumnMove(Direction::Down), help: "next column" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('y'))], action: A::YankPrefix, help: "yank prefix menu" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top" },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom" },
    // ---- SQL preview modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Sql, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close" },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('c'))], action: A::SqlToggleCompiled, help: "toggle raw / compiled SQL" },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down" },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up" },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down" },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up" },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top" },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom" },
    // ---- Stats dashboard modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Stats, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close" },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down" },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up" },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down" },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up" },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top" },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom" },
    // ---- Diff summary modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Diff, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('e'))], action: A::ExportPrImpact, help: "export PR impact report (Markdown)" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('y'))], action: A::YankPrSelector, help: "copy dbt build --select command" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top" },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom" },
    // ---- Column lineage modal (Issue 09): j/k move the ROW cursor (a
    // selection the loop scroll-follows), d/u/g/G page the viewport like every
    // other modal, Enter re-roots to the row's model. ----
    KeyBinding { mode: M::Columns, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close" },
    KeyBinding { mode: M::Columns, triggers: &[Key(Enter)], action: A::ColumnJump, help: "jump to this model" },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('j')), Key(Down)], action: A::ColumnRowMove(Direction::Down), help: "next column row" },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('k')), Key(Up)], action: A::ColumnRowMove(Direction::Up), help: "previous column row" },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down" },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up" },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top" },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom" },
    // ---- Help overlay ----
    KeyBinding { mode: M::Help, triggers: &[Key(Esc), Key(Char('?')), Key(Char('q'))], action: A::HelpToggle, help: "close help" },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down" },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up" },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down" },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up" },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top" },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom" },
];

/// Map `(Mode, KeyEvent)` to an [`Action`], or `None` if the key is unbound.
///
/// Ctrl-C always quits (any mode). Then the [`BINDINGS`] table is consulted for
/// the mode; finally, in `Search` mode, any plain printable char becomes
/// [`Action::SearchType`].
pub fn dispatch(mode: &Mode, key: KeyEvent) -> Option<Action> {
    // Global: Ctrl-C quits regardless of mode (never a printable char).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
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
    None
}

/// One line of the `?` help overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpLine {
    pub mode: ModeKind,
    pub keys: String,
    pub desc: String,
}

/// Synthetic help for actions NOT bound to a fixed key (text input, mouse).
/// The drift test asserts these are exactly the non-`BINDINGS` actions.
static DYNAMIC_HELP: &[(ModeKind, &str, &str)] = &[
    (ModeKind::Search, "a-z..", "type to filter / search"),
    (
        ModeKind::Selection,
        "click",
        "re-root / move cursor to a node",
    ),
    (
        ModeKind::Selection,
        "wheel",
        "scroll the pane under the cursor",
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
        })
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
            col_counts: std::collections::BTreeMap::new(),
            stale: false,
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
    fn stats_mode() -> Mode {
        Mode::Stats(StatsView {
            project: String::new(),
            by_resource_type: vec![],
            by_materialization: vec![],
            testable_total: 0,
            testable_tested: 0,
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
        assert_eq!(dispatch(&sel, press(Char('~'))), None, "unbound key");
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
            dispatch(&Mode::Help { scroll: 0 }, press(Char('q'))),
            Some(Action::HelpToggle)
        );
        assert_eq!(
            dispatch(&detail_mode(), press(Esc)),
            Some(Action::DetailClose)
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
            | Action::DbtParse
            | Action::ThemeCycle
            | Action::DiffOpen
            | Action::ExportPrImpact
            | Action::YankPrSelector
            | Action::ColumnFocusEnter
            | Action::ColumnFocusExit
            | Action::ColumnsOpen
            | Action::ColumnRowMove(_)
            | Action::ColumnJump => true,
            // Dynamic: produced without a fixed key binding (text input). Mouse
            // is handled in the event loop (size-aware), not via the keymap.
            Action::SearchType(_) => false,
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
                ModeKind::Palette => palette_mode(),
                ModeKind::Yank => Mode::Yank,
                ModeKind::Help => Mode::Help { scroll: 0 },
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
            Action::DbtParse,
            Action::ThemeCycle,
            Action::DiffOpen,
            Action::ExportPrImpact,
            Action::YankPrSelector,
            Action::ColumnFocusEnter,
            Action::ColumnFocusExit,
            Action::ColumnsOpen,
            Action::ColumnRowMove(Direction::Down),
            Action::ColumnJump,
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
}
