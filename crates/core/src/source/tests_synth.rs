use super::*;

/// The shared context [`synth_tests`] and [`apply_meta`] thread through every
/// schema-file test pass: the running synth-key counter, the project name, the
/// `ref()`/`source()` extractors + name index a `relationships` test's `to:`
/// resolves against (the same lookups the .sql scan uses), and the var tables
/// a `config.enabled:`/`enabled:` Jinja string evaluates against.
pub(super) struct TestCtx<'a> {
    pub(super) seq: usize,
    pub(super) proj: &'a str,
    pub(super) pat: &'a Patterns,
    pub(super) name_index: &'a HashMap<String, String>,
    pub(super) vars: &'a Vars,
}

/// Attach schema-file metadata (description / materialization / tags / columns)
/// and tests to already-discovered nodes, matched by `<type>.<proj>.<name>`. A
/// schema entry with no matching node (e.g. a documented but absent model) is
/// skipped — we never fabricate a node from docs alone.
///
/// `yml_path` is the declaring file, project-root-relative — the same
/// convention the manifest side normalizes `patch_path` to, so a schema
/// entry makes the node YAML-declared in both modes.
pub(super) fn apply_meta(
    nodes: &mut HashMap<String, RawNode>,
    ctx: &mut TestCtx<'_>,
    type_prefix: &str,
    entries: &[SchemaModel],
    yml_path: &str,
    docs: &DocBlocks,
    warnings: &mut Vec<String>,
) {
    for entry in entries {
        let uid = format!("{type_prefix}.{}.{}", ctx.proj, entry.name);
        // Schema-level `config.enabled:` parks the node like dbt's `disabled`
        // when it evaluates false: drop it (its edges are pruned by
        // Dag::build's prune-first step) and record none of its docs/tests.
        if ctx.vars.schema_disabled(
            entry.config.enabled.as_ref(),
            &format!("{yml_path}: {type_prefix} '{}' config.enabled", entry.name),
            warnings,
        ) {
            nodes.remove(&uid);
            continue;
        }
        // Skip docs for an absent node; this also gates the synth_tests calls
        // below (a documented-but-absent model contributes no tests).
        let Some(node) = nodes.get_mut(&uid) else {
            continue;
        };
        // A schema entry IS what dbt records as `patch_path` — this file is
        // where the node's YAML-declared attributes live.
        node.patch_path = Some(yml_path.to_string());
        if node.description.is_none() {
            node.description = entry.description.clone().map(|d| docs.render(&d, warnings));
        }
        // An explicit schema materialization wins over the `view` default but
        // not over an in-file `config()` (already set, so only fill if None).
        if node.config.materialized.is_none() {
            node.config.materialized = entry.config.materialized.clone();
        }
        // Same precedence for `config.alias`: schema.yml fills only an unset
        // in-file alias (in-file `config(alias=…)` wins, like materialized).
        if node.alias.is_none() {
            node.alias = entry.config.alias.clone();
        }
        // Same chain for `config.unique_key`: schema.yml fills only when no
        // in-file `config(unique_key=…)` declared it; the declared value
        // keeps its shape (a bare string or a string list).
        if node.config.unique_key.is_none() && !entry.config.unique_key.is_null() {
            node.config.unique_key = Some(entry.config.unique_key.clone());
        }
        // The governance surface. `group` is the one resource-agnostic field
        // (dbt groups admit models, seeds, snapshots); the rest are
        // MODEL-only in dbt — recording them on a seed/snapshot would
        // diverge from the real manifest. Each fill respects the same
        // "in-file config() > schema.yml > tree > default" chain
        // materialized follows: only an unset slot is filled.
        // `config:` wins over the bare top-level spelling (declaring both
        // is a dbt parse error — merge degraded like `meta:`'s).
        if node.group.is_none() {
            node.group = entry.config.group.clone().or_else(|| entry.group.clone());
        }
        if type_prefix == "model" {
            if node.access.is_none() {
                node.access = entry.config.access.clone().or_else(|| entry.access.clone());
            }
            if node.contract.is_none() {
                node.contract = contract_value(&entry.config.contract);
            }
            if node.config.incremental_strategy.is_none() {
                node.config.incremental_strategy = entry.config.incremental_strategy.clone();
            }
            // `deprecation_date` and `constraints` are PROPERTIES (no
            // `config:`/`config()`/tree intake) — schema.yml is their only
            // source-mode origin.
            if node.deprecation_date.is_none() {
                node.deprecation_date = entry.deprecation_date.clone();
            }
            if node.constraints.is_empty() {
                node.constraints = entry.constraints.clone();
            }
        }
        if !entry.tags.is_empty() {
            node.tags = entry.tags.clone();
        }
        // Schema `meta:` / `config.meta:` merge per top-level key: the entry's
        // `config.meta` wins over its top-level `meta:` (declaring both is a
        // dbt parse error — degraded here to a warning + that merge), and any
        // in-file `config(meta=…)` wins over both (`entry().or_insert`). The
        // bare top-level `meta:` spelling is accepted for EVERY entry type —
        // not just models — verified against real dbt 1.12.6 (seeds and
        // snapshots honour it identically to `config.meta`).
        if entry.meta.is_object() && entry.config.meta.is_object() {
            warnings.push(format!(
                "{} '{}' declares meta: both at the entry level and under config: (a dbt parse error); merging with config: winning",
                type_prefix, entry.name
            ));
        }
        let what = format!("{type_prefix} '{}'", entry.name);
        let mut schema_meta = meta_map(&entry.meta, warnings, &what);
        schema_meta.extend(meta_map(
            &entry.config.meta,
            warnings,
            &format!("{what} config"),
        ));
        for (k, v) in schema_meta {
            node.meta.entry(k).or_insert(v);
        }
        if !entry.columns.is_empty() {
            node.columns = columns_json(&entry.columns, docs, warnings);
        }
        // `node`'s borrow ends here; synth_tests re-borrows `nodes` mutably.
        synth_tests(
            nodes,
            ctx,
            &uid,
            &entry.name,
            None,
            &entry.tests,
            yml_path,
            warnings,
        );
        synth_tests(
            nodes,
            ctx,
            &uid,
            &entry.name,
            None,
            &entry.data_tests,
            yml_path,
            warnings,
        );
        for col in &entry.columns {
            synth_tests(
                nodes,
                ctx,
                &uid,
                &entry.name,
                Some(&col.name),
                &col.tests,
                yml_path,
                warnings,
            );
            synth_tests(
                nodes,
                ctx,
                &uid,
                &entry.name,
                Some(&col.name),
                &col.data_tests,
                yml_path,
                warnings,
            );
        }
    }
}

