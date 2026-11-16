use super::*;
use crate::manifest::{ref_owner, RefVerdict};

/// The shared context [`synth_tests`] and [`apply_meta`] thread through every
/// schema-file test pass: the running synth-key counter, the project name, the
/// `ref()`/`source()` extractors + name index a `relationships` test's `to:`
/// resolves against (the same lookups the .sql scan uses), the versioned
/// families a `unit_tests:` `versions:` selects over, and the var tables a
/// `config.enabled:`/`enabled:` Jinja string evaluates against.
pub(super) struct TestCtx<'a> {
    pub(super) seq: usize,
    pub(super) proj: &'a str,
    pub(super) pat: &'a Patterns,
    pub(super) name_index: &'a HashMap<String, String>,
    /// `versions:`-declared families the `synthesize_models` stage bound —
    /// a `unit_tests:` `versions:` selector walks its members.
    pub(super) versioned: &'a BTreeMap<String, VersionedFamily>,
    /// The installed packages' `name -> packages` universe — a
    /// `to: "{{ ref('pkg','x') }}"` judges against it the way the .sql
    /// pass does.
    pub(super) installed_refs: &'a HashMap<String, BTreeSet<String>>,
    /// `dependencies.yml`/`packages.yml` `projects:` mesh names.
    pub(super) projects: &'a BTreeSet<String>,
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
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_meta(
    nodes: &mut HashMap<String, RawNode>,
    ctx: &mut TestCtx<'_>,
    type_prefix: &str,
    entries: &[SchemaModel],
    yml_path: &str,
    docs: &DocBlocks,
    disabled: &mut std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<(Option<String>, String)>,
    >,
    in_file: &HashMap<String, EnabledScan>,
    tree: &HashMap<String, TreeConfig>,
    unverifiable: &mut std::collections::BTreeSet<String>,
    // The schema `config.tags:` channel — uid → entries in declaration
    // order — recorded for `apply_tree_fallback` to merge in dbt's exact
    // ordering (the node's dedup'd list takes tree → in-file → yml;
    // `yml_tags` is that last channel).
    yml_tags: &mut HashMap<String, Vec<String>>,
    warnings: &mut Vec<String>,
) {
    for entry in entries {
        let uid = format!("{type_prefix}.{}.{}", ctx.proj, entry.name);
        // The node's `enabled` folds the channels in precedence order —
        // in-file `config(enabled=)` > this schema `config.enabled:` >
        // the tree's `+enabled`; the highest DECLARED level decides.
        // A proven `Off` parks the node like dbt's `disabled`: drop it
        // (its edges are pruned by Dag::build's prune-first step) and
        // record none of its docs/tests — but keep the NAME so
        // `unresolved_references` reports a ref to it under its own
        // "disabled" wording. A schema entry for a node that never
        // existed parks nothing (name_index never saw it either). An
        // `Unproven` final verdict keeps the node and marks it — and a
        // PROVEN level above can clear a mark a lower level recorded at
        // collection (a schema `config.enabled: true` outranks the
        // tree's unproven `+enabled`).
        let schema = Enabled::from_schema(ctx.vars.schema_enabled_state(
            entry.config.enabled.as_ref(),
            &format!("{yml_path}: {type_prefix} '{}' config.enabled", entry.name),
            warnings,
        ));
        let fallback = schema.or_else(|| tree.get(&uid).and_then(|t| t.enabled_verdict()));
        let verdict = match in_file.get(&uid) {
            // The scan folds the lower channels — counting the gated
            // calls against the provable ones. `ctx` names the file the
            // `{% if %}` lives in (the node's own), not the schema file.
            Some(scan) => {
                let ctx = nodes
                    .get(&uid)
                    .and_then(|n| n.original_file_path.clone())
                    .unwrap_or_else(|| uid.clone());
                scan.resolve(fallback, warnings, &ctx)
            }
            None => fallback,
        };
        match verdict {
            Some(Enabled::Off) => {
                unverifiable.remove(&uid);
                if nodes.remove(&uid).is_some() {
                    disabled
                        .entry(entry.name.clone())
                        .or_default()
                        .insert((None, type_prefix.to_string()));
                }
                continue;
            }
            Some(Enabled::Unproven) => {
                unverifiable.insert(uid.clone());
            }
            _ => {
                unverifiable.remove(&uid);
            }
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
        // The schema `config.tags:` channel — appended to the node's tag
        // list LAST of all (tree `+tags` → in-file `config(tags=…)` →
        // here, dedup'd — verified on 1.12.6). The entry's top-level
        // `tags:` property is a no-op dbt parses but never applies, so it
        // has deliberately no field on [`SchemaModel`].
        if let Some(cfg_tags) = &entry.config.tags {
            yml_tags
                .entry(uid.clone())
                .or_default()
                .extend(cfg_tags.iter().cloned());
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

/// The versioned half of [`apply_meta`]: expand a `versions:`-declared
/// model entry over the members `synthesize_models` bound — each member
/// gets the model-level patch PLUS its version entry's overrides
/// (description / `deprecation_date` / `config:` fields), the version's
/// effective columns (top-level set sliced by `include:`/`exclude:`
/// markers, a same-named def replacing its column while its tests
/// union), and the version's effective tests (the model-level list
/// unless the version declares its own — `tests:`/`data_tests:` on a
/// version REPLACES it outright, verified on 1.12.6).
///
/// Config precedence inside each member mirrors the verified chain —
/// in-file `config()` > version `config:` > model `config:` > project
/// tree — every `if unset` fill here sits between the in-file level
/// (already on the node) and the tree fallback (`tree_*` tables).
/// The `enabled` verdicts all folded at the claim stage, so a member
/// whose uid never materialized contributes nothing — its docs/tests
/// park with it, matching dbt.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_versioned_meta(
    nodes: &mut HashMap<String, RawNode>,
    ctx: &mut TestCtx<'_>,
    entry: &SchemaModel,
    family: &VersionedFamily,
    yml_path: &str,
    docs: &DocBlocks,
    // The schema `config.tags:` channel — like `apply_meta`'s, but per
    // member: the version's `config.tags` lands BEFORE the model's on
    // the node (verified on 1.12.6), so each member records
    // `ver ++ model` in declaration order.
    yml_tags: &mut HashMap<String, Vec<String>>,
    warnings: &mut Vec<String>,
) {
    let mut applied: std::collections::HashSet<String> = std::collections::HashSet::new();
    for ver_entry in &entry.versions {
        let Some(rendered) = scalar_to_string(&ver_entry.v) else {
            continue;
        };
        // A repeated `v:` warns at bind and keeps the first — metadata
        // expansion must not double-apply (its tests would duplicate).
        if !applied.insert(rendered.clone()) {
            continue;
        }
        let Some(uid) = family
            .versions
            .get(&rendered)
            .and_then(|u| u.as_ref())
            .cloned()
        else {
            continue;
        };
        let effective_cols = version_columns(
            &entry.columns,
            &ver_entry.columns,
            warnings,
            &format!("{yml_path}: model '{}' version '{rendered}'", entry.name),
        );
        {
            let Some(node) = nodes.get_mut(&uid) else {
                continue;
            };
            // The declaring yml is `patch_path` for every member — the
            // scheme-stripped form `project_rel_patch_path` normalizes a
            // real manifest's `<pkg>://<yml>` to, the same record the
            // unversioned arm writes.
            node.patch_path = Some(yml_path.to_string());
            // The version's own description wins over the model's (verified:
            // v1's description stays v1's while the model's fills the rest).
            node.description = ver_entry
                .description
                .clone()
                .or_else(|| entry.description.clone())
                .map(|d| docs.render(&d, warnings));
            // The version `config:` slots fill only what the impl file's
            // own `config()` left unset — then the model `config:` fills
            // what the version left — the in-file > version > model >
            // tree chain (verified).
            if node.config.materialized.is_none() {
                node.config.materialized = ver_entry
                    .config
                    .materialized
                    .clone()
                    .or_else(|| entry.config.materialized.clone());
            }
            if node.alias.is_none() {
                node.alias = ver_entry
                    .config
                    .alias
                    .clone()
                    .or_else(|| ver_entry.alias.clone())
                    .or_else(|| entry.config.alias.clone());
            }
            if node.config.unique_key.is_none() {
                if !ver_entry.config.unique_key.is_null() {
                    node.config.unique_key = Some(ver_entry.config.unique_key.clone());
                } else if !entry.config.unique_key.is_null() {
                    node.config.unique_key = Some(entry.config.unique_key.clone());
                }
            }
            // The governance surface — `config:` wins over the bare
            // top-level spelling at each level (declaring both is a dbt
            // error — merge degraded like `meta:`'s), version level
            // before model level.
            if node.group.is_none() {
                node.group = ver_entry
                    .config
                    .group
                    .clone()
                    .or_else(|| ver_entry.group.clone())
                    .or_else(|| entry.config.group.clone())
                    .or_else(|| entry.group.clone());
            }
            if node.access.is_none() {
                node.access = ver_entry
                    .config
                    .access
                    .clone()
                    .or_else(|| ver_entry.access.clone())
                    .or_else(|| entry.config.access.clone())
                    .or_else(|| entry.access.clone());
            }
            if node.contract.is_none() {
                node.contract = contract_value(&ver_entry.config.contract)
                    .or_else(|| contract_value(&entry.config.contract));
            }
            if node.config.incremental_strategy.is_none() {
                node.config.incremental_strategy = ver_entry
                    .config
                    .incremental_strategy
                    .clone()
                    .or_else(|| entry.config.incremental_strategy.clone());
            }
            // `deprecation_date` is a PROPERTY — the version's own beats
            // the model's (properties never have a `config:` channel).
            if node.deprecation_date.is_none() {
                node.deprecation_date = ver_entry
                    .deprecation_date
                    .clone()
                    .or_else(|| entry.deprecation_date.clone());
            }
            if node.constraints.is_empty() {
                node.constraints = entry.constraints.clone();
            }
            // `config.tags:` — the version's then the model's, per
            // member, recorded for `apply_tree_fallback` to merge in
            // dbt's exact ordering (the `tags:` property on either level
            // is a no-op dbt never applies — deliberately absent from
            // [`SchemaModel`]/[`SchemaVersion`]).
            let mut member_tags = ver_entry.config.tags.clone().unwrap_or_default();
            member_tags.extend(entry.config.tags.clone().unwrap_or_default());
            yml_tags.entry(uid.clone()).or_default().extend(member_tags);
            // `meta:`/`config.meta:` merge per key — version level wins
            // over model level, `config.meta` wins over the bare `meta:`
            // spelling at each level; in-file `config(meta=…)` wins over
            // all (`entry().or_insert` — so levels apply high to low).
            let what = format!("model '{}'", entry.name);
            for (v, level) in [
                (
                    &ver_entry.config.meta,
                    format!("version '{rendered}' config meta"),
                ),
                (&ver_entry.meta, format!("version '{rendered}' meta")),
                (&entry.config.meta, "config meta".to_string()),
                (&entry.meta, "meta".to_string()),
            ] {
                for (k, v) in meta_map(v, warnings, &format!("{what} {level}")) {
                    node.meta.entry(k).or_insert(v);
                }
            }
            if !effective_cols.is_empty() {
                node.columns = columns_json(
                    &effective_cols
                        .iter()
                        .map(|c| c.col.clone())
                        .collect::<Vec<_>>(),
                    docs,
                    warnings,
                );
            }
        }
        // The version's effective tests — a version's own `tests:`/
        // `data_tests:` REPLACES the model-level list (verified: no
        // include/exclude markers exist there — every entry is a def),
        // while column tests ride each effective column (an excluded
        // column's tests drop with it). `synth_tests` attaches each to
        // the member uid like a real manifest does.
        for (model_tests, ver_tests, family) in [
            (&entry.tests, &ver_entry.tests, ColTests::Tests),
            (
                &entry.data_tests,
                &ver_entry.data_tests,
                ColTests::DataTests,
            ),
        ] {
            let (model_level, per_column) =
                version_tests(model_tests, ver_tests, &effective_cols, family);
            synth_tests(
                nodes,
                ctx,
                &uid,
                &entry.name,
                None,
                &model_level,
                yml_path,
                warnings,
            );
            for (col, defs) in per_column {
                synth_tests(
                    nodes,
                    ctx,
                    &uid,
                    &entry.name,
                    Some(&col),
                    &defs,
                    yml_path,
                    warnings,
                );
            }
        }
    }
}

/// A version's effective column — the resolved definition shape
/// (`columns_json` reads it) PLUS the tests split by provenance: the
/// model-level column's own `tests:`/`data_tests:` (inherited — they
/// ride the column whatever the version declares) vs the version-level
/// def's (added — a same-named def unions its tests onto the inherited
/// set, verified on 1.12.6).
struct EffectiveColumn {
    col: SchemaColumn,
    inherited_tests: Vec<serde_json::Value>,
    added_tests: Vec<serde_json::Value>,
    inherited_data_tests: Vec<serde_json::Value>,
    added_data_tests: Vec<serde_json::Value>,
}

/// The version's effective column set — the model-level `columns:` sliced
/// by the version's `include:`/`exclude:` markers (survivors keep their
/// top-level declaration order — verified: `include: [b, a]` still emits
/// `a` before `b`) and overridden by the version's own `name:`-defs. A
/// same-named def REPLACES the column's declared shape (a def without
/// `description:` shows empty, not the inherited text — verified), while
/// its `tests:`/`data_tests:` UNION onto the inherited column's. No
/// marker → the whole top-level set is the selection, so a def-only
/// `columns:` list still inherits everything — the dbt behavior behind
/// "columns are version-specific by exception".
fn version_columns(
    model_cols: &[SchemaColumn],
    ver_cols: &[serde_json::Value],
    warnings: &mut Vec<String>,
    what: &str,
) -> Vec<EffectiveColumn> {
    let mut include: Option<BTreeSet<String>> = None;
    let mut include_all = false;
    let mut exclude: BTreeSet<String> = BTreeSet::new();
    let mut defs: Vec<SchemaColumn> = Vec::new();
    for raw in ver_cols {
        if version_marker(raw) {
            for name in version_marker_list(raw.get("include")) {
                if matches!(name.as_str(), "*" | "all") {
                    include_all = true;
                } else {
                    include.get_or_insert_with(BTreeSet::new).insert(name);
                }
            }
            for name in version_marker_list(raw.get("exclude")) {
                exclude.insert(name);
            }
            continue;
        }
        match serde_json::from_value::<SchemaColumn>(raw.clone()) {
            Ok(col) => defs.push(col),
            Err(_) => warnings.push(format!(
                "{what}: a column entry is neither a column def nor an include/exclude marker — ignoring it"
            )),
        }
    }
    // dbt permits `exclude:` only beside `include: '*'`/`'all'` —
    // without an include-all marker it is a schema error, degraded here
    // to the lenient read (exclude from the full set) plus a warning.
    if !exclude.is_empty() && !include_all {
        warnings.push(format!(
            "{what}: 'exclude:' without 'include: \"*\"' is a dbt schema error — excluding from the full set anyway"
        ));
    }
    let mut out: Vec<EffectiveColumn> = model_cols
        .iter()
        .filter(|c| {
            (include_all || include.as_ref().is_none_or(|set| set.contains(&c.name)))
                && !exclude.contains(&c.name)
        })
        .map(|c| EffectiveColumn {
            col: c.clone(),
            inherited_tests: c.tests.clone(),
            added_tests: Vec::new(),
            inherited_data_tests: c.data_tests.clone(),
            added_data_tests: Vec::new(),
        })
        .collect();
    for def in defs {
        match out.iter_mut().find(|c| c.col.name == def.name) {
            Some(existing) => {
                // A same-named def REPLACES the column's declared shape
                // outright (verified: an absent `description:` reads
                // empty, not inherited) — only the test lists union.
                existing.col = SchemaColumn {
                    tests: Vec::new(),
                    data_tests: Vec::new(),
                    ..def.clone()
                };
                existing.added_tests.extend(def.tests);
                existing.added_data_tests.extend(def.data_tests);
            }
            None => out.push(EffectiveColumn {
                col: SchemaColumn {
                    tests: Vec::new(),
                    data_tests: Vec::new(),
                    ..def.clone()
                },
                inherited_tests: Vec::new(),
                added_tests: def.tests,
                inherited_data_tests: Vec::new(),
                added_data_tests: def.data_tests,
            }),
        }
    }
    out
}

/// Which of a column's two test lists the pass selects — `tests:` and
/// `data_tests:` are parallel channels in the schema shape, each with
/// its own inherited/added halves.
enum ColTests {
    Tests,
    DataTests,
}

/// The version's effective tests split into `(model-level defs,
/// column -> defs)`. Model-level `tests:`/`data_tests:` work by
/// REPLACEMENT, verified on 1.12.6: a version that declares its own
/// list takes ONLY that list (the model-level set never merges), and
/// every entry is a test def — `include:`/`exclude:` keys are not
/// markers here (they parse as test names of their own). A version
/// declaring neither inherits the model-level list. A column's tests
/// ride on that column: a column the version's `columns:` slice drops
/// takes its tests with it, and a same-named def unions its declared
/// tests onto the inherited set.
fn version_tests(
    model_tests: &[serde_json::Value],
    ver_tests: &[serde_json::Value],
    effective_cols: &[EffectiveColumn],
    family: ColTests,
) -> (
    Vec<serde_json::Value>,
    Vec<(String, Vec<serde_json::Value>)>,
) {
    fn parts<'c>(
        c: &'c EffectiveColumn,
        family: &ColTests,
    ) -> (&'c Vec<serde_json::Value>, &'c Vec<serde_json::Value>) {
        match family {
            ColTests::Tests => (&c.inherited_tests, &c.added_tests),
            ColTests::DataTests => (&c.inherited_data_tests, &c.added_data_tests),
        }
    }
    let model_level: Vec<serde_json::Value> = if ver_tests.is_empty() {
        model_tests.to_vec()
    } else {
        ver_tests.to_vec()
    };
    let per_column: Vec<(String, Vec<serde_json::Value>)> = effective_cols
        .iter()
        .map(|c| {
            let (inherited, added) = parts(c, &family);
            let mut defs = inherited.clone();
            defs.extend(added.iter().cloned());
            (c.col.name.clone(), defs)
        })
        .collect();
    (model_level, per_column)
}

/// Whether a `columns:` list entry is an `include:`/`exclude:` marker
/// rather than a column definition (the keys dbt reserves there —
/// `tests:` lists do NOT have markers, every entry is a def).
fn version_marker(v: &serde_json::Value) -> bool {
    v.as_object()
        .is_some_and(|m| m.contains_key("include") || m.contains_key("exclude"))
}

/// The members of a `columns:` `include:`/`exclude:` — a name list, or a
/// bare `'*'`/`'all'` scalar (the dbt spellings for "the whole inherited
/// set"). Non-scalars can't name a column and drop silently.
fn version_marker_list(v: Option<&serde_json::Value>) -> Vec<String> {
    match v {
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(scalar_to_string).collect(),
        Some(v) => scalar_to_string(v).into_iter().collect(),
        None => Vec::new(),
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
pub(super) fn push_unique(tags: &mut Vec<String>, new_tags: &[String]) {
    for tag in new_tags {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
}

/// A source table's `loaded_at_field` — resolved down table top-level >
/// table `config:` > source top-level > source `config:`. The top-level
/// spelling beats `config:` WITHIN each level (verified on 1.12.6: a
/// table's `loaded_at_field:` wins over its own `config.loaded_at_field`,
/// and the source's top-level value beats the source's
/// `config.loaded_at_field`; the table's config still outranks every
/// source level). Two `null` spellings behave DIFFERENTLY (verified): a
/// table's TOP-LEVEL `loaded_at_field: null` stops the walk outright —
/// the cleared value lands on the wire — while a `config.loaded_at_field:
/// null` (either level) is a silent no-op, inherited through like the key
/// was never declared. `+loaded_at_field` on the `sources:` tree is a key
/// dbt ignores — deliberately not resolved.
pub(super) fn source_loaded_at_field(
    src: &SchemaSource,
    tbl: &SchemaSourceTable,
) -> Option<String> {
    // Table top-level — `Some(None)` (explicit `null`) returns the cleared
    // `None` right here, before any lower level is consulted.
    if let Some(v) = &tbl.loaded_at_field {
        return v.clone();
    }
    // Table `config:` — a concrete value wins; `Some(None)` falls through.
    if let Some(Some(v)) = &tbl.config.loaded_at_field {
        return Some(v.clone());
    }
    // Source top-level — `Option<String>`: an explicit `null` already reads
    // as absent at deserialization (verified: it lets the source config
    // level apply). Source `config:` fills last, its own `null` a no-op.
    src.loaded_at_field
        .clone()
        .or_else(|| src.config.loaded_at_field.clone().flatten())
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
        // A `name:` inside the body is the generated-name override —
        // dbt consumes it as the node name and drops it from kwargs
        // (verified: `{include: {name: custom_test}}` records kind
        // `include` named `custom_test` with `name` absent from kwargs).
        let name_override = body.and_then(|b| b.get("name")).and_then(scalar_to_string);
        let name = match (name_override, column) {
            (Some(n), _) => n,
            (None, Some(c)) => format!("{kind}_{target_name}_{c}"),
            (None, None) => format!("{kind}_{target_name}"),
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
        kwargs.remove("name");
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
        // extractors the .sql scan uses, version pins included: a
        // `to: "{{ ref('m', v=1) }}"` dep names the `.v1` member like
        // the real manifest's record does. Unresolved refs (and non-ref
        // values like `var('x')`) drop out, exactly like a model's.
        let mut deps = vec![target_uid.to_string()];
        if let Some(to) = kwargs.get("to").and_then(|v| v.as_str()) {
            let masked = mask_jinja_comments(to);
            for site in call_sites_masked(&masked) {
                if let CallKind::Ref { name, package, .. } = &site.kind {
                    let pin = ref_pin(&masked, &site);
                    // Only a `Local` verdict names a node the kept graph
                    // carries — a `to:` naming an installed package's
                    // member drops like an unresolvable name.
                    if let RefVerdict::Local = ref_owner(
                        ctx.proj,
                        package.as_ref().map(|p| &masked[p.clone()]),
                        &masked[name.clone()],
                        ctx.name_index.contains_key(&masked[name.clone()] as &str),
                        ctx.installed_refs,
                        ctx.projects,
                    ) {
                        if let Some(uid) = ref_target_of(
                            &masked[name.clone()],
                            &pin,
                            ctx.name_index,
                            ctx.versioned,
                        ) {
                            deps.push(uid.clone());
                        }
                    }
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
        for (version, dep) in
            unit_test_targets(ctx, model, &entry.versions, yml_path, &entry.name, warnings)
        {
            let uid = match &version {
                // `<test>_v<version>` with the declared scalar rendered —
                // `2` → `_v2`, `'v3'` → `_vv3` — the real wire shape.
                Some(v) => format!("unit_test.{}.{}.{}_v{}", ctx.proj, model, entry.name, v),
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
/// the members `synthesize_models` bound.
///
/// A `versions:`-declared family walks its member table in declaration
/// order — an unfiltered entry expands to every LIVE member (a parked
/// `enabled: false` version contributes nothing, the way dbt's disabled
/// map holds it), `include:` selects them (an absent `include` or `'*'`
/// means all), `exclude:` subtracts — matched on the VERBATIM rendered
/// version, the same `v{arg}`-suffix comparison `ref(m, v=…)` performs
/// (`include: [v3]` selects `v: 'v3'`'s `.vv3` member, never `v: 3`'s
/// `.v3` — verified on 1.12.6). An `include`d version the family never
/// declared is warned about (dbt hard-errors there); a declared-but-
/// unbound member just doesn't emit. The recorded `version` is the
/// member's declared scalar rendered — `beta` stays `beta`, `'v3'`
/// stays `v3` — so the wire keeps the real `<test>_v<version>` shape.
///
/// A model with no `versions:` declaration is unversioned: its single
/// `model.<proj>.<name>` uid is the target, and a single-element
/// `include` rides `version` for fidelity only (dbt rejects `versions:`
/// on an unversioned model — degrading to the one target beats dropping
/// a declared test). A `model:` naming nothing the loader knows — or a
/// non-model node — gets no target at all: dbt hard-errors, source mode
/// warns and drops the record like the other malformed declarations.
fn unit_test_targets(
    ctx: &TestCtx<'_>,
    model: &str,
    versions: &SchemaUnitTestVersions,
    yml_path: &str,
    test_name: &str,
    warnings: &mut Vec<String>,
) -> Vec<(Option<String>, String)> {
    let bare = format!("model.{}.{}", ctx.proj, model);
    if let Some(family) = ctx.versioned.get(model) {
        // The member table is keyed by the declared scalar RENDERED —
        // matching is verbatim, the same `v{arg}`-suffix comparison
        // `ref(m, v=…)` performs (`include: [v3]` selects `v: 'v3'`'s
        // `.vv3` member, never `v: 3`'s `.v3`).
        let include = version_set(&versions.include);
        let exclude = version_set(&versions.exclude);
        let declared: std::collections::BTreeSet<String> =
            family.versions.keys().cloned().collect();
        // An `include`d version the family never declared is a dbt parse
        // error — warn-and-skip like the other malformed declarations.
        if let Some(set) = &include {
            for v in set
                .iter()
                .filter(|v| v.as_str() != "*" && v.as_str() != "all")
            {
                if !declared.contains(v) {
                    warnings.push(format!(
                        "{yml_path}: unit test '{test_name}' includes version '{v}' which model '{model}' never declares — ignoring it"
                    ));
                }
            }
        }
        let kept: Vec<_> = family
            .versions
            .iter()
            .filter(|(v, _)| {
                let all = |set: &std::collections::BTreeSet<String>| {
                    set.contains("*") || set.contains("all")
                };
                include
                    .as_ref()
                    .is_none_or(|set| all(set) || set.contains(*v))
                    && !exclude
                        .as_ref()
                        .is_some_and(|set| all(set) || set.contains(*v))
            })
            .filter_map(|(v, uid)| uid.as_ref().map(|u| (Some(v.clone()), u.clone())))
            .collect();
        if kept.is_empty() {
            warnings.push(format!(
                "{yml_path}: unit test '{test_name}' selects no version of model '{model}' — it can't target anything"
            ));
        }
        return kept;
    }
    if ctx.name_index.get(model).is_none_or(|u| u != &bare) {
        // The name answered to a non-model node or nothing at all.
        warnings.push(format!(
            "{yml_path}: unit test '{test_name}' targets unknown model '{model}' — it can't target anything"
        ));
        return Vec::new();
    }
    // A single-element `include` rides `version` for fidelity only; dbt
    // rejects `versions:` on an unversioned model, so degrading to the
    // one target beats dropping a declared test.
    let version = versions
        .include
        .as_array()
        .filter(|l| l.len() == 1)
        .and_then(|l| l.first().and_then(scalar_to_string));
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

/// A `versions:` list's rendered member set — `None` for the
/// absent/wildcard/non-list spellings (all of which select every
/// member), `Some` of the verbatim version strings for a real list
/// (non-scalars can't name a version and drop out).
fn version_set(v: &serde_json::Value) -> Option<std::collections::BTreeSet<String>> {
    let list = v.as_array().filter(|l| !l.is_empty())?;
    Some(list.iter().filter_map(scalar_to_string).collect())
}
