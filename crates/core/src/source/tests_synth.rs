use super::*;

/// The shared context [`synth_tests`] and [`apply_meta`] thread through every
/// schema-file test pass: the running synth-key counter, the project name, and
/// the `ref()`/`source()` extractors + name index a `relationships` test's
/// `to:` resolves against (the same lookups the .sql scan uses).
pub(super) struct TestCtx<'a> {
    pub(super) seq: usize,
    pub(super) proj: &'a str,
    pub(super) pat: &'a Patterns,
    pub(super) name_index: &'a HashMap<String, String>,
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
        // Schema-level `config: enabled: false` parks the node like dbt's
        // `disabled`: drop it (its edges are pruned by Dag::build's
        // prune-first step) and record none of its docs/tests.
        if entry.config.enabled == Some(false) {
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
        synth_tests(nodes, ctx, &uid, &entry.name, None, &entry.tests);
        synth_tests(nodes, ctx, &uid, &entry.name, None, &entry.data_tests);
        for col in &entry.columns {
            synth_tests(nodes, ctx, &uid, &entry.name, Some(&col.name), &col.tests);
            synth_tests(
                nodes,
                ctx,
                &uid,
                &entry.name,
                Some(&col.name),
                &col.data_tests,
            );
        }
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

/// A source table's merged `meta`: the `sources:` tree `+meta` (lowest
/// precedence), then the source-level `meta:` (cascades to every table),
/// then the table-level `meta:` — each level overrides the previous per
/// top-level key (dbt's shallow merge, verified).
pub(super) fn source_meta(
    tree: serde_json::Map<String, serde_json::Value>,
    src: &SchemaSource,
    tbl: &SchemaSourceTable,
    warnings: &mut Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = tree;
    out.extend(meta_map(
        &src.meta,
        warnings,
        &format!("source '{}'", src.name),
    ));
    out.extend(meta_map(
        &tbl.meta,
        warnings,
        &format!("source '{}' table '{}'", src.name, tbl.name),
    ));
    out
}

/// The source table's effective `freshness` — each threshold (`warn_after`,
/// `error_after`) resolves down table > source > `dbt_project.yml`
/// `+freshness` independently. Verified against real dbt: a table's
/// `error_after` keeps the inherited `warn_after` (a per-threshold merge,
/// not a wholesale replace). An explicit `freshness: null` STOPS the
/// inheritance walk at that level — a table's own declared thresholds above
/// it still count.
pub(super) fn effective_freshness(
    table: &Option<Option<RawFreshness>>,
    source: &Option<Option<RawFreshness>>,
    tree: &Option<RawFreshness>,
) -> Option<RawFreshness> {
    // Declared levels in precedence order — `Some(None)` is an explicit
    // `freshness: null` (stops the walk); `None` levels pass through.
    let chain = [
        table.as_ref().map(Option::as_ref),
        source.as_ref().map(Option::as_ref),
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
pub(super) fn synth_tests(
    nodes: &mut HashMap<String, RawNode>,
    ctx: &mut TestCtx<'_>,
    target_uid: &str,
    target_name: &str,
    column: Option<&str>,
    tests: &[serde_json::Value],
) {
    for test in tests {
        let Some(kind) = test_kind(test) else {
            continue;
        };
        let key = format!("test.{}.{}", ctx.proj, ctx.seq);
        ctx.seq += 1;
        let name = match column {
            Some(c) => format!("{kind}_{target_name}_{c}"),
            None => format!("{kind}_{target_name}"),
        };
        // The config map under `{kind: {...}}` — a `relationships` test's
        // `to:`/`field:` live there; a bare-string test has none.
        let kwargs: serde_json::Map<String, serde_json::Value> = match test {
            serde_json::Value::Object(m) => m
                .get(&kind)
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default(),
            _ => Default::default(),
        };
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
