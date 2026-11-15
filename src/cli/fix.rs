//! The `dbtl lint --fix`/`--fix-dry-run` driver — the file-facing half
//! of the fix machinery. `dbtl_core` owns the WHAT (which byte ranges a
//! fixable finding lets go, with the Jinja/mapping/overlap/re-parse
//! guards inside); this module owns the HOW: which project root
//! `original_file_path` resolves against, whether the file on disk still
//! is the text the graph was built from, the fixpoint loop that
//! re-lints a patched in-memory dag until nothing safe remains, the
//! atomic file write, and the dry-run unified diff.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;

/// The fixpoint cap. Every supported fix deletes only literal bytes —
/// the finding it resolved can't reappear — so a second round only ever
/// sees edits the first round's deletion made safe (rare). Reaching the
/// cap means the loop stopped converging; keep what applied rather than
/// spin.
const MAX_FIX_ROUNDS: usize = 8;

/// What `run_fixes` hands back to `run_lint`: the post-fix dag to re-lint
/// for the report, the file changes (applied or proposed), and the count
/// of findings the accepted edits resolved.
pub(super) struct FixOutcome {
    /// The rebuilt dag the final report lints. In-memory only — for
    /// `--fix-dry-run` it describes a state that was never on disk.
    pub dag: dbtl::Dag,
    /// Fix-specific warnings (drifted files, refused edits, edits that
    /// would introduce findings) — printed through `warn_each` beside
    /// the load/eval warnings.
    pub warnings: Vec<String>,
    /// `(project-relative file, absolute path, before, after)` — the
    /// write set for `--fix`, the diff set for `--fix-dry-run`.
    pub files: Vec<FixFile>,
    /// Findings the accepted edits resolved (`--fix`) or would resolve
    /// (`--fix-dry-run`) — pre-fix minus post-fix fixable-rule findings.
    pub fixed_findings: usize,
}

/// One changed model file.
pub(super) struct FixFile {
    /// The `original_file_path` the manifest recorded (the diff's label).
    pub ofp: String,
    /// `root.join(ofp)` — where `--fix` writes.
    pub path: PathBuf,
    /// The bytes on disk when the run started (the TOCTOU check at
    /// write time re-reads against this).
    pub before: String,
    /// The post-fix text.
    pub after: String,
}

/// Everything [`run_fixes`] needs that `run_lint` already computed —
/// bundled so the driver's signature stays readable.
pub(super) struct FixRequest<'a> {
    /// The global source flags (`--source`/`--manifest`/`--project`,
    /// `--vars`, scope) — the path decision and the source-mode reload
    /// read them.
    pub src: &'a DataSourceArgs<'a>,
    /// The resolved current source — its `dag` supplies the raw/compiled
    /// text the patched rebuilds restore per node.
    pub resolved: &'a ResolvedSource,
    /// The effective lint config (post `--preset`/`--rules`) — it
    /// decides WHICH findings are fixable.
    pub cfg: &'a dbtl::LintConfig,
    /// The introduced-findings guard's config: preset + `.dbtl.yml` but
    /// NOT the `--rules` overlay, and evaluated without `--select` — the
    /// user's real lint set. Under a narrowed run a fix could introduce a
    /// finding the narrowing never evaluated; the guard must see it.
    pub guard_cfg: &'a dbtl::LintConfig,
    /// The `--diff` baseline dag, same as the report path's.
    pub base: Option<&'a dbtl::Dag>,
    /// The applied lint baseline, same as the report path's.
    pub baseline: Option<&'a dbtl::Baseline>,
    /// The resolved fail threshold — the fix rounds gate identically to
    /// the report so a fix never redefines what counts.
    pub fail_on: dbtl::FailOn,
    /// The catalog the lint resolver carries (usually `None`).
    pub catalog: &'a Option<Rc<dbtl::Catalog>>,
    /// The baseline-side SQL resolver (`--diff` only).
    pub base_sql: Option<&'a dbtl::SqlParseResolver>,
    /// The `--select` narrowing — fixes only ever touch findings the
    /// report would show.
    pub selection: Option<&'a BTreeSet<String>>,
    /// The parse dialect the lint resolver runs under.
    pub dialect: dbtl::SqlDialect,
}

