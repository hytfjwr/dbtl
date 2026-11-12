use super::*;

impl Dag {
    /// The manifest's macros (`macro.<project>.<name>` → [`MacroInfo`]). Empty
    /// for a synthesized source-mode manifest — macro-diff detection is
    /// manifest-vs-manifest only, by design.
    pub fn macros(&self) -> &BTreeMap<String, MacroInfo> {
        &self.macros
    }

    /// The macro `unique_id`s a kept node's compiled SQL calls
    /// (`depends_on.macros`). Empty for nodes that call no macro — and for
    /// every node in source mode.
    pub fn node_macros(&self, unique_id: &str) -> &[String] {
        self.node_macros.get(unique_id).map_or(&[], Vec::as_slice)
    }

    /// The unified `unique_id -> NodeInfo` map (read-only access).
    pub fn nodes(&self) -> &HashMap<String, NodeInfo> {
        &self.nodes
    }

    /// Number of entries in the unified map.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the unified map is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether the given `unique_id` is present in the unified map.
    pub fn contains(&self, unique_id: &str) -> bool {
        self.nodes.contains_key(unique_id)
    }

    /// Look up a node by `unique_id`.
    pub fn get(&self, unique_id: &str) -> Option<&NodeInfo> {
        self.nodes.get(unique_id)
    }

    /// Count of kept nodes with the given `resource_type`.
    pub fn count_by_resource_type(&self, resource_type: &str) -> usize {
        self.nodes
            .values()
            .filter(|n| n.resource_type == resource_type)
            .count()
    }

    /// The dbt project name — the ROOT project's name when the manifest
    /// records it (`metadata.project_name`, dbt ≥1.6; source mode synthesizes
    /// it from `dbt_project.yml`). That is the authoritative answer: an
    /// installed package whose name sorts first in `unique_id`s can no longer
    /// masquerade as the project.
    ///
    /// Fallback (old/hand-built manifests with no metadata): the
    /// lexicographically-smallest `<project>` segment of the node
    /// `unique_id`s (`<resource_type>.<project>.<name>`) — deterministic and
    /// stable for the usual single-project manifest. `None` when no node
    /// carries a project segment.
    ///
    /// Shared by the docs index heading and the title-bar stats so the two
    /// never disagree.
    ///
    /// NOTE: the two name fallbacks diverge by design — `project_name` takes
    /// the lexicographically SMALLEST segment (a stable display name) while
    /// [`primary_package`](Dag::primary_package) takes the most FREQUENT (the
    /// likeliest graph owner). Under a recorded `metadata.project_name` they
    /// agree; without it a multi-package manifest can name two different
    /// packages — which is why the scope path warns on that exact ambiguity
    /// ([`PackageScope::apply_with_warnings`]).
    pub fn project_name(&self) -> Option<&str> {
        self.root_project.as_deref().or_else(|| {
            self.nodes
                .keys()
                .filter_map(|uid| uid.split('.').nth(1))
                .filter(|seg| !seg.is_empty())
                .min()
        })
    }

    /// The adapter the manifest was compiled against (`metadata.adapter_type`
    /// — `duckdb`, `redshift`, `snowflake`, …), or `None` when the manifest
    /// does not record one (source mode never guesses: it does not read
    /// `profiles.yml`). CLL's dialect selection and the `--sql-dialect`
    /// override consult it.
    pub fn adapter_type(&self) -> Option<&str> {
        self.adapter_type.as_deref()
    }

    /// The project overview — the root project's `{% docs __overview__ %}`
    /// body, already rendered (dbt expands `doc()` refs at parse time; source
    /// mode expands them through `DocBlocks::render`). `None` when the
    /// project declares none — installed-package entries
    /// (`doc.dbt.__overview__` and friends) never count.
    pub fn overview(&self) -> Option<&str> {
        self.overview.as_deref()
    }

