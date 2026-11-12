use super::*;

/// Trim a description (strip surrounding whitespace / trailing newlines), turning
/// a blank one into `None` — [`description_blank`] is the shared "no real
/// content" predicate every description judgment uses.
pub(super) fn clean_desc(desc: &Option<String>) -> Option<String> {
    desc.as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !description_blank(s))
}

/// Build [`ColumnInfo`]s from a manifest `columns` object, preserving definition
/// order (the `Map` is order-preserving via `serde_json/preserve_order`). The
/// column name is the entry's `name` field, falling back to the map key. A
/// whitespace-only `description` counts as missing ([`description_blank`]) but
/// a real value is kept VERBATIM — trimming it would drop the trailing newline
/// a `|` block carries and change the TUI/docs output.
pub(super) fn columns_from(map: &serde_json::Map<String, serde_json::Value>) -> Vec<ColumnInfo> {
    map.iter()
        .map(|(key, v)| ColumnInfo {
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or(key)
                .to_string(),
            data_type: v
                .get("data_type")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            description: v
                .get("description")
                .and_then(|x| x.as_str())
                .filter(|s| !description_blank(s))
                .map(str::to_string),
            constraints: v
                .get("constraints")
                .and_then(|x| serde_json::from_value::<Vec<RawConstraint>>(x.clone()).ok())
                .map(|cs| constraints_from(&cs))
                .unwrap_or_default(),
        })
        .collect()
}

/// Convert wire [`RawConstraint`]s to domain [`ConstraintInfo`]s — drops dbt's
/// adapter bookkeeping (`warn_unenforced`/`warn_unsupported` aren't modeled)
/// and normalizes the optional column lists to empty.
pub(super) fn constraints_from(raw: &[RawConstraint]) -> Vec<ConstraintInfo> {
    raw.iter()
        .map(|c| ConstraintInfo {
            constraint_type: c.constraint_type.clone().unwrap_or_default(),
            name: c.name.clone(),
            expression: c.expression.clone(),
            to: c.to.clone(),
            to_columns: c.to_columns.clone().unwrap_or_default(),
            columns: c.columns.clone().unwrap_or_default(),
        })
        .collect()
}

