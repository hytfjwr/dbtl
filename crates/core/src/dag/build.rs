use super::*;

impl Dag {
    /// Build the DAG from a parsed manifest.
    ///
    /// Excludes `test` / `operation` / `analysis` / `check` nodes, merges
    /// `nodes` + `sources` into a single map, and prunes `parent_map` / `child_map` down
    /// to the kept set
    /// (both keys and neighbour lists) so traversal never touches an excluded
    /// node as an endpoint or as an intermediate hop.
    ///
    /// Two side maps are also built: `details` (per kept node) and `tests`. The
    /// `tests` map is populated **pre-prune** — while walking the raw nodes,
    /// before any topology is built — purely as a recording keyed by the test's
    /// target node(s); it never touches `parent_map` / `child_map`, so a test is
    /// never a transit hop.
    pub fn build(manifest: &RawManifest) -> Self {
        let mut nodes: HashMap<String, NodeInfo> = HashMap::new();
        let mut details: HashMap<String, NodeDetail> = HashMap::new();
        let mut tests: HashMap<String, Vec<TestInfo>> = HashMap::new();
        let mut sql: HashMap<String, String> = HashMap::new();
        let mut compiled: HashMap<String, CompiledSql> = HashMap::new();
        let mut exposures: HashMap<String, ExposureInfo> = HashMap::new();
        let mut node_macros: HashMap<String, Vec<String>> = HashMap::new();

        for (unique_id, node) in &manifest.nodes {
            if node.resource_type == "test" {
                register_test(&mut tests, node); // capture pre-prune, then skip
                continue;
            }
            if is_excluded(&node.resource_type) {
                continue;
            }
            if !node.depends_on.macros.is_empty() {
                node_macros.insert(unique_id.clone(), node.depends_on.macros.clone());
            }
            nodes.insert(
                unique_id.clone(),
                NodeInfo {
                    unique_id: unique_id.clone(),
                    name: node.name.clone(),
                    resource_type: node.resource_type.clone(),
                    path: node.path.clone(),
                    materialized: node.config.materialized.clone(),
                    ..Default::default() // direct_up/down filled after pruning below
                },
            );
            details.insert(unique_id.clone(), detail_from_node(node));
            if let Some(code) = &node.raw_code {
                sql.insert(unique_id.clone(), code.clone());
            }
            // Manifest-carried compiled SQL: only a compile/build-produced
            // manifest fills `compiled_code` (dbt parse leaves it null).
            // Non-empty only — an empty field falls through to the
            // `target/compiled/` files like an absent one. Never stale: it
            // ships in the same artifact as `raw_code`.
            if let Some(code) = node.compiled_code.as_ref().filter(|c| !c.trim().is_empty()) {
                compiled.insert(
                    unique_id.clone(),
                    CompiledSql {
                        code: code.clone(),
                        stale: false,
                        origin: CompiledOrigin::Manifest,
                    },
                );
            }
        }

        for (unique_id, source) in &manifest.sources {
            // Sources are never test/operation, but guard for completeness.
            if is_excluded(&source.resource_type) {
                continue;
            }
            nodes.insert(
                unique_id.clone(),
                NodeInfo {
                    unique_id: unique_id.clone(),
                    name: source.name.clone(),
                    resource_type: source.resource_type.clone(),
                    path: source.path.clone(),
                    materialized: None,
                    ..Default::default() // direct_up/down filled after pruning below
                },
            );
            details.insert(unique_id.clone(), detail_from_source(source));
        }

        for (unique_id, exposure) in &manifest.exposures {
            nodes.insert(
                unique_id.clone(),
                NodeInfo {
                    unique_id: unique_id.clone(),
                    name: exposure.name.clone(),
                    resource_type: "exposure".to_string(),
                    path: exposure.path.clone(),
                    materialized: None,
                    ..Default::default() // direct_up/down filled after pruning below
                },
            );
            details.insert(unique_id.clone(), detail_from_exposure(exposure));
            exposures.insert(
                unique_id.clone(),
                ExposureInfo {
                    exposure_type: exposure.exposure_type.clone(),
                    owner_name: exposure.owner.name.clone(),
                    owner_email: exposure.owner.email.clone(),
                    url: exposure.url.clone(),
                },
            );
        }

        // Semantic-layer resources (semantic models / metrics / saved queries)
        // — declared downstream consumers, exactly like exposures: terminator
        // nodes fed by their own `depends_on` (metrics chain off semantic
        // models, which is fine — all three maps become nodes).
        for (map, resource_type) in [
            (&manifest.semantic_models, "semantic_model"),
            (&manifest.metrics, "metric"),
            (&manifest.saved_queries, "saved_query"),
        ] {
            for (unique_id, res) in map {
                nodes.insert(
                    unique_id.clone(),
                    NodeInfo {
                        unique_id: unique_id.clone(),
                        name: res.name.clone(),
                        resource_type: resource_type.to_string(),
                        path: res.path.clone(),
                        materialized: None,
                        ..Default::default()
                    },
                );
                details.insert(
                    unique_id.clone(),
                    NodeDetail {
                        description: clean_desc(&res.description),
                        original_file_path: res.original_file_path.clone(),
                        ..Default::default()
                    },
                );
            }
        }

        // dbt ≥1.8 unit tests: excluded from the DAG like schema tests,
        // recorded per-target model. Resolution prefers the manifest's own
        // record: `depends_on.nodes` carries the tested model's
        // `model.<pkg>.<name>[.<v>]` uid FIRST (any `given:` input `ref`s
        // follow) — a version-scoped test (`versions: include:`) names ITS
        // `.vN` there, which a name lookup can't pick (several versions
        // share one name). `ut.version` picks the scoped member, the
        // tested-model name separates it from input refs, and manifest
        // order is the last tie-break — so no `HashMap` order ever leaks
        // into the choice. The bare `unit_test.<pkg>.<model>.<test>` uid
        // shape and the `model:` name are the fallbacks — every fallback
        // is uid-sorted for the same determinism.
        for (uid, ut) in &manifest.unit_tests {
            let version = ut.version.as_deref();
            // The tested model's bare name — the uid shape's third
            // segment first, the manifest's own `model:` field as backup.
            let wanted_name = uid
                .split('.')
                .nth(2)
                .filter(|s| !s.is_empty())
                .or(ut.model.as_deref());
            let deps: Vec<&str> = ut
                .depends_on
                .nodes
                .iter()
                .map(String::as_str)
                .filter(|d| nodes.get(*d).is_some_and(|n| n.resource_type == "model"))
                .collect();
            let target = if !deps.is_empty() {
                deps.iter()
                    .find(|d| version.is_some_and(|v| uid_has_version(d, v)))
                    .or_else(|| {
                        wanted_name
                            .and_then(|w| deps.iter().find(|d| d.split('.').nth(2) == Some(w)))
                    })
                    .or_else(|| deps.first())
                    .map(|d| (*d).to_string())
            } else {
                match (uid_package(uid), wanted_name) {
                    (Some(pkg), Some(model)) if !pkg.is_empty() => {
                        // The same-package members of this model's family —
                        // sorted so the pick is deterministic; the version
                        // filter prefers the scoped one, the smallest uid
                        // is the last resort.
                        let mut cands: Vec<&str> = nodes
                            .values()
                            .filter(|n| {
                                n.resource_type == "model"
                                    && n.name == model
                                    && uid_package(&n.unique_id) == Some(pkg)
                            })
                            .map(|n| n.unique_id.as_str())
                            .collect();
                        cands.sort_unstable();
                        cands
                            .iter()
                            .find(|u| version.is_some_and(|v| uid_has_version(u, v)))
                            .or_else(|| cands.first())
                            .map(|u| (*u).to_string())
                            .or_else(|| model_uid_by_name(&nodes, model, version))
                    }
                    _ => ut
                        .model
                        .as_ref()
                        .and_then(|m| model_uid_by_name(&nodes, m, version)),
                }
            };
            if let Some(target) = target {
                tests.entry(target).or_default().push(TestInfo {
                    name: ut.name.clone(),
                    kind: "unit".to_string(),
                    unit: Some(UnitTestShape {
                        inputs: ut.given.len(),
                        // The total is honest only when every `given:` input
                        // carries an inline `rows` list — a partial count would
                        // understate the real input size.
                        given_rows: ut
                            .given
                            .iter()
                            .map(|io| io.rows.as_ref()?.as_array().map(Vec::len))
                            .collect::<Option<Vec<usize>>>()
                            .map(|v| v.into_iter().sum()),
                        expect_rows: ut
                            .expect
                            .as_ref()
                            .and_then(|io| io.rows.as_ref()?.as_array().map(Vec::len)),
                    }),
                    ..Default::default()
                });
            }
        }

        // Deterministic test ordering per node (HashMap iteration is
        // unordered, and `sort_by` is STABLE — a key tie would leak insertion
        // order). The key must cover the displayed payload: two `relationships`
        // tests on the same column synthesize identical names (source mode
        // names tests after the ATTACHED node), so `ref_uid`/`ref_column`
        // break their tie. `ref_name` derives from `ref_uid`, and unit-test
        // names are manifest-unique per node — the key below is total.
        for v in tests.values_mut() {
            v.sort_by(|a, b| {
                a.kind
                    .cmp(&b.kind)
                    .then_with(|| a.column_name.cmp(&b.column_name))
                    .then_with(|| a.name.cmp(&b.name))
                    .then_with(|| a.ref_uid.cmp(&b.ref_uid))
                    .then_with(|| a.ref_column.cmp(&b.ref_column))
            });
        }

        // Relationship targets name-resolve once the `nodes` map is complete
        // (register_test runs DURING it — the target may not have been built
        // yet). `None` stays `None` (a target pruned out of the kept graph —
        // the display then falls back to the raw uid).
        for v in tests.values_mut() {
            for t in v.iter_mut() {
                if t.ref_name.is_none() {
                    if let Some(u) = &t.ref_uid {
                        t.ref_name = nodes.get(u).map(|n| n.name.clone());
                    }
                }
            }
        }

        let kept: HashSet<&String> = nodes.keys().collect();
        let mut parents = prune_adjacency(&manifest.parent_map, &kept);
        let mut children = prune_adjacency(&manifest.child_map, &kept);

        // Exposure edges come from BOTH the manifest's parent/child maps (a real
        // dbt manifest records them there, pruned above) AND each exposure's own
        // `depends_on` (the only adjacency a synthesized source-mode manifest
        // fills), deduplicated — so both modes converge on the same graph.
        // Iterated in sorted uid order so the appended-edge order (and thus the
        // adjacency lists) is deterministic. Exposures only ever gain PARENTS:
        // they are terminator leaves by construction.
        let mut exposure_ids: Vec<&String> = manifest.exposures.keys().collect();
        exposure_ids.sort();
        for uid in exposure_ids {
            for dep in &manifest.exposures[uid].depends_on.nodes {
                if !kept.contains(dep) {
                    continue; // a dangling dep is dropped, like an unresolved ref
                }
                let p = parents.entry(uid.clone()).or_default();
                if !p.contains(dep) {
                    p.push(dep.clone());
                }
                let c = children.entry(dep.clone()).or_default();
                if !c.contains(uid) {
                    c.push(uid.clone());
                }
            }
        }
        // Semantic-layer resources: same terminator-edge rule — real manifests
        // already carry them in parent/child_map, but `depends_on` is the
        // guaranteed adjacency (metrics may even reference semantic models).
        let mut semantic_edges: Vec<(&String, &Vec<String>)> = Vec::new();
        for map in [
            &manifest.semantic_models,
            &manifest.metrics,
            &manifest.saved_queries,
        ] {
            for (uid, res) in map {
                semantic_edges.push((uid, &res.depends_on.nodes));
            }
        }
        semantic_edges.sort_by_key(|(uid, _)| *uid);
        // `semantic_model` consumers also stamp the consumed node's
        // `semantic_dependents` — the manifest-mode half of the channel
        // source mode fills through `RawNode::semantic_dependents` (the
        // source loader synthesizes no semantic-layer nodes). Metric /
        // saved-query consumers don't stamp: they are graph nodes only in
        // manifest mode, and the field exists to answer "a `semantic_models:`
        // entry reads this" identically in both modes.
        let mut semantic_dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (uid, deps) in semantic_edges {
            for dep in deps {
                if !kept.contains(dep) {
                    continue; // a dangling dep is dropped, like an unresolved ref
                }
                let p = parents.entry(uid.clone()).or_default();
                if !p.contains(dep) {
                    p.push(dep.clone());
                }
                let c = children.entry(dep.clone()).or_default();
                if !c.contains(uid) {
                    c.push(uid.clone());
                }
                if nodes[uid].resource_type == "semantic_model" {
                    semantic_dependents
                        .entry(dep.clone())
                        .or_default()
                        .insert(nodes[uid].name.clone());
                }
            }
        }
        for (uid, names) in semantic_dependents {
            if let Some(d) = details.get_mut(&uid) {
                d.semantic_dependents.extend(names);
                d.semantic_dependents.sort();
                d.semantic_dependents.dedup();
            }
        }

        // Stamp each node's direct (1-hop) parent/child counts from the pruned
        // adjacency (list-pane dependency badges + orphan detection) and its
        // tests count (the lineage box's `tests:N` label).
        for (uid, info) in nodes.iter_mut() {
            info.direct_up = parents.get(uid).map_or(0, |v| v.len());
            info.direct_down = children.get(uid).map_or(0, |v| v.len());
            info.test_count = tests.get(uid).map_or(0, |v| v.len());
        }

        // The macro side map: every macro the manifest declares, keyed by its
        // `unique_id`. Sorted-map storage so macro-driven output is ordered.
        let macros: BTreeMap<String, MacroInfo> = manifest
            .macros
            .iter()
            .map(|(uid, m)| (uid.clone(), macro_info(m)))
            .collect();

        let mut dag = Dag {
            nodes,
            parents,
            children,
            details,
            tests,
            sql,
            compiled,
            exposures,
            macros,
            node_macros,
            // A blank metadata name is as good as absent — the same guard the
            // `unique_id`-segment fallbacks apply (`!seg.is_empty()`), plus
            // whitespace-only.
            root_project: manifest
                .metadata
                .project_name
                .clone()
                .filter(|s| !s.trim().is_empty()),
            adapter_type: manifest
                .metadata
                .adapter_type
                .clone()
                .filter(|s| !s.trim().is_empty()),
            overview: None,
            // Source-mode loads record the macro calls they could not
            // resolve in `RawManifest::unresolved_adjacent` (`#[serde(skip)]`
            // — always empty on a real manifest) — the same "unprovable
            // adjacency" record a `PackageScope::Root` prune writes, so
            // degree-judging rules treat a hidden-macro edge exactly like a
            // severed package edge. A manifest-mode build starts empty:
            // only `PackageScope::apply_with_warnings` adds to it.
            pruned_adjacent: manifest
                .unresolved_adjacent
                .iter()
                .map(|(uid, a)| {
                    (
                        uid.clone(),
                        PrunedAdjacent {
                            parents: a.parents,
                            children: a.children,
                        },
                    )
                })
                .collect(),
        };
        // The overview needs the resolved root package, which is a property of
        // the built graph — a second pass over the manifest's `docs` map.
        dag.overview = overview_from_docs(&manifest.docs, dag.primary_package().as_deref());
        dag
    }