    /// The detail payload (materialized / schema / columns / …) for a node, or
    /// `None` if unknown. Used by the structure modal and the status line.
    pub fn detail(&self, unique_id: &str) -> Option<&NodeDetail> {
        self.details.get(unique_id)
    }

    /// The kept 1-hop parent uids of `unique_id` (empty slice when none or
    /// unknown). Order is the pruned adjacency's — deterministic per manifest.
    /// The column-lineage engine resolves each node's edges against exactly
    /// this set.
    pub fn direct_parents(&self, unique_id: &str) -> &[String] {
        self.parents.get(unique_id).map_or(&[], Vec::as_slice)
    }

    /// The kept 1-hop child uids of `unique_id` (empty slice when none or
    /// unknown) — the counterpart of [`direct_parents`](Dag::direct_parents),
    /// for callers scanning a node's consumers (column-lineage downstream
    /// queries) without a full subgraph build.
    pub fn direct_children(&self, unique_id: &str) -> &[String] {
        self.children.get(unique_id).map_or(&[], Vec::as_slice)
    }

    /// The adjacency a `PackageScope::Root` prune severed for `unique_id`
    /// (installed-package parents/children it used to touch), or `None`
    /// when the scope never pruned one of its neighbours. The
    /// connectedness rules (`root_models`, `unused_sources`) add these
    /// counts back to `direct_*` so a package-trimmed node isn't reported
    /// as isolated, and the degree-judging rules (`rejoining_upstream`,
    /// `model_fanout`, `chained_views`) treat a severed child as
    /// unprovable — a premise their finding needs never holds.
    pub fn pruned_adjacent(&self, unique_id: &str) -> Option<&PrunedAdjacent> {
        self.pruned_adjacent.get(unique_id)
    }

    /// Test-only seam: inject a compiled-SQL entry directly. A `stale` flag
    /// cannot be expressed in a manifest literal, and forging
    /// `target/compiled/` mtimes inside a unit test needs `filetime`-grade APIs
    /// the crate does not carry.
    #[cfg(test)]
    pub fn set_compiled_for_test(&mut self, unique_id: &str, code: &str, stale: bool) {
        self.compiled.insert(
            unique_id.to_string(),
            CompiledSql {
                code: code.to_string(),
                stale,
                origin: crate::compiled::CompiledOrigin::File,
            },
        );
    }

    /// The root project name the manifest RECORDED (`metadata.project_name`,
    /// dbt ≥1.6; source mode synthesizes it from `dbt_project.yml`) — `None`
    /// for pre-1.6 or hand-built manifests, where [`project_name`] and
    /// [`primary_package`] fall back to `unique_id`-segment heuristics. The
    /// scope path branches on this to warn when the fallback is actually
    /// ambiguous (more than one package present).
    ///
    /// [`project_name`]: Dag::project_name
    /// [`primary_package`]: Dag::primary_package
    pub fn recorded_project(&self) -> Option<&str> {
        self.root_project.as_deref()
    }

    /// The name of the package that OWNS this graph — the ROOT project's name
    /// when the manifest records it (see [`project_name`](Dag::project_name)),
    /// else the most frequent `unique_id` project segment
    /// (`model.<pkg>.<name>`), robust to installed packages that happen to
    /// sort first; ties break to the smaller name. Drives the `Root`
    /// [`PackageScope`] (which therefore scopes to the root project, not the
    /// biggest package).
    ///
    /// Without recorded metadata the answer is a GUESS: a manifest whose
    /// installed packages outnumber the root project would invert it.
    /// [`PackageScope::apply_with_warnings`] warns on exactly that case (no
    /// `metadata.project_name` AND more than one package), so the guess is
    /// never silent. This is the resolve-ONCE entry point for sweeps — pair it
    /// with [`in_package`] instead of calling [`is_root_project`](Dag::is_root_project)
    /// per node.
    pub fn primary_package(&self) -> Option<String> {
        if let Some(root) = &self.root_project {
            return Some(root.clone());
        }
        self.package_histogram()
            .into_iter()
            .max_by(|(a, ca), (b, cb)| ca.cmp(cb).then_with(|| b.cmp(a)))
            .map(|(name, _)| name.to_string())
    }

