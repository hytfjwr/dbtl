/// How a node's label cells are classified for render-layer styling. Carried as
/// a parallel per-cell array on [`CharGrid`] (NOT a glyph change), so it never
/// affects grid width, `to_text()`, or `emphasis_regions()` — the golden diamond
/// exact-match stays byte-identical.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MaterializationClass {
    /// Connector cells, padding, or an unstyled node.
    #[default]
    Plain,
    Table,
    View,
    Incremental,
    Ephemeral,
    /// A model whose materialization is unknown / missing.
    OtherModel,
    Source,
    Seed,
    Snapshot,
    /// A declared downstream consumer (dashboard / notebook / …) — a terminator
    /// leaf on the right edge of the diagram.
    Exposure,
    /// A `+M more` / `-M less` collapse marker (the sibling-collapse stage):
    /// display chrome standing in for hidden siblings, never a real resource
    /// (its `resource_type` is the synthetic `"more"`, which no manifest emits).
    Collapsed,
}

impl MaterializationClass {
    /// Classify a node from its resource type and (for models) materialization —
    /// the single source of truth for the (resource_type, materialized) → class
    /// rule. Sources/seeds/snapshots classify by type; a model with an unknown or
    /// missing materialization falls back to `OtherModel`.
    pub fn classify(resource_type: &str, materialized: Option<&str>) -> Self {
        match resource_type {
            "source" => MaterializationClass::Source,
            "seed" => MaterializationClass::Seed,
            "snapshot" => MaterializationClass::Snapshot,
            "exposure" => MaterializationClass::Exposure,
            "model" => match materialized {
                Some("table") => MaterializationClass::Table,
                Some("view") => MaterializationClass::View,
                Some("incremental") => MaterializationClass::Incremental,
                Some("ephemeral") => MaterializationClass::Ephemeral,
                _ => MaterializationClass::OtherModel,
            },
            // The collapse marker's synthetic resource_type — classified as
            // display chrome, not a real resource.
            "more" => MaterializationClass::Collapsed,
            _ => MaterializationClass::Plain,
        }
    }

    /// The short display tag shown in a node box's top border (`+Tag---+`).
    /// Exhaustive over the enum, so adding a variant is a compile error here and
    /// the tag can never silently drift from the colour class.
    pub fn tag(self) -> &'static str {
        match self {
            MaterializationClass::Table => "Table",
            MaterializationClass::View => "View",
            MaterializationClass::Incremental => "Incremental",
            MaterializationClass::Ephemeral => "Ephemeral",
            MaterializationClass::OtherModel => "Model",
            MaterializationClass::Source => "Source",
            MaterializationClass::Seed => "Seed",
            MaterializationClass::Snapshot => "Snapshot",
            MaterializationClass::Exposure => "Exposure",
            MaterializationClass::Collapsed => "More",
            MaterializationClass::Plain => "Node",
        }
    }
}

/// A SEMANTIC lineage-lens tint, set by `App::lineage_styles` for the ACTIVE lens
/// only. Layout stays ratatui-free, so this names an *intent* (Warn / heat bucket
/// / which layer / a layer-rank violation) and `ui::lineage::attr_style` maps each
/// to a concrete `Color`. `None` (the default) leaves the materialization class
/// colour showing through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LensTint {
    /// No tint — the class colour shows.
    #[default]
    None,
    /// Coverage lens: a testable resource with zero tests (the old `untested`).
    Warn,
    /// Degree-heat lens, low / mid / high transitive-downstream buckets.
    HeatLow,
    HeatMid,
    HeatHigh,
    /// Layer lens, one tint per dbt logical layer.
    LayerStaging,
    LayerIntermediate,
    LayerMarts,
    LayerUtilities,
    LayerOther,
    /// Layer-violation lens: incident to a marts→staging-style backward edge.
    Violation,
    /// Diff lens: node added vs the `--diff` baseline.
    DiffAdd,
    /// Diff lens: node modified vs the `--diff` baseline.
    DiffMod,
    /// RunStatus lens: the node's last run ended `warn` (error/fail reuse
    /// `Violation`'s red; success keeps the class colour).
    RunWarn,
    /// RunStatus lens: the node's last run was `skipped` (muted).
    RunSkip,
    /// Column-impact lens (Issue 09): a node on the traced column's DOWNSTREAM
    /// blast radius — gold, the "this column reaches it" answer.
    ColumnImpact,
}

/// Per-cell render attributes, orthogonal to the char and the emphasis flag:
/// the materialization `class` (→ a colour) plus render-only overlay state that
/// COMPOSES with it. Tests are shown as data only (the `tests:N` label in the
/// bottom border, from `NodeInfo::test_count`), never as a style — an underline
/// across the box's border glyphs reads as doubled lines. A `struct` (not an
/// enum) so the orthogonal overlays can ride alongside `class`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellAttr {
    pub class: MaterializationClass,
    /// The active lineage lens's tint for this node (`None` = no tint, the class
    /// colour shows). Render-only; set by `App::lineage_styles` for whichever lens
    /// is selected (→ a foreground that wins over the class colour, but loses to
    /// `dimmed`).
    pub lens: LensTint,
    /// Focus dim: the cursor is off-root and this node is NOT on the root↔cursor
    /// path. Render-only; a dim foreground that wins over both the lens tint and
    /// the class colour, so the path nodes stand out.
    pub dimmed: bool,
    /// On the root↔cursor lineage path (automatic when the cursor is off-root).
    /// Render-only; an orthogonal background band so it composes with the
    /// foreground (lens / class / dim) above.
    pub on_path: bool,
}
