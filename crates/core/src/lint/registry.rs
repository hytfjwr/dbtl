//! The lint rule registry: every rule's metadata declared in ONE table, in
//! [`LintRule::ALL`] order. The registry is the single source of truth —
//! `LintRule::name()`, `LintRule::from_name()`, and the config parser's
//! per-rule parameter validation all read it, so a rule exists in exactly one
//! place. Order is meaningful: evaluation (`run_rules`), the `--rules` error
//! listing, TUI/docs summaries, and READMEs all follow it.

use serde::{Deserialize, Serialize};

use super::LintRule;

/// The lint rule registry — one row per rule, in `LintRule::ALL` order.
pub static REGISTRY: &[RuleInfo] = &[
    RuleInfo {
        rule: LintRule::DirectJoinToSource,
        id: "direct_join_to_source",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a model directly depends on both a source and a model",
        rationale: "Mixing a raw source with modeled data in one query hides \
                    where the model's real inputs come from. A layer that reads \
                    sources AND models skips the staging contract, so a broken \
                    upstream model can silently change what the raw source \
                    contributes.",
        example: "order_summary joins stg_orders (model) with \
                  raw.payments (source). Fix: model payments through staging \
                  and ref the staging model instead.",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::MultipleSourcesJoined,
        id: "multiple_sources_joined",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a model directly depends on 2+ sources",
        rationale: "Joining several raw sources in one model buries the \
                    integration logic where nobody can test or reuse it. Each \
                    source should get its own staging model first, so the join \
                    happens between documented, tested surfaces.",
        example: "fct_orders selects from raw.shopify.orders and \
                  raw.stripe.charges. Fix: stage each source as stg_shopify \
                  / stg_stripe and join those models.",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::SourceFanout,
        id: "source_fanout",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a source directly feeds too many models",
        rationale: "When one raw source feeds many downstream models, every \
                    one of them re-derives the same cleanup and they drift \
                    apart. A single staging model owns the source contract; \
                    consumers ref that.",
        example: "raw.crm.contacts is read by stg_contacts, fct_leads and \
                  rpt_pipeline. Fix: only stg_contacts reads the source; the \
                  marts ref stg_contacts.",
        params: &[ParamSpec::int("threshold", 1, 2, "minimum direct children to flag")],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ModelFanout,
        id: "model_fanout",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a model directly feeds too many leaf models",
        rationale: "A hub model that feeds many leaf models concentrates risk: \
                    one bad change fans out to every downstream report, and the \
                    hub's name gives no hint of which consumers break. Split \
                    the hub along ownership lines so each branch has a clear \
                    contract.",
        example: "int_customers feeds fct_orders, fct_returns, rpt_cohorts, \
                  rpt_ltv and dim_customers. Fix: introduce narrower \
                    intermediates per domain instead of one grab-bag hub.",
        params: &[ParamSpec::int(
            "threshold",
            1,
            crate::PR_HUB_MIN_DOWNSTREAM,
            "minimum direct leaf children to flag",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::RootModels,
        id: "root_models",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a model has no parents",
        rationale: "A model that refs nothing upstream is either a hand-maintained \
                    table — data nobody can trace or rebuild — or a forgotten \
                    ref that should have been a source. Real roots belong in \
                    seeds or declared sources so their provenance is explicit.",
        example: "dim_regions is a plain `select ...` with no ref/source \
                  calls. Fix: make it a seed, or declare it as a source if an \
                  external system owns it.",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::RejoiningUpstream,
        id: "rejoining_upstream",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a redundant hop: A→B→C while A→C already exists",
        rationale: "Re-reading an upstream model when a direct edge already \
                    exists means the intermediate node adds nothing — it just \
                    duplicates a dependency. The extra hop slows builds and \
                    muddies the graph's intent.",
        example: "fct_orders refs int_pricing, and int_pricing's only other \
                  parent stg_orders is also refed directly by fct_orders. Fix: \
                  drop the redundant stg_orders ref or move the shared logic \
                    into int_pricing.",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::MartsDependOnSource,
        id: "marts_depend_on_source",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a marts/intermediate model directly depends on a source",
        rationale: "Marts and intermediate models are supposed to consume \
                    staged data. Reading a raw source directly bypasses the \
                    staging layer's naming, typing and freshness guarantees, \
                    so the mart breaks the moment the raw shape changes.",
        example: "fct_orders reads raw.shopify.orders. Fix: create \
                  stg_shopify__orders and ref that from the mart.",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::MissingPrimaryKeyTests,
        id: "missing_primary_key_tests",
        category: Category::Testing,
        default_severity: Severity::Error,
        summary: "a model lacks a primary-key test (unique+not_null on one column, or unique_combination_of_columns)",
        rationale: "Joins and fanout checks assume every model has a stable \
                    grain. Without a uniqueness test, a duplicated key turns a \
                    downstream join into a silent row explosion that no lint \
                    rule can see.",
        example: "stg_orders has no test on order_id. Fix: add unique and \
                  not_null tests on order_id (or a \
                  unique_combination_of_columns test for a composite grain).",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::PublicModelsWithoutContract,
        id: "public_models_without_contract",
        category: Category::Governance,
        default_severity: Severity::Error,
        summary: "access: public without contract: enforced",
        rationale: "Declaring a model public advertises it as a stable \
                    interface, but without an enforced contract dbt won't \
                    check that its columns keep their promised names and \
                    types. A rename ships a breaking change with no warning.",
        example: "rpt_monthly_revenue sets access: public but no contract. \
                  Fix: add contract: { enforced: true } so schema drift fails \
                  the build instead of downstream BI.",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::UndocumentedModels,
        id: "undocumented_models",
        category: Category::Documentation,
        default_severity: Severity::Error,
        summary: "a model has no description",
        rationale: "A model with no description forces every reader to \
                    reverse-engineer intent from SQL. Descriptions are the \
                    cheapest documentation that exists — the generated docs \
                    site and the TUI inspector are only as useful as this one \
                    string.",
        example: "stg_customers has no description. Fix: add a `description:` \
                  in its schema.yml entry (or a docs block) saying what one \
                  row represents.",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::NamingConventions,
        id: "naming_conventions",
        category: Category::Structure,
        default_severity: Severity::Error,
        summary: "a model's name misses its layer's prefix",
        rationale: "Layer prefixes let a reader know a model's role before \
                    opening it — and let rules like marts_depend_on_source \
                    classify the DAG at all. A marts model named stg_x is \
                    checked against the wrong policy or none at all.",
        example: "models/marts/orders.sql is named `orders` but marts expects \
                  a fct_/dim_/rpt_ prefix. Fix: rename to fct_orders (or widen \
                  `lint.naming_prefixes.marts` in .dbtl.yml).",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::LayerViolation,
        id: "layer_violation",
        category: Category::Modeling,
        default_severity: Severity::Error,
        summary: "a backward edge between logical layers",
        rationale: "Layers exist so data flows one way: staging cleans, \
                    intermediate composes, marts serve. An edge that jumps \
                    backward makes the layer boundaries meaningless and turns \
                    refactors into guesswork.",
        example: "stg_orders refs fct_payments (staging reading a mart). Fix: \
                  move the shared logic upstream — staging should never need \
                  a marts model.",
        params: &[],
        requires: Requires::Graph,
    },
];

/// A rule family — used for grouping in `--list-rules`-style output and for
/// deciding what data a rule may rely on. All eight values exist today even
/// though only five have rules: the set is the taxonomy, not the census.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// DAG shape and layering (joins, fanout, hops, layer direction).
    Modeling,
    /// dbt tests coverage on models.
    Testing,
    /// Descriptions and doc coverage.
    Documentation,
    /// Naming and file/structure conventions.
    Structure,
    /// Query-shape findings (needs SQL text — no rules yet).
    Performance,
    /// Access/contract conventions.
    Governance,
    /// SQL-body findings (needs `Requires::Sql`).
    Sql,
    /// Warehouse-catalog findings (needs `Requires::Catalog`).
    Warehouse,
}

impl Category {
    /// The stable lowercase tag (same as the serde form).
    pub fn name(self) -> &'static str {
        match self {
            Category::Modeling => "modeling",
            Category::Testing => "testing",
            Category::Documentation => "documentation",
            Category::Structure => "structure",
            Category::Performance => "performance",
            Category::Governance => "governance",
            Category::Sql => "sql",
            Category::Warehouse => "warehouse",
        }
    }
}

