use super::*;

impl App {
    /// The static-lint findings under the active [`LintConfig`](crate::LintConfig)
    /// — the shared [`crate::run_rules`] output every surface reads (Lint lens,
    /// stats worklist). `dbtl check --lint` gates on the same vector.
    pub fn lint_findings(&self) -> &[crate::Finding] {
        &self.lint_findings
    }

    /// The active lint configuration (`.dbtl.yml` or the shipped defaults).
    pub fn lint_config(&self) -> &crate::LintConfig {
        &self.lint_config
    }

    /// Replace the lint configuration and recompute findings (test/CLI seam —
    /// production gets its config from the auto-detected `.dbtl.yml`). Findings
    /// are a style input, so the generation bumps like a reload.
    pub fn set_lint_config(&mut self, config: crate::LintConfig) {
        self.lint_config = config;
        self.lint_findings = crate::run_rules(&self.dag, &self.lint_config);
        self.generation += 1;
    }

    /// The loaded `run_results.json` side-map, if any (auto-detected sibling or
    /// an explicit `--run-results`). The RunStatus/RunTime lenses and the
    /// runtime-weighted critical path read it; `None` skips them all.
    pub fn run_results(&self) -> Option<&RunResults> {
        self.run_results.as_ref()
    }

    /// Point the app at a specific `run_results.json` (`--run-results`). A load
    /// failure is the caller's error to report — the App only ever stores a
    /// successfully parsed file. Re-read by [`reload`](App::reload). Run
    /// results are a style input (the RunStatus/RunTime lenses and the
    /// inspector's last-run section read them), so the cache generation
    /// bumps like `set_lint_config`'s does.
    pub fn set_run_results(&mut self, path: PathBuf) -> Result<()> {
        self.run_results = Some(RunResults::load(&path)?);
        self.run_results_path = Some(path);
        self.generation += 1;
        Ok(())
    }

    /// The loaded `catalog.json` side-map, if any — the detail modal reads it
    /// for real column types, row counts, byte sizes, and doc↔warehouse drift.
    pub fn catalog(&self) -> Option<&crate::Catalog> {
        self.catalog.as_deref()
    }