    /// Fill the `compiled` side map from `target/compiled/` — the
    /// Jinja-expanded SQL `dbt compile`/`build`/`run`/`test` writes per node
    /// as `<compiled_dir>/<package>/<original_file_path>`. The disk loaders
    /// ([`load_dag`], `source::load_dag_from_source_with_warnings`) call this
    /// automatically; `Dag::build` itself stays pure (manifest-only) so a
    /// `RawManifest` literal is all a test needs.
    ///
    /// Per-node resolution order: a non-empty manifest `compiled_code`
    /// already in the map wins (the same artifact as `raw_code`); otherwise
    /// the on-disk file. Missing/unreadable files and whitespace-only
    /// contents are skipped — absence is never an error. A file read is
    /// marked stale when its source is newer than the expansion — only when
    /// `project_root` is `Some` (a `None` root means the staleness root is
    /// undeterminable — e.g. a manifest not living in a `target/` dir).
    /// Deterministic: each entry depends only on its own node's coordinates,
    /// never on iteration order.
    pub fn ingest_compiled(&mut self, compiled_dir: &Path, project_root: Option<&Path>) {
        if !compiled_dir.is_dir() {
            return;
        }
        // Computed once: the package that owns this graph decides whether a
        // node's ofp is project-root-relative (local) or package-root-relative
        // (installed → `dbt_packages/`/`dbt_internal_packages/` only — never
        // the project root, where an unrelated file sharing the path would
        // flag a false stale). `package_name.is_none()` nodes are local too:
        // comparing to the primary segment yields the same answer.
        let primary = self.primary_package();
        for (uid, detail) in &self.details {
            if self.compiled.contains_key(uid) {
                continue; // manifest compiled_code already won
            }
            let Some(ofp) = detail.original_file_path.as_deref() else {
                continue;
            };
            // Compiled artifacts are .sql files — plus the `.py` bodies dbt
            // keeps for Python models (Snowpark/Databricks), which run
            // unexpanded. A source's `.yml` or a seed's `.csv` `original_file_path`
            // can never resolve.
            if !(ofp.ends_with(".sql") || ofp.ends_with(".py")) {
                continue;
            }
            // The package segment: `package_name` in a real manifest (an
            // empty string falls through to the uid segment — dbt never
            // writes one, a hand-rolled one might); the uid's second segment
            // covers synthesized source-mode nodes (which leave
            // `package_name` None — every node is local).
            let Some(pkg) = detail
                .package_name
                .as_deref()
                .filter(|p| !p.is_empty())
                .or_else(|| uid_package(uid))
            else {
                continue;
            };
            let Some(path) = crate::compiled::compiled_file_path(compiled_dir, pkg, ofp) else {
                continue;
            };
            let Ok(code) = std::fs::read_to_string(&path) else {
                continue;
            };
            if code.trim().is_empty() {
                continue;
            }
            let local = primary.as_deref().is_none_or(|p| p == pkg);
            let stale = project_root
                .is_some_and(|root| crate::compiled::is_stale(&path, root, local, pkg, ofp));
            self.compiled.insert(
                uid.clone(),
                CompiledSql {
                    code,
                    stale,
                    origin: CompiledOrigin::File,
                },
            );
        }
    }
}

/// The root project's `__overview__` block out of a manifest's `docs` map.
///
/// A doc qualifies when its `package_name` matches `primary` (the root
/// package, resolved the same way `project_name`/`primary_package` resolve
/// it) or when it is unattributed (`package_name: null` — a hand-built
/// manifest may carry that; dbt's boilerplate `doc.dbt.__overview__` always
/// records `dbt`, so it never slips through under an unattributed-candidate
/// rule). An attributed doc with no resolvable root package is conservatively
/// rejected — a wrong overview is worse than none. Deterministic: the
/// smallest uid wins when more than one candidate survives.
fn overview_from_docs(docs: &HashMap<String, RawDoc>, primary: Option<&str>) -> Option<String> {
    docs.iter()
        .filter(|(_, d)| d.name == "__overview__")
        .filter(|(_, d)| match (d.package_name.as_deref(), primary) {
            (None, _) => true,
            (Some(p), Some(root)) => p == root,
            (Some(_), None) => false,
        })
        .map(|(uid, d)| (uid, d.block_contents.trim()))
        .filter(|(_, body)| !body.is_empty())
        .min_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, body)| body.to_string())
}
