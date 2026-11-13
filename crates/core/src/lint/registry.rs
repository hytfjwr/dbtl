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
        default_severity: Severity::Warn,
        summary: "a model directly depends on both a source and a model",
        rationale: "Mixing a raw source with modeled data in one query hides \
                    where the model's real inputs come from. A layer that reads \
                    sources AND models skips the staging contract, so a broken \
                    upstream model can silently change what the raw source \
                    contributes.",
        example: "order_summary joins stg_orders (model) with \
                  raw.payments (source). Fix: model payments through staging \
                  and ref the staging model instead.",
        help: "model the source through a staging model and ref the staging model instead",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::MultipleSourcesJoined,
        id: "multiple_sources_joined",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a model directly depends on 2+ sources",
        rationale: "Joining several raw sources in one model buries the \
                    integration logic where nobody can test or reuse it. Each \
                    source should get its own staging model first, so the join \
                    happens between documented, tested surfaces.",
        example: "fct_orders selects from raw.shopify.orders and \
                  raw.stripe.charges. Fix: stage each source as stg_shopify \
                  / stg_stripe and join those models.",
        help: "give each source its own staging model and join those instead",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::SourceFanout,
        id: "source_fanout",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a source directly feeds too many models",
        rationale: "When one raw source feeds many downstream models, every \
                    one of them re-derives the same cleanup and they drift \
                    apart. A single staging model owns the source contract; \
                    consumers ref that.",
        example: "raw.crm.contacts is read by stg_contacts, fct_leads and \
                  rpt_pipeline. Fix: only stg_contacts reads the source; the \
                  marts ref stg_contacts.",
        help: "let one staging model read the source and ref that model from the others",
        params: &[ParamSpec::int("threshold", 1, 2, "minimum direct children to flag")],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ModelFanout,
        id: "model_fanout",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a model directly feeds too many leaf models",
        rationale: "A hub model that feeds many leaf models concentrates risk: \
                    one bad change fans out to every downstream report, and the \
                    hub's name gives no hint of which consumers break. Split \
                    the hub along ownership lines so each branch has a clear \
                    contract.",
        example: "int_customers feeds fct_orders, fct_returns, rpt_cohorts, \
                  rpt_ltv and dim_customers. Fix: introduce narrower \
                    intermediates per domain instead of one grab-bag hub.",
        help: "split the hub into narrower intermediate models per domain and ref those",
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
        default_severity: Severity::Warn,
        summary: "a model has no parents",
        rationale: "A model that refs nothing upstream is either a hand-maintained \
                    table — data nobody can trace or rebuild — or a forgotten \
                    ref that should have been a source. Real roots belong in \
                    seeds or declared sources so their provenance is explicit.",
        example: "dim_regions is a plain `select ...` with no ref/source \
                  calls. Fix: make it a seed, or declare it as a source if an \
                  external system owns it.",
        help: "select from a source() or ref() so the model has lineage",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::RejoiningUpstream,
        id: "rejoining_upstream",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a redundant hop: A->B->C while A->C already exists",
        rationale: "Re-reading an upstream model when a direct edge already \
                    exists means the intermediate node adds nothing — it just \
                    duplicates a dependency. The extra hop slows builds and \
                    muddies the graph's intent.",
        example: "fct_orders refs int_pricing, and int_pricing's only other \
                  parent stg_orders is also refed directly by fct_orders. Fix: \
                  drop the redundant stg_orders ref or move the shared logic \
                    into int_pricing.",
        help: "drop the redundant direct ref, or move the shared logic into the intermediate model",
        params: &[ParamSpec::selector_list(
            "ignore_shared_parents",
            &[],
            "selectors matching shared parents (the A node) to exempt — e.g. shared dim_* dimensions",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::MartsDependOnSource,
        id: "marts_depend_on_source",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a marts/intermediate model directly depends on a source",
        rationale: "Marts and intermediate models are supposed to consume \
                    staged data. Reading a raw source directly bypasses the \
                    staging layer's naming, typing and freshness guarantees, \
                    so the mart breaks the moment the raw shape changes.",
        example: "fct_orders reads raw.shopify.orders. Fix: create \
                  stg_shopify__orders and ref that from the mart.",
        help: "stage the source and ref the staging model from the mart",
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
                    rule can see. A declared primary_key constraint is NOT \
                    counted by default: most warehouses never enforce it, so \
                    the constraint alone can't stand in for a test — set \
                    accept_primary_key_constraint: true only where the \
                    warehouse does enforce it.",
        example: "stg_orders has no test on order_id. Fix: add unique and \
                  not_null tests on order_id (or a \
                  unique_combination_of_columns test for a composite grain).",
        help: "add unique and not_null tests to one key column, or a unique_combination_of_columns test",
        params: &[
            ParamSpec::string_list(
                "resource_types",
                &["model"],
                Some(&["model", "snapshot", "seed"]),
                "resource kinds the primary-key test applies to",
            ),
            ParamSpec::bool(
                "accept_primary_key_constraint",
                false,
                "count declared primary_key constraints as a uniqueness guarantee",
            ),
        ],
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
        help: "set contract: { enforced: true } on the model",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::UndocumentedModels,
        id: "undocumented_models",
        category: Category::Documentation,
        default_severity: Severity::Warn,
        summary: "a model has no description",
        rationale: "A model with no description forces every reader to \
                    reverse-engineer intent from SQL. Descriptions are the \
                    cheapest documentation that exists — the generated docs \
                    site and the TUI inspector are only as useful as this one \
                    string.",
        example: "stg_customers has no description. Fix: add a `description:` \
                  in its schema.yml entry (or a docs block) saying what one \
                  row represents.",
        help: "add a description to the model's schema.yml entry",
        params: &[ParamSpec::string_list(
            "resource_types",
            &["model"],
            Some(&["model", "snapshot", "seed"]),
            "resource kinds that require a description",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::NamingConventions,
        id: "naming_conventions",
        category: Category::Structure,
        default_severity: Severity::Warn,
        summary: "a model's name misses its layer's prefix",
        rationale: "Layer prefixes let a reader know a model's role before \
                    opening it — and let rules like marts_depend_on_source \
                    classify the DAG at all. A marts model named stg_x is \
                    checked against the wrong policy or none at all.",
        example: "models/marts/orders.sql is named `orders` but marts expects \
                  a fct_/dim_/rpt_ prefix. Fix: rename to fct_orders (or widen \
                  `lint.naming_prefixes.marts` in .dbtl.yml).",
        help: "rename the model with one of its layer's prefixes, or widen lint.naming_prefixes",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::LayerViolation,
        id: "layer_violation",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a backward edge between logical layers",
        rationale: "Layers exist so data flows one way: staging cleans, \
                    intermediate composes, marts serve. An edge that jumps \
                    backward makes the layer boundaries meaningless and turns \
                    refactors into guesswork.",
        example: "stg_orders refs fct_payments (staging reading a mart). Fix: \
                  move the shared logic upstream — staging should never need \
                  a marts model.",
        help: "move the shared logic upstream so the edge points toward marts",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::UnusedSources,
        id: "unused_sources",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a source is declared but never referenced",
        rationale: "A source declaration is a promise that a table matters to \
                    the project. When nothing refs it, the entry is dead \
                    configuration — it rots in the schema.yml, misleads readers \
                    about what feeds the DAG, and inflates the source inventory \
                    beyond the real pipeline.",
        example: "raw.legacy_exports is declared but no model or exposure \
                  reads it. Fix: delete the declaration, or build the staging \
                  model that was meant to consume it.",
        help: "delete the declaration, or ref the source from a model",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::DuplicateSources,
        id: "duplicate_sources",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "two or more sources point at the same warehouse relation",
        rationale: "Declaring the same relation under two source names splits \
                    its tests, docs and freshness into competing records, and a \
                    model can silently read the 'wrong' one. One relation \
                    should have exactly one declaration every model shares.",
        example: "raw.app.users and mirror.app_users both resolve to \
                  app.public.users. Fix: keep one declaration and update every \
                  source() call to it.",
        help: "keep one declaration and point every source() call at it",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::StagingDependsOnStaging,
        id: "staging_depends_on_staging",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a staging model directly depends on another staging model",
        rationale: "Staging models own raw-source cleanup — renaming, typing, \
                    light dedup. Chaining staging into staging hides the fact \
                    that a real transformation leaked in: the second model is \
                    doing intermediate work under a staging name.",
        example: "stg_order_items refs stg_orders to compute line totals. Fix: \
                  move the join or aggregation into an int_ model so staging \
                  stays a pure source-mapping layer.",
        help: "move the join or aggregation into an intermediate model so staging reads sources only",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::TooManyJoins,
        id: "too_many_joins",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a model directly depends on too many data resources",
        rationale: "A model that reads many parents at once is a join hub: the \
                    SQL is doing the work of several intermediate models, and \
                    every upstream change can break it. Splitting the inputs \
                    into intermediate steps keeps each model's contract \
                    testable.",
        example: "fct_checkout reads 9 models and sources. Fix: pre-join the \
                  related inputs in int_ models and ref those instead.",
        help: "pre-join related inputs in intermediate models and ref those instead",
        params: &[ParamSpec::int(
            "threshold",
            1,
            7,
            "minimum direct data-resource parents to flag",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ChainedViews,
        id: "chained_views",
        category: Category::Performance,
        default_severity: Severity::Warn,
        summary: "a chain of consecutive view/ephemeral models is too long",
        rationale: "Views and ephemeral models persist nothing — every one in \
                    the chain is re-planned at query time. A long chain \
                    compounds that work into a single slow query where one or \
                    two materialized checkpoints would do.",
        example: "rpt_daily runs five views deep from stg_events. Fix: \
                  materialize the heavy middle step (table or incremental) so \
                  the chain has a checkpoint.",
        help: "materialize a middle step of the chain as a table or incremental model",
        params: &[ParamSpec::int(
            "threshold",
            1,
            5,
            "minimum consecutive view/ephemeral chain length to flag",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ExposureParentsMaterializations,
        id: "exposure_parents_materializations",
        category: Category::Performance,
        default_severity: Severity::Warn,
        summary: "an exposure directly depends on a source or a view/ephemeral model",
        rationale: "An exposure is what a dashboard actually reads. Pointing it \
                    at a source or an unmaterialized view means every refresh \
                    re-executes the full chain and can break on raw-source \
                    drift — the last hop before the consumer should be a \
                    durable model.",
        example: "the churn dashboard reads int_features (a view) and \
                  raw.events. Fix: point the exposure at the mart that already \
                  materializes those inputs.",
        help: "point the exposure at the durable mart that materializes the inputs",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ExposuresDependentOnPrivateModels,
        id: "exposures_dependent_on_private_models",
        category: Category::Governance,
        default_severity: Severity::Warn,
        summary: "an exposure depends on a private/protected model",
        rationale: "Private and protected access levels mark a model as \
                    internal — ref-able only inside its group or project. \
                    Declaring an external consumer on an internal model \
                    advertises a dependency that violates the access contract's \
                    intent.",
        example: "the finance dashboard depends on dim_salaries (access: \
                  private). Fix: publish a public mart for the dashboard, or \
                  narrow the exposure's depends_on.",
        help: "publish a public mart for the exposure, or narrow the exposure's depends_on",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::UndocumentedSources,
        id: "undocumented_sources",
        category: Category::Documentation,
        default_severity: Severity::Warn,
        summary: "a source has no description",
        rationale: "Sources are the edge where external data enters the \
                    project — the place readers most need provenance notes. An \
                    undocumented source forces everyone to guess what the table \
                    holds and who owns it.",
        example: "raw.billing.events has no description. Fix: add a \
                  description in its schema.yml entry saying what the upstream \
                  system emits.",
        help: "add a description to the source's schema.yml entry",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::UndocumentedColumns,
        id: "undocumented_columns",
        category: Category::Documentation,
        default_severity: Severity::Warn,
        summary: "a model has declared columns without descriptions",
        rationale: "Declaring a column without describing it documents the \
                    schema's shape but not its meaning — the generated docs \
                    show a table of names with no semantics. Column \
                    descriptions are cheap and pay off every time the docs are \
                    read.",
        example: "fct_orders declares order_total without a description. Fix: \
                  describe each column in schema.yml (units, semantics, edge \
                  cases).",
        help: "describe each column in schema.yml",
        params: &[ParamSpec::string_list(
            "resource_types",
            &["model"],
            Some(&["model", "snapshot", "seed", "source"]),
            "resource kinds whose declared columns require descriptions",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ModelDirectories,
        id: "model_directories",
        category: Category::Structure,
        default_severity: Severity::Warn,
        summary: "a model's name prefix and directory point at different layers",
        rationale: "Layer conventions only work when the file location and the \
                    name tell the same story. A marts-prefixed model inside \
                    staging/ (or the reverse) is filed where nobody looks for \
                    it — and folder-scoped config applies to the wrong layer.",
        example: "models/staging/fct_orders.sql. Fix: move the file to \
                  models/marts/, or rename it stg_* if it truly belongs to \
                  staging.",
        help: "move the file under the directory matching its name prefix, or rename it",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::RequiredMetaKeys,
        id: "required_meta_keys",
        category: Category::Governance,
        default_severity: Severity::Warn,
        summary: "a model is missing a required meta key",
        rationale: "Projects that rely on meta for ownership, cost centers or \
                    data-domain tagging only get value when every model carries \
                    the keys. A missing key means the model escapes the reports \
                    and groupings built on that tag.",
        example: "with keys: [owner] configured, dim_products lacks meta.owner. \
                  Fix: add owner: data-team to the model's meta.",
        help: "add the required keys to the model's meta",
        params: &[ParamSpec::string_list(
            "keys",
            &[],
            None,
            "meta keys every model must declare",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ContractColumnsWithoutType,
        id: "contract_columns_without_type",
        category: Category::Governance,
        default_severity: Severity::Warn,
        summary: "a contract-enforced model has declared columns without data_type",
        rationale: "An enforced contract promises the warehouse will check the \
                    declared schema — but a column without data_type \
                    contributes nothing to that check. The contract is only as \
                    strong as the column definitions behind it.",
        example: "dim_customers enforces a contract but column segment has no \
                  data_type. Fix: declare data_type for every contract column.",
        help: "declare data_type for every contract column",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::IncrementalWithoutUniqueKey,
        id: "incremental_without_unique_key",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "an incremental model has no unique_key",
        rationale: "An incremental run merges new rows into the existing \
                    table; without a unique_key dbt cannot match incoming \
                    rows to existing ones, so every run risks silently \
                    duplicating data. Strategies that never merge on a key — \
                    append, insert_overwrite, microbatch — carry no key by \
                    design and are skipped.",
        example: "fct_events materializes incrementally but declares no \
                  unique_key. Fix: set unique_key: event_id in its config \
                  (or a list for a composite grain).",
        help: "set unique_key in the model's config, or exclude the model when the strategy needs no key",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::SourcesWithoutFreshness,
        id: "sources_without_freshness",
        category: Category::Testing,
        default_severity: Severity::Warn,
        summary: "a source has no freshness configured",
        rationale: "Freshness is the only contract a raw feed offers: \
                    without warn_after/error_after thresholds \
                    `dbt source freshness` can never flag the table stale, \
                    so a dead upstream pipeline stays invisible until a \
                    model starts serving old data.",
        example: "source raw declares tables but no freshness block. Fix: \
                  add freshness: {warn_after: {count: 12, period: hour}} at \
                  the source level — it cascades to every table.",
        help: "add a freshness block with warn_after or error_after thresholds to the source",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::SelectStar,
        id: "select_star",
        category: Category::Sql,
        default_severity: Severity::Warn,
        summary: "a model's final SELECT reads * or t.*",
        rationale: "A wildcard projection inherits whatever the upstream \
                    happens to emit today — new columns ride through \
                    unannounced, a removed column surfaces as a downstream \
                    error far from the cause, and the model's contract is \
                    invisible to readers and docs.",
        example: "fct_orders ends with `select * from joined`. Fix: spell \
                  out the columns the model actually produces so its output \
                  contract is explicit.",
        help: "list the columns the model produces",
        params: &[ParamSpec::bool(
            "allow_final_star_over_single_cte",
            false,
            "exempt a final `select *` whose FROM reads exactly one CTE that names every column explicitly",
        )],
        requires: Requires::Sql,
    },
    RuleInfo {
        rule: LintRule::HardCodedReferences,
        id: "hard_coded_references",
        category: Category::Sql,
        default_severity: Severity::Warn,
        summary: "a model names a relation literally instead of ref()/source()",
        rationale: "A hand-written `from analytics.orders` is a dependency \
                    dbt can't see — it misses the DAG (no ref edge), builds \
                    out of order, and reads the wrong table when the schema \
                    or target changes. ref()/source() is how a model \
                    declares what it needs. Not counted: names Jinja \
                    renders, CTE names, `dual`, `information_schema` reads \
                    at any qualification depth, file literals \
                    (`from 'data/x.parquet'`), and names headed by an \
                    earlier-bound FROM alias (`from t, t.arr` reads `t`'s \
                    array column, not a relation).",
        example: "stg_events selects from prod.raw_events instead of \
                  source('prod', 'raw_events'). Fix: declare the relation \
                  with ref()/source() so it joins the lineage and gets \
                  environment-aware naming.",
        help: "declare the relation with ref() or source() so it joins the lineage",
        params: &[ParamSpec::string_list(
            "ignore",
            &[],
            None,
            "glob patterns for relation names to skip",
        )],
        requires: Requires::Sql,
    },
    RuleInfo {
        rule: LintRule::CartesianJoin,
        id: "cartesian_join",
        category: Category::Sql,
        default_severity: Severity::Warn,
        summary: "a JOIN carries no join condition",
        rationale: "A conditionless join multiplies every row of one side by \
                    every row of the other — usually a missing ON clause, and \
                    always a row explosion nobody asked for. `cross join` is \
                    the intentional spelling, so the rule leaves it alone — \
                    the flag is for the join that FORGOT its condition: a \
                    comma-separated `from a, b` with no WHERE, or an `ON` \
                    that can never reject a row (`on true`, `on 1 = 1`).",
        example: "fct_events writes `join dim_users` with no ON. Fix: add \
                  the join predicate (`on e.user_id = u.id`) — or spell it \
                  `cross join` when the product really is intended.",
        help: "add an ON or USING join condition (or spell it `cross join`)",
        params: &[],
        requires: Requires::Sql,
    },
    RuleInfo {
        rule: LintRule::UnknownColumnReference,
        id: "unknown_column_reference",
        category: Category::Sql,
        default_severity: Severity::Error,
        summary: "a model names a column a trusted upstream doesn't emit",
        rationale: "A column that doesn't exist in the parent is either a \
                    typo that fails at run time or a leftover name from a \
                    renamed column — both surface as warehouse errors at \
                    build time instead of at review time.",
        example: "fct_orders selects p.status but parent stg_payments \
                  (enforced contract) declares only order_id/amount. Fix: \
                  correct the column name, or extend the parent's declared \
                  columns.",
        help: "correct the column name, or add it to the upstream's declared columns",
        params: &[],
        requires: Requires::Sql,
    },
    RuleInfo {
        rule: LintRule::ForbiddenPatterns,
        id: "forbidden_patterns",
        category: Category::Sql,
        default_severity: Severity::Warn,
        summary: "a model's raw SQL matches a configured forbidden pattern",
        rationale: "Some constructs are banned by project policy — unsafe \
                    functions, non-deterministic calls, debug leftovers. A \
                    regex list keeps that policy in `.dbtl.yml` where review \
                    can see it, instead of a tribal knowledge check.",
        example: "with patterns: [\"\\\\bUNSAFE_UDF\\\\b\"], stg_events calls \
                  UNSAFE_UDF(). Fix: remove the call, or narrow the pattern \
                  in .dbtl.yml when the match is legitimate.",
        help: "remove the matched construct, or narrow the configured patterns",
        params: &[ParamSpec::regex_list(
            "patterns",
            &[],
            "regular expressions the raw SQL must not match",
        )],
        requires: Requires::Sql,
    },
    RuleInfo {
        rule: LintRule::WarehouseTypeDrift,
        id: "warehouse_type_drift",
        category: Category::Warehouse,
        default_severity: Severity::Warn,
        summary: "a declared column's data_type disagrees with the warehouse",
        rationale: "schema.yml says one type, the warehouse reports another — \
                    the docs lie about the model's real shape, and a reader \
                    who trusts the declared type writes casts and joins that \
                    behave differently than expected.",
        example: "stg_orders declares amount as int but the catalog records \
                  NUMERIC(10,2). Fix: correct the declared data_type (or the \
                    model's cast) so docs match the warehouse.",
        help: "correct the declared data_type or the model's cast so it matches the warehouse",
        params: &[],
        requires: Requires::Catalog,
    },
    RuleInfo {
        rule: LintRule::ColumnsMissingInWarehouse,
        id: "columns_missing_in_warehouse",
        category: Category::Warehouse,
        default_severity: Severity::Warn,
        summary: "a declared column doesn't exist in the warehouse",
        rationale: "Documenting a column the warehouse doesn't have means \
                    the model's docs promise a field its build never \
                    produced — the declaration is a typo'd name, a dropped \
                    select, or a stale schema.yml.",
        example: "fct_orders declares ship_method but the catalog lists no \
                  such column. Fix: rename the declaration to the real \
                  column, or add it back to the model's output.",
        help: "rename the declared column to the real one, or restore it to the model's output",
        params: &[],
        requires: Requires::Catalog,
    },
    RuleInfo {
        rule: LintRule::UnusedModels,
        id: "unused_models",
        category: Category::Modeling,
        default_severity: Severity::Warn,
        summary: "a model in a checked layer has no downstream",
        rationale: "A model nothing reads is dead weight: it still builds on \
                    every run, still sits in the lineage, and its name keeps \
                    promising a purpose it no longer serves. An unused \
                    intermediate is a refactor leftover; an unused staging \
                    model keeps a source contract alive that the project \
                    silently abandoned. Either wire it into a consumer or \
                    delete it so the DAG shows what actually runs.",
        example: "int_pricing_v2 was left behind when fct_orders moved to \
                  int_pricing. Fix: delete the model — or ref it from the \
                  consumer it was built for.",
        help: "delete the model, or ref it from the consumer it was built for",
        params: &[ParamSpec::layer_list(
            "layers",
            &["staging", "intermediate"],
            &["staging", "intermediate", "marts", "other"],
            "model layers the unused check applies to — a built-in layer or a `lint.naming_prefixes` layer",
        )],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::ReferencesDeprecatedModel,
        id: "references_deprecated_model",
        category: Category::Governance,
        default_severity: Severity::Warn,
        summary: "a node directly depends on a model with a deprecation_date",
        rationale: "A deprecation_date is the upstream's promise that the \
                    model is going away. Every dependent that hasn't migrated \
                    breaks when the cutoff passes — and the warning is easy \
                    to miss until the build stops. Repointing early keeps \
                    the cutover on the consumer's schedule, not the \
                    deprecated model's.",
        example: "fct_orders refs dim_customers.v1, which carries \
                  deprecation_date 2026-12-01. Fix: repoint the ref to \
                  dim_customers.v2 before the cutoff.",
        help: "repoint the ref to the deprecated model's successor",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::UndeclaredWarehouseColumns,
        id: "undeclared_warehouse_columns",
        category: Category::Warehouse,
        default_severity: Severity::Warn,
        summary: "a warehouse column the node doesn't declare",
        rationale: "The catalog reports columns schema.yml never mentions — \
                    the declared schema is incomplete. Undeclared columns hide \
                    from column tests, docs coverage, and every check that \
                    trusts the declared shape, so they drift unnoticed until \
                    a reader trips over them.",
        example: "fct_orders declares order_id and total, but the warehouse \
                  also carries currency. Fix: declare the missing columns in \
                  the model's schema.yml entry.",
        help: "declare the warehouse's extra columns in the node's schema.yml entry",
        params: &[],
        requires: Requires::Catalog,
    },
    RuleInfo {
        rule: LintRule::TrailingSemicolon,
        id: "trailing_semicolon",
        category: Category::Sql,
        default_severity: Severity::Error,
        summary: "a model's SQL ends with a semicolon",
        rationale: "dbt wraps a model's SELECT in `create … as (…)` or an \
                    incremental merge — a statement terminator at the end of \
                    the file splits that wrapper into broken syntax at build \
                    time. The semicolon fails the model's first run and then \
                    sits in review waiting to break again.",
        example: "stg_orders ends `select * from raw_orders;`. Fix: drop the \
                  trailing `;` — the model's SELECT must stay a single \
                  statement.",
        help: "remove the trailing semicolon",
        params: &[],
        requires: Requires::Graph,
    },
    RuleInfo {
        rule: LintRule::DuplicateOutputColumns,
        id: "duplicate_output_columns",
        category: Category::Sql,
        default_severity: Severity::Error,
        summary: "a model's final SELECT emits the same output column name twice",
        rationale: "Two outputs sharing one name leave readers no way to say \
                    which column they mean — downstream refs resolve to one of \
                    them silently (or the warehouse rejects the model), and \
                    column order becomes the only thing telling them apart. \
                    Names an un-aliased expression takes from its own shape \
                    (`sum(a)` → `sum`) collide only under Postgres and \
                    Redshift, which really assign them — every other \
                    dialect names such outputs by position or expression \
                    text, so a repeat is never a written-name collision.",
        example: "fct_orders selects `unit_price, unit_price` — the second was \
                  meant to be discounted_price. Fix: alias one of them so every \
                  output name is unique.",
        help: "alias one of the columns so every output name is unique",
        params: &[],
        requires: Requires::Sql,
    },
    RuleInfo {
        rule: LintRule::UnusedCte,
        id: "unused_cte",
        category: Category::Sql,
        default_severity: Severity::Warn,
        summary: "a CTE is defined but never read",
        rationale: "A WITH clause the query never reads is dead code reviewers \
                    still parse — usually the sign a refactor dropped the \
                    consumer, or that the wrong CTE name landed where the read \
                    was meant to go.",
        example: "fct_orders defines `with dedup as (…) select * from orders` — \
                  dedup is never selected from. Fix: delete it, or select from \
                  it where it was meant to be used.",
        help: "delete the CTE, or select from it",
        params: &[],
        requires: Requires::Sql,
    },
    RuleInfo {
        rule: LintRule::OrderByWithoutLimit,
        id: "order_by_without_limit",
        category: Category::Sql,
        default_severity: Severity::Warn,
        summary: "a model's final query has ORDER BY without LIMIT",
        rationale: "A table or view doesn't promise row order — the outer \
                    ORDER BY's cost is paid every build while no reader can \
                    rely on the result. The consumer that wants sorted rows \
                    should sort them at read time. (OFFSET alone isn't a \
                    bound — it shifts the window but never caps the row \
                    count, so `order by … offset n` still flags.)",
        example: "fct_orders ends `select * from items order by created_at` \
                  with no limit. Fix: drop the ORDER BY and let the query \
                  reading the model sort instead.",
        help: "remove the ORDER BY; sort in the query that reads the model",
        params: &[],
        requires: Requires::Sql,
    },
];

/// A rule the registry once carried and has since dropped. The id stays
/// reserved so every surface that names rules — `.dbtl.yml`, `--rules`,
/// `--explain`, a baseline file — can explain the removal and its
/// replacements instead of reporting a bare unknown id.
#[derive(Debug, Clone, Copy)]
pub struct RemovedRule {
    /// The dropped rule id.
    pub id: &'static str,
    /// The live rules covering the same ground — named in the removal
    /// guidance the entry points print.
    pub replacements: &'static [&'static str],
}

/// Rules removed from [`REGISTRY`] — the ONE table every removed-id path
/// reads, so `.dbtl.yml`, `--rules`, `--explain` and the baseline warning
/// can never describe the removal differently.
pub static REMOVED_RULES: &[RemovedRule] = &[RemovedRule {
    id: "undocumented_public_models",
    replacements: &["undocumented_models", "undocumented_columns"],
}];

/// The [`REMOVED_RULES`] row for `id`, when `id` names a dropped rule.
pub fn removed_rule(id: &str) -> Option<&'static RemovedRule> {
    REMOVED_RULES.iter().find(|r| r.id == id)
}

/// The removal sentence the named-rule entry points share for a dropped
/// id — states the removal and the replacement rules; callers embed it in
/// their own usage-error context. `None` when `id` is not a removed rule.
pub fn removed_rule_error(id: &str) -> Option<String> {
    removed_rule(id).map(|r| {
        format!(
            "lint rule '{id}' was removed; use {} instead",
            r.replacements.join(", ")
        )
    })
}

/// A rule family — used for grouping in `--list-rules`-style output and for
/// deciding what data a rule may rely on. All eight values have rules today.
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
    /// Build/rebuild cost findings (view chains, exposure materializations).
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

/// The data a rule needs to evaluate. `Graph` rules run on the `Dag` alone;
/// `Sql` and `Catalog` rules additionally need the parsed-SQL resolver and
/// the warehouse catalog — `run_rules` reports a skip when an enabled rule's
/// required input wasn't supplied.
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

/// One declared rule parameter.
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

    /// A string-list parameter — a default item list plus an optional
    /// allowed-value set (`None` = any strings).
    pub const fn string_list(
        name: &'static str,
        default: &'static [&'static str],
        allowed: Option<&'static [&'static str]>,
        description: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            kind: ParamKind::StringList { default, allowed },
            description,
        }
    }

    /// A regular-expression-list parameter — every item must compile; the
    /// config parser rejects invalid patterns at load (a rule
    /// implementation never sees a broken pattern).
    pub const fn regex_list(
        name: &'static str,
        default: &'static [&'static str],
        description: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            kind: ParamKind::RegexList { default },
            description,
        }
    }

    /// A boolean parameter with a default used when the config doesn't
    /// set it.
    pub const fn bool(name: &'static str, default: bool, description: &'static str) -> Self {
        ParamSpec {
            name,
            kind: ParamKind::Bool { default },
            description,
        }
    }

    /// A selector-list parameter — every item must parse under the same
    /// selector grammar `exclude:` uses (the config parser validates at
    /// load; `state:` selectors are rejected since a lint evaluation has
    /// no diff for them to read). Items land in
    /// [`LintConfig::list_params`](crate::LintConfig) like a `StringList`.
    pub const fn selector_list(
        name: &'static str,
        default: &'static [&'static str],
        description: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            kind: ParamKind::SelectorList { default },
            description,
        }
    }

    /// A layer-list parameter — items name model layers: `builtins` is the
    /// shipped layer set and `lint.naming_prefixes` keys are accepted on
    /// top, so a project-invented layer can be checked like a built-in
    /// one. Items land in
    /// [`LintConfig::list_params`](crate::LintConfig) like a `StringList`.
    pub const fn layer_list(
        name: &'static str,
        default: &'static [&'static str],
        builtins: &'static [&'static str],
        description: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            kind: ParamKind::LayerList { default, builtins },
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
    /// A list of strings — `allowed` bounds the items to a fixed set when
    /// `Some` (`None` = any strings are valid).
    StringList {
        default: &'static [&'static str],
        allowed: Option<&'static [&'static str]>,
    },
    /// A list of regular expressions — the config parser validates each
    /// item compiles at load; items land in `LintConfig::list_params` like
    /// a `StringList`.
    RegexList { default: &'static [&'static str] },
    /// A list of `exclude:`-grammar selectors — the config parser
    /// validates each item at load and rejects `state:` (a lint run has
    /// no diff context for it); items land in
    /// `LintConfig::list_params` like a `StringList`.
    SelectorList { default: &'static [&'static str] },
    /// A list of model-layer names — `builtins` names the shipped layers
    /// and `lint.naming_prefixes` keys are accepted on top (the config
    /// parser validates each item against the union); items land in
    /// `LintConfig::list_params` like a `StringList`.
    LayerList {
        default: &'static [&'static str],
        builtins: &'static [&'static str],
    },
    /// A `true`/`false` switch with a default used when the config
    /// doesn't set it; values land in `LintConfig::bool_params`.
    Bool { default: bool },
}

