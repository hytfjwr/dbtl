use super::*;

impl App {
    /// The on-disk origin of the loaded data, for the `--watch` poller:
    /// `(path, is_dir)` — the manifest FILE in manifest mode, the project DIR
    /// in source mode.
    pub fn watch_root(&self) -> (&Path, bool) {
        match &self.source {
            DataSource::Manifest(path) => (path, false),
            DataSource::Project(dir) => (dir, true),
        }
    }

    /// Whether the data was inferred from project source (no compiled manifest)
    /// — the approximate-lineage mode the startup notice recommends upgrading
    /// from via `P` (`dbt parse`).
    pub fn is_source_mode(&self) -> bool {
        matches!(self.source, DataSource::Project(_))
    }

    /// The dbt project root (where `dbt_project.yml` is expected): the project
    /// dir in source mode, or the manifest's `target/..` parent in manifest
    /// mode. The `dbt parse` effect runs here.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Switch the data source to a compiled manifest at `path` and reload from
    /// it. On a load error the previous source (and all data, including the
    /// project root) is restored, so a truncated/corrupt manifest never strands
    /// the app — same never-corrupt contract as [`reload`](App::reload). On
    /// success the project root is re-derived from the manifest path, keeping
    /// the `$EDITOR` jump correct even for a manifest under a different root.
    /// Returns the manifest's load warnings (unhandled `resource_type`s /
    /// `functions`, plus the package-scope warnings from re-applying
    /// `self.scope`) like [`reload`](App::reload) does, so the caller can
    /// toast them.
    pub fn adopt_manifest(&mut self, path: PathBuf) -> Result<Vec<String>> {
        let new_root = derive_project_root(&path);
        let prev = std::mem::replace(&mut self.source, DataSource::Manifest(path));
        match self.reload() {
            Err(err) => {
                self.source = prev;
                Err(err)
            }
            Ok(warnings) => {
                self.project_root = new_root;
                Ok(warnings)
            }
        }
    }

