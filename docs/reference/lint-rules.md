<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->

# Lint rules

40 rules, evaluated by `dbtl lint`, `dbtl check --lint`, the docs generator, and the TUI's Lint lens — one registry table also backs `--list-rules`, `--explain`, SARIF metadata, and `.dbtl.yml` validation, so all of them describe the same rule set.

| Rule | Category | Default severity | Inputs | Summary |
| ---- | -------- | ---------------- | ------ | ------- |
| [`direct_join_to_source`](#direct_join_to_source) | modeling | warn | graph | a model directly depends on both a source and a model |
| [`multiple_sources_joined`](#multiple_sources_joined) | modeling | warn | graph | a model directly depends on 2+ sources |
| [`source_fanout`](#source_fanout) | modeling | warn | graph | a source directly feeds too many models |
| [`model_fanout`](#model_fanout) | modeling | warn | graph | a model directly feeds too many leaf models |
| [`root_models`](#root_models) | modeling | warn | graph | a model has no parents |
| [`rejoining_upstream`](#rejoining_upstream) | modeling | warn | graph | a redundant hop: A->B->C while A->C already exists |
| [`marts_depend_on_source`](#marts_depend_on_source) | modeling | warn | graph | a marts/intermediate model directly depends on a source |
| [`missing_primary_key_tests`](#missing_primary_key_tests) | testing | error | graph | a model lacks a primary-key test (unique+not_null on one column, or unique_combination_of_columns) |
| [`public_models_without_contract`](#public_models_without_contract) | governance | error | graph | access: public without contract: enforced |
| [`undocumented_models`](#undocumented_models) | documentation | warn | graph | a model has no description |
| [`naming_conventions`](#naming_conventions) | structure | warn | graph | a model's name misses its layer's prefix |
| [`layer_violation`](#layer_violation) | modeling | warn | graph | a backward edge between logical layers |
| [`unused_sources`](#unused_sources) | modeling | warn | graph | a source is declared but never referenced |
| [`duplicate_sources`](#duplicate_sources) | modeling | warn | graph | two or more sources point at the same warehouse relation |
| [`staging_depends_on_staging`](#staging_depends_on_staging) | modeling | warn | graph | a staging model directly depends on another staging model |
| [`too_many_joins`](#too_many_joins) | modeling | warn | graph | a model directly depends on too many data resources |
| [`chained_views`](#chained_views) | performance | warn | graph | a chain of consecutive view/ephemeral models is too long |
| [`exposure_parents_materializations`](#exposure_parents_materializations) | performance | warn | graph | an exposure directly depends on a source or a view/ephemeral model |
| [`exposures_dependent_on_private_models`](#exposures_dependent_on_private_models) | governance | warn | graph | an exposure depends on a private/protected model |
| [`undocumented_sources`](#undocumented_sources) | documentation | warn | graph | a source has no description |
| [`undocumented_columns`](#undocumented_columns) | documentation | warn | graph | a model has declared columns without descriptions |
| [`model_directories`](#model_directories) | structure | warn | graph | a model's name prefix and directory point at different layers |
| [`required_meta_keys`](#required_meta_keys) | governance | warn | graph | a model is missing a required meta key |
| [`contract_columns_without_type`](#contract_columns_without_type) | governance | warn | graph | a contract-enforced model has declared columns without data_type |
| [`incremental_without_unique_key`](#incremental_without_unique_key) | modeling | warn | graph | an incremental model has no unique_key |
| [`sources_without_freshness`](#sources_without_freshness) | testing | warn | graph | a source has no freshness configured |
| [`select_star`](#select_star) | sql | warn | graph + parsed SQL | a model's final SELECT reads * or t.* |
| [`hard_coded_references`](#hard_coded_references) | sql | warn | graph + parsed SQL | a model names a relation literally instead of ref()/source() |
| [`cartesian_join`](#cartesian_join) | sql | warn | graph + parsed SQL | a JOIN carries no join condition |
| [`unknown_column_reference`](#unknown_column_reference) | sql | error | graph + parsed SQL | a model names a column a trusted upstream doesn't emit |
| [`forbidden_patterns`](#forbidden_patterns) | sql | warn | graph + parsed SQL | a model's raw SQL matches a configured forbidden pattern |
| [`warehouse_type_drift`](#warehouse_type_drift) | warehouse | warn | graph + catalog | a declared column's data_type disagrees with the warehouse |
| [`columns_missing_in_warehouse`](#columns_missing_in_warehouse) | warehouse | warn | graph + catalog | a declared column doesn't exist in the warehouse |
| [`unused_models`](#unused_models) | modeling | warn | graph | a model in a checked layer has no downstream |
| [`references_deprecated_model`](#references_deprecated_model) | governance | warn | graph | a node directly depends on a model with a deprecation_date |
| [`undeclared_warehouse_columns`](#undeclared_warehouse_columns) | warehouse | warn | graph + catalog | a warehouse column the node doesn't declare |
| [`trailing_semicolon`](#trailing_semicolon) | sql | error | graph | a model's SQL ends with a semicolon |
| [`duplicate_output_columns`](#duplicate_output_columns) | sql | error | graph + parsed SQL | a model's final SELECT emits the same output column name twice |
| [`unused_cte`](#unused_cte) | sql | warn | graph + parsed SQL | a CTE is defined but never read |
| [`order_by_without_limit`](#order_by_without_limit) | sql | warn | graph + parsed SQL | a model's final query has ORDER BY without LIMIT |

## `direct_join_to_source`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A model directly depends on both a source and a model.

**Why:** Mixing a raw source with modeled data in one query hides where the model's real inputs come from. A layer that reads sources AND models skips the staging contract, so a broken upstream model can silently change what the raw source contributes.

**Example:** order_summary joins stg_orders (model) with raw.payments (source). Fix: model payments through staging and ref the staging model instead.

**Fix:** model the source through a staging model and ref the staging model instead

**Parameters:** none.

## `multiple_sources_joined`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A model directly depends on 2+ sources.

**Why:** Joining several raw sources in one model buries the integration logic where nobody can test or reuse it. Each source should get its own staging model first, so the join happens between documented, tested surfaces.

**Example:** fct_orders selects from raw.shopify.orders and raw.stripe.charges. Fix: stage each source as stg_shopify / stg_stripe and join those models.

**Fix:** give each source its own staging model and join those instead

**Parameters:** none.

## `source_fanout`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A source directly feeds too many models.

**Why:** When one raw source feeds many downstream models, every one of them re-derives the same cleanup and they drift apart. A single staging model owns the source contract; consumers ref that.

**Example:** raw.crm.contacts is read by stg_contacts, fct_leads and rpt_pipeline. Fix: only stg_contacts reads the source; the marts ref stg_contacts.

**Fix:** let one staging model read the source and ref that model from the others

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `threshold` | integer ≥ 1 | `2` | minimum direct children to flag |

## `model_fanout`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A model directly feeds too many leaf models.

**Why:** A hub model that feeds many leaf models concentrates risk: one bad change fans out to every downstream report, and the hub's name gives no hint of which consumers break. Split the hub along ownership lines so each branch has a clear contract.

**Example:** int_customers feeds fct_orders, fct_returns, rpt_cohorts, rpt_ltv and dim_customers. Fix: introduce narrower intermediates per domain instead of one grab-bag hub.

**Fix:** split the hub into narrower intermediate models per domain and ref those

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `threshold` | integer ≥ 1 | `5` | minimum direct leaf children to flag |

## `root_models`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A model has no parents.

**Why:** A model that refs nothing upstream is either a hand-maintained table — data nobody can trace or rebuild — or a forgotten ref that should have been a source. Real roots belong in seeds or declared sources so their provenance is explicit.

**Example:** dim_regions is a plain `select ...` with no ref/source calls. Fix: make it a seed, or declare it as a source if an external system owns it.

**Fix:** select from a source() or ref() so the model has lineage

**Parameters:** none.

## `rejoining_upstream`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A redundant hop: A->B->C while A->C already exists.

**Why:** Re-reading an upstream model when a direct edge already exists means the intermediate node adds nothing — it just duplicates a dependency. The extra hop slows builds and muddies the graph's intent.

**Example:** fct_orders refs int_pricing, and int_pricing's only other parent stg_orders is also refed directly by fct_orders. Fix: drop the redundant stg_orders ref or move the shared logic into int_pricing.

**Fix:** drop the redundant direct ref, or move the shared logic into the intermediate model

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `ignore_shared_parents` | list of selectors | `[]` | selectors matching shared parents (the A node) to exempt — e.g. shared dim_* dimensions |

## `marts_depend_on_source`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A marts/intermediate model directly depends on a source.

**Why:** Marts and intermediate models are supposed to consume staged data. Reading a raw source directly bypasses the staging layer's naming, typing and freshness guarantees, so the mart breaks the moment the raw shape changes.

**Example:** fct_orders reads raw.shopify.orders. Fix: create stg_shopify__orders and ref that from the mart.

**Fix:** stage the source and ref the staging model from the mart

**Parameters:** none.

## `missing_primary_key_tests`

Category: `testing` · Default severity: `error` · Inputs: graph

A model lacks a primary-key test (unique+not_null on one column, or unique_combination_of_columns).

**Why:** Joins and fanout checks assume every model has a stable grain. Without a uniqueness test, a duplicated key turns a downstream join into a silent row explosion that no lint rule can see. A declared primary_key constraint is NOT counted by default: most warehouses never enforce it, so the constraint alone can't stand in for a test — set accept_primary_key_constraint: true only where the warehouse does enforce it.

**Example:** stg_orders has no test on order_id. Fix: add unique and not_null tests on order_id (or a unique_combination_of_columns test for a composite grain).

**Fix:** add unique and not_null tests to one key column, or a unique_combination_of_columns test

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `resource_types` | list of strings (one of `model` `snapshot` `seed`) | `[model]` | resource kinds the primary-key test applies to |
| `accept_primary_key_constraint` | boolean | `false` | count declared primary_key constraints as a uniqueness guarantee |

## `public_models_without_contract`

Category: `governance` · Default severity: `error` · Inputs: graph

Access: public without contract: enforced.

**Why:** Declaring a model public advertises it as a stable interface, but without an enforced contract dbt won't check that its columns keep their promised names and types. A rename ships a breaking change with no warning.

**Example:** rpt_monthly_revenue sets access: public but no contract. Fix: add contract: { enforced: true } so schema drift fails the build instead of downstream BI.

**Fix:** set contract: { enforced: true } on the model

**Parameters:** none.

## `undocumented_models`

Category: `documentation` · Default severity: `warn` · Inputs: graph

A model has no description.

**Why:** A model with no description forces every reader to reverse-engineer intent from SQL. Descriptions are the cheapest documentation that exists — the generated docs site and the TUI inspector are only as useful as this one string.

**Example:** stg_customers has no description. Fix: add a `description:` in its schema.yml entry (or a docs block) saying what one row represents.

**Fix:** add a description to the model's schema.yml entry

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `resource_types` | list of strings (one of `model` `snapshot` `seed`) | `[model]` | resource kinds that require a description |

## `naming_conventions`

Category: `structure` · Default severity: `warn` · Inputs: graph

A model's name misses its layer's prefix.

**Why:** Layer prefixes let a reader know a model's role before opening it — and let rules like marts_depend_on_source classify the DAG at all. A marts model named stg_x is checked against the wrong policy or none at all.

**Example:** models/marts/orders.sql is named `orders` but marts expects a fct_/dim_/rpt_ prefix. Fix: rename to fct_orders (or widen `lint.naming_prefixes.marts` in .dbtl.yml).

**Fix:** rename the model with one of its layer's prefixes, or widen lint.naming_prefixes

**Parameters:** none.

## `layer_violation`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A backward edge between logical layers.

**Why:** Layers exist so data flows one way: staging cleans, intermediate composes, marts serve. An edge that jumps backward makes the layer boundaries meaningless and turns refactors into guesswork.

**Example:** stg_orders refs fct_payments (staging reading a mart). Fix: move the shared logic upstream — staging should never need a marts model.

**Fix:** move the shared logic upstream so the edge points toward marts

**Parameters:** none.

## `unused_sources`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A source is declared but never referenced.

**Why:** A source declaration is a promise that a table matters to the project. When nothing refs it, the entry is dead configuration — it rots in the schema.yml, misleads readers about what feeds the DAG, and inflates the source inventory beyond the real pipeline.

**Example:** raw.legacy_exports is declared but no model or exposure reads it. Fix: delete the declaration, or build the staging model that was meant to consume it.

**Fix:** delete the declaration, or ref the source from a model

**Parameters:** none.

## `duplicate_sources`

Category: `modeling` · Default severity: `warn` · Inputs: graph

Two or more sources point at the same warehouse relation.

**Why:** Declaring the same relation under two source names splits its tests, docs and freshness into competing records, and a model can silently read the 'wrong' one. One relation should have exactly one declaration every model shares.

**Example:** raw.app.users and mirror.app_users both resolve to app.public.users. Fix: keep one declaration and update every source() call to it.

**Fix:** keep one declaration and point every source() call at it

**Parameters:** none.

## `staging_depends_on_staging`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A staging model directly depends on another staging model.

**Why:** Staging models own raw-source cleanup — renaming, typing, light dedup. Chaining staging into staging hides the fact that a real transformation leaked in: the second model is doing intermediate work under a staging name.

**Example:** stg_order_items refs stg_orders to compute line totals. Fix: move the join or aggregation into an int_ model so staging stays a pure source-mapping layer.

**Fix:** move the join or aggregation into an intermediate model so staging reads sources only

**Parameters:** none.

## `too_many_joins`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A model directly depends on too many data resources.

**Why:** A model that reads many parents at once is a join hub: the SQL is doing the work of several intermediate models, and every upstream change can break it. Splitting the inputs into intermediate steps keeps each model's contract testable.

**Example:** fct_checkout reads 9 models and sources. Fix: pre-join the related inputs in int_ models and ref those instead.

**Fix:** pre-join related inputs in intermediate models and ref those instead

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `threshold` | integer ≥ 1 | `7` | minimum direct data-resource parents to flag |

## `chained_views`

Category: `performance` · Default severity: `warn` · Inputs: graph

A chain of consecutive view/ephemeral models is too long.

**Why:** Views and ephemeral models persist nothing — every one in the chain is re-planned at query time. A long chain compounds that work into a single slow query where one or two materialized checkpoints would do.

**Example:** rpt_daily runs five views deep from stg_events. Fix: materialize the heavy middle step (table or incremental) so the chain has a checkpoint.

**Fix:** materialize a middle step of the chain as a table or incremental model

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `threshold` | integer ≥ 1 | `5` | minimum consecutive view/ephemeral chain length to flag |

## `exposure_parents_materializations`

Category: `performance` · Default severity: `warn` · Inputs: graph

An exposure directly depends on a source or a view/ephemeral model.

**Why:** An exposure is what a dashboard actually reads. Pointing it at a source or an unmaterialized view means every refresh re-executes the full chain and can break on raw-source drift — the last hop before the consumer should be a durable model.

**Example:** the churn dashboard reads int_features (a view) and raw.events. Fix: point the exposure at the mart that already materializes those inputs.

**Fix:** point the exposure at the durable mart that materializes the inputs

**Parameters:** none.

## `exposures_dependent_on_private_models`

Category: `governance` · Default severity: `warn` · Inputs: graph

An exposure depends on a private/protected model.

**Why:** Private and protected access levels mark a model as internal — ref-able only inside its group or project. Declaring an external consumer on an internal model advertises a dependency that violates the access contract's intent.

**Example:** the finance dashboard depends on dim_salaries (access: private). Fix: publish a public mart for the dashboard, or narrow the exposure's depends_on.

**Fix:** publish a public mart for the exposure, or narrow the exposure's depends_on

**Parameters:** none.

## `undocumented_sources`

Category: `documentation` · Default severity: `warn` · Inputs: graph

A source has no description.

**Why:** Sources are the edge where external data enters the project — the place readers most need provenance notes. An undocumented source forces everyone to guess what the table holds and who owns it.

**Example:** raw.billing.events has no description. Fix: add a description in its schema.yml entry saying what the upstream system emits.

**Fix:** add a description to the source's schema.yml entry

**Parameters:** none.

## `undocumented_columns`

Category: `documentation` · Default severity: `warn` · Inputs: graph

A model has declared columns without descriptions.

**Why:** Declaring a column without describing it documents the schema's shape but not its meaning — the generated docs show a table of names with no semantics. Column descriptions are cheap and pay off every time the docs are read.

**Example:** fct_orders declares order_total without a description. Fix: describe each column in schema.yml (units, semantics, edge cases).

**Fix:** describe each column in schema.yml

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `resource_types` | list of strings (one of `model` `snapshot` `seed` `source`) | `[model]` | resource kinds whose declared columns require descriptions |

## `model_directories`

Category: `structure` · Default severity: `warn` · Inputs: graph

A model's name prefix and directory point at different layers.

**Why:** Layer conventions only work when the file location and the name tell the same story. A marts-prefixed model inside staging/ (or the reverse) is filed where nobody looks for it — and folder-scoped config applies to the wrong layer.

**Example:** models/staging/fct_orders.sql. Fix: move the file to models/marts/, or rename it stg_* if it truly belongs to staging.

**Fix:** move the file under the directory matching its name prefix, or rename it

**Parameters:** none.

## `required_meta_keys`

Category: `governance` · Default severity: `warn` · Inputs: graph

A model is missing a required meta key.

**Why:** Projects that rely on meta for ownership, cost centers or data-domain tagging only get value when every model carries the keys. A missing key means the model escapes the reports and groupings built on that tag.

**Example:** with keys: [owner] configured, dim_products lacks meta.owner. Fix: add owner: data-team to the model's meta.

**Fix:** add the required keys to the model's meta

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `keys` | list of strings | `[]` | meta keys every model must declare |

## `contract_columns_without_type`

Category: `governance` · Default severity: `warn` · Inputs: graph

A contract-enforced model has declared columns without data_type.

**Why:** An enforced contract promises the warehouse will check the declared schema — but a column without data_type contributes nothing to that check. The contract is only as strong as the column definitions behind it.

**Example:** dim_customers enforces a contract but column segment has no data_type. Fix: declare data_type for every contract column.

**Fix:** declare data_type for every contract column

**Parameters:** none.

## `incremental_without_unique_key`

Category: `modeling` · Default severity: `warn` · Inputs: graph

An incremental model has no unique_key.

**Why:** An incremental run merges new rows into the existing table; without a unique_key dbt cannot match incoming rows to existing ones, so every run risks silently duplicating data. Strategies that never merge on a key — append, insert_overwrite, microbatch — carry no key by design and are skipped.

**Example:** fct_events materializes incrementally but declares no unique_key. Fix: set unique_key: event_id in its config (or a list for a composite grain).

**Fix:** set unique_key in the model's config, or exclude the model when the strategy needs no key

**Parameters:** none.

## `sources_without_freshness`

Category: `testing` · Default severity: `warn` · Inputs: graph

A source has no freshness configured.

**Why:** Freshness is the only contract a raw feed offers: without warn_after/error_after thresholds `dbt source freshness` can never flag the table stale, so a dead upstream pipeline stays invisible until a model starts serving old data.

**Example:** source raw declares tables but no freshness block. Fix: add freshness: {warn_after: {count: 12, period: hour}} at the source level — it cascades to every table.

**Fix:** add a freshness block with warn_after or error_after thresholds to the source

**Parameters:** none.

## `select_star`

Category: `sql` · Default severity: `warn` · Inputs: graph + parsed SQL

A model's final SELECT reads * or t.*.

**Why:** A wildcard projection inherits whatever the upstream happens to emit today — new columns ride through unannounced, a removed column surfaces as a downstream error far from the cause, and the model's contract is invisible to readers and docs.

**Example:** fct_orders ends with `select * from joined`. Fix: spell out the columns the model actually produces so its output contract is explicit.

**Fix:** list the columns the model produces

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `allow_final_star_over_single_cte` | boolean | `false` | exempt a final `select *` whose FROM reads exactly one CTE that names every column explicitly |

## `hard_coded_references`

Category: `sql` · Default severity: `warn` · Inputs: graph + parsed SQL

A model names a relation literally instead of ref()/source().

**Why:** A hand-written `from analytics.orders` is a dependency dbt can't see — it misses the DAG (no ref edge), builds out of order, and reads the wrong table when the schema or target changes. ref()/source() is how a model declares what it needs. Not counted: names Jinja renders, CTE names, `dual`, `information_schema` reads at any qualification depth, file literals (`from 'data/x.parquet'`), and names headed by an earlier-bound FROM alias (`from t, t.arr` reads `t`'s array column, not a relation).

**Example:** stg_events selects from prod.raw_events instead of source('prod', 'raw_events'). Fix: declare the relation with ref()/source() so it joins the lineage and gets environment-aware naming.

**Fix:** declare the relation with ref() or source() so it joins the lineage

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `ignore` | list of strings | `[]` | glob patterns for relation names to skip |

## `cartesian_join`

Category: `sql` · Default severity: `warn` · Inputs: graph + parsed SQL

A JOIN carries no join condition.

**Why:** A conditionless join multiplies every row of one side by every row of the other — usually a missing ON clause, and always a row explosion nobody asked for. `cross join` is the intentional spelling, so the rule leaves it alone — the flag is for the join that FORGOT its condition: a comma-separated `from a, b` with no WHERE, or an `ON` that can never reject a row (`on true`, `on 1 = 1`).

**Example:** fct_events writes `join dim_users` with no ON. Fix: add the join predicate (`on e.user_id = u.id`) — or spell it `cross join` when the product really is intended.

**Fix:** add an ON or USING join condition (or spell it `cross join`)

**Parameters:** none.

## `unknown_column_reference`

Category: `sql` · Default severity: `error` · Inputs: graph + parsed SQL

A model names a column a trusted upstream doesn't emit.

**Why:** A column that doesn't exist in the parent is either a typo that fails at run time or a leftover name from a renamed column — both surface as warehouse errors at build time instead of at review time.

**Example:** fct_orders selects p.status but parent stg_payments (enforced contract) declares only order_id/amount. Fix: correct the column name, or extend the parent's declared columns.

**Fix:** correct the column name, or add it to the upstream's declared columns

**Parameters:** none.

## `forbidden_patterns`

Category: `sql` · Default severity: `warn` · Inputs: graph + parsed SQL

A model's raw SQL matches a configured forbidden pattern.

**Why:** Some constructs are banned by project policy — unsafe functions, non-deterministic calls, debug leftovers. A regex list keeps that policy in `.dbtl.yml` where review can see it, instead of a tribal knowledge check.

**Example:** with patterns: ["\\bUNSAFE_UDF\\b"], stg_events calls UNSAFE_UDF(). Fix: remove the call, or narrow the pattern in .dbtl.yml when the match is legitimate.

**Fix:** remove the matched construct, or narrow the configured patterns

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `patterns` | list of regular expressions | `[]` | regular expressions the raw SQL must not match |

## `warehouse_type_drift`

Category: `warehouse` · Default severity: `warn` · Inputs: graph + catalog

A declared column's data_type disagrees with the warehouse.

**Why:** schema.yml says one type, the warehouse reports another — the docs lie about the model's real shape, and a reader who trusts the declared type writes casts and joins that behave differently than expected.

**Example:** stg_orders declares amount as int but the catalog records NUMERIC(10,2). Fix: correct the declared data_type (or the model's cast) so docs match the warehouse.

**Fix:** correct the declared data_type or the model's cast so it matches the warehouse

**Parameters:** none.

## `columns_missing_in_warehouse`

Category: `warehouse` · Default severity: `warn` · Inputs: graph + catalog

A declared column doesn't exist in the warehouse.

**Why:** Documenting a column the warehouse doesn't have means the model's docs promise a field its build never produced — the declaration is a typo'd name, a dropped select, or a stale schema.yml.

**Example:** fct_orders declares ship_method but the catalog lists no such column. Fix: rename the declaration to the real column, or add it back to the model's output.

**Fix:** rename the declared column to the real one, or restore it to the model's output

**Parameters:** none.

## `unused_models`

Category: `modeling` · Default severity: `warn` · Inputs: graph

A model in a checked layer has no downstream.

**Why:** A model nothing reads is dead weight: it still builds on every run, still sits in the lineage, and its name keeps promising a purpose it no longer serves. An unused intermediate is a refactor leftover; an unused staging model keeps a source contract alive that the project silently abandoned. Either wire it into a consumer or delete it so the DAG shows what actually runs.

**Example:** int_pricing_v2 was left behind when fct_orders moved to int_pricing. Fix: delete the model — or ref it from the consumer it was built for.

**Fix:** delete the model, or ref it from the consumer it was built for

**Parameters:**

| Name | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `layers` | list of layer names (`staging` `intermediate` `marts` `other` or a `lint.naming_prefixes` layer) | `[staging, intermediate]` | model layers the unused check applies to — a built-in layer or a `lint.naming_prefixes` layer |

## `references_deprecated_model`

Category: `governance` · Default severity: `warn` · Inputs: graph

A node directly depends on a model with a deprecation_date.

**Why:** A deprecation_date is the upstream's promise that the model is going away. Every dependent that hasn't migrated breaks when the cutoff passes — and the warning is easy to miss until the build stops. Repointing early keeps the cutover on the consumer's schedule, not the deprecated model's.

**Example:** fct_orders refs dim_customers.v1, which carries deprecation_date 2026-12-01. Fix: repoint the ref to dim_customers.v2 before the cutoff.

**Fix:** repoint the ref to the deprecated model's successor

**Parameters:** none.

## `undeclared_warehouse_columns`

Category: `warehouse` · Default severity: `warn` · Inputs: graph + catalog

A warehouse column the node doesn't declare.

**Why:** The catalog reports columns schema.yml never mentions — the declared schema is incomplete. Undeclared columns hide from column tests, docs coverage, and every check that trusts the declared shape, so they drift unnoticed until a reader trips over them.

**Example:** fct_orders declares order_id and total, but the warehouse also carries currency. Fix: declare the missing columns in the model's schema.yml entry.

**Fix:** declare the warehouse's extra columns in the node's schema.yml entry

**Parameters:** none.

## `trailing_semicolon`

Category: `sql` · Default severity: `error` · Inputs: graph

A model's SQL ends with a semicolon.

**Why:** dbt wraps a model's SELECT in `create … as (…)` or an incremental merge — a statement terminator at the end of the file splits that wrapper into broken syntax at build time. The semicolon fails the model's first run and then sits in review waiting to break again.

**Example:** stg_orders ends `select * from raw_orders;`. Fix: drop the trailing `;` — the model's SELECT must stay a single statement.

**Fix:** remove the trailing semicolon

**Parameters:** none.

## `duplicate_output_columns`

Category: `sql` · Default severity: `error` · Inputs: graph + parsed SQL

A model's final SELECT emits the same output column name twice.

**Why:** Two outputs sharing one name leave readers no way to say which column they mean — downstream refs resolve to one of them silently (or the warehouse rejects the model), and column order becomes the only thing telling them apart. Names an un-aliased expression takes from its own shape (`sum(a)` → `sum`) collide only under Postgres and Redshift, which really assign them — every other dialect names such outputs by position or expression text, so a repeat is never a written-name collision.

**Example:** fct_orders selects `unit_price, unit_price` — the second was meant to be discounted_price. Fix: alias one of them so every output name is unique.

**Fix:** alias one of the columns so every output name is unique

**Parameters:** none.

## `unused_cte`

Category: `sql` · Default severity: `warn` · Inputs: graph + parsed SQL

A CTE is defined but never read.

**Why:** A WITH clause the query never reads is dead code reviewers still parse — usually the sign a refactor dropped the consumer, or that the wrong CTE name landed where the read was meant to go.

**Example:** fct_orders defines `with dedup as (…) select * from orders` — dedup is never selected from. Fix: delete it, or select from it where it was meant to be used.

**Fix:** delete the CTE, or select from it

**Parameters:** none.

## `order_by_without_limit`

Category: `sql` · Default severity: `warn` · Inputs: graph + parsed SQL

A model's final query has ORDER BY without LIMIT.

**Why:** A table or view doesn't promise row order — the outer ORDER BY's cost is paid every build while no reader can rely on the result. The consumer that wants sorted rows should sort them at read time. (OFFSET alone isn't a bound — it shifts the window but never caps the row count, so `order by … offset n` still flags.)

**Example:** fct_orders ends `select * from items order by created_at` with no limit. Fix: drop the ORDER BY and let the query reading the model sort instead.

**Fix:** remove the ORDER BY; sort in the query that reads the model

**Parameters:** none.