    /// The detail-modal catalog block for a node: `catalog:` stats line (rows,
    /// size), a per-column real type when the manifest only documents a name,
    /// and a `drift:` line when documented/actual column sets disagree. Empty
    /// when the catalog doesn't cover this uid.
    pub fn catalog_note(&self, uid: &str, detail: &NodeDetail) -> Vec<String> {
        let Some(node) = self.catalog.as_ref().and_then(|c| c.get(uid)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stats = Vec::new();
        if let Some(rows) = node.row_count() {
            stats.push(format!("{rows} rows"));
        }
        if let Some(bytes) = node.size_bytes() {
            stats.push(format!("{bytes} bytes"));
        }
        if !stats.is_empty() {
            out.push(format!("catalog: {}", stats.join(", ")));
        }
        // Real warehouse type per documented column (the manifest's data_type
        // is usually None — schema.yml rarely declares it).
        for col in &detail.columns {
            if let Some(ty) = node.column_type(&col.name) {
                if col.data_type.as_deref() != Some(ty) {
                    out.push(format!("  {}: {}", col.name, ty));
                }
            }
        }
        let (missing_in_wh, missing_in_docs) = node.drift(
            &detail
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
        );
        if !missing_in_wh.is_empty() || !missing_in_docs.is_empty() {
            out.push(format!(
                "drift:   {} documented-but-missing, {} undocumented",
                missing_in_wh.len(),
                missing_in_docs.len()
            ));
        }
        out
    }

    /// The status-bar readout for the loaded run results:
    /// `run 12/34 @ 2024-01-02` — covered/dag-nodes plus the artifact's date —
    /// so a stale or partial run is visible at a glance. `None` when unloaded.
    pub fn run_status_label(&self) -> Option<String> {
        let rr = self.run_results.as_ref()?;
        let covered = rr.covered_in(&self.dag);
        let total = self.dag.nodes().len();
        let date = rr
            .generated_at
            .as_deref()
            .map(|ts| ts.get(..10).unwrap_or(ts)) // ISO date part only
            .unwrap_or("?");
        Some(format!("run {covered}/{total} @{date}"))
    }

    /// Load a `--diff` baseline: store it (with its display label AND the
    /// baseline-side CLL resolver the column analysis reads through — one
    /// dialect shared with the current side per
    /// [`SqlDialect::for_diff_baseline`], or the pinned `--sql-dialect`),
    /// compute the diff against the current Dag, and bump the cache
    /// generation — the diff is a style input (the Diff lens reads it), so
    /// any cached styled layout from before the baseline landed must miss.
    /// Recomputed on every reload; the baseline itself never changes after
    /// startup.
    pub fn set_diff_base(&mut self, base: Dag, label: String) {
        let dialect = self
            .sql_dialect
            .unwrap_or_else(|| SqlDialect::for_diff_baseline(&self.dag, &base));
        let base_res = SqlParseResolver::new(None, dialect);
        self.diff = Some(compute_diff_with(&base, &self.dag, &base_res, &self.cll));
        self.diff_cll = Some(base_res);
        self.diff_base = Some(base);
        self.diff_label = label;
        self.generation += 1;
    }

    /// The computed baseline↔current diff (`None` without a `--diff` baseline).
    pub fn diff(&self) -> Option<&DagDiff> {
        self.diff.as_ref()
    }

    /// The `--diff` baseline's display label (empty without a baseline).
    pub fn diff_label(&self) -> &str {
        &self.diff_label
    }

    /// Set the `meta` key the model list groups by (`config.yml`
    /// `group_by_meta`; `None` = the default layer grouping) and rebuild the
    /// list like a sort change — selection re-resolves by `unique_id`. An
    /// empty or whitespace-only key is `None` — it would otherwise group
    /// every model under a literal `(no  )` bucket.
    pub fn set_group_by_meta(&mut self, key: Option<String>) {
        let key = key.filter(|k| !k.trim().is_empty());
        if self.group_by_meta == key {
            return;
        }
        self.group_by_meta = key;
        let current = self.selected_unique_id();
        self.model_list =
            build_model_list_grouped(&self.dag, self.sort, self.group_by_meta.as_deref());
        // Fold keys are group LABELS — a regrouping renames every group, so
        // layer-keyed folds would pin phantoms. Same prune `reload` runs.
        self.folded_groups
            .retain(|k| self.model_list.groups.iter().any(|g| &g.layer == k));
        // A rebuilt model_list is a home-search input — stamp the cache.
        self.home_stamp += 1;
        self.apply_list_filter();
        if let Some(uid) = &current {
            self.select_by_unique_id(uid);
        }
    }

    /// Set the `meta` key the impact reports read as "affected owners"
    /// (`config.yml` `owner_meta_key`). It feeds the next `diff_report`
    /// snapshot AND the inspector's owner line, so the generation bumps
    /// like every style input — a mid-session call never serves a stale
    /// cached snapshot.
    pub fn set_owner_meta_key(&mut self, key: impl Into<String>) {
        self.owner_meta_key = key.into();
        self.generation += 1;
    }

    /// Set the sibling-collapse threshold (`config.yml` `lineage_collapse`;
    /// `0` disables the `+M more` markers entirely). The threshold is a cache
    /// key input, so no explicit invalidation is needed — the next layout
    /// read simply misses and rebuilds.
    pub fn set_collapse_threshold(&mut self, threshold: usize) {
        self.collapse_threshold = threshold;
    }

    /// Drop every expanded sibling-collapse group — the re-root
    /// re-initialisation. Called at the event loop's selection-change
    /// chokepoint (which also re-homes the cursor) and internally wherever a
    /// selection-identity change is detected; `reload` calls it on the new
    /// graph's behalf.
    pub fn clear_collapse_expansions(&mut self) {
        self.tab_mut().collapse_expanded.clear();
    }

    /// The package scope the Dag was loaded under (`--include-packages` ⇒
    /// [`PackageScope::All`]). [`reload`](App::reload) re-applies it to the
    /// re-loaded graph and the status bar prints its `scope:` tag.
    pub fn scope(&self) -> crate::PackageScope {
        self.scope
    }

    /// Record the package scope the load path already applied to `dag`
    /// (`build_app` calls this once, right after `App::new`/`from_source`):
    /// `reload` re-scopes the freshly parsed graph with it and the status
    /// bar shows its tag. It does NOT re-filter the live Dag — the loaded
    /// graph is already scoped by then.
    pub fn set_scope(&mut self, scope: crate::PackageScope) {
        self.scope = scope;
    }

    /// Pin the column-lineage SQL dialect (`--sql-dialect`); rebuilds the
    /// resolver on the spot — and the baseline side's, then the diff, since
    /// the pinned dialect applies to both parse surfaces. `reload` re-derives
    /// the auto dialect when none was pinned. Bumps the cache generation:
    /// resolved columns and the recomputed diff are style inputs (column
    /// trace, Diff lens), so layouts built under the old dialect must miss.
    pub fn set_sql_dialect(&mut self, dialect: SqlDialect) {
        self.sql_dialect = Some(dialect);
        self.cll = SqlParseResolver::new(self.catalog.clone(), dialect);
        if let Some(base) = &self.diff_base {
            let base_res = SqlParseResolver::new(None, dialect);
            self.diff = Some(compute_diff_with(base, &self.dag, &base_res, &self.cll));
            self.diff_cll = Some(base_res);
        }
        self.generation += 1;
    }

    /// The status-bar diff chip: `"diff +A ~M -R"` (added / modified / removed
    /// node counts vs the baseline), or `"diff clean"` when the Dags match.
    /// `None` without a baseline. ASCII by construction.
    pub fn diff_status_label(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        if diff.is_empty() {
            return Some("diff clean".to_string());
        }
        let (a, m, r) = diff.counts();
        Some(format!("diff +{a} ~{m} -{r}"))
    }

    /// The active colour theme (what the loop hands to `RenderCtx`).
    pub fn active_theme(&self) -> &Theme {
        &self.themes[self.theme_index].1
    }

    /// The active theme's name (the toast / status text).
    pub fn theme_name(&self) -> &str {
        &self.themes[self.theme_index].0
    }

    /// Replace the loaded theme list and the active index (the `--theme` CLI
    /// seam). An empty list is ignored and an out-of-range index clamps — the
    /// App must always have an active theme.
    pub fn set_themes(&mut self, themes: Vec<(String, Theme)>, index: usize) {
        if !themes.is_empty() {
            self.themes = themes;
        }
        self.theme_index = index.min(self.themes.len().saturating_sub(1));
    }

    /// Step the active theme to the next loaded one (wrapping) and record the
    /// landing name as the toast notice.
    pub fn cycle_theme(&mut self) {
        self.theme_index = (self.theme_index + 1) % self.themes.len();
        let name = self.themes[self.theme_index].0.clone();
        self.set_notice(format!("theme: {name}"));
    }

    /// The notification log's capacity (Issue 08): the centre keeps the
    /// newest `NOTIFICATION_CAP` entries; the oldest evicts beyond it.
    /// Memory-only — the log is never persisted.
    pub const NOTIFICATION_CAP: usize = 100;
}