/// The `(project root, RawManifest)` pair the fix rounds need: the root
/// anchors `original_file_path` resolution; the manifest is the patch
/// target `dag_from_manifest` rebuilds from (each accepted round's edits
/// rewrite `raw_code` in place of a stale `compiled_code`).
///
/// Source mode → the project dir + a fresh source parse. A
/// `<root>/target/manifest.json` → the root + the manifest re-read (the
/// parsed `Dag` can't be patched — `RawManifest` is the mutable layer).
/// A manifest anywhere else can't anchor project-relative paths — a
/// usage error naming `--source`, per the issue's root rule.
fn fix_plan(src: &DataSourceArgs) -> Result<(PathBuf, dbtl::RawManifest), CliError> {
    match source_path(src.manifest, src.source, src.project, src.diff) {
        Some(SourcePath::Project(dir)) => {
            let vars = src.cli_vars()?;
            let (manifest, _) =
                dbtl::manifest_from_source_with_warnings(&dir, vars.as_ref()).input()?;
            Ok((dir, manifest))
        }
        Some(SourcePath::Manifest(path)) => {
            let root = path
                .parent()
                .filter(|d| d.file_name().is_some_and(|n| n == "target"))
                .and_then(|d| d.parent())
                .ok_or_else(|| {
                    CliError::usage(anyhow::anyhow!(
                        "--fix needs the project root to resolve model source \
                         paths — pass `--source <project dir>` (a manifest \
                         not under <project>/target/ can't be mapped back \
                         to the files it was compiled from)"
                    ))
                })?;
            let manifest = dbtl::load_manifest(&path).input()?;
            Ok((root.to_path_buf(), manifest))
        }
        // `resolve()` already produced a dag — a source always exists.
        None => unreachable!("fix runs after a successful resolve"),
    }
}

/// `manifest` after `overlay`: `manifest_patched` rewrites `raw_code`
/// AND the patched nodes' adjacency (a deleted `{{ ref() }}` loses its
/// edge — the second return names each uid's dropped parents, which
/// the introduced-findings attribution reads), then the untouched
/// nodes re-carry the resolved dag's compiled view so the SQL rules
/// see the same text they did before the fix pass.
fn patched_manifest(
    manifest: &dbtl::RawManifest,
    overlay: &BTreeMap<String, String>,
    resolved: &ResolvedSource,
) -> (dbtl::RawManifest, BTreeMap<String, Vec<String>>) {
    let (mut m, dropped) = dbtl::manifest_patched(manifest, overlay);
    for (uid, node) in &mut m.nodes {
        if !overlay.contains_key(uid) && node.compiled_code.is_none() {
            if let Some(c) = resolved.dag.compiled_code(uid) {
                node.compiled_code = Some(c.to_string());
            }
        }
    }
    (m, dropped)
}

/// Rebuild the dag with `overlay`'s patched `raw_code`: the same
/// `dag_from_manifest` the loaders funnel through, then the same package
/// scope `resolve_source` applied.
fn patched_dag(
    manifest: &dbtl::RawManifest,
    overlay: &BTreeMap<String, String>,
    resolved: &ResolvedSource,
    scope: dbtl::PackageScope,
) -> dbtl::Dag {
    let (m, _) = patched_manifest(manifest, overlay, resolved);
    let (dag, _) = dbtl::dag_from_manifest(&m);
    scope.apply_with_warnings(dag).0
}