/// Stringify a `meta` dictionary for display/selection: scalars keep their
/// plain form (`"alice"` → `alice`, `1` → `1`, `true` → `true`, `null` →
/// `null`); objects/arrays become compact JSON (`{"a":1}`). Sorted into a
/// `BTreeMap` so output is deterministic regardless of manifest order.
pub(super) fn meta_strings(
    map: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, String> {
    map.iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect()
}

/// Normalize `config.unique_key` to `Vec<String>`: dbt accepts a bare string
/// (`unique_key: id`) or a string list (`unique_key: [a, b]`) and keeps that
/// declared shape on the wire. Anything else (null, numbers, a map) is
/// unconfigured → empty.
pub(super) fn unique_key_from(value: &Option<serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Strip the `<package>://` scheme off a manifest `patch_path`
/// (`sample://models/schema.yml` → `models/schema.yml`) so it is directly
/// comparable with `original_file_path` — the same project-relative
/// convention source mode emits when it records the declaring `.yml`.
pub(super) fn project_rel_patch_path(patch_path: &Option<String>) -> Option<String> {
    patch_path.as_deref().map(|p| {
        p.split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(p)
            .to_string()
    })
}

/// Keep a freshness threshold only when it carries a real `count` — dbt
/// writes `{"count": null, "period": null}` for the unset side of a partially
/// configured `freshness` block, and a count-less threshold is absent.
pub(super) fn freshness_threshold(t: &Option<RawFreshnessThreshold>) -> Option<FreshnessThreshold> {
    t.as_ref().and_then(|t| {
        t.count.map(|count| FreshnessThreshold {
            count,
            period: t.period.clone(),
        })
    })
}

/// Whether `n`'s `unique_id` package segment equals `pkg`: `model.<pkg>.<name>`
/// → `pkg`, `source.<pkg>.<src>.<tbl>` → `pkg`, and so on for every
/// `<resource>.<pkg>.…` uid shape. A uid with no non-empty second segment is
/// unattributable → counts as in-scope (scope never drops what it cannot
/// name). The per-node half of [`Dag::is_root_project`], `pub` so sweeps can
/// resolve the owning package ONCE ([`Dag::primary_package`]) and share it —
/// [`Dag::root_scoped`] and [`PackageScope::apply_with_warnings`] do the same
/// instead of re-resolving per node.
pub fn in_package(n: &NodeInfo, pkg: &str) -> bool {
    n.unique_id
        .split('.')
        .nth(1)
        .filter(|s| !s.is_empty())
        .is_none_or(|seg| seg == pkg)
}

/// Detail for a model / seed / snapshot node.
pub(super) fn detail_from_node(node: &RawNode) -> NodeDetail {
    NodeDetail {
        materialized: node.config.materialized.clone(),
        unique_key: unique_key_from(&node.config.unique_key),
        database: node.database.clone(),
        schema: node.schema.clone(),
        tags: node.tags.clone(),
        description: clean_desc(&node.description),
        columns: columns_from(&node.columns),
        original_file_path: node.original_file_path.clone(),
        patch_path: project_rel_patch_path(&node.patch_path),
        package_name: node.package_name.clone(),
        access: node.access.clone(),
        group: node.group.clone(),
        contract_enforced: node.contract.as_ref().is_some_and(|c| c.enforced),
        incremental_strategy: node.config.incremental_strategy.clone(),
        deprecation_date: node.deprecation_date.clone(),
        language: node.language.clone(),
        constraints: constraints_from(&node.constraints),
        version: node.version.clone(),
        latest_version: node.latest_version.clone(),
        meta: meta_strings(&node.meta),
        // `alias` IS the relation name in real manifests — record only the
        // meaningful case (an actual override) so `None` stays the fast path.
        relation_name: node
            .alias
            .clone()
            .filter(|a| !a.trim().is_empty() && a != &node.name),
        // The ingestion fields below are source-only.
        ..Default::default()
    }
}

/// Detail for an exposure (no materialization / warehouse coordinates; the
/// kind / owner / url live in the [`ExposureInfo`] side map instead).
pub(super) fn detail_from_exposure(exposure: &RawExposure) -> NodeDetail {
    NodeDetail {
        description: clean_desc(&exposure.description),
        original_file_path: exposure.original_file_path.clone(),
        ..Default::default()
    }
}

/// Convert a wire `RawMacro` into its stored [`MacroInfo`] payload.
pub(super) fn macro_info(m: &RawMacro) -> MacroInfo {
    MacroInfo {
        name: m.name.clone(),
        sql: m.macro_sql.clone(),
        depends_on: m.depends_on.macros.clone(),
    }
}

/// Detail for a source node (no materialization; it's an external table).
pub(super) fn detail_from_source(source: &RawSource) -> NodeDetail {
    NodeDetail {
        materialized: None,
        database: source.database.clone(),
        schema: source.schema.clone(),
        tags: Vec::new(),
        description: clean_desc(&source.description),
        columns: columns_from(&source.columns),
        original_file_path: source.original_file_path.clone(),
        package_name: source.package_name.clone(),
        meta: meta_strings(&source.meta),
        relation_name: source
            .identifier
            .clone()
            .filter(|i| !i.trim().is_empty() && i != &source.name),
        loaded_at_field: source.loaded_at_field.clone(),
        // dbt writes `loader: ""` for an unconfigured source — normalize to
        // `None` so it reads the same as an absent one.
        loader: source.loader.clone().filter(|l| !l.is_empty()),
        freshness_warn_after: source
            .freshness
            .as_ref()
            .and_then(|f| freshness_threshold(&f.warn_after)),
        freshness_error_after: source
            .freshness
            .as_ref()
            .and_then(|f| freshness_threshold(&f.error_after)),
        ..Default::default() // sources carry no governance fields
    }
}

/// Resolve a model NAME to its `model.<pkg>.<name>` uid — the fallback when a
/// unit_test's uid doesn't carry the standard `unit_tests.<pkg>.<model>` shape.
/// Returns the single match (a name colliding across packages can't be
/// resolved reliably anyway).
pub(super) fn model_uid_by_name(nodes: &HashMap<String, NodeInfo>, name: &str) -> Option<String> {
    nodes
        .values()
        .filter(|n| n.resource_type == "model" && n.name == name)
        .map(|n| n.unique_id.clone())
        .next()
}

/// Record a test node into the per-target tests map. The target is the test's
/// `attached_node` (the node it is declared on); singular tests have none, so we
/// fall back to every node in `depends_on`. This is a pure recording — it never
/// touches the adjacency maps, so a test is never a lineage transit hop.
pub(super) fn register_test(tests: &mut HashMap<String, Vec<TestInfo>>, node: &RawNode) {
    let kind = node
        .test_metadata
        .as_ref()
        .and_then(|m| m.name.clone())
        .unwrap_or_else(|| "singular".to_string());
    // A `relationships` test's `to:` target: dbt's depends_on carries the
    // attached node AND the referenced one, so `ref` = the smallest
    // non-attached dep (min for determinism — a malformed triple-dep test
    // still resolves the same way every run). `ends_with` catches the bare
    // kind plus package-wrapped spellings.
    let ref_uid = if kind.ends_with("relationships") {
        node.depends_on
            .nodes
            .iter()
            .filter(|d| Some(*d) != node.attached_node.as_ref())
            .min()
            .cloned()
    } else {
        None
    };
    let ref_column = node
        .test_metadata
        .as_ref()
        .and_then(|m| m.kwargs.as_ref())
        .and_then(|k| k.get("field"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let info = TestInfo {
        name: node.name.clone(),
        kind,
        // `node.column_name` covers column-level tests; a MODEL-level generic
        // test (`unique: {column_name: id}`, or the dbt >=1.10 `arguments:`
        // spelling dbt flattens into kwargs) leaves `node.column_name` null
        // and carries the column in kwargs instead — read it from there so
        // `has_pk_guarantee` sees the same column either spelling declared.
        column_name: node.column_name.clone().or_else(|| {
            node.test_metadata
                .as_ref()
                .and_then(|m| m.kwargs.as_ref())
                .and_then(|k| k.get("column_name"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }),
        ref_uid,
        ref_column,
        ..Default::default()
    };
    match &node.attached_node {
        Some(target) => tests.entry(target.clone()).or_default().push(info),
        None => {
            for target in &node.depends_on.nodes {
                tests.entry(target.clone()).or_default().push(info.clone());
            }
        }
    }
}