/// How loudly a finding reports: `warn` reports but should not gate, `error`
/// gates. Ordered — `Warn < Error` — so `fail_on` thresholds compare with `>=`.
/// `off` is deliberately NOT a Severity: it's the absence of a severity in
/// the config's per-rule overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warn,
    Error,
}

impl Severity {
    /// The stable lowercase tag (same as the serde form).
    pub fn name(self) -> &'static str {
        match self {
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

/// The data a rule needs to evaluate. Today everything is `Graph` — SQL-body
/// and catalog rules are declared here so `run_rules`' input struct has a
/// stable place to hang them when they arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requires {
    /// The project `Dag` only.
    Graph,
    /// `Graph` plus parsed SQL bodies.
    Sql,
    /// `Graph` plus warehouse catalog information.
    Catalog,
}

/// One declared rule parameter. Only integers exist today; the struct leaves
/// room for new kinds without changing the registry's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamSpec {
    /// The config key under `rules.<id>.` — also the compat flat key's
    /// stripped name (`model_fanout_threshold` → `threshold`).
    pub name: &'static str,
    pub kind: ParamKind,
    /// What the knob controls — surfaced in `--explain`/`--list-rules`.
    pub description: &'static str,
}

impl ParamSpec {
    /// An integer parameter with an inclusive minimum and a default.
    pub const fn int(
        name: &'static str,
        min: usize,
        default: usize,
        description: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            kind: ParamKind::Int { min, default },
            description,
        }
    }
}

