//! The synthesis pipeline behind [`manifest_from_source_with_warnings`]: a
//! [`Synthesis`] carries the shared state the stages mutate — project config,
//! collected docs blocks, the node/source/exposure maps, the deferred SQL
//! list, and the project-tree fallback tables — and each stage is a method so
//! the public entry point reads top-to-bottom in the order dbt's own pipeline
//! applies them. Warning ORDER is significant (first-seen dedup happens in
//! `finish`), so the stage order is a contract, not a preference.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::*;
use crate::{
    dag::{version_scope_key, versioned_model_segs},
    normalize_version, DisabledRef, RawConfig, RawDoc, RawExposure, RawExposureConfig,
    RawExposureOwner, RawMetadata, RefScope, UnresolvedAdjacent,
};

/// The shared state the source-synthesis stages build up: the parsed project
/// file and its resource paths, the collected docs blocks, the synthesized
/// node/source/exposure maps, the name index `ref()` resolves through, the
/// deferred comment-masked SQL the edge pass scans, the project-tree fallback
/// config tables, and the recoverable warnings collected along the way.
pub(super) struct Synthesis {
    dir: PathBuf,
    /// `dbt_project.yml`'s `name` — the root project, and the uid segment
    /// every synthesized resource carries.
    proj: String,
    /// The whole deserialized project file — the resource config trees
    /// (`models:`/`seeds:`/`snapshots:`/`sources:`) live on it.
    project: DbtProject,
    vars: Vars,
    model_paths: Vec<String>,
    seed_paths: Vec<String>,
    snapshot_paths: Vec<String>,
    /// The project-local `{% macro %}` table (collected in `load` like the
    /// docs blocks) — the edge pass binds `{{ name(…) }}`/`{% … name(…) %}`
    /// invocations against it for macro-derived `ref`/`source` edges.
    macros: Macros,
    pat: Patterns,
    /// The collected `{% docs %}` blocks every description intake renders
    /// through (`{{ doc() }}` expansion).
    docs: DocBlocks,
    nodes: HashMap<String, RawNode>,
    sources: HashMap<String, RawSource>,
    exposures: HashMap<String, RawExposure>,
    /// name -> unique_id for models/seeds/snapshots, so `ref('x')` can resolve.
    /// A `versions:`-declared family lands here under its logical name
    /// pointing at the latest version's `.<v>` member uid; a member's
    /// file stem (`fct_orders_v2`, a `defined_in:` stem) is never a
    /// refable name — dbt reaches those nodes only through the family.
    name_index: HashMap<String, String>,
    /// Model files collected but not yet turned into nodes — a `.sql`/
    /// `.py` file is an independent model OR a `versions:`-declared
    /// member (via the `<name>_v<v>`/`<name>` conventions or
    /// `defined_in:`), which only the parsed schema files can tell.
    /// `synthesize_models` binds them after `parse_schema_files`.
    pending_models: Vec<PendingModel>,
    /// The collect-time claim registry — every stem a collected
    /// resource took, in collection order (pending models reserve their
    /// stems here before the schema pass says where they land, keeping
    /// the models-first ordering seeds/snapshots check against). After
    /// `synthesize_models` the `name_index` is the universe — a claimed
    /// member stem stays claimed without ever becoming refable.
    claimed: HashSet<String>,
    /// `versions:`-declared families `synthesize_models` bound —
    /// family name → its version table — the `ref('fam', v=)` lookup
    /// and the per-version metadata expansion target.
    versioned: BTreeMap<String, VersionedFamily>,
    /// `unit_tests:` declarations → the `RawManifest::unit_tests` map —
    /// filled in `apply_schema_files` once `name_index` is complete.
    unit_tests: HashMap<String, RawUnitTest>,
    /// (uid, Jinja-comment-masked SQL) deferred to the ref/source pass —
    /// masking preserves byte length, so call-site offsets index the raw text.
    node_sql: Vec<(String, String)>,
    /// `.py` models' parsed `dbt.*` calls deferred to the edge pass — the
    /// python half of `node_sql`.
    node_python: Vec<DeferredPy>,
    /// uid -> dbt_project.yml tree materialization, applied AFTER schema.yml
    /// (precedence: in-file config() > schema.yml > project tree > default view).
    tree_mat: HashMap<String, String>,
    /// uid -> dbt_project.yml tree `+meta` dict, merged AFTER schema.yml like
    /// `tree_mat` (the meta precedence chain is the same shape: in-file
    /// config(meta=…) > schema.yml meta/config.meta > project tree).
    tree_meta: HashMap<String, serde_json::Map<String, serde_json::Value>>,
    /// uid -> dbt_project.yml tree `+unique_key` — filled like `tree_mat`
    /// (in-file config() > schema.yml config: > tree), the whole declared
    /// value (dbt accepts a string or a string list).
    tree_uk: HashMap<String, serde_json::Value>,
    /// uid -> the whole resolved tree config, for the governance keys
    /// (`+access`/`+group`/`+contract`/`+incremental_strategy`) — the same
    /// lowest-precedence slot `tree_mat`/`tree_uk` occupy.
    tree_gov: HashMap<String, TreeConfig>,
    /// uid -> the file's own `config(tags=…)` / `dbt.config(tags=…)` list
    /// — the middle channel of a node's tag union. Recorded at node
    /// creation; `apply_tree_fallback` merges all channels in dbt's order
    /// once every schema file has contributed its share.
    infile_tags: HashMap<String, Vec<String>>,
    /// uid -> the schema `config.tags:` channel — the LAST segment of the
    /// node's tag union (a `versions:` member's is `ver ++ model`,
    /// recorded by `apply_versioned_meta`).
    yml_tags: HashMap<String, Vec<String>>,
    /// `enabled: false`-parked refable names — `name -> {(version,
    /// resource kind)}` — recorded instead of dropped:
    /// `unresolved_references` reports a `ref()` to one under its own
    /// "disabled" wording, matching where dbt parks them
    /// (`manifest.disabled`). A parked version member records its
    /// declared version so a `v=` pin reads "disabled version" rather
    /// than "no such version"; `None` marks an unversioned resource —
    /// the same shape `DisabledRef` carries on the wire.
    disabled_names: BTreeMap<String, BTreeSet<(Option<String>, String)>>,
    /// `(source, table)` pairs `config.enabled`/`+enabled` parked — the
    /// source-side half of `disabled_names`.
    disabled_sources: BTreeSet<(String, String)>,
    /// uid -> the IN-FILE `enabled` verdict — `config(enabled=…)` or a
    /// `.py` model's `dbt.config(enabled=…)` — recorded when the file
    /// declares one. `apply_meta` folds it as the highest-precedence
    /// channel over schema.yml `config.enabled:` and the tree's
    /// `+enabled`.
    in_file_enabled: HashMap<String, Enabled>,
    /// uids whose `enabled` stayed [`Enabled::Unproven`] after every
    /// declared channel folded — kept in the graph (a guess either way
    /// could drop a live node dbt keeps… or a dead one) but recorded so
    /// `RawManifest::enabled_unverifiable` carries them and lint rules
    /// never build a claim on a maybe-absent node or its edges.
    enabled_unverifiable: BTreeSet<String>,
    /// Names `ref()` may legitimately resolve to without a synthesized
    /// node: `versions:`/`defined_in:`-declared model names and the
    /// `<name>` families of `<name>_v<ver>` model files — a family whose
    /// members all went missing/disabled keeps resolving rather than
    /// fabricating a "no such name" verdict dbt can't confirm either
    /// (the declaration alone is a dbt parse error we degrade).
    assumed_names: BTreeSet<String>,
    /// Recoverable oddities (an unparsable schema.yml, a duplicate resource
    /// name, a non-mapping `meta:`) are reported as warnings, not errors.
    warnings: Vec<String>,
}

/// One parsed schema/properties file plus the roots it was collected under:
/// `(schema, file, resource-root)` — the `apply_schema_files` pass needs the
/// file (for `original_file_path`) and the resource root (exposure `path` is
/// recorded relative to it, the way dbt does for models).
type ParsedSchemaFile = (SchemaFile, PathBuf, PathBuf);

/// What `resolve_edges` produces beyond the two adjacency maps — the macro
/// metadata and the unprovable-adjacency record `finish` folds into the
/// manifest.
#[derive(Debug, Default)]
pub(super) struct EdgeExtras {
    /// uid → `macro.<proj>.<name>` unique_ids the node's Jinja invokes
    /// (direct calls only — dbt records nested callees on the callee's
    /// own `depends_on.macros`) — `depends_on.macros`'s source-mode
    /// value.
    pub node_macros: HashMap<String, Vec<String>>,
    /// The unresolvable-call record — `unresolved_adjacent`'s raw
    /// material.
    pub unresolved: UnresolvedCalls,
}

/// A `.py` model's parsed `dbt.*` calls deferred to the edge pass — the
/// python half of `node_sql`. `jinja` flags a file dbt itself rejects
/// (Jinja markup is a parse error in a `.py`): its calls are still read,
/// but the file may hide adjacency the text can't show.
pub(super) struct DeferredPy {
    pub(super) uid: String,
    /// The `dbt.ref`/`dbt.source` calls — `dbt.config` calls configure the
    /// node at collection and never produce edges.
    pub(super) calls: Vec<PyCall>,
    pub(super) jinja: bool,
}

/// `resolve_edges`'s product — the two adjacency maps plus the extra
/// metadata that accompanies them.
#[derive(Debug, Default)]
pub(super) struct ResolvedEdges {
    /// uid → sorted parent uids (only nodes WITH parents get an entry).
    pub parent_map: HashMap<String, Vec<String>>,
    /// uid → sorted child uids — the adjacency map's other direction.
    pub child_map: HashMap<String, Vec<String>>,
    /// The macro metadata and unprovable-adjacency record.
    pub extras: EdgeExtras,
}

/// A model file collected before the schema files name its fate: an
/// independent `model.<proj>.<stem>` or a `versions:` member's
/// implementation (`model.<proj>.<fam>.v<ver>`). Nothing is synthesized
/// from it until `synthesize_models` — the `claimed` registry already
/// holds the stem, preserving the collection-order name claims.
pub(super) struct PendingModel {
    /// The file stem — the model name when the file stays independent.
    name: String,
    /// The source file — `path`/`original_file_path` (and `.py` scans).
    file: PathBuf,
    /// The model-paths root `file` was collected under (`path` is
    /// relative to it, like dbt records).
    root: PathBuf,
    /// The raw file text — `raw_code`.
    raw: String,
    /// The file's own `config(enabled=)`/`dbt.config(enabled=)` verdict —
    /// the highest-precedence `enabled` channel wherever the file lands
    /// (a member file's own config parks ITS version only).
    in_file: Option<Enabled>,
    /// The language-specific collected payload.
    kind: PendingKind,
}

/// What a pending model file carries past the schema pass.
pub(super) enum PendingKind {
    /// A `.sql` model — the Jinja-comment-masked text the in-file config
    /// extractors and the edge pass read.
    Sql { masked: String },
    /// A `.py` model — the merged `dbt.config` surface and the deferred
    /// `dbt.*` dependency calls.
    Py {
        cfg: Box<PyFileConfig>,
        calls: Vec<PyCall>,
        jinja: bool,
    },
}

/// A `versions:`-declared model family — the members `synthesize_models`
/// bound to implementation files: the `ref('fam', v=…)` lookup table and
/// the per-version metadata expansion target.
pub(super) struct VersionedFamily {
    /// `ref('fam')`'s target — the declared `latest_version:` or the
    /// computed newest — as the RENDERED version string (`"2"`, `"beta"`)
    /// the `versions` keys share.
    pub latest: String,
    /// rendered version → member uid. A version whose member is parked
    /// (`enabled: false`) or file-less stays `None` — a `v=` pin on it
    /// resolves to nothing and `disabled_refs`/`unresolved` words why.
    pub versions: BTreeMap<String, Option<String>>,
}