/// A `contract` declaration → the wire type `RawNode.contract` carries.
/// dbt's canonical shape is the `{enforced: bool}` object (`contract:
/// {enforced: true}` in YAML, `config(contract={'enforced': True})` in
/// Jinja); a bare bool is tolerated as `enforced: <bool>`. Anything else —
/// Null, an unresolvable expression, a malformed map — yields no contract
/// rather than fabricating one (the same "never guess" stance the other
/// config readers take).
pub(super) fn contract_value(v: &serde_json::Value) -> Option<RawContract> {
    match v {
        serde_json::Value::Bool(b) => Some(RawContract { enforced: *b }),
        serde_json::Value::Object(m) => Some(RawContract {
            enforced: m.get("enforced").and_then(|e| e.as_bool()).unwrap_or(false),
        }),
        _ => None,
    }
}

/// A YAML `meta:` mapping → the `serde_json` map `RawNode.meta` /
/// `RawSource.meta` carry. A `Null`/absent `meta:` yields the empty map; a
/// non-mapping `meta:` (a dbt error — meta must be a key/value dict) warns
/// and degrades to empty, never a parse failure.
pub(super) fn meta_map(
    v: &serde_json::Value,
    warnings: &mut Vec<String>,
    what: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                out.insert(k.clone(), v.clone());
            }
            out
        }
        serde_json::Value::Null => out,
        _ => {
            warnings.push(format!(
                "{what} declares a non-mapping 'meta:' — a key/value dict is expected; ignoring it"
            ));
            out
        }
    }
}

/// A source table's merged `meta`: each level overrides the previous per
/// top-level key (dbt's shallow merge). Precedence low → high, verified:
/// `sources:` tree `+meta` < source `meta:` < source `config.meta` < table
/// `meta:` < table `config.meta` — a table's top-level `meta:` beats the
/// source's `config.meta`, so the level order is props, then config, then
/// the next level down.
pub(super) fn source_meta(
    tree: serde_json::Map<String, serde_json::Value>,
    src: &SchemaSource,
    tbl: &SchemaSourceTable,
    warnings: &mut Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = tree;
    for (v, what) in [
        (&src.meta, format!("source '{}'", src.name)),
        (&src.config.meta, format!("source '{}' config", src.name)),
        (
            &tbl.meta,
            format!("source '{}' table '{}'", src.name, tbl.name),
        ),
        (
            &tbl.config.meta,
            format!("source '{}' table '{}' config", src.name, tbl.name),
        ),
    ] {
        out.extend(meta_map(v, warnings, &what));
    }
    out
}