/// The value space of a [`ParamSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// A non-negative integer with an inclusive minimum and a default used
    /// when the config doesn't set it.
    Int { min: usize, default: usize },
}

/// One registry row — everything there is to say about a rule except its
/// evaluator function.
#[derive(Debug, Clone, Copy)]
pub struct RuleInfo {
    /// The enum variant this row describes (back-link for `LintRule::info`).
    pub rule: LintRule,
    /// The stable rule id — `LintRule`'s serde name, the `.dbtl.yml` key,
    /// `--rules`' argument and the baseline's `rule` field all in one.
    pub id: &'static str,
    pub category: Category,
    /// Applied when the config doesn't set `severity` for this rule. Rules
    /// that predate severities are `Error` (that was the only behavior);
    /// rules added later ship `Warn` so they don't silently gate a project
    /// that never opted in.
    pub default_severity: Severity,
    /// A single line — what the rule flags (the old `description()` text).
    pub summary: &'static str,
    /// Why the rule exists — what goes wrong when the pattern is ignored.
    pub rationale: &'static str,
    /// A concrete bad shape and the fix.
    pub example: &'static str,
    /// The rule's accepted object-form parameters (`rules.<id>.<param>`).
    /// Unknown parameters in config are a hard error naming the rule and
    /// these names.
    pub params: &'static [ParamSpec],
    pub requires: Requires,
}

impl RuleInfo {
    /// `(min, default)` of the declared integer parameter `name`, if any.
    /// The config parser validates against `min` and `LintConfig::int_param`
    /// falls back to `default`, so the registry is the single place both
    /// read.
    pub fn int_param(&self, name: &str) -> Option<(usize, usize)> {
        self.params.iter().find_map(|p| match p.kind {
            ParamKind::Int { min, default } if p.name == name => Some((min, default)),
            _ => None,
        })
    }
}
