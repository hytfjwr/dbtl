/// The lineage colour lens, cycled by `t` (replaces the old coverage on/off bool).
/// Each non-`Off` lens recolours the lineage node boxes by a different metric; the
/// `Coverage` arm is exactly the old behaviour, so ONE `t` press from the default
/// reproduces the muscle-memory coverage view (and keeps the status-bar cov%).
///
/// Cycle order (`cycle_lens`): Off → Coverage → DegreeHeat → Layer → LayerViolation
/// → Lint → Diff → RunStatus → RunTime → ColumnImpact → PrimaryKey → Off. The
/// Lint lens needs at least one static-lint [`Finding`](crate::Finding), the
/// Diff lens needs a `--diff` baseline, the Run* lenses need a loaded
/// `run_results.json`, and ColumnImpact needs an active column focus to show
/// anything, so the `apply_action` CycleLens arm SKIPS them when the App lacks
/// the data — the raw [`cycle_lens`](crate::UiState::cycle_lens) here stays a pure,
/// App-blind step. PrimaryKey is never skipped: its input (`Dag::tests`) is
/// always loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineageLens {
    /// No lens — node boxes show their materialization-class colour.
    #[default]
    Off,
    /// Test-coverage: testable nodes with zero tests are warning-coloured (the
    /// old `coverage_lens` behaviour; also drives the status-bar cov% segment).
    Coverage,
    /// Degree heat: nodes coloured by transitive-downstream "blast radius".
    DegreeHeat,
    /// Layer: nodes coloured by their dbt logical layer.
    Layer,
    /// Layer violations: nodes incident to a backward (e.g. marts→staging) edge.
    LayerViolation,
    /// Static-lint findings: nodes flagged by `dbtl_core::run_rules` (the same
    /// findings `dbtl check --lint` gates on) are violation-coloured.
    Lint,
    /// Diff vs the `--diff` baseline: added nodes green, modified nodes amber.
    Diff,
    /// Last-run status from `run_results.json`: error/fail red, warn yellow,
    /// skipped muted (success keeps its class colour).
    RunStatus,
    /// Last-run `execution_time` heat: the slowest nodes glow on the same
    /// low/mid/high ramp the DegreeHeat lens uses.
    RunTime,
    /// Column impact (Issue 09): the nodes the TRACED column's closure reaches
    /// downstream glow — the "who does this column feed" answer. Skipped by
    /// `CycleLens` while no column focus is active.
    ColumnImpact,
    /// Primary-key guarantee (Issue 11): models lacking the shared
    /// [`has_pk_guarantee`](crate::has_pk_guarantee) predicate — a `unique` +
    /// `not_null` pair on one column, or a `unique_combination_of_columns`
    /// test — are violation-coloured. Data-free (needs only `Dag::tests`), so
    /// the cycle never skips it.
    PrimaryKey,
}

impl LineageLens {
    /// The next lens in the `t`-cycle (wraps `PrimaryKey → Off`).
    pub fn next(self) -> LineageLens {
        match self {
            LineageLens::Off => LineageLens::Coverage,
            LineageLens::Coverage => LineageLens::DegreeHeat,
            LineageLens::DegreeHeat => LineageLens::Layer,
            LineageLens::Layer => LineageLens::LayerViolation,
            LineageLens::LayerViolation => LineageLens::Lint,
            LineageLens::Lint => LineageLens::Diff,
            LineageLens::Diff => LineageLens::RunStatus,
            LineageLens::RunStatus => LineageLens::RunTime,
            LineageLens::RunTime => LineageLens::ColumnImpact,
            LineageLens::ColumnImpact => LineageLens::PrimaryKey,
            LineageLens::PrimaryKey => LineageLens::Off,
        }
    }
}

/// The inspector pane's visibility pref (screen-spec §4): what `I` cycles and
/// what persists — NOT the effective on-screen state, which the event loop
/// resolves per frame from this value + the terminal width (the only
/// size-aware layer) and stores back via [`UiState::set_inspector_shown`](crate::UiState::set_inspector_shown).
///
/// Cycle order (`I`): Auto → On → Off → Auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InspectorPolicy {
    /// Width-driven (the default): the pane shows itself at terminals
    /// ≥160 columns and hides below — §12's complete "160 列以上で自動"
    /// rule, resolved via [`shown_at`](InspectorPolicy::shown_at).
    #[default]
    Auto,
    /// Always request the pane — `pane_layout` still display-clamps its
    /// width when the terminal can't fit it, so `On` at a tiny terminal can
    /// still yield a zero-width pane (never an error, by §1's clamp rule).
    On,
    /// Never show the pane (and never accept its focus).
    Off,
}

impl InspectorPolicy {
    /// The next pref in the `I` cycle (`auto → on → off → auto`).
    pub fn next(self) -> InspectorPolicy {
        match self {
            InspectorPolicy::Auto => InspectorPolicy::On,
            InspectorPolicy::On => InspectorPolicy::Off,
            InspectorPolicy::Off => InspectorPolicy::Auto,
        }
    }

    /// The on-screen answer for a terminal `width` columns wide: `On`/`Off`
    /// answer for themselves (explicit prefs beat every automatic decision),
    /// `Auto` resolves to §12's "≥160 columns" floor. The event loop resolves
    /// the pref through this every frame — the single pure predicate the
    /// width boundary lives behind, so the 159/160 edge is unit-testable.
    pub fn shown_at(self, width: u16) -> bool {
        match self {
            InspectorPolicy::On => true,
            InspectorPolicy::Off => false,
            InspectorPolicy::Auto => width >= super::geom::INSPECTOR_AUTO_W,
        }
    }

    /// The persisted/config spelling (`inspector: auto|on|off` — kebab-free,
    /// matching the other pref names).
    pub fn name(self) -> &'static str {
        match self {
            InspectorPolicy::Auto => "auto",
            InspectorPolicy::On => "on",
            InspectorPolicy::Off => "off",
        }
    }

    /// Parse a [`InspectorPolicy::name`] string; `None` on unknown values —
    /// the pref-apply seam skips rather than errors, like every prefs field.
    pub fn from_name(name: &str) -> Option<InspectorPolicy> {
        Some(match name {
            "auto" => InspectorPolicy::Auto,
            "on" => InspectorPolicy::On,
            "off" => InspectorPolicy::Off,
            _ => return None,
        })
    }
}

/// Which pane currently has focus: the left model list, the center lineage
/// pane, or the right inspector pane (Issue 04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The left model list pane.
    List,
    /// The center lineage pane.
    RightPane,
    /// The right inspector pane (Issue 04) — reachable via `Tab` only while
    /// the pane is shown; `j`/`k`/`d`/`u`/`g`/`G`/`h`/`l` and the wheel act
    /// on its content while focused.
    Inspector,
}

impl Focus {
    /// The next pane in the `Tab` ring (the ORDER — visibility filtering is
    /// [`UiState::toggle_focus`]'s job, since only UiState knows which panes
    /// are on screen). `List → RightPane → Inspector → List` (§4's
    /// 一覧 → Lineage → インスペクタ → 一覧).
    pub(super) fn next(self) -> Self {
        match self {
            Focus::List => Focus::RightPane,
            Focus::RightPane => Focus::Inspector,
            Focus::Inspector => Focus::List,
        }
    }
}

/// What the event loop should do after a key was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Keep running.
    Continue,
    /// The user asked to quit (`q` / `Ctrl-c`).
    Quit,
}