/// A source table's `tags`: dbt accumulates every tag channel instead of
/// overriding — the `config.tags` of the source then the table, then the
/// top-level `tags:` of the TABLE then the source (verified on 1.12.6:
/// source config `feed_cfg`, table config `lane_cfg`, table props
/// `lane_tbl`, source props `nightly` lands as
/// `['feed_cfg','lane_cfg','lane_tbl','nightly']`) — deduplicated in that
/// order.
pub(super) fn source_tags(src: &SchemaSource, tbl: &SchemaSourceTable) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tags in [&src.config.tags, &tbl.config.tags, &tbl.tags, &src.tags] {
        push_unique(&mut out, tags);
    }
    out
}

/// Append the tags `out` doesn't already carry, preserving `new_tags`
/// order (dbt's tag union is ordered, first occurrence keeps its slot).
fn push_unique(tags: &mut Vec<String>, new_tags: &[String]) {
    for tag in new_tags {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
}

/// A source table's `loaded_at_field` — resolved down table top-level >
/// table `config:` > source top-level > source `config:`, each level
/// three-state (absent inherits, `null` clears, a value overrides).
/// UNLIKE `meta:`/`freshness:`, the top-level spelling beats `config:`
/// within each level (verified on 1.12.6: a table's `loaded_at_field:`
/// wins over its own `config.loaded_at_field`, and the source's top-level
/// value beats the source's `config.loaded_at_field`; the table's config
/// still outranks every source level). `+loaded_at_field` on the
/// `sources:` tree is a key dbt ignores — deliberately not resolved.
pub(super) fn source_loaded_at_field(
    src: &SchemaSource,
    tbl: &SchemaSourceTable,
) -> Option<String> {
    tbl.loaded_at_field
        .clone()
        .or_else(|| tbl.config.loaded_at_field.clone())
        .or_else(|| src.loaded_at_field.clone().map(Some))
        .or_else(|| src.config.loaded_at_field.clone())
        .flatten()
}

/// The source table's effective `freshness` — each threshold (`warn_after`,
/// `error_after`) resolves down the levels independently: table `config:` >
/// table top-level > source `config:` > source top-level > `dbt_project.yml`
/// `+freshness`. Verified against real dbt: a table's `error_after` keeps
/// the inherited `warn_after` (a per-threshold merge, not a wholesale
/// replace). An explicit `freshness: null` STOPS the inheritance walk at
/// that level — a table's own declared thresholds above it still count.
pub(super) fn effective_freshness(
    table: &SchemaSourceTable,
    source: &SchemaSource,
    tree: &Option<RawFreshness>,
) -> Option<RawFreshness> {
    // Declared levels in precedence order — `Some(None)` is an explicit
    // `freshness: null` (stops the walk); `None` levels pass through.
    let chain = [
        table.config.freshness.as_ref().map(Option::as_ref),
        table.freshness.as_ref().map(Option::as_ref),
        source.config.freshness.as_ref().map(Option::as_ref),
        source.freshness.as_ref().map(Option::as_ref),
        Some(tree.as_ref()),
    ];
    let mut warn_after: Option<RawFreshnessThreshold> = None;
    let mut error_after: Option<RawFreshnessThreshold> = None;
    for level in &chain {
        match level {
            Some(Some(f)) => {
                warn_after = warn_after.or_else(|| f.warn_after.clone());
                error_after = error_after.or_else(|| f.error_after.clone());
            }
            Some(None) => break,
            None => {}
        }
    }
    (warn_after.is_some() || error_after.is_some()).then_some(RawFreshness {
        warn_after,
        error_after,
    })
}

/// Record each schema-file test as a synthetic `resource_type: "test"` node so
/// the existing [`Dag::build`] captures it pre-prune. The map KEY uses a counter
/// so two tests on the same (node, column) can never collide and silently drop;
/// `attached_node` points the capture straight at the target.
///
/// A single-key-map entry's config value rides along as `test_metadata.kwargs`
/// (the same shape a real manifest records), and a `relationships`-style `to:`
/// is resolved through the SAME `ref()`/`source()` extractors the .sql scan
/// uses into `depends_on.nodes` ([attached, ref] — the real manifest's shape),
/// so `Dag::build` fills `TestInfo::ref_uid`/`ref_column` identically in both
/// load modes. An unresolvable `to:` leaves only the attached dep — the test
/// stays registered, just target-less.
#[allow(clippy::too_many_arguments)]
pub(super) fn synth_tests(
    nodes: &mut HashMap<String, RawNode>,
    ctx: &mut TestCtx<'_>,
    target_uid: &str,
    target_name: &str,
    column: Option<&str>,
    tests: &[serde_json::Value],
    yml: &str,
    warnings: &mut Vec<String>,
) {
    for test in tests {
        let Some(kind) = test_kind(test) else {
            continue;
        };
        // The declared body under `{kind: {...}}` — a `relationships` test's
        // `to:`/`field:` live there; a bare-string test has none.
        let body = test
            .as_object()
            .and_then(|m| m.get(&kind))
            .and_then(|v| v.as_object());
        // `enabled:` — under `config:` or the deprecated top-level spelling —
        // parks the test under dbt's `disabled` when it evaluates false
        // (verified: both spellings land there; declaring the same key in
        // both is a dbt error, resolved `config:`-first by convention). A
        // `{{ }}` string resolves through the shared evaluator; a value
        // dbt's boolean validation would reject keeps the test enabled
        // with a warning.
        let declared = body.and_then(|b| {
            b.get("config")
                .and_then(|c| c.get("enabled"))
                .or_else(|| b.get("enabled"))
        });
        if ctx.vars.schema_disabled(
            declared,
            &format!("{yml}: {kind} test on '{target_name}' enabled"),
            warnings,
        ) {
            continue;
        }
        let key = format!("test.{}.{}", ctx.proj, ctx.seq);
        ctx.seq += 1;
        let name = match column {
            Some(c) => format!("{kind}_{target_name}_{c}"),
            None => format!("{kind}_{target_name}"),
        };
        // The config map under `{kind: {...}}` — a `relationships` test's
        // `to:`/`field:` live there; a bare-string test has none. `config:`
        // and top-level `enabled:` are dbt's config keys (recorded under
        // `config`, never in `kwargs`) — strip them so the synthesized
        // kwargs carry the same shape a real manifest records.
        let mut kwargs: serde_json::Map<String, serde_json::Value> =
            body.cloned().unwrap_or_default();
        kwargs.remove("config");
        kwargs.remove("enabled");
        // dbt >=1.10's `arguments:` key is the canonical test-args home —
        // the manifest records it FLATTENED into kwargs (verified on
        // 1.12.6: `arguments: {column_name: id}` lands as
        // `kwargs.column_name`, identical to the legacy top-level
        // spelling). Mirror that here so `register_test`'s kwargs read —
        // and the consistency suite's multiset — sees the same shape.
        if let Some(serde_json::Value::Object(args)) = kwargs.remove("arguments") {
            for (k, v) in args {
                kwargs.insert(k, v);
            }
        }
        // `to:` is a Jinja-string reference — resolve through the same
        // extractors the .sql scan uses. Unresolved refs (and non-ref values
        // like `var('x')`) drop out, exactly like a model's.
        let mut deps = vec![target_uid.to_string()];
        if let Some(to) = kwargs.get("to").and_then(|v| v.as_str()) {
            for ref_name in ctx.pat.refs(to) {
                if let Some(uid) = ctx.name_index.get(&ref_name) {
                    deps.push(uid.clone());
                }
            }
            for (src, tbl) in ctx.pat.sources(to) {
                deps.push(format!("source.{}.{}.{}", ctx.proj, src, tbl));
            }
        }
        deps.sort();
        deps.dedup();
        nodes.insert(
            key,
            RawNode {
                name,
                resource_type: "test".to_string(),
                test_metadata: Some(RawTestMetadata {
                    name: Some(kind),
                    kwargs: Some(kwargs),
                }),
                attached_node: Some(target_uid.to_string()),
                column_name: column.map(str::to_string),
                depends_on: RawDependsOn {
                    nodes: deps,
                    macros: vec![],
                },
                ..Default::default()
            },
        );
    }
}

/// The test kind from a schema-file `tests:`/`data_tests:` entry: a bare string
/// (`unique`) is the kind; a single-key map (`accepted_values: {...}`) uses its
/// key. Anything else is ignored.
pub(super) fn test_kind(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(m) => m.keys().next().cloned(),
        _ => None,
    }
}