/// The file gate every fix runs once per node — `Some` only when the
/// node maps to a project file whose CURRENT disk bytes are exactly
/// what the resolved graph was built from:
///
/// - package-node files (`dbt_packages/`…) and `.py` models are never
///   edited — the SQL rules don't flag them anyway, the belt over the
///   rule filter;
/// - a missing `original_file_path` or an unreadable file leaves the
///   finding alone (the unreadable case warns — a `.sql` the graph
///   says exists but the run can't open);
/// - `disk bytes != the resolved dag's raw_code` means a stale manifest
///   or a file touched mid-run — never patched, warned once.
fn node_file(
    dag: &dbtl::Dag,
    resolved: &ResolvedSource,
    root: &Path,
    primary: Option<&str>,
    uid: &str,
    disk: &mut BTreeMap<PathBuf, Option<String>>,
    warnings: &mut BTreeSet<String>,
) -> Option<PathBuf> {
    if let (Some(node), Some(pkg)) = (dag.get(uid), primary) {
        if !dbtl::in_package(node, pkg) {
            return None;
        }
    }
    let ofp = dag.detail(uid)?.original_file_path.clone()?;
    if ofp.to_ascii_lowercase().ends_with(".py") {
        return None;
    }
    // `ofp` is manifest-carried data — only a plain relative path may
    // join the root, or a crafted manifest could point the write outside
    // the project (the same predicate `contained_resource_paths` uses).
    if !Path::new(&ofp).components().all(|c| {
        matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        warnings.insert(format!(
            "--fix: {ofp}: not a project-relative path — skipped"
        ));
        return None;
    }
    let path = root.join(&ofp);
    let on_disk = disk
        .entry(path.clone())
        .or_insert_with(|| std::fs::read_to_string(&path).ok());
    let Some(on_disk) = on_disk else {
        warnings.insert(format!(
            "--fix: {ofp}: model source file is missing or unreadable — skipped"
        ));
        return None;
    };
    let expected = resolved.dag.raw_code(uid)?;
    if on_disk != expected {
        warnings.insert(format!(
            "--fix: {ofp}: file on disk differs from {} — skipped",
            if resolved.is_source {
                "the loaded project (was it edited mid-run?)"
            } else {
                "the manifest's raw_code (a stale manifest is never patched)"
            }
        ));
        return None;
    }
    Some(path)
}

/// The finding identity the new-findings guard compares — `(rule, node,
/// related)`, the same key `--diff`'s pre-existing classification uses.
fn finding_key(f: &dbtl::Finding) -> (dbtl::LintRule, String, Vec<String>) {
    let (rule, node, related) = f.key();
    (rule, node.to_string(), related.to_vec())
}

/// The fixpoint loop: lint the current (patched) dag, group fixable
/// findings by node, plan + apply edits, rebuild, and re-lint until no
/// safe fix remains or `MAX_FIX_ROUNDS` trips. Each accepted file's
/// edits are committed to `overlay` only after the post-edit dag proves
/// the edit introduced no NEW finding on the node — a fix that creates
/// work is reverted, per the issue's guard. `write` (`--fix`) persists
/// the accepted set and drops any file that drifted mid-run from the
/// outcome, so the report dag describes what's really on disk.
pub(super) fn run_fixes(req: &FixRequest, write: bool) -> Result<FixOutcome, CliError> {
    let (root, manifest) = fix_plan(req.src)?;
    let primary = req.resolved.dag.primary_package();
    let mut overlay: BTreeMap<String, String> = BTreeMap::new();
    // uid → its gated file (`None` = failed the gate; never re-asked).
    let mut checked: BTreeMap<String, Option<(String, PathBuf)>> = BTreeMap::new();
    // abs path → disk bytes at first read — the drift check AND the
    // write-time TOCTOU baseline.
    let mut disk: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
    let mut warnings = BTreeSet::new();
    // Culprits already warned about — a permanently unfixable file
    // would otherwise re-warn every round the loop looks at it.
    let mut warned_culprits: BTreeSet<String> = BTreeSet::new();
    let mut fixable_pre = 0usize;

    let mut dag = patched_dag(&manifest, &overlay, req.resolved, req.src.scope);
    let mut hit_cap = true;
    for _round in 0..MAX_FIX_ROUNDS {
        // A fresh resolver every round — the lazy resolver's per-uid
        // memos would otherwise serve the pre-edit parse forever.
        let resolver = dbtl::SqlParseResolver::new(req.catalog.clone(), req.dialect);
        let outcome = evaluate_lint(
            &dag,
            req.cfg,
            req.base,
            req.baseline,
            req.fail_on,
            LintAux {
                sql: Some(&resolver),
                catalog: req.catalog.as_deref(),
                base_sql: req.base_sql,
            },
            req.selection,
        );
        if _round == 0 {
            fixable_pre = outcome.findings.iter().filter(|f| f.rule.fixable()).count();
        }
        // uid → the fixable rules flagged there this round.
        let mut by_node: BTreeMap<String, BTreeSet<dbtl::LintRule>> = BTreeMap::new();
        for f in &outcome.findings {
            if f.rule.fixable() {
                by_node
                    .entry(f.node_uid.clone())
                    .or_default()
                    .insert(f.rule);
            }
        }
        if by_node.is_empty() {
            hit_cap = false;
            break;
        }
        // Plan + tentatively apply, per node.
        struct Cand {
            uid: String,
            /// The rules whose findings the edits target — a post-edit
            /// finding under one of these is an incomplete fix, never an
            /// introduced one.
            rules: BTreeSet<dbtl::LintRule>,
            text: String,
        }
        let mut cands = Vec::new();
        for (uid, rules) in &by_node {
            let Some(raw) = dag.raw_code(uid) else {
                continue;
            };
            let gated = checked.entry(uid.clone()).or_insert_with(|| {
                node_file(
                    &dag,
                    req.resolved,
                    &root,
                    primary.as_deref(),
                    uid,
                    &mut disk,
                    &mut warnings,
                )
                .map(|path| {
                    (
                        dag.detail(uid)
                            .and_then(|d| d.original_file_path.clone())
                            .unwrap_or_default(),
                        path,
                    )
                })
            });
            if gated.is_none() {
                continue;
            }
            let edits = dbtl::plan_node_edits(&dag, &resolver, uid, rules);
            if edits.is_empty() {
                warnings.insert(format!(
                    "--fix: {uid}: no safe edit for the finding(s) — left unchanged"
                ));
                continue;
            }
            let Some(text) = dbtl::apply_fixes(raw, &edits, req.dialect) else {
                warnings.insert(format!(
                    "--fix: {uid}: planned edits overlap or fail the re-parse — left unchanged"
                ));
                continue;
            };
            if text != raw {
                cands.push(Cand {
                    uid: uid.clone(),
                    rules: rules.clone(),
                    text,
                });
            }
        }
        if cands.is_empty() {
            hit_cap = false;
            break;
        }
        // Tentative overlay → rebuild → re-evaluate: the accepted set
        // is what the next round's dag must describe exactly.
        let mut candidate = overlay.clone();
        for c in &cands {
            candidate.insert(c.uid.clone(), c.text.clone());
        }
        let (post_manifest, dropped) = patched_manifest(&manifest, &candidate, req.resolved);
        let (post_dag, _) = dbtl::dag_from_manifest(&post_manifest);
        let post_dag = req.src.scope.apply_with_warnings(post_dag).0;
        let post_resolver = dbtl::SqlParseResolver::new(req.catalog.clone(), req.dialect);
        let post = evaluate_lint(
            &post_dag,
            req.guard_cfg,
            req.base,
            req.baseline,
            req.fail_on,
            LintAux {
                sql: Some(&post_resolver),
                catalog: req.catalog.as_deref(),
                base_sql: req.base_sql,
            },
            None,
        );
        // The "no new findings" guard: a fix may only REMOVE
        // findings under the rules it ran — anything new under any
        // other rule means the round's edits broke something. New
        // findings under the fixed rules are legal (the `a`-deleted-
        // makes-`b`-unread cascade the fixpoint exists for).
        let round_rules: BTreeSet<dbtl::LintRule> =
            cands.iter().flat_map(|c| c.rules.iter().copied()).collect();
        // The guard's pre-edit key set is evaluated under the FULL lint
        // set — no `--rules` overlay, no `--select` — or an introduced
        // finding a narrowed run can't see would slip through. It runs
        // only once a round has real candidates.
        let guard_pre = evaluate_lint(
            &dag,
            req.guard_cfg,
            req.base,
            req.baseline,
            req.fail_on,
            LintAux {
                sql: Some(&resolver),
                catalog: req.catalog.as_deref(),
                base_sql: req.base_sql,
            },
            None,
        );
        let pre_keys: BTreeSet<_> = guard_pre.findings.iter().map(finding_key).collect();
        let introduced: Vec<&dbtl::Finding> = post
            .findings
            .iter()
            .filter(|f| !pre_keys.contains(&finding_key(f)) && !round_rules.contains(&f.rule))
            .collect();
        // Attribute each introduced finding to the file that caused it:
        // the finding's own uid when that's an edited node (post-edit
        // text judges differently — a shadowed name unmasked, say), else
        // the edited node whose patched text dropped that parent (the
        // deleted CTE was its last reader — `unused_models` fires a
        // node over, on the ORPHANED side of the lost edge).
        let mut culprits: BTreeSet<String> = BTreeSet::new();
        let mut unattributed = false;
        for f in &introduced {
            let mut named = false;
            for c in &cands {
                let owns = c.uid == f.node_uid
                    || dropped
                        .get(&c.uid)
                        .is_some_and(|ps| ps.contains(&f.node_uid));
                if owns {
                    culprits.insert(c.uid.clone());
                    named = true;
                }
            }
            unattributed |= !named;
        }
        let rule_names = |introduced: &[&dbtl::Finding]| {
            introduced
                .iter()
                .map(|f| f.rule.name())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join("/")
        };
        let mut changed = false;
        for c in cands {
            if unattributed || culprits.contains(&c.uid) {
                // The `;` kill is a single `;` byte — it drops no
                // parent and unmasks no token, so every attributable
                // culprit did it with a CTE delete. Keep the file's
                // other-rule edits (`;` fixes stay provably side-
                // effect-free); only the `unused_cte` delete reverts.
                let reduced: BTreeSet<dbtl::LintRule> = c
                    .rules
                    .iter()
                    .copied()
                    .filter(|r| *r != dbtl::LintRule::UnusedCte)
                    .collect();
                let mut kept_other = false;
                if !reduced.is_empty() {
                    if let Some(raw) = dag.raw_code(&c.uid) {
                        let edits = dbtl::plan_node_edits(&dag, &resolver, &c.uid, &reduced);
                        if !edits.is_empty() {
                            if let Some(text) = dbtl::apply_fixes(raw, &edits, req.dialect) {
                                if text != raw {
                                    kept_other = true;
                                    overlay.insert(c.uid.clone(), text);
                                }
                            }
                        }
                    }
                }
                // Warn only when an edit was actually reverted — a
                // `;`-only file kept its fix (provably side-effect-free)
                // and has nothing to say.
                if reduced.len() < c.rules.len()
                    && warned_culprits.insert(format!("{}:{}", c.uid, rule_names(&introduced)))
                {
                    warnings.insert(format!(
                        "--fix: {}: the unused_cte edit would introduce a new {} finding — reverted{}",
                        c.uid,
                        rule_names(&introduced),
                        if kept_other { " (other fixes kept)" } else { "" }
                    ));
                }
                changed |= kept_other;
                continue;
            }
            overlay.insert(c.uid, c.text);
            changed = true;
        }
        if !changed {
            hit_cap = false;
            break;
        }
        dag = patched_dag(&manifest, &overlay, req.resolved, req.src.scope);
    }
    // The final evaluation counts what's left — `fixed` is the findings
    // that disappeared, not the edits attempted.
    let mut files = Vec::new();
    let mut fixed_findings = 0usize;
    if !overlay.is_empty() {
        for uid in overlay.keys() {
            // Every accepted uid passed `node_file` — a `(ofp, path)`
            // pair and its disk bytes both exist by construction.
            let Some((ofp, path)) = checked.get(uid).and_then(|g| g.as_ref()) else {
                continue;
            };
            let Some(before) = disk.get(path).cloned().flatten() else {
                continue;
            };
            files.push(FixFile {
                ofp: ofp.clone(),
                path: path.clone(),
                before,
                after: overlay[uid].clone(),
            });
        }
        files.sort_by(|a, b| a.ofp.cmp(&b.ofp));
        if write {
            let (written, skipped) = write_fixed(&files)?;
            warnings.extend(skipped);
            if written.len() < files.len() {
                // A file that drifted mid-run kept its old bytes — its
                // edits leave the overlay so the report dag (and the
                // fixed count) describe what's really on disk.
                let keep: BTreeSet<&PathBuf> = written.iter().collect();
                files.retain(|f| keep.contains(&f.path));
                overlay.retain(|uid, _| {
                    checked
                        .get(uid)
                        .and_then(|g| g.as_ref())
                        .is_some_and(|(_, p)| keep.contains(p))
                });
                dag = patched_dag(&manifest, &overlay, req.resolved, req.src.scope);
            }
        }
        let resolver = dbtl::SqlParseResolver::new(req.catalog.clone(), req.dialect);
        let fin = evaluate_lint(
            &dag,
            req.cfg,
            req.base,
            req.baseline,
            req.fail_on,
            LintAux {
                sql: Some(&resolver),
                catalog: req.catalog.as_deref(),
                base_sql: req.base_sql,
            },
            req.selection,
        );
        let fixable_post = fin.findings.iter().filter(|f| f.rule.fixable()).count();
        fixed_findings = fixable_pre.saturating_sub(fixable_post);
        // A round hitting the cap that still fixed everything IS a
        // convergence — only warn when fixable findings remain.
        if hit_cap && fixable_post > 0 {
            warnings.insert(format!(
                "--fix: stopped after {MAX_FIX_ROUNDS} rounds without converging — \
                 findings that remain are reported"
            ));
        }
    }
    Ok(FixOutcome {
        dag,
        warnings: warnings.into_iter().collect(),
        files,
        fixed_findings,
    })
}

/// `--fix`'s write: a sibling temp file + `rename`, so a crash mid-write
/// can never leave a half-written `.sql`. The `before` bytes are
/// re-checked first — a file that drifted between the read and the write
/// is skipped with a warning, never stomped (the compare→rename gap
/// can't close fully on POSIX; the re-read narrows it to microseconds).
/// Returns the written paths plus the skip warnings.
fn write_fixed(files: &[FixFile]) -> Result<(Vec<PathBuf>, Vec<String>), CliError> {
    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for f in files {
        let current = std::fs::read_to_string(&f.path).ok();
        if current.as_deref() != Some(f.before.as_str()) {
            skipped.push(format!(
                "--fix: {}: file changed on disk during the run — left unchanged",
                f.ofp
            ));
            continue;
        }
        let name = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let tmp = f.path.with_file_name(format!(".{name}.dbtl-tmp"));
        if let Err(e) = std::fs::write(&tmp, &f.after) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e)
                .with_context(|| format!("failed to write {}", tmp.display()))
                .input();
        }
        // The rename would otherwise drop the file's mode (execute bit,
        // restrictive umask) — carry the original permissions over. A
        // file we can't re-permission stays untouched rather than land
        // with a wider default mode.
        if let Ok(meta) = std::fs::metadata(&f.path) {
            if std::fs::set_permissions(&tmp, meta.permissions()).is_err() {
                let _ = std::fs::remove_file(&tmp);
                skipped.push(format!(
                    "--fix: {}: could not carry the file's permissions to the replacement — left unchanged",
                    f.ofp
                ));
                continue;
            }
        }
        if let Err(e) = std::fs::rename(&tmp, &f.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e)
                .with_context(|| format!("failed to replace {}", f.path.display()))
                .input();
        }
        written.push(f.path.clone());
    }
    Ok((written, skipped))
}