/// What a `ref()` call's `v=`/`version=` kwarg resolves to — the three
/// states dbt's versioned-target lookup distinguishes: no pin (the
/// family's LATEST answers), a literal pin (that exact rendered
/// version), and a kwarg source can't read (`v=var('x')`, a macro's
/// unbound parameter — resolved to the family's latest, the same
/// approximation a bare ref gives, since dbt binds it at run time).
#[derive(Debug)]
pub(super) enum RefPin {
    /// No `v=`/`version=` kwarg — `ref('x')`.
    Absent,
    /// `v=`/`version=` a literal — rendered the way dbt writes it (a
    /// number's `str()` — `1.10` → `"1.1"` — a string verbatim).
    Literal(String),
    /// A `v=` kwarg that isn't a literal — unprovable; the family's
    /// latest is the approximation (and on an unversioned name the ref
    /// resolves to nothing, like dbt's error).
    Dynamic,
}

impl Synthesis {
    /// Read `dbt_project.yml`, load the `var()` tables, resolve the resource
    /// path lists, and collect the `{% docs %}` blocks — everything the later
    /// stages read before a single resource is synthesized.
    pub(super) fn load(dir: &Path, cli_vars: Option<&CliVars>) -> Result<Self> {
        let proj_path = dir.join("dbt_project.yml");
        let proj_str = fs::read_to_string(&proj_path)
            .with_context(|| format!("failed to read dbt_project.yml: {}", proj_path.display()))?;
        // Recoverable oddities (an unparsable schema.yml, a duplicate resource
        // name, a non-mapping `meta:`) are reported as warnings, not errors.
        let mut warnings: Vec<String> = Vec::new();
        // dbt's YAML is PyYAML's: the project file parses through the same
        // style-aware loader the schema files do (a quoted `"yes"` stays a
        // string; a bare `yes` is the bool dbt records).
        let project: DbtProject = yaml_to_json(&proj_str, &mut warnings)
            .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse dbt_project.yml: {}: {e}",
                    proj_path.display()
                )
            })?;

        // `var('name')` scope for `+enabled` / `config(enabled=)` — the
        // project-file render and a node's render consult different var
        // tables (see vars.rs), so both are loaded once here, the `--vars`
        // map sitting above each like dbt's CLI-vars precedence.
        let vars = Vars::load(dir, &project, cli_vars, &mut warnings);

        let proj = project.name.clone();
        // The resource-path lists resolve through the ONE shared resolver
        // (`paths.rs`): `source-paths`/`data-paths` aliases, per-key dbt
        // defaults, `contained_resource_paths` containment, and the
        // docs-paths union — so every later join site (the .sql/.csv
        // collectors AND the schema.yml scan) and the staleness/watch
        // scans agree on which directories are dbt inputs.
        let paths = resolve_resource_paths(&project, &mut warnings);

        // Every `{% macro %}` under `macro-paths` — collected up front
        // like the docs blocks so the edge pass resolves `{{ name(…) }}`
        // calls in model/snapshot SQL against the project's own table.
        let macros = Macros::load(dir, &paths.macro_paths, &mut warnings);

        let pat = Patterns::new();

        // --- docs blocks (.md) ---
        // dbt renders `{{ doc('x') }}` in descriptions from `{% docs x %}…{% enddocs %}`
        // blocks collected from .md files under `docs-paths` — unset means EVERY
        // resource path (models/seeds/snapshots/macros/analyses/tests, verified
        // against real dbt). Collect them up front so every description intake
        // below renders through the one resolver.
        let docs = DocBlocks::new(
            proj.clone(),
            collect_doc_blocks(dir, &paths.doc_paths, &pat, &mut warnings),
        );

        Ok(Synthesis {
            dir: dir.to_path_buf(),
            proj,
            project,
            vars,
            model_paths: paths.model_paths,
            seed_paths: paths.seed_paths,
            snapshot_paths: paths.snapshot_paths,
            macros,
            pat,
            docs,
            nodes: HashMap::new(),
            sources: HashMap::new(),
            exposures: HashMap::new(),
            name_index: HashMap::new(),
            pending_models: Vec::new(),
            claimed: HashSet::new(),
            versioned: BTreeMap::new(),
            unit_tests: HashMap::new(),
            node_sql: Vec::new(),
            node_python: Vec::new(),
            tree_mat: HashMap::new(),
            tree_meta: HashMap::new(),
            tree_uk: HashMap::new(),
            tree_gov: HashMap::new(),
            infile_tags: HashMap::new(),
            yml_tags: HashMap::new(),
            disabled_names: BTreeMap::new(),
            disabled_sources: BTreeSet::new(),
            in_file_enabled: HashMap::new(),
            enabled_unverifiable: BTreeSet::new(),
            assumed_names: BTreeSet::new(),
            warnings,
        })
    }

    /// The models stage: every `.sql` under `model-paths` is collected as
    /// a [`PendingModel`] — the schema pass still has to say whether a
    /// file is an independent model or a `versions:`-declared member, so
    /// node synthesis defers to [`Synthesis::synthesize_models`]. What
    /// collect CAN decide it does here: the in-file `config(enabled=)`
    /// verdict (a file's own config parks it wherever it lands — an
    /// Off file is kept pending too, so the claim stage records the
    /// parked member under its family rather than its stem), the
    /// duplicate-name check (`claimed` reserves the stem in collection
    /// order — models always precede seeds/snapshots), and the
    /// `<name>_v<ver>` family-name assumption.
    pub(super) fn collect_models(&mut self) {
        for (file, root) in collect_with_root(&self.dir, &self.model_paths, "sql") {
            // Keep the pre-mask text for the SQL preview modal (comments intact);
            // the ref/source/config extractors still see the Jinja-comment-masked
            // copy (dbt's Jinja-before-SQL-comments semantics) — masking preserves
            // byte length, so offsets in it index `raw` too.
            let raw = read(&file);
            // `raw` moves into `RawNode` at synthesis, so the mask is owned
            // here — comment-free files skip the byte loop and keep the copy.
            let masked = mask_jinja_comments(&raw).into_owned();
            let name = file_stem(&file);
            // A `<name>_v<ver>` stem MAY declare the `<name>` family — the
            // ref scope counts it resolved whether or not a `versions:`
            // declaration later binds it (deliberately loose: an
            // under-report, never a false "no such name").
            if let Some(family) = version_family(&name) {
                self.assumed_names.insert(family.to_string());
            }
            // The in-file `config(enabled=)` verdict is the only enabled
            // channel knowable before the member claims resolve the real
            // fqn — a file's own `false` parks it under either fate, so
            // it never claims the stem (matching dbt: a disabled resource
            // doesn't answer a name claim either).
            let in_file = self.pat.enabled(
                &masked,
                &self.vars,
                &mut self.warnings,
                &rel_path(&self.dir, &file),
            );
            config_shape_warnings(&masked, &mut self.warnings, &rel_path(&self.dir, &file));
            if in_file != Some(Enabled::Off) {
                // `collect_with_root` sorts by path, so the first file to
                // claim a name wins deterministically; drop (and warn on)
                // a later same-stem file before the synthesis stage can
                // pick it up.
                if !self.claimed.insert(name.clone()) {
                    self.warnings.push(format!(
                        "duplicate resource name '{name}': ignoring {}",
                        rel_path(&self.dir, &file)
                    ));
                    continue;
                }
            }
            self.pending_models.push(PendingModel {
                name,
                file,
                root,
                raw,
                in_file,
                kind: PendingKind::Sql { masked },
            });
        }
    }

    /// The python half of model discovery — the same [`PendingModel`]
    /// deferral `collect_models` takes (`language: "python"`, `raw_code`
    /// the file's whole text, and `dbt.config(...)` literals as the
    /// in-file config — kwargs or a positional dict, dbt accepts both),
    /// deferred until `synthesize_models` binds the `versions:` members.
    /// A `.sql` file of the same stem claims the name first (models are
    /// collected before python files, both in path order). `def
    /// model`-missing files and Jinja-marked files are dbt parse errors —
    /// kept with warnings, like every other recoverable oddity.
    pub(super) fn collect_python_models(&mut self) {
        for (file, root) in collect_with_root(&self.dir, &self.model_paths, "py") {
            let raw = read(&file);
            let name = file_stem(&file);
            // A `<name>_v<ver>` stem may declare the `<name>` family —
            // same convention as `.sql` stems.
            if let Some(family) = version_family(&name) {
                self.assumed_names.insert(family.to_string());
            }
            let ofp = rel_path(&self.dir, &file);
            let scan = scan_python(&raw);
            if scan.model_defs != 1 {
                self.warnings.push(format!(
                    "{ofp}: dbt expects exactly one 'def model' per python file (found {}); the model is kept anyway",
                    scan.model_defs
                ));
            }
            if scan.has_jinja {
                self.warnings.push(format!(
                    "{ofp}: Jinja markup is not allowed in a python model (a dbt parse error); its dbt.* calls may be misread"
                ));
            }
            let cfg = py_file_config(&scan, &mut self.warnings, &ofp);
            // The in-file `enabled` verdict is the only channel knowable
            // before the member claims resolve the real fqn — a proven
            // literal `False` parks the file under either fate (a
            // declared-but-unreadable or non-bool value warns and stays
            // enabled — the `Enabled::Unproven` stance).
            let in_file = match cfg.enabled.as_ref() {
                Some(Some(serde_json::Value::Bool(b))) => {
                    Some(if *b { Enabled::On } else { Enabled::Off })
                }
                Some(Some(v)) => {
                    self.warnings.push(format!(
                        "{ofp}: config(enabled={v}) is not a boolean literal; the resource stays enabled"
                    ));
                    Some(Enabled::Unproven)
                }
                Some(None) => {
                    self.warnings.push(format!(
                        "{ofp}: config(enabled=...) could not be evaluated as a boolean; the resource stays enabled"
                    ));
                    Some(Enabled::Unproven)
                }
                None => None,
            };
            if in_file != Some(Enabled::Off) && !self.claimed.insert(name.clone()) {
                self.warnings
                    .push(format!("duplicate resource name '{name}': ignoring {ofp}"));
                continue;
            }
            self.pending_models.push(PendingModel {
                name,
                file,
                root,
                raw,
                in_file,
                kind: PendingKind::Py {
                    cfg: Box::new(cfg),
                    // `dbt.config` calls configure the node — only the
                    // dependency calls defer to the edge pass.
                    calls: scan
                        .calls
                        .into_iter()
                        .filter(|c| c.kind != PyCallKind::Config)
                        .collect(),
                    jinja: scan.has_jinja,
                },
            });
        }
    }

    /// The seeds stage: every `.csv` under `seed-paths` becomes a `seed.*`
    /// node — tree `+enabled` filtered, duplicate names dropped, CSV header
    /// columns synthesized into the wire `columns` object.
    pub(super) fn collect_seeds(&mut self) {
        for (file, root) in collect_with_root(&self.dir, &self.seed_paths, "csv") {
            let name = file_stem(&file);
            let uid = format!("seed.{}.{name}", self.proj);
            let path = rel_path(&root, &file);
            let tree = resolve_tree_config(
                &self.project.seeds,
                &tree_segments(&self.proj, &path),
                "seeds".to_string(),
                &mut self.warnings,
                Some(&self.vars),
            );
            // A seed's only `enabled` channel is the tree's.
            let verdict = tree.enabled_verdict();
            if verdict == Some(Enabled::Off) {
                self.disabled_names
                    .entry(name)
                    .or_default()
                    .insert((None, "seed".to_string()));
                continue;
            }
            // `claimed` — not `name_index` — is the dup registry: a
            // pending model file holds its stem here before the schema
            // pass says where it lands.
            if self.claimed.contains(&name) {
                self.warnings.push(format!(
                    "duplicate resource name '{name}': ignoring {}",
                    rel_path(&self.dir, &file)
                ));
                continue;
            }
            self.claimed.insert(name.clone());
            if verdict == Some(Enabled::Unproven) {
                self.enabled_unverifiable.insert(uid.clone());
            }
            if !tree.meta.is_empty() {
                self.tree_meta.insert(uid.clone(), tree.meta.clone());
            }
            if let Some(uk) = &tree.unique_key {
                self.tree_uk.insert(uid.clone(), uk.clone());
            }
            self.tree_gov.insert(uid.clone(), tree.clone());
            self.nodes.insert(
                uid.clone(),
                RawNode {
                    name: name.clone(),
                    resource_type: "seed".to_string(),
                    path: Some(path),
                    original_file_path: Some(rel_path(&self.dir, &file)),
                    // dbt records seeds with materialization "seed".
                    config: RawConfig {
                        materialized: Some("seed".to_string()),
                        ..Default::default()
                    },
                    columns: csv_header_columns(&file, &self.docs, &mut self.warnings),
                    ..Default::default()
                },
            );
            self.name_index.insert(name, uid);
        }
    }

    /// The `{% snapshot %}` stage: each block in a `snapshot-paths` `.sql`
    /// file becomes a `snapshot.*` node — file-wide `enabled` filter, then
    /// per-block tree config keyed on dbt's `[pkg, *dirs, <file-stem>,
    /// <name>]` fqn.
    pub(super) fn collect_snapshots(&mut self) {
        for (file, root) in collect_with_root(&self.dir, &self.snapshot_paths, "sql") {
            let raw = read(&file);
            let masked = mask_jinja_comments(&raw);
            let path = rel_path(&root, &file);
            let ofp = rel_path(&self.dir, &file);
            // An in-file `enabled` verdict applies to every block in the
            // file (rare) — a proven `false` parks them all like dbt's
            // `disabled`; an unproven one marks each block it covers.
            let in_file = self
                .pat
                .enabled(&masked, &self.vars, &mut self.warnings, &ofp);
            config_shape_warnings(&masked, &mut self.warnings, &ofp);
            if in_file == Some(Enabled::Off) {
                for name in self.pat.snapshots(&masked) {
                    self.disabled_names
                        .entry(name)
                        .or_default()
                        .insert((None, "snapshot".to_string()));
                }
                continue;
            }
            // Refs in a multi-block file are attributed to every block in it (rare).
            for name in self.pat.snapshots(&masked) {
                let uid = format!("snapshot.{}.{name}", self.proj);
                // dbt's {% snapshot %} fqn KEEPS the file stem and appends the
                // snapshot name on top (`[pkg, *dirs, <file-stem>, <name>]` —
                // SnapshotParser.get_fqn: "the fqn includes the filename"), so a
                // `snapshots:` tree can configure a whole file's blocks at the
                // stem level AND a single block under it. YAML-defined snapshots
                // are the different, prefix-only shape (handled below).
                let mut segments = tree_segments(&self.proj, &path);
                segments.push(name.clone());
                let tree = resolve_tree_config(
                    &self.project.snapshots,
                    &segments,
                    "snapshots".to_string(),
                    &mut self.warnings,
                    Some(&self.vars),
                );
                let verdict = in_file.or(tree.enabled_verdict());
                if verdict == Some(Enabled::Off) {
                    self.disabled_names
                        .entry(name.clone())
                        .or_default()
                        .insert((None, "snapshot".to_string()));
                    continue;
                }
                if self.claimed.contains(&name) {
                    self.warnings
                        .push(format!("duplicate resource name '{name}': ignoring {ofp}"));
                    continue;
                }
                self.claimed.insert(name.clone());
                if verdict == Some(Enabled::Unproven) {
                    self.enabled_unverifiable.insert(uid.clone());
                }
                if !tree.meta.is_empty() {
                    self.tree_meta.insert(uid.clone(), tree.meta.clone());
                }
                if let Some(uk) = &tree.unique_key {
                    self.tree_uk.insert(uid.clone(), uk.clone());
                }
                self.tree_gov.insert(uid.clone(), tree.clone());
                // The `{{ config(tags=…) }}` inside a `{% snapshot %}`
                // block — extracted file-wide like the other pattern
                // extractors (multi-block files share the config).
                let tags = config_tags(&masked);
                if !tags.is_empty() {
                    self.infile_tags.insert(uid.clone(), tags);
                }
                self.nodes.insert(
                    uid.clone(),
                    RawNode {
                        name: name.clone(),
                        resource_type: "snapshot".to_string(),
                        path: Some(path.clone()),
                        original_file_path: Some(ofp.clone()),
                        config: RawConfig {
                            materialized: Some("snapshot".to_string()),
                            // The `{{ config(…) }}` inside a `{% snapshot %}`
                            // block carries `unique_key` — extracted file-wide
                            // like the other pattern extractors.
                            unique_key: config_unique_key(&masked),
                            ..Default::default()
                        },
                        // Snapshots can sit in a dbt `group`; `access`/`contract`
                        // stay None — dbt records them on models only.
                        group: config_governance(&masked).group,
                        // dbt records `language: "sql"` on .sql-defined snapshots.
                        language: Some("sql".to_string()),
                        // The `{{ config(…) }}` inside a `{% snapshot %}` block is
                        // extracted file-wide like the other pattern extractors
                        // (multi-block files share the config — rare).
                        meta: config_meta(&masked).unwrap_or_default(),
                        // The whole (multi-block) file is stored per block; raw_code
                        // is never compared, and snapshots are rare, so this is fine.
                        raw_code: Some(raw.clone()),
                        alias: config_alias(&masked),
                        ..Default::default()
                    },
                );
                self.name_index.insert(name, uid.clone());
                // The file's `enabled` verdict is the block's in-file
                // channel — `apply_meta` folds a schema-level
                // `config.enabled:` under it.
                if let Some(v) = in_file {
                    self.in_file_enabled.insert(uid.clone(), v);
                }
                self.node_sql.push((uid, masked.clone().into_owned()));
            }
        }
    }

    /// Collect and parse the `schema.yml`/properties files once — dbt reads
    /// properties yml ONLY from the configured resource paths, so the scan is
    /// restricted to them (scanning the whole project dir would ingest
    /// vendored projects, build artifacts, and virtualenvs — synthesizing
    /// phantom sources/tests under this project's namespace). `(file,
    /// resource root)` pairs dedup by FILE alone (sorting puts equal files
    /// adjacent): overlapping resource paths collect the same file under two
    /// roots, and scanning it twice would duplicate its synthesized tests.
    ///
    /// The parsed result is kept for BOTH the YAML-snapshot synthesis pass
    /// AND the schema-metadata pass, so a file is never re-parsed. A
    /// non-schema yml deserializes to all-empty defaults; only a YAML shape
    /// error (e.g. a top-level sequence) fails to parse — reported as a
    /// warning (not silently skipped) so one broken file never sinks the rest
    /// of the project.
    pub(super) fn parse_schema_files(&mut self) -> Vec<ParsedSchemaFile> {
        let mut yml_files: Vec<(PathBuf, PathBuf)> = Vec::new();
        for sub in self
            .model_paths
            .iter()
            .chain(&self.seed_paths)
            .chain(&self.snapshot_paths)
        {
            let sub_root = self.dir.join(sub);
            let mut files = Vec::new();
            collect_files(&sub_root, "yml", &mut files);
            collect_files(&sub_root, "yaml", &mut files);
            for file in files {
                yml_files.push((file, sub_root.clone()));
            }
        }
        yml_files.sort();
        yml_files.dedup_by(|a, b| a.0 == b.0);

        let mut parsed: Vec<ParsedSchemaFile> = Vec::new();
        for (file, yml_root) in yml_files {
            match yaml_to_json(&read(&file), &mut self.warnings)
                .and_then(|v| serde_json::from_value::<SchemaFile>(v).map_err(|e| e.to_string()))
            {
                Ok(schema) => {
                    // A `versions:`-declared family name keeps resolving
                    // even when no member file survives (all parked or
                    // missing — dbt errors at parse there, we under-report),
                    // and a `defined_in:` stem is a file the loader may not
                    // bind — the ref scope counts both as resolved names.
                    for model in &schema.models {
                        if model.versions.is_empty() {
                            continue;
                        }
                        self.assumed_names.insert(model.name.clone());
                        for v in &model.versions {
                            if let Some(d) = &v.defined_in {
                                self.assumed_names.insert(d.clone());
                            }
                        }
                    }
                    parsed.push((schema, file, yml_root))
                }
                Err(err) => self.warnings.push(format!(
                    "skipped {}: invalid schema file ({err})",
                    rel_path(&self.dir, &file)
                )),
            }
        }
        parsed
    }

    /// The versioned-model stage: every `versions:`-declared schema entry
    /// binds its declared versions to implementation files and emits one
    /// `model.<proj>.<name>.v<version>` node per member — the same nodes
    /// a real manifest carries (the member's OWN file drives `path`/
    /// `original_file_path`/`raw_code`/in-file config), so every later
    /// stage — edges, schema metadata, tests, tree fallback, the lint
    /// scope — sees the manifest shape through one path. Files no family
    /// claims become independent models under their own stem, the fate
    /// collection used to hand every file.
    ///
    /// The enabled verdicts a member folds here are version-aware: the
    /// impl file's own `config(enabled=)` > the version's
    /// `config.enabled:` > the model's > the member-fqn tree — verified
    /// precedence on real dbt 1.12.6. A parked member records
    /// `(version, "model")` under the FAMILY name so a `v=` pin reads
    /// "disabled version", the same wording a manifest's `disabled` map
    /// gives.
    pub(super) fn synthesize_models(&mut self, parsed: &[ParsedSchemaFile]) {
        let pending: Vec<Option<PendingModel>> = std::mem::take(&mut self.pending_models)
            .into_iter()
            .map(Some)
            .collect();
        let mut pending = pending;
        // stem → the pending file the family conventions/`defined_in:`
        // reach. A live file wins over an in-file-disabled one for the
        // same stem (the parked file would drop out of dbt's parse
        // anyway — its own verdict still records at claim).
        let mut by_stem: HashMap<String, usize> = HashMap::new();
        for (i, p) in pending.iter().enumerate() {
            let Some(p) = p else { continue };
            match by_stem.entry(p.name.clone()) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if p.in_file != Some(Enabled::Off)
                        && pending[*e.get()]
                            .as_ref()
                            .is_some_and(|x| x.in_file == Some(Enabled::Off))
                    {
                        e.insert(i);
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
            }
        }
        let mut claimed: HashSet<usize> = HashSet::new();
        let mut declared: HashSet<String> = HashSet::new();
        for (schema, file, _root) in parsed {
            let yml_path = rel_path(&self.dir, file);
            for entry in &schema.models {
                if entry.versions.is_empty() {
                    continue;
                }
                declared.insert(entry.name.clone());
                // A second `versions:` declaration of one family is a
                // dbt duplicate error — warn and keep the FIRST bind
                // (its members already claimed the files).
                if self.versioned.contains_key(&entry.name) {
                    self.warnings.push(format!(
                        "{yml_path}: model '{}' declares versions twice (a dbt duplicate error) — the first declaration wins",
                        entry.name
                    ));
                    continue;
                }
                self.synthesize_family(entry, &yml_path, &mut pending, &by_stem, &mut claimed);
            }
        }
        // The stems a family bound — a SECOND file carrying one (a
        // same-stem file in another model dir — a duplicate resource
        // name in dbt) must not materialize as an independent model.
        // (`pending[i]` is already `take`n at claim — `by_stem` maps stem
        // → index, so resolve each claimed index back through it.)
        let bound_stems: HashSet<String> = by_stem
            .iter()
            .filter(|(_, &i)| claimed.contains(&i))
            .map(|(stem, _)| stem.clone())
            .collect();
        for (i, p) in pending.iter_mut().enumerate() {
            if claimed.contains(&i) {
                continue;
            }
            let Some(p) = p.take() else { continue };
            // A leftover `<fam>`-stemmed file next to a `versions:`-
            // declared `fam` — dbt's DuplicateVersionedUnversionedError —
            // degraded to a warning + the file dropped (an unversioned
            // `fam` node would shadow the family's own name claims).
            if declared.contains(&p.name) {
                self.warnings.push(format!(
                    "{}: an unversioned '{}' and versioned members of '{}' can't coexist (a dbt parse error); ignoring the file",
                    rel_path(&self.dir, &p.file),
                    p.name,
                    p.name
                ));
                continue;
            }
            if bound_stems.contains(&p.name) {
                self.warnings.push(format!(
                    "{}: '{}' is already bound to a versioned model (a duplicate model file — a dbt parse error); ignoring it",
                    rel_path(&self.dir, &p.file),
                    p.name
                ));
                continue;
            }
            // A `<fam>_v<v>` stem whose family declared `versions:` but
            // left this file unclaimed names an undeclared version of a
            // versioned model — a dbt parse error, never an independent
            // `fam_v` model. Drop it the same way.
            if let Some(family) = version_family(&p.name) {
                if declared.contains(family) {
                    self.warnings.push(format!(
                        "{}: '{}' is an implementation file for versioned model '{}' but matches no declared version (a dbt parse error); ignoring it",
                        rel_path(&self.dir, &p.file),
                        p.name,
                        family
                    ));
                    continue;
                }
            }
            self.materialize_unversioned(p);
        }
    }

    /// Bind one `versions:`-declared family: render each declared `v:`,
    /// resolve `latest_version` (the declared one, or dbt's pairwise
    /// newest — numbers compare as floats, everything else as text, so
    /// `'beta'` outranks `2`), find each version's implementation file
    /// (`defined_in:` > `<name>_v<v>` convention > `<name>` for the
    /// latest only), and mint `model.<proj>.<fam>.v<v>` nodes through
    /// [`Synthesis::claim_member`]. The `name_index` learns the family
    /// name → latest member uid — `ref('fam')`'s resolution — and
    /// `self.versioned` records the table pins and metadata expansion
    /// need.
    fn synthesize_family(
        &mut self,
        entry: &SchemaModel,
        yml_path: &str,
        pending: &mut [Option<PendingModel>],
        by_stem: &HashMap<String, usize>,
        claimed: &mut HashSet<usize>,
    ) {
        // Declared versions in declaration order — `v:` renders the way
        // dbt writes the uid segment (a number's str()/a string verbatim).
        // A missing/non-scalar or repeated `v:` is a dbt error, degraded
        // to a warning + skip (first declaration wins).
        let mut decls: Vec<(usize, String)> = Vec::new();
        for (i, v) in entry.versions.iter().enumerate() {
            let Some(rendered) = scalar_to_string(&v.v) else {
                self.warnings.push(format!(
                    "{yml_path}: model '{}' declares a version without a valid 'v:' — ignoring the entry",
                    entry.name
                ));
                continue;
            };
            if decls.iter().any(|(_, r)| r == &rendered) {
                self.warnings.push(format!(
                    "{yml_path}: model '{}' declares version '{rendered}' twice (a dbt error) — the first wins",
                    entry.name
                ));
                continue;
            }
            decls.push((i, rendered));
        }
        if decls.is_empty() {
            return;
        }
        // The default latest is dbt's pairwise max over the declared
        // values. An explicit `latest_version:` must name one of them —
        // anything else is a dbt parse error, degraded to a warning +
        // the computed value.
        let computed = decls
            .iter()
            .map(|(i, r)| (*i, r.clone()))
            .reduce(|a, b| {
                if version_cmp(&entry.versions[a.0].v, &entry.versions[b.0].v)
                    == std::cmp::Ordering::Greater
                {
                    a
                } else {
                    b
                }
            })
            .map(|(_, r)| r)
            .unwrap_or_default();
        let latest = match scalar_to_string(&entry.latest_version) {
            Some(declared) => {
                if decls.iter().any(|(_, r)| r == &declared) {
                    declared
                } else {
                    self.warnings.push(format!(
                        "{yml_path}: model '{}' declares latest_version '{declared}' which is not a declared version (a dbt parse error) — the computed '{computed}' wins",
                        entry.name
                    ));
                    computed
                }
            }
            None => computed,
        };
        // Each declared version → its implementation file: `defined_in:`
        // names it (a stem — a directory path never resolves, verified),
        // else `<name>_v<v>`; the LATEST alone may also live in
        // `<name>.sql`/`.py`.
        let mut members: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (i, rendered) in &decls {
            let ver_entry = &entry.versions[*i];
            let mut candidates: Vec<String> = Vec::new();
            if let Some(d) = &ver_entry.defined_in {
                // `defined_in` is a file stem — tolerate a declared
                // extension (`x.sql` → `x`) like dbt's stem lookup.
                candidates.push(
                    d.strip_suffix(".sql")
                        .or_else(|| d.strip_suffix(".py"))
                        .unwrap_or(d.as_str())
                        .to_string(),
                );
            } else {
                candidates.push(format!("{}_v{}", entry.name, rendered));
                if rendered == &latest {
                    candidates.push(entry.name.clone());
                }
            }
            let idx = candidates.iter().find_map(|s| by_stem.get(s));
            members.insert(
                rendered.clone(),
                match idx {
                    Some(&i) if claimed.insert(i) => {
                        let p = pending[i].take().expect("claimed pending");
                        self.claim_member(entry, ver_entry, rendered, &latest, p, yml_path)
                    }
                    Some(_) => {
                        self.warnings.push(format!(
                            "{yml_path}: version '{rendered}' of model '{}' resolves to a file an earlier declaration claimed — ignoring it",
                            entry.name
                        ));
                        None
                    }
                    None => {
                        self.warnings.push(format!(
                            "{yml_path}: version '{rendered}' of model '{}' has no implementation file ({}); the version is skipped",
                            entry.name,
                            candidates.join(" or ") + ".sql/.py"
                        ));
                        None
                    }
                },
            );
        }
        // `ref('fam')` lands on the latest member — the bare family name
        // enters the index only when that member is live (a parked/
        // missing latest never resolves; `self.versioned` still records
        // the table for pins and metadata expansion).
        if let Some(uid) = members.get(&latest).and_then(|u| u.as_ref()) {
            self.name_index.insert(entry.name.clone(), uid.clone());
        }
        self.versioned.insert(
            entry.name.clone(),
            VersionedFamily {
                latest,
                versions: members,
            },
        );
    }

    /// Mint `model.<proj>.<fam>.v<rendered>` for one bound member — the
    /// file's own path/raw/in-file config, the member-fqn tree
    /// (`[proj, *impl-dirs, <fam>, v<v>]` — dbt's recorded fqn shape, so a
    /// `models:` tree configures versions under the family), and the
    /// version-aware `enabled` fold. Returns the uid (the family table's
    /// value), or `None` when the enabled verdict parks the member (the
    /// parked version then records under the family name).
    fn claim_member(
        &mut self,
        fam: &SchemaModel,
        ver_entry: &SchemaVersion,
        rendered: &str,
        latest: &str,
        pending: PendingModel,
        yml_path: &str,
    ) -> Option<String> {
        let uid = format!("model.{}.{}.v{}", self.proj, fam.name, rendered);
        // The member fqn: the impl file's directories, the LOGICAL name,
        // then `v<version>` — dbt's recorded fqn shape (a `models:` tree
        // configures the whole family at `<fam>` and single versions at
        // `<fam>: v<v>`; the impl file's stem is never a level).
        let mut segs = tree_segments(&self.proj, &rel_path(&pending.root, &pending.file));
        segs.pop();
        segs.push(fam.name.clone());
        segs.push(format!("v{rendered}"));
        let tree = resolve_tree_config(
            &self.project.models,
            &segs,
            "models".to_string(),
            &mut self.warnings,
            Some(&self.vars),
        );
        // `enabled` precedence — in-file `config(enabled=)` > the
        // version's `config.enabled:` > the model's > the member-fqn
        // tree's `+enabled` (verified on 1.12.6). A proven `Off` parks
        // the member under the FAMILY name with its version, so a `v=`
        // pin reads "disabled version 'v'" like a manifest `disabled`
        // record gives. `Unproven` keeps the member and marks it.
        let schema_v = Enabled::from_schema(self.vars.schema_enabled_state(
            ver_entry.config.enabled.as_ref(),
            &format!(
                "{yml_path}: model '{}' version '{rendered}' config.enabled",
                fam.name
            ),
            &mut self.warnings,
        ));
        let schema_m = Enabled::from_schema(self.vars.schema_enabled_state(
            fam.config.enabled.as_ref(),
            &format!("{yml_path}: model '{}' config.enabled", fam.name),
            &mut self.warnings,
        ));
        let verdict = pending
            .in_file
            .or(schema_v)
            .or(schema_m)
            .or_else(|| tree.enabled_verdict());
        if verdict == Some(Enabled::Off) {
            self.disabled_names
                .entry(fam.name.clone())
                .or_default()
                .insert((Some(rendered.to_string()), "model".to_string()));
            return None;
        }
        if verdict == Some(Enabled::Unproven) {
            self.enabled_unverifiable.insert(uid.clone());
        }
        // The member-fqn tree's fallback tables key by the member uid —
        // `apply_tree_fallback` fills what schema.yml and the file leave
        // unset, same slot as an unversioned model.
        if !tree.meta.is_empty() {
            self.tree_meta.insert(uid.clone(), tree.meta.clone());
        }
        if let Some(uk) = &tree.unique_key {
            self.tree_uk.insert(uid.clone(), uk.clone());
        }
        self.tree_gov.insert(uid.clone(), tree.clone());
        let mut node = RawNode {
            name: fam.name.clone(),
            resource_type: "model".to_string(),
            // `path`/`original_file_path` name the IMPL file — dbt
            // records `fct_orders_v2.sql` for `fct_orders.v2`, the
            // defined_in file for a declared one.
            path: Some(rel_path(&pending.root, &pending.file)),
            original_file_path: Some(rel_path(&self.dir, &pending.file)),
            version: Some(rendered.to_string()),
            latest_version: Some(latest.to_string()),
            raw_code: Some(pending.raw),
            ..Default::default()
        };
        match pending.kind {
            PendingKind::Sql { masked } => {
                if let Some(m) = &tree.materialized {
                    self.tree_mat.insert(uid.clone(), m.clone());
                }
                // The in-file `config(…)` spellings — the highest
                // precedence level over the version's and model's
                // `config:` blocks, same chain as an unversioned file.
                let gov = config_governance(&masked);
                node.config = RawConfig {
                    materialized: self.pat.materialized(&masked),
                    unique_key: config_unique_key(&masked),
                    incremental_strategy: gov.incremental_strategy.clone(),
                };
                node.access = gov.access.clone();
                node.group = gov.group.clone();
                node.contract = gov.contract.clone();
                node.language = Some("sql".to_string());
                node.meta = config_meta(&masked).unwrap_or_default();
                node.alias = config_alias(&masked);
                let tags = config_tags(&masked);
                if !tags.is_empty() {
                    self.infile_tags.insert(uid.clone(), tags);
                }
                self.node_sql.push((uid.clone(), masked));
            }
            PendingKind::Py { cfg, calls, jinja } => {
                // A python member's `table` default outranks the tree's
                // `+materialized` — `tree_mat` stays empty like the
                // unversioned path.
                node.config = RawConfig {
                    materialized: cfg.materialized,
                    unique_key: cfg.unique_key,
                    incremental_strategy: cfg.incremental_strategy.clone(),
                };
                if !cfg.tags.is_empty() {
                    self.infile_tags.insert(uid.clone(), cfg.tags.clone());
                }
                node.access = cfg.access;
                node.group = cfg.group;
                node.contract = cfg.contract;
                node.language = Some("python".to_string());
                node.meta = cfg.meta;
                node.alias = cfg.alias;
                self.node_python.push(DeferredPy {
                    uid: uid.clone(),
                    calls,
                    jinja,
                });
            }
        }
        self.nodes.insert(uid.clone(), node);
        if let Some(v) = pending.in_file {
            self.in_file_enabled.insert(uid.clone(), v);
        }
        Some(uid)
    }

    /// The unversioned fate — a pending file no `versions:` family
    /// claimed becomes `model.<proj>.<stem>` exactly the way collection
    /// used to synthesize it (stem-fqn tree, in-file config surface,
    /// `name_index` claim already held).
    fn materialize_unversioned(&mut self, pending: PendingModel) {
        let path = rel_path(&pending.root, &pending.file);
        let ofp = rel_path(&self.dir, &pending.file);
        let tree = resolve_tree_config(
            &self.project.models,
            &tree_segments(&self.proj, &path),
            "models".to_string(),
            &mut self.warnings,
            Some(&self.vars),
        );
        let uid = format!("model.{}.{}", self.proj, pending.name);
        let verdict = pending.in_file.or(tree.enabled_verdict());
        if verdict == Some(Enabled::Off) {
            self.disabled_names
                .entry(pending.name)
                .or_default()
                .insert((None, "model".to_string()));
            return;
        }
        if verdict == Some(Enabled::Unproven) {
            self.enabled_unverifiable.insert(uid.clone());
        }
        if !tree.meta.is_empty() {
            self.tree_meta.insert(uid.clone(), tree.meta.clone());
        }
        if let Some(uk) = &tree.unique_key {
            self.tree_uk.insert(uid.clone(), uk.clone());
        }
        self.tree_gov.insert(uid.clone(), tree.clone());
        let mut node = RawNode {
            name: pending.name.clone(),
            resource_type: "model".to_string(),
            path: Some(path),
            original_file_path: Some(ofp),
            raw_code: Some(pending.raw),
            ..Default::default()
        };
        match pending.kind {
            PendingKind::Sql { masked } => {
                if let Some(m) = &tree.materialized {
                    self.tree_mat.insert(uid.clone(), m.clone());
                }
                // The in-file `config(…)` spellings of the model governance
                // fields — each the highest level of the same precedence
                // chain `materialized` follows.
                let gov = config_governance(&masked);
                node.config = RawConfig {
                    materialized: self.pat.materialized(&masked),
                    unique_key: config_unique_key(&masked),
                    incremental_strategy: gov.incremental_strategy.clone(),
                };
                node.access = gov.access.clone();
                node.group = gov.group.clone();
                node.contract = gov.contract.clone();
                node.language = Some("sql".to_string());
                node.meta = config_meta(&masked).unwrap_or_default();
                node.alias = config_alias(&masked);
                let tags = config_tags(&masked);
                if !tags.is_empty() {
                    self.infile_tags.insert(uid.clone(), tags);
                }
                self.node_sql.push((uid.clone(), masked));
            }
            PendingKind::Py { cfg, calls, jinja } => {
                // The tree's other `+` keys DO reach a python model — but
                // `+materialized` deliberately does not: dbt's python
                // default `table` outranks the tree (verified), so
                // `tree_mat` never gains an entry for it.
                node.config = RawConfig {
                    materialized: cfg.materialized,
                    unique_key: cfg.unique_key,
                    incremental_strategy: cfg.incremental_strategy.clone(),
                };
                if !cfg.tags.is_empty() {
                    self.infile_tags.insert(uid.clone(), cfg.tags.clone());
                }
                node.access = cfg.access;
                node.group = cfg.group;
                node.contract = cfg.contract;
                node.language = Some("python".to_string());
                node.meta = cfg.meta;
                node.alias = cfg.alias;
                self.node_python.push(DeferredPy {
                    uid: uid.clone(),
                    calls,
                    jinja,
                });
            }
        }
        self.nodes.insert(uid.clone(), node);
        self.name_index.insert(pending.name.clone(), uid.clone());
        if let Some(v) = pending.in_file {
            self.in_file_enabled.insert(uid.clone(), v);
        }
    }

    /// The `ref()` target under a [`RefPin`] — [`ref_target_of`] over
    /// this load's tables.
    fn ref_target(&self, name: &str, pin: &RefPin) -> Option<&String> {
        ref_target_of(name, pin, &self.name_index, &self.versioned)
    }

    /// Every `ref()` target inside a Jinja-bearing `text` fragment —
    /// an exposure `depends_on:` entry, a semantic model's `model:` —
    /// resolved with the same pin semantics the .sql pass uses.
    fn ref_targets_in(&self, text: &str) -> Vec<String> {
        let masked = mask_jinja_comments(text);
        self.pat
            .call_sites_masked(&masked)
            .iter()
            .filter_map(|site| match &site.kind {
                CallKind::Ref { name, .. } => self
                    .ref_target(&masked[name.clone()], &ref_pin(&masked, site))
                    .cloned(),
                _ => None,
            })
            .collect()
    }

    /// YAML-defined snapshots (dbt 1.9+ `snapshots:` schema entries with a
    /// `relation:`, replacing the `{% snapshot %}` block) become nodes here,
    /// BEFORE ref/source resolution, so both a model that refs one AND the
    /// relation's own ref/source resolve.
    pub(super) fn synthesize_yaml_snapshots(&mut self, parsed: &[ParsedSchemaFile]) {
        for (schema, file, yml_root) in parsed {
            let yml_path = rel_path(&self.dir, file);
            for entry in &schema.snapshots {
                let Some(rel) = &entry.relation else {
                    continue;
                };
                // The schema entry's `config.enabled` is a YAML
                // snapshot's highest `enabled` channel (there is no
                // in-file config to outrank it); the tree fills its
                // absence only.
                let schema = Enabled::from_schema(self.vars.schema_enabled_state(
                    entry.config.enabled.as_ref(),
                    &format!("{yml_path}: snapshot '{}' config.enabled", entry.name),
                    &mut self.warnings,
                ));
                if schema == Some(Enabled::Off) {
                    self.disabled_names
                        .entry(entry.name.clone())
                        .or_default()
                        .insert((None, "snapshot".to_string()));
                    continue;
                }
                // `synthesize_models` has already run — `name_index` is
                // the post-binding universe: a member stem stays free
                // (it never was a refable name) and a live family name
                // answers.
                if self.name_index.contains_key(&entry.name) {
                    self.warnings.push(format!(
                        "duplicate resource name '{}': ignoring {}",
                        entry.name,
                        rel_path(&self.dir, file)
                    ));
                    continue;
                }
                // dbt keys snapshot configs by NAME (same rule as the
                // `{% snapshot %}` path).
                let yml_rel = rel_path(yml_root, file);
                let mut segments = tree_segments(&self.proj, &yml_rel);
                segments.pop();
                segments.push(entry.name.clone());
                let tree = resolve_tree_config(
                    &self.project.snapshots,
                    &segments,
                    "snapshots".to_string(),
                    &mut self.warnings,
                    Some(&self.vars),
                );
                let uid = format!("snapshot.{}.{}", self.proj, entry.name);
                let verdict = schema.or(tree.enabled_verdict());
                if verdict == Some(Enabled::Off) {
                    self.disabled_names
                        .entry(entry.name.clone())
                        .or_default()
                        .insert((None, "snapshot".to_string()));
                    continue;
                }
                if verdict == Some(Enabled::Unproven) {
                    self.enabled_unverifiable.insert(uid.clone());
                }
                if let Some(v) = schema {
                    self.in_file_enabled.insert(uid.clone(), v);
                }
                // dbt V2 synthesizes `select * from {relation}` (wrapping a bare
                // ref()/source() in {{ }}) and feeds it through the normal
                // pipeline — mirror that so the SQL preview and the edge
                // extractors see real text.
                let body = if rel.contains("{{") {
                    rel.clone()
                } else {
                    format!("{{{{ {rel} }}}}")
                };
                let sql = format!("select * from {body}");
                if !tree.meta.is_empty() {
                    self.tree_meta.insert(uid.clone(), tree.meta.clone());
                }
                if let Some(uk) = &tree.unique_key {
                    self.tree_uk.insert(uid.clone(), uk.clone());
                }
                self.tree_gov.insert(uid.clone(), tree.clone());
                // dbt records a YAML-defined snapshot's `path` as a virtual
                // `<name>.sql` (there is no real .sql file; dbt ≤1.11 prefixed the
                // declaring yml, 1.12 records the bare name — the loader tracks the
                // current convention so source and manifest mode agree).
                let path = format!("{}.sql", entry.name);
                self.nodes.insert(
                    uid.clone(),
                    RawNode {
                        name: entry.name.clone(),
                        resource_type: "snapshot".to_string(),
                        path: Some(path),
                        original_file_path: Some(rel_path(&self.dir, file)),
                        config: RawConfig {
                            materialized: Some("snapshot".to_string()),
                            ..Default::default()
                        },
                        // dbt records `language: "sql"` on YAML snapshots too.
                        language: Some("sql".to_string()),
                        raw_code: Some(sql.clone()),
                        ..Default::default()
                    },
                );
                self.name_index.insert(entry.name.clone(), uid.clone());
                self.node_sql.push((uid, sql));
            }
        }
    }

    /// Resolve `ref()`/`source()` call sites in the deferred masked SQL into
    /// `(parent_map, child_map)` — unresolved refs (cross-package, typo,
    /// gated) drop out; a source referenced but not declared in schema.yml
    /// dangles here and is pruned later.
    ///
    /// A second pass over each node's Jinja tags resolves `{{ name(…) }}` /
    /// `{% … name(…) %}` calls against the project macro table: an invoked
    /// macro's own `ref`/`source` calls (args bound to the invocation's
    /// literal values, nested calls followed under the depth cap) emit the
    /// same edge shapes direct calls do, the invoked macro's unique_id lands
    /// in `depends_on.macros`, and a call the table can't prove — a package
    /// `ns.name()`, an `adapter.dispatch` of an unknown name, a project
    /// macro whose bound args stayed non-literal — counts into the
    /// unprovable-adjacency record `finish` writes to
    /// `RawManifest::unresolved_adjacent`. That record is what keeps
    /// degree-judging lint rules honest: `root_models` can't claim "no
    /// parents" on a node whose hidden parents live inside a package macro
    /// body, and `unused_sources` can't claim "no consumers" on a source
    /// table one of those calls literally named.
    pub(super) fn resolve_edges(&self) -> ResolvedEdges {
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut extras = EdgeExtras::default();
        for (uid, sql) in &self.node_sql {
            let mut parents: Vec<String> = Vec::new();
            let mut invoked: BTreeSet<String> = BTreeSet::new();
            // `sql` is already comment-masked — no second mask pass.
            for site in self.pat.call_sites_masked(sql) {
                match site.kind {
                    // Unresolved refs (cross-package, typo, gated) are dropped, not faked.
                    CallKind::Ref { ref name, .. } => {
                        let pin = ref_pin(sql, &site);
                        if let Some(parent) = self.ref_target(&sql[name.clone()], &pin) {
                            parents.push(parent.clone());
                        }
                    }
                    // A source referenced but not declared in schema.yml dangles
                    // here and is pruned later — it never becomes a node.
                    CallKind::Source { source, table } => {
                        parents.push(format!(
                            "source.{}.{}.{}",
                            self.proj, &sql[source], &sql[table]
                        ));
                    }
                    CallKind::Config => {}
                }
            }
            for call in jinja_calls(sql) {
                match self.macros.classify_callee(call.name, &self.proj) {
                    // `ref`/`source`/`config` belong to the call-site pass
                    // above; dbt/Jinja builtins carry no deps.
                    Callee::DepCall | Callee::Builtin => {}
                    Callee::Project(def) => {
                        // `proj.name(…)` qualifies back to the bare macro —
                        // `depends_on.macros` records the bare name's uid.
                        let bare = call
                            .name
                            .rsplit_once('.')
                            .map(|(_, n)| n)
                            .unwrap_or(call.name);
                        let res = self.macros.resolve_call(bare, def, call.args, &self.proj);
                        self.absorb_resolution(
                            uid,
                            res,
                            call.args,
                            &mut parents,
                            &mut invoked,
                            &mut extras.unresolved,
                        );
                    }
                    Callee::Dispatch => {
                        let res = self.macros.resolve_dispatch(call.args, &self.proj);
                        self.absorb_resolution(
                            uid,
                            res,
                            call.args,
                            &mut parents,
                            &mut invoked,
                            &mut extras.unresolved,
                        );
                    }
                    // A package `ns.name(…)` or an unknown bare call — the
                    // callee's `ref()`/`source()` reach can't be proven
                    // (`dbt_packages/` is never read), so the call counts
                    // as unprovable adjacency and its literal arguments are
                    // kept as source-table candidates.
                    Callee::Foreign => {
                        *extras.unresolved.parents.entry(uid.clone()).or_default() += 1;
                        extras
                            .unresolved
                            .candidates
                            .extend(Macros::literal_args_of(call.args));
                    }
                }
            }
            if !invoked.is_empty() {
                extras
                    .node_macros
                    .insert(uid.clone(), invoked.into_iter().collect());
            }
            parents.sort();
            parents.dedup();
            for parent in &parents {
                child_map
                    .entry(parent.clone())
                    .or_default()
                    .push(uid.clone());
            }
            if !parents.is_empty() {
                parent_map.insert(uid.clone(), parents);
            }
        }
        // The `.py` models' `dbt.ref`/`dbt.source` calls — resolved like
        // the SQL pass: a literal call names its target (an unresolvable
        // name drops like an unresolved SQL ref), and a call that can't
        // literal-eval — a variable, an f-string arg, a `*`/`**` spread,
        // an unbalanced shape — fabricates no edge but counts into the
        // unprovable-adjacency record (its string literals stay
        // source-table candidates), the same stance the macro path takes.
        for d in &self.node_python {
            let mut parents: Vec<String> = Vec::new();
            if d.jinja {
                *extras.unresolved.parents.entry(d.uid.clone()).or_default() += 1;
            }
            for call in &d.calls {
                match call.kind {
                    PyCallKind::Config => {}
                    PyCallKind::Ref => match &call.args {
                        Some(args) if args.is_literal() => {
                            match ref_name(args) {
                                Some(name) => {
                                    // A `v=`/`version=` kwarg pins like
                                    // the SQL spelling — a literal
                                    // version names the member; a
                                    // declared-but-unreadable one is the
                                    // dynamic pin (the family's latest).
                                    let pin = match args
                                        .value_of("v")
                                        .or_else(|| args.value_of("version"))
                                    {
                                        Some(Some(v)) => scalar_to_string(&v)
                                            .map(RefPin::Literal)
                                            .unwrap_or(RefPin::Dynamic),
                                        Some(None) => RefPin::Dynamic,
                                        None => RefPin::Absent,
                                    };
                                    if let Some(parent) = self.ref_target(name, &pin) {
                                        parents.push(parent.clone());
                                    }
                                }
                                // A literal call dbt itself would reject
                                // (>2 positionals, a non-string name).
                                None => {
                                    *extras.unresolved.parents.entry(d.uid.clone()).or_default() +=
                                        1;
                                }
                            }
                        }
                        Some(args) => {
                            *extras.unresolved.parents.entry(d.uid.clone()).or_default() += 1;
                            extras
                                .unresolved
                                .candidates
                                .extend(args.literals.iter().cloned());
                        }
                        None => {
                            *extras.unresolved.parents.entry(d.uid.clone()).or_default() += 1;
                        }
                    },
                    PyCallKind::Source => match &call.args {
                        Some(args) if args.is_literal() => match source_name(args) {
                            Some((src, tbl)) => {
                                parents.push(format!("source.{}.{}.{}", self.proj, src, tbl));
                            }
                            None => {
                                *extras.unresolved.parents.entry(d.uid.clone()).or_default() += 1;
                            }
                        },
                        Some(args) => {
                            *extras.unresolved.parents.entry(d.uid.clone()).or_default() += 1;
                            extras
                                .unresolved
                                .candidates
                                .extend(args.literals.iter().cloned());
                        }
                        None => {
                            *extras.unresolved.parents.entry(d.uid.clone()).or_default() += 1;
                        }
                    },
                }
            }
            if !parents.is_empty() {
                parents.sort();
                parents.dedup();
                for parent in &parents {
                    child_map
                        .entry(parent.clone())
                        .or_default()
                        .push(d.uid.clone());
                }
                parent_map.insert(d.uid.clone(), parents);
            }
        }
        for children in child_map.values_mut() {
            children.sort();
            children.dedup();
        }
        ResolvedEdges {
            parent_map,
            child_map,
            extras,
        }
    }

    /// Fold one macro-resolution outcome into the edge pass's running
    /// state: resolved `ref`/`source` targets join `parents` (an
    /// unresolvable name drops like an ordinary unresolved `ref`), the
    /// invoked macro names land in `invoked`, and an `incomplete`
    /// resolution counts as unprovable adjacency — its literal args stay
    /// candidates for the source tables a hidden `source()` may have read.
    fn absorb_resolution(
        &self,
        uid: &str,
        res: Resolution,
        args_text: &str,
        parents: &mut Vec<String>,
        invoked: &mut BTreeSet<String>,
        unresolved: &mut UnresolvedCalls,
    ) {
        for (name, pin) in &res.refs {
            if let Some(parent) = self.ref_target(name, pin) {
                parents.push(parent.clone());
            }
        }
        for (src, tbl) in &res.sources {
            parents.push(format!("source.{}.{}.{}", self.proj, src, tbl));
        }
        invoked.extend(
            res.macros
                .iter()
                .map(|m| format!("macro.{}.{}", self.proj, m)),
        );
        if res.incomplete {
            *unresolved.parents.entry(uid.to_string()).or_default() += 1;
            unresolved
                .candidates
                .extend(Macros::literal_args_of(args_text));
        }
    }

    /// The schema-metadata pass: source nodes, exposures, semantic-model
    /// dependents, and metadata/tests for every resource — all from the
    /// parsed schema files. The test-synthesis context borrows `name_index`
    /// shared — built only now that every resource discovery pass above has
    /// finished mutating it.
    pub(super) fn apply_schema_files(&mut self, parsed: &[ParsedSchemaFile]) {
        let mut test_ctx = TestCtx {
            seq: 0,
            proj: &self.proj,
            pat: &self.pat,
            name_index: &self.name_index,
            versioned: &self.versioned,
            vars: &self.vars,
        };
        let mut applied_families: HashSet<String> = HashSet::new();
        for (schema, file, yml_root) in parsed {
            // dbt records a source's `path`/`original_file_path` as the declaring
            // schema file (project-root-relative) — match it so the detail modal
            // and the `$EDITOR` jump work identically in both modes.
            let yml_path = rel_path(&self.dir, file);
            for src in &schema.sources {
                for tbl in &src.tables {
                    // `config.enabled` is the ONLY disable channel for source
                    // tables (a table's top-level `enabled:` is ignored by
                    // dbt; the source's is a parse error). The chain is the
                    // usual precedence: the table's `config.enabled` beats the
                    // source's — `enabled: true` under a disabled source
                    // re-enables the table (verified). A `false` outcome
                    // parks the table under `disabled` like dbt, tests and
                    // all; an unprovable one keeps it and marks it — the
                    // table may not exist under real dbt.
                    let schema = Enabled::from_schema(self.vars.schema_enabled_state(
                        tbl.config.enabled.as_ref().or(src.config.enabled.as_ref()),
                        &format!(
                            "{yml_path}: source table '{}.{}' config.enabled",
                            src.name, tbl.name
                        ),
                        &mut self.warnings,
                    ));
                    if schema == Some(Enabled::Off) {
                        self.disabled_sources
                            .insert((src.name.clone(), tbl.name.clone()));
                        continue;
                    }
                    let uid = format!("source.{}.{}.{}", self.proj, src.name, tbl.name);
                    // The `sources:` tree keys by the source fqn: the project
                    // name, the declaring yml's directories relative to the
                    // resource root, then `<source>: <table>` — e.g.
                    // `sample: staging: raw: orders` (verified against real
                    // dbt's recorded fqn). `tree_segments` includes the file
                    // stem, which the fqn replaces with source+table.
                    let mut segments = tree_segments(&self.proj, &rel_path(yml_root, file));
                    segments.pop();
                    segments.push(src.name.clone());
                    segments.push(tbl.name.clone());
                    let tree = resolve_source_tree_config(
                        &self.project.sources,
                        &segments,
                        "sources".to_string(),
                        &mut self.warnings,
                        Some(&self.vars),
                    );
                    // `sources:` `+enabled` parks the table like the other
                    // resource trees (dbt disables `source.p.s.t` by fqn) —
                    // the schema verdict above outranks it; the tree fills
                    // its absence only.
                    match schema.or(tree.enabled_verdict()) {
                        Some(Enabled::Off) => {
                            self.disabled_sources
                                .insert((src.name.clone(), tbl.name.clone()));
                            continue;
                        }
                        Some(Enabled::Unproven) => {
                            self.enabled_unverifiable.insert(uid.clone());
                        }
                        _ => {}
                    }
                    self.sources.insert(
                        uid.clone(),
                        RawSource {
                            name: tbl.name.clone(),
                            resource_type: "source".to_string(),
                            path: Some(yml_path.clone()),
                            original_file_path: Some(yml_path.clone()),
                            description: tbl
                                .description
                                .clone()
                                .map(|d| self.docs.render(&d, &mut self.warnings)),
                            columns: columns_json(&tbl.columns, &self.docs, &mut self.warnings),
                            // A source with no `schema:` resolves to the SOURCE
                            // NAME — dbt's default (verified on 1.12.6), not null:
                            // two tables under schema-less sources of the same
                            // name are the same relation, which `duplicate_sources`
                            // can only see once the default is recorded.
                            schema: src.schema.clone().or_else(|| Some(src.name.clone())),
                            database: src.database.clone(),
                            // schema.yml's `identifier:` — the source table's real
                            // warehouse name when it differs from `name`.
                            identifier: tbl.identifier.clone(),
                            // Source mode synthesizes a manifest for the local
                            // project only — no foreign package can appear, so
                            // package identity is vacuous here.
                            package_name: None,
                            // meta merge: `sources:` tree `+meta` < source
                            // `meta:` < source `config.meta` < table `meta:` <
                            // table `config.meta` — each overrides the
                            // previous per key (dbt's shallow merge, verified).
                            meta: source_meta(tree.meta, src, tbl, &mut self.warnings),
                            // Source-level only: dbt cascades it to every table
                            // and ignores table-level/`+loader` declarations
                            // (verified on 1.12.6).
                            loader: src.loader.clone(),
                            // `loaded_at_field` resolves props-before-config
                            // per level (the reverse of meta/freshness):
                            // table top-level > table `config:` > source
                            // top-level > source `config:`; an explicit
                            // `null` at the table level clears the inherited
                            // value (`+loaded_at_field` is one dbt ignores —
                            // deliberately not resolved).
                            loaded_at_field: source_loaded_at_field(src, tbl),
                            // `freshness:` merges PER THRESHOLD down table
                            // `config:` > table > source `config:` > source >
                            // `+freshness` — a table's `error_after` keeps the
                            // inherited `warn_after` (verified); `freshness:
                            // null` clears it all.
                            freshness: effective_freshness(tbl, src, &tree.freshness),
                            // `tags` accumulate — config channel (source then
                            // table) then property channel (TABLE then
                            // source — verified: the table's own `tags:`
                            // precede the inherited source's on the wire).
                            tags: source_tags(src, tbl),
                        },
                    );
                    synth_tests(
                        &mut self.nodes,
                        &mut test_ctx,
                        &uid,
                        &tbl.name,
                        None,
                        &tbl.tests,
                        &yml_path,
                        &mut self.warnings,
                    );
                    synth_tests(
                        &mut self.nodes,
                        &mut test_ctx,
                        &uid,
                        &tbl.name,
                        None,
                        &tbl.data_tests,
                        &yml_path,
                        &mut self.warnings,
                    );
                    for col in &tbl.columns {
                        synth_tests(
                            &mut self.nodes,
                            &mut test_ctx,
                            &uid,
                            &tbl.name,
                            Some(&col.name),
                            &col.tests,
                            &yml_path,
                            &mut self.warnings,
                        );
                        synth_tests(
                            &mut self.nodes,
                            &mut test_ctx,
                            &uid,
                            &tbl.name,
                            Some(&col.name),
                            &col.data_tests,
                            &yml_path,
                            &mut self.warnings,
                        );
                    }
                }
            }

            // Exposures: pure leaf declarations. Their `depends_on` entries are
            // Jinja strings (`ref('x')` / `source('a','b')`), so the SAME extractors
            // the .sql scan uses resolve them — an unresolved ref drops out, exactly
            // like a model's. dbt records `path` resource-root-relative (like a
            // model's) and `original_file_path` project-root-relative.
            for exp in &schema.exposures {
                // `config: enabled:` parks the exposure like dbt's
                // `disabled`; an unprovable one keeps it and marks it.
                let schema = Enabled::from_schema(self.vars.schema_enabled_state(
                    exp.config.enabled.as_ref(),
                    &format!("{yml_path}: exposure '{}' config.enabled", exp.name),
                    &mut self.warnings,
                ));
                if schema == Some(Enabled::Off) {
                    continue;
                }
                let uid = format!("exposure.{}.{}", self.proj, exp.name);
                let mut deps: Vec<String> = Vec::new();
                for dep in &exp.depends_on {
                    deps.extend(self.ref_targets_in(dep));
                    for (src, tbl) in self.pat.sources(dep) {
                        deps.push(format!("source.{}.{src}.{tbl}", self.proj));
                    }
                }
                deps.sort();
                deps.dedup();
                // The `exposures:` tree keys by the exposure fqn: the
                // project name, the declaring yml's directories relative
                // to the resource root, then the name — e.g.
                // `sample: marts: churn_notebook` (the fqn a compiled
                // manifest records). The file stem drops out, like the
                // `sources:` walk.
                let mut segments = tree_segments(&self.proj, &rel_path(yml_root, file));
                segments.pop();
                segments.push(exp.name.clone());
                let tree = resolve_tree_config(
                    &self.project.exposures,
                    &segments,
                    "exposures".to_string(),
                    &mut self.warnings,
                    Some(&self.vars),
                );
                // `exposures:` `+enabled` parks the exposure like the other
                // resource trees (dbt disables `exposure.p.e` by fqn) —
                // the schema verdict outranks it; the tree fills its
                // absence only.
                match schema.or(tree.enabled_verdict()) {
                    Some(Enabled::Off) => continue,
                    Some(Enabled::Unproven) => {
                        self.enabled_unverifiable.insert(uid.clone());
                    }
                    _ => {}
                }
                // `exposures:` tree `+meta` < `meta:` < `config: meta:` —
                // each overrides the previous per top-level key, the same
                // precedence a model entry's meta follows (verified: real
                // dbt merges `exposures:` tree `+meta` at the bottom and
                // records the whole merged map at the exposure's top
                // level).
                let what = format!("exposure '{}'", exp.name);
                let mut meta = tree.meta;
                meta.extend(meta_map(&exp.meta, &mut self.warnings, &what));
                meta.extend(meta_map(
                    &exp.config.meta,
                    &mut self.warnings,
                    &format!("{what} config"),
                ));
                // `tags` — a `config.tags` DECLARED on the entry replaces
                // the tree's `+tags` outright (verified on 1.12.6: even
                // `config: {tags: []}` suppresses it) and the wire list is
                // `config.tags ++ tags:`; undeclared, it's `tags: ++ +tags`.
                // Either way the wire sorts + dedups the union (verified —
                // exposures order tags alphabetically, unlike model nodes).
                let mut tags: Vec<String> = match &exp.config.tags {
                    Some(cfg) => cfg.iter().chain(exp.tags.iter()).cloned().collect(),
                    None => exp.tags.iter().chain(tree.tags.iter()).cloned().collect(),
                };
                tags.sort();
                tags.dedup();
                // `group` lives only in `config` on the wire — the entry's
                // own `group:` property never lands (a top-level `group:`
                // is a dbt parse error, surfaced here as a warning like
                // every other recoverable oddity), and the tree's `+group`
                // fills only what `config.group` leaves undeclared
                // (verified on 1.12.6).
                if exp.group.is_some() {
                    self.warnings.push(format!(
                        "{what}: an exposure's top-level 'group:' is not a dbt property (a parse error in dbt); it is ignored — use config.group or a groups entry"
                    ));
                }
                let group = exp.config.group.clone().or(tree.group);
                self.exposures.insert(
                    uid,
                    RawExposure {
                        name: exp.name.clone(),
                        exposure_type: exp.exposure_type.clone(),
                        owner: RawExposureOwner {
                            name: exp.owner.name.clone(),
                            email: exp.owner.email.clone(),
                        },
                        url: exp.url.clone(),
                        description: exp
                            .description
                            .clone()
                            .map(|d| self.docs.render(&d, &mut self.warnings)),
                        path: Some(rel_path(yml_root, file)),
                        original_file_path: Some(yml_path.clone()),
                        depends_on: RawDependsOn {
                            nodes: deps,
                            macros: vec![],
                        },
                        meta,
                        config: RawExposureConfig {
                            tags: tags.clone(),
                            group,
                        },
                        tags,
                    },
                );
            }

            // Semantic models: a YAML-only consumer like the exposures above —
            // but no node is synthesized for it (a real manifest would carry
            // `semantic_models` records + edges; the source loader leaves those
            // maps empty). Instead each entry's `model: ref('x')` stamps the
            // consumed node's `semantic_dependents`, the `#[serde(skip)]`
            // channel `detail_from_node` copies into `NodeDetail` — so "the
            // semantic layer reads this model" answers identically in both
            // modes (`unused_models` depends on it).
            for sm in &schema.semantic_models {
                // `enabled:` — either spelling — parks the entry like dbt's
                // `disabled` (it never consumes its `model:` then); the
                // `config:` spelling wins when both are declared.
                if self.vars.schema_disabled(
                    sm.config.enabled.as_ref().or(sm.enabled.as_ref()),
                    &format!("{yml_path}: semantic model '{}' enabled", sm.name),
                    &mut self.warnings,
                ) {
                    continue;
                }
                let Some(model) = &sm.model else {
                    continue;
                };
                for uid in self.ref_targets_in(model) {
                    if let Some(node) = self.nodes.get_mut(&uid) {
                        node.semantic_dependents.push(sm.name.clone());
                    }
                }
            }

            for entry in &schema.models {
                // A `versions:`-declared family expands per member — its
                // enabled verdict already folded at the claim stage —
                // while an ordinary entry attaches to its one node. The
                // first declaration alone expands (a redeclaration was
                // already warned and skipped at bind).
                if entry.versions.is_empty() {
                    apply_meta(
                        &mut self.nodes,
                        &mut test_ctx,
                        "model",
                        std::slice::from_ref(entry),
                        &yml_path,
                        &self.docs,
                        &mut self.disabled_names,
                        &self.in_file_enabled,
                        &self.tree_gov,
                        &mut self.enabled_unverifiable,
                        &mut self.yml_tags,
                        &mut self.warnings,
                    );
                } else if let Some(family) = self.versioned.get(&entry.name) {
                    // The first declaration alone expands (a
                    // redeclaration was already warned and skipped at
                    // bind).
                    if !applied_families.insert(entry.name.clone()) {
                        continue;
                    }
                    apply_versioned_meta(
                        &mut self.nodes,
                        &mut test_ctx,
                        entry,
                        family,
                        &yml_path,
                        &self.docs,
                        &mut self.yml_tags,
                        &mut self.warnings,
                    );
                }
            }
            apply_meta(
                &mut self.nodes,
                &mut test_ctx,
                "seed",
                &schema.seeds,
                &yml_path,
                &self.docs,
                &mut self.disabled_names,
                &self.in_file_enabled,
                &self.tree_gov,
                &mut self.enabled_unverifiable,
                &mut self.yml_tags,
                &mut self.warnings,
            );
            apply_meta(
                &mut self.nodes,
                &mut test_ctx,
                "snapshot",
                &schema.snapshots,
                &yml_path,
                &self.docs,
                &mut self.disabled_names,
                &self.in_file_enabled,
                &self.tree_gov,
                &mut self.enabled_unverifiable,
                &mut self.yml_tags,
                &mut self.warnings,
            );

            // `unit_tests:` — dbt ≥1.8 unit tests, synthesized into the
            // `RawManifest.unit_tests` map a real parse emits so
            // `Dag::build` attaches them to their `model:` targets
            // through the one intake path.
            synth_unit_tests(
                &mut self.unit_tests,
                &test_ctx,
                &schema.unit_tests,
                &yml_path,
                &mut self.warnings,
            );
        }
    }

    /// Fill remaining model materializations from the dbt_project.yml tree, then
    /// dbt's default `view` (the tree is applied AFTER schema.yml so the full
    /// precedence chain is in-file > schema.yml > tree > default, like dbt).
    /// The tree's `+meta`/`+unique_key`/governance keys are likewise the
    /// LOWEST precedence, filling only keys not already set by schema.yml or
    /// in-file `config()`. `semantic_dependents` are sorted/deduped here once
    /// every schema file has stamped them.
    pub(super) fn apply_tree_fallback(&mut self) {
        for (uid, node) in self.nodes.iter_mut() {
            if node.resource_type == "model" && node.config.materialized.is_none() {
                node.config.materialized = if node.language.as_deref() == Some("python") {
                    // A python model's default is `table`, and the
                    // project tree's `+materialized` never reaches it
                    // (verified: a `+materialized: view` tree still
                    // yields `table` on a config-less `.py`). Only
                    // schema.yml `config: materialized:` sits above it.
                    Some("table".to_string())
                } else {
                    self.tree_mat
                        .get(uid)
                        .cloned()
                        .or_else(|| Some("view".to_string()))
                };
            }
            if let Some(tree) = self.tree_meta.get(uid) {
                for (k, v) in tree {
                    node.meta.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            // `+unique_key` sits under in-file `config(unique_key=…)` and
            // schema.yml `config: unique_key:` — the whole declared value (a
            // bare string or a string list; `Dag::build` normalizes either).
            if node.config.unique_key.is_none() {
                node.config.unique_key = self.tree_uk.get(uid).cloned();
            }
            // The governance keys — the tree is again the lowest rung, filling
            // only what in-file `config()` and schema.yml left unset.
            // `group` is resource-agnostic; `access`/`contract`/
            // `incremental_strategy` are model-only in dbt. An undeclared
            // model's `access` lands on dbt's `protected` default (verified:
            // every undeclared model records it on the wire); the other fields
            // simply stay absent.
            let gov = self.tree_gov.get(uid);
            if node.group.is_none() {
                node.group = gov.and_then(|g| g.group.clone());
            }
            if node.resource_type == "model" {
                if node.access.is_none() {
                    node.access = gov
                        .and_then(|g| g.access.clone())
                        .or_else(|| Some("protected".to_string()));
                }
                if node.contract.is_none() {
                    node.contract = gov.and_then(|g| g.contract.clone());
                }
                if node.config.incremental_strategy.is_none() {
                    node.config.incremental_strategy =
                        gov.and_then(|g| g.incremental_strategy.clone());
                }
            }
            // `tags` — the three channels deduplicated in the order dbt's
            // node list keeps (verified on 1.12.6): the project `+tags`
            // tree (outer→inner — `push_unique` covers dbt deduping a tag
            // declared on two nested rungs or twice in one `+tags:` list),
            // the file's own `config(tags=…)`, then the schema entry's
            // `config.tags:`. The wire `config.tags` holds the same
            // channels in a DIFFERENT order (tree → yml → in-file,
            // duplicates kept) — nothing downstream reads it, so only
            // this list is synthesized.
            let mut tags: Vec<String> = Vec::new();
            push_unique(&mut tags, gov.map(|g| g.tags.as_slice()).unwrap_or(&[]));
            push_unique(
                &mut tags,
                self.infile_tags.get(uid).map_or(&[][..], Vec::as_slice),
            );
            push_unique(
                &mut tags,
                self.yml_tags.get(uid).map_or(&[][..], Vec::as_slice),
            );
            node.tags = tags;
            // Deterministic `semantic_dependents` — stamped per schema file,
            // so sort/dedup once here (a name can't legitimately repeat, but a
            // duplicate `semantic_models:` entry across files must not double).
            node.semantic_dependents.sort();
            node.semantic_dependents.dedup();
        }
    }

    /// The `{% docs __overview__ %}` block rides into the manifest's `docs`
    /// map in exactly the shape a real manifest carries
    /// (`doc.<proj>.__overview__`), so `Dag::build` picks the overview up
    /// through ONE intake path for both load modes. `DocBlocks::render`
    /// expands `doc()` refs and strips `{# #}` comments — what dbt writes to
    /// `block_contents`. Other docs blocks are not ingested (they already
    /// expanded where referenced; nothing else consumes them).
    pub(super) fn manifest_docs(&mut self) -> HashMap<String, RawDoc> {
        let mut manifest_docs = HashMap::new();
        if let Some(body) = self.docs.blocks.get("__overview__") {
            manifest_docs.insert(
                format!("doc.{}.__overview__", self.proj),
                RawDoc {
                    name: "__overview__".to_string(),
                    package_name: Some(self.proj.clone()),
                    block_contents: self.docs.render(body, &mut self.warnings),
                },
            );
        }
        manifest_docs
    }

    /// Assemble the synthesized `RawManifest` and hand back the deduplicated
    /// warnings — identical warnings collapse (a misdeclared `+meta` level
    /// every node descends through would otherwise warn once PER NODE; the
    /// message is the same string, so first-seen order survives dedup).
    pub(super) fn finish(
        mut self,
        parent_map: HashMap<String, Vec<String>>,
        child_map: HashMap<String, Vec<String>>,
        manifest_docs: HashMap<String, RawDoc>,
        extras: EdgeExtras,
    ) -> (RawManifest, Vec<String>) {
        let mut seen_warnings = HashSet::new();
        self.warnings.retain(|w| seen_warnings.insert(w.clone()));

        // Each node's invoked-macro unique_ids — `depends_on.macros`, the
        // field a real manifest carries for a model calling a macro.
        // `depends_on.nodes` stays empty (as before): `parent_map` is the
        // synthesized manifest's adjacency source.
        for (uid, macros) in extras.node_macros {
            if let Some(node) = self.nodes.get_mut(&uid) {
                node.depends_on.macros = macros;
            }
        }

        // The unprovable-adjacency record. Two directions fold into the
        // same map the `Dag` calls `pruned_adjacent`: a counted call on a
        // node means it may have parents the graph doesn't show, and each
        // literal argument those calls carried names a source table that
        // may have a consumer the graph doesn't show. Only NOW (post
        // `apply_schema_files`) do declared sources exist to match the
        // candidates against.
        let mut unresolved_adjacent: HashMap<String, UnresolvedAdjacent> = HashMap::new();
        for (uid, n) in extras.unresolved.parents {
            unresolved_adjacent.entry(uid).or_default().parents += n;
        }
        for table in &extras.unresolved.candidates {
            for (uid, src) in &self.sources {
                if &src.name == table {
                    unresolved_adjacent.entry(uid.clone()).or_default().children += 1;
                }
            }
        }

        // The `enabled`-unverifiable marks, kept only for uids that
        // actually reached the manifest (a disabled or duplicate-dropped
        // resource's mark goes away with it).
        let mut enabled_unverifiable = self.enabled_unverifiable;
        enabled_unverifiable.retain(|uid| {
            self.nodes.contains_key(uid)
                || self.sources.contains_key(uid)
                || self.exposures.contains_key(uid)
        });

        let raw_macros = self.macros.raw_macros(&self.proj);

        // The ref()/source() name universe `unresolved_references` judges
        // against — carried on the manifest the way dbt's own parse would
        // remember it (manifest mode derives the same scope from its
        // node/source/disabled maps in `Dag::build`). Root names come from
        // the loader's own indexes; installed `dbt_packages/` dirs
        // contribute their file-stem/yml names without ever being parsed;
        // `dependencies.yml`/`packages.yml` mark the cross-project
        // (`projects:`) and still-missing (`packages:` vs installed dirs)
        // surfaces. `versions` records the synthesized `.<v>` member uids
        // — the same shape a real manifest's scope build reads — so a
        // `v=`-pinned ref judges here exactly like manifest mode.
        let deps = DeclaredDeps::load(&self.dir, &mut self.warnings);
        let installed =
            InstalledPackages::load(&self.dir, &self.project, &self.pat, &mut self.warnings);
        let mut refs = installed.refs;
        for (name, uid) in &self.name_index {
            // A parked (`disabled`) name stays in `name_index` — edges and
            // test `to:` still resolve to the dropped uid — but it is NOT
            // a live `ref()` target: it judges as "disabled" against
            // `disabled_refs` below, never as resolved here. The parked
            // record is (version, kind)-scoped: a parked VERSION of a live
            // family doesn't hide the bare name — `ref('fam')` still
            // resolves to the family's live members.
            let (kind, version) = match versioned_model_segs(uid) {
                Some((_, vseg)) => ("model", Some(normalize_version(vseg).to_string())),
                None => (uid.split('.').next().unwrap_or("model"), None),
            };
            if self.disabled_names.get(name).is_some_and(|set| {
                set.iter()
                    .any(|(v, k)| v.as_deref() == version.as_deref() && k == kind)
            }) {
                continue;
            }
            refs.entry(name.clone())
                .or_default()
                .insert(self.proj.clone());
        }
        // A versioned family keeps answering `ref('fam', v=N)` on its
        // live members — `name_index` only names the latest.
        for (name, family) in &self.versioned {
            if family.versions.values().any(Option::is_some) {
                refs.entry(name.clone())
                    .or_default()
                    .insert(self.proj.clone());
            }
        }
        let mut sources = installed.sources;
        for uid in self.sources.keys() {
            let mut segs = uid.split('.');
            if let (Some("source"), Some(_), Some(src), Some(tbl), None) = (
                segs.next(),
                segs.next(),
                segs.next(),
                segs.next(),
                segs.next(),
            ) {
                sources.insert((src.to_string(), tbl.to_string()));
            }
        }
        let ref_scope = RefScope {
            refs,
            versions: Some(
                self.nodes
                    .keys()
                    .filter_map(|u| version_scope_key(u))
                    .collect(),
            ),
            sources,
            disabled_refs: self
                .disabled_names
                .iter()
                .map(|(n, records)| {
                    (
                        n.clone(),
                        records
                            .iter()
                            .map(|(version, kind)| DisabledRef {
                                package: self.proj.clone(),
                                kind: kind.clone(),
                                version: version.clone(),
                            })
                            .collect(),
                    )
                })
                .collect(),
            disabled_sources: self.disabled_sources,
            assumed: self.assumed_names,
            projects: deps.projects,
            uninstalled_packages: deps.declared.saturating_sub(installed.installed),
        };

        (
            RawManifest {
                // `dbt_project.yml`'s `name` IS the root project — record it where
                // real dbt does so `Dag::project_name` resolves identically in
                // both load modes (the consistency suite asserts that).
                metadata: RawMetadata {
                    project_name: Some(self.proj),
                    // Source mode deliberately does NOT read profiles.yml, so the
                    // adapter is unknown here — CLL falls back to its default
                    // dialect (`--sql-dialect` is the override knob).
                    adapter_type: None,
                },
                nodes: self.nodes,
                sources: self.sources,
                exposures: self.exposures,
                parent_map,
                child_map,
                unresolved_adjacent,
                enabled_unverifiable,
                // The project's own `{% macro %}` table — real manifests
                // carry these (and the diff walks their depends_on for
                // shared-macro impact), so the synthesized manifest fills
                // the same shape instead of leaving the map absent.
                macros: raw_macros.into_iter().collect(),
                // Source mode never synthesizes semantic-layer resources —
                // they're YAML-only resources the source parser doesn't
                // scan. `semantic_models:` entries ARE scanned, but only to mark
                // each consumed node's `semantic_dependents` (above) — the same
                // fact `Dag::build` derives from this map's records in manifest
                // mode, so neither side needs the nodes themselves.
                semantic_models: HashMap::new(),
                metrics: HashMap::new(),
                saved_queries: HashMap::new(),
                // The `unit_tests:` declarations synthesized in
                // `apply_schema_files` — the same map a real manifest
                // carries, so `Dag::build` attaches each test through
                // the one intake path.
                unit_tests: self.unit_tests,
                functions: HashMap::new(),
                docs: manifest_docs,
                // Parked names live on the `RefScope` (below), not the
                // `disabled` map — a real manifest's parked payloads are
                // never read either.
                disabled: HashMap::new(),
                ref_scope: Some(ref_scope),
            },
            self.warnings,
        )
    }
}

/// The `ref()` target under a [`RefPin`] over an explicit table pair —
/// [`Synthesis::ref_target`]'s shared core, which the schema-file test
/// pass (`to:` deps) calls with its own view of the same tables: an
/// absent pin asks `name_index` (which carries each family's latest
/// under the bare name); a pin asks the family's version table — and a
/// pinned name that isn't versioned resolves to nothing, like dbt's
/// "unversioned model got a version arg" error.
pub(super) fn ref_target_of<'a>(
    name: &str,
    pin: &RefPin,
    name_index: &'a HashMap<String, String>,
    versioned: &'a BTreeMap<String, VersionedFamily>,
) -> Option<&'a String> {
    match pin {
        RefPin::Absent => name_index.get(name),
        RefPin::Literal(v) => versioned
            .get(name)?
            .versions
            .get(v)
            .and_then(Option::as_ref),
        RefPin::Dynamic => versioned
            .get(name)
            .and_then(|f| f.versions.get(&f.latest))
            .and_then(Option::as_ref),
    }
}

/// dbt's `latest_version` default — the pairwise max over the declared
/// `v:` values: two numbers compare as floats, anything else compares
/// as rendered text (so `'beta'` outranks `2`, and `1.5` outranks `10`
/// only as numbers — the pairwise fold is Python's `max` with this key).
fn version_cmp(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => scalar_to_string(a)
            .unwrap_or_default()
            .cmp(&scalar_to_string(b).unwrap_or_default()),
    }
}

/// A `ref()` call's `v=`/`version=` kwarg as a [`RefPin`] — a literal
/// renders the way dbt writes it (a number's `str()` — `1.10` → `"1.1"`,
/// a string verbatim); a kwarg that isn't a literal is the dynamic pin.
pub(super) fn ref_pin(sql: &str, site: &CallSite) -> RefPin {
    let Some(args) = &site.args else {
        return RefPin::Absent;
    };
    let args = &sql[args.clone()];
    let Some(expr) = args
        .contains('=')
        .then(|| kwarg(args, "v").or_else(|| kwarg(args, "version")))
        .flatten()
    else {
        return RefPin::Absent;
    };
    lit_value(expr)
        .and_then(|v| scalar_to_string(&v))
        .map(RefPin::Literal)
        .unwrap_or(RefPin::Dynamic)
}