/// Synthesize `unit_tests:` declarations into `RawManifest::unit_tests`
/// records — the same wire shape a real `dbt parse` emits, keyed
/// `unit_test.<proj>.<model>.<name>[_v<version>]`, so [`Dag::build`]
/// attaches each test to its `model:` target through the one intake
/// path (`kind: "unit"` on the `tests` side map, never a graph node).
/// `depends_on.nodes` carries the target uid exactly like the real
/// record — `Dag::build` resolves it and falls back to the `model:`
/// name when it can't (a disabled or missing model, the same drop the
/// name fallbacks give a manifest-side record).
///
/// `config.enabled` is the only disable channel (a top-level `enabled:`
/// is a dbt parse error — never read): a proven `false` parks the test
/// the way dbt's `disabled` map does.
pub(super) fn synth_unit_tests(
    unit_tests: &mut HashMap<String, RawUnitTest>,
    ctx: &TestCtx<'_>,
    entries: &[SchemaUnitTest],
    yml_path: &str,
    warnings: &mut Vec<String>,
) {
    for entry in entries {
        if ctx.vars.schema_disabled(
            entry.config.enabled.as_ref(),
            &format!("{yml_path}: unit test '{}' config.enabled", entry.name),
            warnings,
        ) {
            continue;
        }
        let Some(model) = &entry.model else {
            // `model:` is required by dbt's schema — warn-and-skip like
            // the other malformed declarations rather than guessing a
            // target.
            warnings.push(format!(
                "{yml_path}: unit test '{}' declares no 'model:' — it can't target anything",
                entry.name
            ));
            continue;
        };
        for (version, dep) in unit_test_targets(ctx, model, &entry.versions) {
            let uid = match &version {
                Some(v) => format!(
                    "unit_test.{}.{}.{}_v{}",
                    ctx.proj,
                    model,
                    entry.name,
                    normalize_version(v)
                ),
                None => format!("unit_test.{}.{}.{}", ctx.proj, model, entry.name),
            };
            // A repeated (model, name) declaration is a dbt error —
            // degraded here to a warning + first-declared-wins, the same
            // convention the duplicate resource names follow.
            if unit_tests.contains_key(&uid) {
                warnings.push(format!(
                    "{yml_path}: duplicate unit test name '{}' on model '{model}' — ignoring it",
                    entry.name
                ));
                continue;
            }
            unit_tests.insert(
                uid,
                RawUnitTest {
                    name: entry.name.clone(),
                    model: Some(model.clone()),
                    depends_on: RawDependsOn {
                        nodes: vec![dep],
                        macros: vec![],
                    },
                    version,
                    given: entry.given.iter().map(manifest_io_shape).collect(),
                    expect: entry.expect.as_ref().map(manifest_io_shape),
                },
            );
        }
    }
}

