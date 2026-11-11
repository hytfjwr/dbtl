use super::*;

/// The structure-modal payload: cloned from the [`Dag`](crate::Dag) side maps
/// when the modal opens, so the render layer never needs a `Dag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailView {
    pub model_id: String,
    pub name: String,
    /// Boxed so the `Mode::Detail` variant stays small — `NodeDetail` is the
    /// biggest member by far and the enum shares its footprint across every
    /// variant.
    pub detail: Box<NodeDetail>,
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
    /// direct children out, per the contract — NOT the transitive
    /// closure). `BTreeMap` keeps the render order deterministic; empty when
    /// the resolver sees no columns. Keys are the resolver's normalized names,
    /// so a declared `col_names` entry looks itself up through
    /// `norm_column` — an un-normalizable name (rare: declared names come
    /// from manifest keys, which dbt already lowers) just gets no count.
    pub col_counts: std::collections::BTreeMap<String, (usize, usize)>,
    /// Any of this node's column edges was resolved against compiled SQL
    /// older than its source — the columns header gets a `[stale]` tag.
    pub stale: bool,
    /// This node's column edges fell back to the name-match
    /// approximation (unparsed or missing SQL) — the columns header gets a
    /// `[name-match]` tag.
    pub name_match: bool,
    pub scroll: usize,
}

/// One lint finding attached to a node, snapshotted for the inspector's Lint
/// section: `severity` picks the mark (`x`/`!` — `Chrome`'s run glyphs) and
/// the colour, `rule` is the registry's snake_case id, `summary` its one-line
/// description, `message` the finding's own explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorLintFinding {
    pub severity: crate::Severity,
    pub rule: String,
    pub summary: String,
    pub message: String,
}

/// The right inspector pane's snapshot for the node under the lineage cursor
/// (or the rooted selection): `detail` reuses the structure modal's
/// `detail_view_for` assembly verbatim (the "share the DetailView
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
    /// `impact_breakdown_for` the two-number `DetailView` drops. The pane shows it
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
    /// This node's lint findings (only findings whose `node_uid` IS this
    /// node — `related` uids are context for the OTHER node's violation,
    /// not this node's).
    pub lint: Vec<InspectorLintFinding>,
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

/// One rule's slice of the static-lint worklist: the registry's snake_case
/// id, the severity the rule ran at (per-RULE — the registry default or the
/// config's override, uniform across the group), and the sorted violating
/// node names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintWorklist {
    pub rule: String,
    pub severity: crate::Severity,
    pub names: Vec<String>,
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
    /// Column-level test coverage: declared columns carrying ≥1
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
    /// The static-lint worklist: one [`LintWorklist`] per rule that has ≥1
    /// finding, in [`LintRule::ALL`](crate::LintRule) order — the SAME
    /// [`run_rules`](crate::run_rules) findings `dbtl check --lint` gates
    /// on, so the dashboard can never disagree with the CI gate. Empty when
    /// nothing violates.
    pub lint_findings: Vec<LintWorklist>,
    /// The severity totals over the same shared findings — the heading's
    /// split (`e errors, w warnings`) and the home Health tail's source, so
    /// every surface quotes the one vector.
    pub lint_errors: usize,
    pub lint_warnings: usize,
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
    /// Root-project models whose column lineage resolved through the
    /// name-match fallback (SQL missing or unparsable) — the aggregate of the
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
    /// and what it breaks downstream), the same `DiffReport::column_changes`
    /// data the Markdown/JSON surfaces emit.
    pub column_changes: Vec<ColumnChange>,
    /// The reviewer-shaped PR impact analysis over the changed set (blast
    /// radius / exposures / CI command / risk flags), snapshotted at open.
    pub pr: PrImpact,
    pub scroll: usize,
}

/// The command-palette state: the live query and the selected candidate index.
/// The candidate LIST is derived on demand ([`crate::App::palette_rows`], keyed off
/// [`BINDINGS`] — or the model list when the query is `@`-prefixed), never
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
    /// a NEW tab instead of re-rooting the active one (a plain Ctrl-p
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
    /// root" the rule names — the current selection until tabs exist).
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
    /// FIRST in the layout (the opening mode's section first).
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

/// One row of the column-lineage modal (`Mode::Columns`): either a
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

/// The column-lineage modal payload, snapshotted when the modal
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

/// A notification's severity: drives the toast's TTL/border
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

/// One logged notification: the wall-clock `HH:MM:SS` stamp applied
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

/// The notification-centre modal payload, snapshotted at open like
/// every other modal so the render layer never needs a [`Dag`](crate::Dag).
/// MUST be `Eq` (`Mode` derives `Eq`). `rows` are newest-first (the centre's
/// display order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationsView {
    pub rows: Vec<NotificationEntry>,
    pub scroll: usize,
}