/// The `--fix-dry-run` preview for one file — a unified-diff-shaped
/// listing: `---`/`+++` carry the project-relative path, hunks hold 3
/// lines of context, and a last line lacking its newline gets the
/// conventional `\ No newline at end of file` marker. Line-based LCS —
/// enough for "what would change", not a `patch(1)` byte-perfect copy.
pub(super) fn unified_diff(ofp: &str, before: &str, after: &str) -> String {
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let (n, m) = (a.len(), b.len());
    // The LCS table is O(n·m) — a generated SQL file over ~10k lines a
    // side would allocate hundreds of MB for a preview. Past the cap a
    // coarse whole-file hunk shows the same before/after.
    const MAX_LCS_CELLS: usize = 4_000_000;
    if n.saturating_mul(m) > MAX_LCS_CELLS {
        let mut out = format!("--- a/{ofp}\n+++ b/{ofp}\n@@ -1,{n} +1,{m} @@\n");
        for line in &a {
            emit_diff_line(&mut out, '-', line);
        }
        for line in &b {
            emit_diff_line(&mut out, '+', line);
        }
        return out;
    }
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    // Op walk: `K` keep, `D` delete from `a`, `I` insert from `b` — each
    // carries its own index so hunks can compute the `@@` ranges.
    #[derive(Clone, Copy)]
    enum Op {
        K(usize, usize),
        D(usize),
        I(usize),
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::K(i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::D(i));
            i += 1;
        } else {
            ops.push(Op::I(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::D(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::I(j));
        j += 1;
    }
    let a_line = |o: &Op| matches!(o, Op::K(..) | Op::D(_));
    let b_line = |o: &Op| matches!(o, Op::K(..) | Op::I(_));
    // Group into hunks: changed ops plus CTX keep-lines either side,
    // two changes merging when their context windows would touch.
    const CTX: usize = 3;
    let mut out = format!("--- a/{ofp}\n+++ b/{ofp}\n");
    let mut idx = 0usize;
    while idx < ops.len() {
        let mut s = idx;
        while s < ops.len() && matches!(ops[s], Op::K(..)) {
            s += 1;
        }
        if s == ops.len() {
            break;
        }
        let mut e = s;
        loop {
            let mut g = e + 1;
            while g < ops.len() && matches!(ops[g], Op::K(..)) && g - e <= 2 * CTX {
                g += 1;
            }
            if g < ops.len() && !matches!(ops[g], Op::K(..)) {
                e = g;
            } else {
                break;
            }
        }
        let hunk_start = s.saturating_sub(CTX);
        let mut hunk_end = e + 1;
        let mut kept = 0;
        while hunk_end < ops.len() && matches!(ops[hunk_end], Op::K(..)) && kept < CTX {
            hunk_end += 1;
            kept += 1;
        }
        let hunk = &ops[hunk_start..hunk_end];
        let o_count = hunk.iter().filter(|o| a_line(o)).count();
        let n_count = hunk.iter().filter(|o| b_line(o)).count();
        // `@@` starts: the first a/b-carrying op's 1-based index — or,
        // for a pure-insert/delete hunk, the count of that side's lines
        // consumed before the hunk (the `-0,0` insertion convention).
        let o_start = hunk
            .iter()
            .find_map(|o| match o {
                Op::K(ai, _) | Op::D(ai) => Some(ai + 1),
                _ => None,
            })
            .unwrap_or_else(|| ops[..hunk_start].iter().filter(|o| a_line(o)).count());
        let n_start = hunk
            .iter()
            .find_map(|o| match o {
                Op::K(_, bi) | Op::I(bi) => Some(bi + 1),
                _ => None,
            })
            .unwrap_or_else(|| ops[..hunk_start].iter().filter(|o| b_line(o)).count());
        out += &format!("@@ -{o_start},{o_count} +{n_start},{n_count} @@\n");
        for op in hunk {
            match *op {
                Op::K(ai, _) => emit_diff_line(&mut out, ' ', a[ai]),
                Op::D(ai) => emit_diff_line(&mut out, '-', a[ai]),
                Op::I(bi) => emit_diff_line(&mut out, '+', b[bi]),
            }
        }
        idx = hunk_end;
    }
    out
}

/// One diff body line — the prefix char, the line text, and the
/// end-of-file marker when the line carries no `\n`.
fn emit_diff_line(out: &mut String, prefix: char, line: &str) {
    out.push(prefix);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
        out.push_str("\\ No newline at end of file\n");
    }
}