/// The `(version, depends_on uid)` pairs a `unit_tests:` entry expands
/// to — the per-version emission dbt performs (`versions: {include: …}`
/// mints one `unit_test…_v<version>` record per selected version), over
/// the members source mode can see.
///
/// Source mode synthesizes no `.vN` uid segments, but the file-name
/// convention dbt requires — `<model>_v<version>.sql` — makes a
/// versioned family's members visible as names: `fct_orders_v2` IS
/// version 2's model. A member set therefore stands in for the model's
/// declared `versions:` — an unfiltered entry expands to every member,
/// `include:` selects them (an absent `include` or `'*'` means all),
/// `exclude:` subtracts — matched through [`normalize_version`] so the
/// `v2`/`2`/`V2` spellings name the same member. An `include`d version
/// no member answers to is skipped (dbt hard-errors there); the
/// recorded `version` is the member's own declared value — the file
/// suffix — so the wire keeps `beta`-style versions verbatim.
///
/// A model with no `<model>_v*` members is unversioned: its single
/// `model.<proj>.<name>` uid is the target, and a single-element
/// `include` rides `version` for fidelity only (dbt rejects `versions:`
/// on an unversioned model — degrading to the one target beats dropping
/// a declared test).
fn unit_test_targets(
    ctx: &TestCtx<'_>,
    model: &str,
    versions: &SchemaUnitTestVersions,
) -> Vec<(Option<String>, String)> {
    let bare = format!("model.{}.{}", ctx.proj, model);
    // The bare `model:` name wins when it IS a model — a `fct_orders`
    // sitting next to a coincidentally-named `fct_orders_v2` file stays
    // the target. A real versioned family carries no bare name (dbt's
    // file convention names every version `<model>_v<version>.sql`), so
    // only then do the `_v*` members expand.
    if ctx.name_index.get(model).is_none_or(|u| u != &bare) {
        let prefix = format!("{model}_v");
        let mut members: Vec<(&str, &String)> = ctx
            .name_index
            .iter()
            .filter_map(|(name, uid)| name.strip_prefix(&prefix).map(|v| (v, uid)))
            .collect();
        if !members.is_empty() {
            members.sort();
            let include = version_set(&versions.include);
            let exclude = version_set(&versions.exclude);
            return members
                .into_iter()
                .filter(|(mv, _)| {
                    include
                        .as_ref()
                        .is_none_or(|set| set.contains(normalize_version(mv)))
                        && !exclude
                            .as_ref()
                            .is_some_and(|set| set.contains(normalize_version(mv)))
                })
                .map(|(mv, uid)| (Some(mv.to_string()), uid.clone()))
                .collect();
        }
    }
    // An unversioned model (or an unresolvable name — `Dag::build`'s
    // fallbacks drop it like a missing-model record does). A
    // single-element `include` rides `version` for fidelity only; dbt
    // rejects `versions:` on an unversioned model, so degrading to the
    // one target beats dropping a declared test.
    let version = versions
        .include
        .as_array()
        .filter(|l| l.len() == 1)
        .and_then(|l| l.first().and_then(version_scalar));
    vec![(version, bare)]
}