    /// The `<pkg>`-segment histogram over the node `unique_id`s
    /// (`model.<pkg>.<name>` → `pkg` counts one) — the data behind
    /// [`primary_package`](Dag::primary_package)'s frequency fallback AND the
    /// scope path's "more than one package" ambiguity test. Private so both
    /// share ONE scan shape.
    pub(super) fn package_histogram(&self) -> BTreeMap<&str, usize> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for uid in self.nodes.keys() {
            if let Some(seg) = uid.split('.').nth(1).filter(|s| !s.is_empty()) {
                *counts.entry(seg).or_default() += 1;
            }
        }
        counts
    }

    /// Whether `n` belongs to the ROOT project — the ONE package-scope
    /// predicate every scope-aware consumer shares (the default load filter,
    /// the aggregate metrics, `dbtl check`). The node's declaring package is
    /// its `unique_id`'s second segment (`model.<pkg>.<name>`; a source uid
    /// `source.<pkg>.<src>.<table>` resolves the same way), compared against
    /// [`primary_package`](Dag::primary_package) — the recorded root project
    /// (`metadata.project_name`), else the most frequent segment.
    ///
    /// Boundary rules (all bias toward NOT dropping data):
    /// - a `unique_id` with no non-empty package segment is unattributable →
    ///   in scope;
    /// - an unresolvable owning package (an empty or segment-free graph) →
    ///   every node is in scope.
    ///
    /// Per-call cost is O(1) when `metadata.project_name` is recorded (dbt
    /// ≥1.6, source mode); only pre-1.6 / hand-built manifests fall back to
    /// the O(nodes) `primary_package` scan. This is the ONE-OFF test — sweeps
    /// and bulk filters must resolve the package ONCE via
    /// [`primary_package`](Dag::primary_package) and test each node with
    /// [`in_package`] (`primary.as_deref().is_none_or(|p| in_package(n, p))`
    /// is exactly this predicate), the shape [`root_scoped`](Dag::root_scoped)
    /// already models.
    pub fn is_root_project(&self, n: &NodeInfo) -> bool {
        match self.primary_package() {
            Some(pkg) => in_package(n, &pkg),
            None => true,
        }
    }

    /// The sub-Dag holding only the root project's own nodes — the `Root`
    /// [`PackageScope`] applied: installed-package nodes pruned, every edge
    /// endpoint and side-map entry of theirs dropped, and the severed
    /// adjacency recorded in [`pruned_adjacent`](Dag::pruned_adjacent).
    /// This is exactly what `--project-only` produced; it is now the
    /// default load shape, so a manifest-mode Dag carries the same nodes a
    /// source-mode parse of the project would.
    ///
    /// A root model whose only upstream/downstream was a package node loses
    /// that edge here — the same "unresolved ref drops" shape source mode
    /// already produces — so `direct_*` degrees (and thus orphan detection)
    /// agree across modes under the default scope.
    ///
    /// Delegates to [`PackageScope::apply`] on a clone, NOT to
    /// [`filtered`](Dag::filtered): this IS the package boundary, so the
    /// severed-adjacency record must be written — a plain `filtered` cut
    /// would leave the map empty and let degree-reading rules misjudge a
    /// package-trimmed node.
    pub fn root_scoped(&self) -> Dag {
        PackageScope::Root.apply(self.clone())
    }

    /// The sub-Dag containing only nodes `keep` accepts — every side map
    /// (`details`/`tests`/`sql`/`exposures`/`node_macros`) and every edge
    /// endpoint pruned to the same uid set, so no dangling reference survives.
    /// Deterministic (same maps, subset). Powers [`root_scoped`](Dag::root_scoped)
    /// (the `Root` package scope, formerly `--project-only`).
    pub fn filtered(&self, keep: impl Fn(&NodeInfo) -> bool) -> Dag {
        let nodes: HashMap<String, NodeInfo> = self
            .nodes
            .iter()
            .filter(|(_, n)| keep(n))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let prune_map = |m: &HashMap<String, Vec<String>>,
                         keep: &HashMap<String, NodeInfo>|
         -> HashMap<String, Vec<String>> {
            m.iter()
                .filter(|(k, _)| keep.contains_key(*k))
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.iter()
                            .filter(|u| keep.contains_key(u.as_str()))
                            .cloned()
                            .collect(),
                    )
                })
                .collect()
        };
        let parents = prune_map(&self.parents, &nodes);
        let children = prune_map(&self.children, &nodes);
        // direct_up/direct_down on NodeInfo must reflect the SURVIVING edges,
        // not the pre-filter topology.
        let mut nodes = nodes;
        for (uid, info) in nodes.iter_mut() {
            info.direct_up = parents.get(uid).map_or(0, Vec::len);
            info.direct_down = children.get(uid).map_or(0, Vec::len);
        }
        Dag {
            parents,
            children,
            details: self
                .details
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            tests: self
                .tests
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            sql: self
                .sql
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            compiled: self
                .compiled
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            exposures: self
                .exposures
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            // The macro registry is small and shared: keep it whole so
            // surviving nodes' macro deps still resolve.
            macros: self.macros.clone(),
            node_macros: self
                .node_macros
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            // The severed-adjacency record is carried over, never
            // recomputed — it describes the package boundary's prune, not
            // this (presentation) cut.
            pruned_adjacent: self
                .pruned_adjacent
                .iter()
                .filter(|(k, _)| nodes.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            // The root identity is a property of the source manifest, not of
            // the kept uid set — a `--project-only` view is still that project.
            root_project: self.root_project.clone(),
            adapter_type: self.adapter_type.clone(),
            overview: self.overview.clone(),
            nodes,
        }
    }

    /// The tests attached to a node (empty slice when none). Captured pre-prune,
    /// sorted by `(kind, column, name)` for determinism.
    pub fn tests(&self, unique_id: &str) -> &[TestInfo] {
        self.tests.get(unique_id).map_or(&[], |v| v.as_slice())
    }

    /// The raw (uncompiled) SQL for a node, or `None` (sources/seeds, or models
    /// loaded from a manifest that omitted `raw_code`). Stored as a side map so
    /// it never widens the hot subgraph clone/equality path nor `NodeDetail`'s
    /// frozen literals.
    pub fn raw_code(&self, unique_id: &str) -> Option<&str> {
        self.sql.get(unique_id).map(String::as_str)
    }

    /// The compiled-SQL side-map entry for a node (expanded text + `stale`
    /// freshness flag), or `None` when neither the manifest's `compiled_code`
    /// nor a `target/compiled/` file covered it. Counterpart of
    /// [`detail`](Dag::detail) for the [`compiled_code`](Dag::compiled_code)
    /// accessor.
    pub fn compiled(&self, unique_id: &str) -> Option<&CompiledSql> {
        self.compiled.get(unique_id)
    }

    /// The compiled (Jinja-expanded) SQL text for a node, or `None` — the
    /// counterpart of [`raw_code`](Dag::raw_code). Resolution order:
    /// non-empty manifest `compiled_code`, then `target/compiled/` (see
    /// [`Dag::ingest_compiled`]).
    pub fn compiled_code(&self, unique_id: &str) -> Option<&str> {
        self.compiled.get(unique_id).map(|c| c.code.as_str())
    }

    /// The exposure payload (kind / owner / url) for an exposure node, or
    /// `None` for every other node. Side map, like [`detail`](Dag::detail).
    pub fn exposure(&self, unique_id: &str) -> Option<&ExposureInfo> {
        self.exposures.get(unique_id)
    }
}