    /// Re-read the manifest from disk and rebuild, preserving the current
    /// selection by `unique_id` when possible. On a load error the app is left
    /// unchanged (the `?` returns before any mutation), so reload never corrupts
    /// state — the caller can keep running on the old data. Returns the load
    /// warnings for whichever source is active: source mode's parse warnings
    /// (unparsable schema.yml, duplicate resource names), or manifest mode's
    /// unhandled `resource_type`/`functions` warnings (a synthesized source
    /// manifest never produces the latter — that channel is manifest-only),
    /// plus the package-scope warnings from re-applying `self.scope`
    /// ([`crate::PackageScope::apply_with_warnings`] — e.g. an ambiguously-rooted or
    /// emptied graph); an unremarkable reload returns an empty vec.
    pub fn reload(&mut self) -> Result<Vec<String>> {
        // Re-read from the same source; returns before mutating on error.
        // The package scope the load path applied at startup is re-applied
        // here — a recompiled manifest can gain/drop `dbt_packages/` contents
        // between reloads, and the default `Root` scope must keep hiding them.
        let (dag, mut warnings) = match &self.source {
            DataSource::Manifest(path) => load_dag_with_warnings(path)?,
            DataSource::Project(dir) => load_dag_from_source_with_warnings(dir)?,
        };
        // The SAME scope application the CLI load path runs, warnings included:
        // an emptied or ambiguously-rooted re-load is reported exactly like a
        // first load (the toast quotes the returned vec).
        let (dag, scope_warnings) = self.scope.apply_with_warnings(dag);
        warnings.extend(scope_warnings);
        self.model_list = build_model_list_grouped(&dag, self.sort, self.group_by_meta.as_deref());
        self.stats = compute_stats(&dag);
        self.dag = dag;
        // New graph identity: every cache key embedding the generation misses
        // from here on, so no pre-reload subgraph/layout/impact can survive.
        self.generation += 1;
        // Drop bookmarks whose model vanished; surviving ids persist (ids stable).
        self.bookmarks.retain(|id| self.dag.get(id).is_some());
        self.recent.retain(|id| self.dag.get(id).is_some());
        // The Resume row's uid is a model-tab root — a surviving-but-
        // non-model id can't be re-rooted (same rule the tab retain uses).
        self.home_resume = self
            .home_resume
            .take()
            .filter(|id| self.dag.get(id).is_some_and(|n| n.resource_type == "model"));
        // Folded-group keys index into the list's group LABELS — a group the
        // rebuilt list no longer has (renamed dir, emptied group, a
        // `group_by_meta` change) would pin a phantom fold; prune to live
        // keys. `apply_list_filter` below rebuilds the fold view itself.
        self.folded_groups
            .retain(|k| self.model_list.groups.iter().any(|g| &g.layer == k));
        // Same prune for every tab's re-root history: a stale id would make
        // history_back/forward pop the entry, push the current node onto the
        // opposite stack, and then fail to select — consuming the step and
        // accumulating bogus duplicates instead of landing on a survivor.
        // The `is_rootable` rule (not mere survival): a node still in the
        // graph but no longer a `model` can never be a history landing —
        // inlined here (not the `App::is_rootable` method) so the retain
        // closures borrow `dag` disjoint from `tabs`.
        for t in &mut self.tabs {
            t.back
                .retain(|id| self.dag.get(id).is_some_and(|n| n.resource_type == "model"));
            t.forward
                .retain(|id| self.dag.get(id).is_some_and(|n| n.resource_type == "model"));
            // Pruning can leave adjacent equal entries (A → vanished → A);
            // collapse them so a history step never "moves" to the same node.
            t.back.dedup();
            t.forward.dedup();
        }
        // Same source, fresh outcomes: re-read the run_results the lenses and
        // the weighted critical path read (a `dbt run` between reloads shows up
        // without a restart). A vanished file → no run data, never an error.
        self.run_results = self
            .run_results_path
            .as_ref()
            .filter(|p| p.is_file())
            .and_then(|p| RunResults::load(p).ok());
        self.catalog = self
            .catalog_path
            .as_ref()
            .filter(|p| p.is_file())
            .and_then(|p| crate::Catalog::load(p).ok())
            .map(Rc::new);
        // The CLL resolver memoizes column lists/SQL parses PER UID — over a
        // replaced Dag those memos are poison, so rebuild (memos inside the
        // engine are per-call and die with it anyway).
        let dialect = self
            .sql_dialect
            .unwrap_or_else(|| SqlDialect::from_dag(&self.dag));
        // Same generic-dialect notice as first load — a recompiled manifest
        // can change adapter_type between reloads (an explicit
        // --sql-dialect suppresses it).
        if self.sql_dialect.is_none() {
            if let Some(a) = SqlDialect::unrecognized_adapter(&self.dag) {
                warnings.push(format!(
                    "column lineage: unknown adapter '{a}' — using the generic SQL dialect"
                ));
            }
        }
        self.cll = SqlParseResolver::new(self.catalog.clone(), dialect);
        // `.dbtl.yml` reloads alongside the graph like the other side maps —
        // an edit mid-session applies on the next reload, and a file created
        // or deleted mid-session is picked up/dropped too (the path is
        // re-detected from the data source, not just replayed). A malformed
        // file falls back to shipped defaults + a warning (same policy as
        // startup).
        self.lint_config_path = self.source.lint_config_path();
        match &self.lint_config_path {
            Some(path) => match crate::LintConfig::load(path) {
                Ok(c) => self.lint_config = c,
                Err(e) => {
                    warnings.push(format!("ignoring .dbtl.yml: {e}"));
                    self.lint_config = crate::LintConfig::default();
                }
            },
            None => self.lint_config = crate::LintConfig::default(),
        }
        self.lint_findings = crate::run_rules(&self.dag, &self.lint_config);
        self.filter = None;
        self.mode = self.idle_mode();
        // Any yank-menu context stashed against the OLD graph dies here too.
        self.pending_col = None;
        self.pending_node = None;
        // Every tab's graph-indexed state dies the same way: a column trace
        // (the rebuilt Dag may not carry the column, or resolve different
        // edges for it), the lineage cursor (selection is restored BY ID so
        // the loop's selection-change chokepoint won't fire — reset here or a
        // pre-reload cursor survives into the rebuilt graph), the
        // sibling-collapse expansions (group keys index into the OLD graph's
        // columns), and the stashed lineage viewport (its CharGrid offsets
        // index into the OLD layout). The path anchor prunes by id rather
        // than dropping — a surviving anchor keeps its path view.
        for t in &mut self.tabs {
            t.column_focus = None;
            t.lineage_cursor = None;
            t.collapse_expanded.clear();
            t.path_anchor = t.path_anchor.take().filter(|id| self.dag.get(id).is_some());
            t.scroll = None;
        }
        // Drop the tabs whose root vanished from the rebuilt graph — a root
        // that survives but is no longer a `model` can't be re-rooted either
        // (§2: 消えたモデルのタブは捨てる). The HOME tab always survives
        // (Issue 10's fixed left tab); it can never empty the strip.
        // The `is_rootable` rule, inlined (not `App::is_rootable`) so the
        // predicate borrows `dag` disjoint from the `tabs` iteration below.
        let live = |t: &TabState| {
            t.home
                || t.root
                    .as_deref()
                    .is_some_and(|r| self.dag.get(r).is_some_and(|n| n.resource_type == "model"))
        };
        // `retain` slides survivors left — the active tab follows across
        // left-of-active prunes the same way `close_tab` shifts `active`:
        // its new index is the count of SURVIVORS below the old index.
        let survivors_below = self
            .tabs
            .iter()
            .take(self.active)
            .filter(|t| live(t))
            .count();
        self.tabs.retain(|t| live(t));
        if self.tabs.is_empty() {
            self.tabs.push(TabState::home());
        }
        // Seat NOW: `apply_list_filter` below resolves the selection through
        // the active tab, so `active` can't point past the shrunk strip —
        // and must not sit on a DIFFERENT tab than the one that was live.
        self.active = survivors_below.min(self.tabs.len() - 1);
        // Re-diff the rebuilt Dag against the (unchanged) --diff baseline, so
        // the Diff lens / chip / modal always describe the CURRENT graph.
        // The baseline resolver's per-uid memos stay valid (the baseline never
        // changes); the CURRENT side reads the just-rebuilt `self.cll`. The
        // baseline resolver is rebuilt only when its dialect drifted off the
        // shared one-dialect-per-diff rule (`for_diff_baseline` follows the
        // CURRENT dag's adapter, which a reload can move) — a pinned
        // --sql-dialect never drifts.
        self.diff = match &self.diff_base {
            Some(b) => {
                let want = self
                    .sql_dialect
                    .unwrap_or_else(|| SqlDialect::for_diff_baseline(&self.dag, b));
                if self.diff_cll.as_ref().is_none_or(|r| r.dialect() != want) {
                    self.diff_cll = Some(SqlParseResolver::new(None, want));
                }
                let res = self.diff_cll.as_ref().expect("baseline resolver set");
                Some(compute_diff_with(b, &self.dag, res, &self.cll))
            }
            None => None,
        };
        // Recompute the persistent-filter view over the rebuilt list (it also
        // sets the model count to the active list's length).
        self.apply_list_filter();
        // Seat the (possibly shifted) active tab: clamp the index past any
        // dropped tabs, re-anchor the list cursor on its root, and flag the
        // switch so the loop re-anchors the lineage viewport.
        self.enter_tab_forced();
        Ok(warnings)
    }
}