/// Rewrite a `given:`/`expect:` entry's `rows:` into the shape the
/// manifest carries. A `csv`-format string is a CSV block real dbt
/// parses into a list of row dicts at parse time — the manifest's
/// `rows` IS an array, so `UnitTestShape`'s row counts read it. Rather
/// than fabricate dict contents source mode can't know, the string
/// becomes an array of `null`s, one per data record: the count survives
/// the one `as_array().len()` read in `Dag::build`, and parity holds.
/// (`format: sql` strings are queries, not rows — they stay strings,
/// uncounted like the manifest's.)
fn manifest_io_shape(io: &RawUnitTestIo) -> RawUnitTestIo {
    let mut io = io.clone();
    // `format: sql` keeps its query string on the wire — only the absent
    // (default `csv`) or explicit `csv` formats parse into row lists.
    let parses_to_rows = io.format.as_deref().is_none_or(|f| f == "csv");
    if parses_to_rows {
        if let Some(serde_json::Value::String(text)) = &io.rows {
            if let Some(records) = csv_records(text) {
                // First record is the header row.
                io.rows = Some(serde_json::Value::Array(vec![
                    serde_json::Value::Null;
                    records.saturating_sub(1)
                ]));
            }
        }
    }
    io
}

/// The record count of a CSV payload — header included, so the caller
/// subtracts one. `"…"` quoting is honored: a newline or `""` escape
/// inside a quoted field never splits a record. `None` on an empty
/// payload or unbalanced quotes — the raw string then rides through
/// uncounted, which matches what a malformed manifest would surface.
fn csv_records(text: &str) -> Option<usize> {
    if text.trim().is_empty() {
        return None;
    }
    let mut records = 0usize;
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            '\n' if !in_quotes => records += 1,
            _ => {}
        }
    }
    if in_quotes {
        return None;
    }
    if !text.ends_with('\n') {
        records += 1;
    }
    Some(records)
}

/// A `versions:` list's normalized member set — `None` for the
/// absent/wildcard/non-list spellings (all of which select every
/// member), `Some` of the normalized version strings for a real list.
fn version_set(v: &serde_json::Value) -> Option<std::collections::BTreeSet<String>> {
    let list = v.as_array().filter(|l| !l.is_empty())?;
    Some(
        list.iter()
            .filter_map(|v| version_scalar(v).map(|s| normalize_version(&s).to_string()))
            .collect(),
    )
}

/// A `versions:` item's version string — `2` → `"2"`, `'beta'` →
/// `"beta"`; non-scalars can't name a version and yield `None`.
fn version_scalar(v: &serde_json::Value) -> Option<String> {
    scalar_to_string(v)
}