/// One registry row — everything there is to say about a rule except its
/// implementation function.
#[derive(Debug, Clone, Copy)]
pub struct RuleInfo {
    /// The enum variant this row describes (back-link for `LintRule::info`).
    pub rule: LintRule,
    /// The stable rule id — `LintRule`'s serde name, the `.dbtl.yml` key,
    /// `--rules`' argument and the baseline's `rule` field all in one.
    pub id: &'static str,
    pub category: Category,
    /// Applied when the config doesn't set `severity` for this rule. Only
    /// rules that represent a broken guarantee or a build failure default
    /// to `Error`; every other rule ships `Warn` so it reports without
    /// silently gating a project that never opted in.
    pub default_severity: Severity,
    /// A single line — what the rule flags (the old `description()` text).
    pub summary: &'static str,
    /// Why the rule exists — what goes wrong when the pattern is ignored.
    pub rationale: &'static str,
    /// A concrete bad shape and the fix.
    pub example: &'static str,
    /// One imperative sentence telling the reader what to change — the
    /// fix guidance surfaces print under a finding (SARIF's `help.text`,
    /// json's `help`, `--explain`'s `Help:`). Starts lowercase, ends
    /// without a period, mentions no other tools.
    pub help: &'static str,
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

    /// `(default, allowed)` of the declared list parameter `name`, if any —
    /// `RegexList`/`SelectorList` rows report `allowed: None` (any string
    /// is a candidate; the regex-compile / selector-grammar check is the
    /// validation), `LayerList` rows report `allowed: Some(builtins)` (the
    /// config parser adds `lint.naming_prefixes` keys on top). The config
    /// parser validates `StringList` items against `allowed` and
    /// [`LintConfig::str_list_param`](crate::LintConfig::str_list_param)
    /// falls back to `default`, so the registry is the single place both
    /// read.
    pub fn str_list_param(
        &self,
        name: &str,
    ) -> Option<(&'static [&'static str], Option<&'static [&'static str]>)> {
        self.params.iter().find_map(|p| match p.kind {
            ParamKind::StringList { default, allowed } if p.name == name => {
                Some((default, allowed))
            }
            // A layer list's `builtins` ARE its fixed-enum set — the
            // `lint.naming_prefixes` keys add to them at parse, not at
            // registry level.
            ParamKind::LayerList { default, builtins } if p.name == name => {
                Some((default, Some(builtins)))
            }
            ParamKind::RegexList { default } | ParamKind::SelectorList { default }
                if p.name == name =>
            {
                Some((default, None))
            }
            _ => None,
        })
    }

    /// The declared boolean parameter `name`'s default, if any —
    /// [`LintConfig::bool_param`](crate::LintConfig::bool_param) falls back
    /// to it, so the registry is the single place both read.
    pub fn bool_param(&self, name: &str) -> Option<bool> {
        self.params.iter().find_map(|p| match p.kind {
            ParamKind::Bool { default } if p.name == name => Some(default),
            _ => None,
        })
    }
}
