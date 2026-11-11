use std::collections::BTreeSet;

use super::*;

/// The `docs` subcommand's options, bundled like `CheckArgs`/`LintArgs` so
/// the runner's signature stays readable (the fields map 1:1 onto the clap
/// variant's args).
pub struct DocsArgs<'a> {
    /// `--out` — the output directory.
    pub out: &'a str,
    /// `--lineage-depth` — `N`/`all`, overrides `.dbtl.yml`.
    pub lineage_depth: Option<&'a str>,
    /// `--quiet`.
    pub quiet: bool,
    /// `--clean` — delete stale marked pages after writing.
    pub clean: bool,
    /// `--check` — compare instead of writing.
    pub check: bool,
    /// `--select` — generate only the matching nodes' pages.
    pub select: &'a [String],
    /// `--exclude` — subtract the matching nodes from the generated set.
    pub exclude: &'a [String],
}

/// What `docs --select`/`--exclude` narrowed the generated set to — every
/// `None` is the untouched whole-graph doc set (`check --docs-dir` passes
/// exactly that: a freshness gate compares the FULL set, selection or not).
#[derive(Default)]
pub(super) struct DocsScope<'a> {
    /// The generated node uid set (`None` = every node, byte-identical).
    pub nodes: Option<&'a BTreeSet<String>>,
    /// The `--select` spec(s) the README echoes.
    pub select_label: Option<&'a str>,
    /// The `--exclude` spec(s) the README echoes.
    pub exclude_label: Option<&'a str>,
}

/// Run the `docs` subcommand: resolve the data source (shared with the TUI),
/// generate the Markdown doc set in memory (the pure [`dbtl::generate_docs`]),
/// then write each file under `out` — creating the directory tree as needed and
/// overwriting existing pages. `--check` writes nothing and exits 1 when the
/// on-disk set differs; `--clean` additionally deletes marked generated files
/// the run did not regenerate (stale pages of removed models). Prints a
/// one-line summary to stdout on success. Every failure (load error, IO error)
/// returns `Err`, which `main` reports as plain text + the exit code its
/// [`ExitKind`] assigns — no panic, no terminal init.
pub fn run_docs(src: &DataSourceArgs, args: &DocsArgs) -> Result<(), CliError> {
    let scope = src.scope;
    // `--select`/`--exclude` carry the ONE strict selector grammar —
    // validated before the load so a typo is a usage error, never a
    // silently-empty page set. `--diff` on docs exists ONLY to feed `state:`
    // atoms: either half alone is a usage error (clap already rejects
    // --clean/--check combinations — a partial set can't run a full-set
    // comparison).
    validate_selector_specs(args.select, "--select")?;
    validate_selector_specs(args.exclude, "--exclude")?;
    let all_specs: Vec<String> = args
        .select
        .iter()
        .chain(args.exclude.iter())
        .cloned()
        .collect();
    require_diff_for_state(&all_specs, src.diff.is_some())?;
    if src.diff.is_some() && !specs_use_state(&all_specs) {
        return Err(CliError::usage(anyhow::anyhow!(
            "--diff on 'dbtl docs' is only meaningful with a 'state:' selector"
        )));
    }
    let resolved = src.resolve()?;
    let dag = &resolved.dag;
    // Only `state:` selectors reach here with `--diff` — the comparison
    // uses the same resolver pair `impact`/`check` build, so `state:` picks
    // the same change set the impact report counts.
    let diff = match src.diff {
        Some(d) => {
            let (base, _) = load_baseline(d, scope, &src.project_dir())?;
            Some(selector_diff(src, &resolved, &base)?)
        }
        None => None,
    };
    // The effective page set: `--select` resolves it, `--exclude` subtracts
    // (no --select = the whole graph minus the exclusions). An empty set
    // still writes the README — with a warning on stderr.
    let selecting = !args.select.is_empty() || !args.exclude.is_empty();
    let selected = if selecting {
        let mut set: BTreeSet<String> = if args.select.is_empty() {
            dag.nodes().keys().cloned().collect()
        } else {
            selector_set(dag, diff.as_ref(), args.select)
        };
        for uid in selector_set(dag, diff.as_ref(), args.exclude) {
            set.remove(&uid);
        }
        if set.is_empty() {
            eprintln!("warning: --select/--exclude matched no nodes — writing the README only");
        }
        Some(set)
    } else {
        None
    };
    let select_label = (!args.select.is_empty()).then(|| args.select.join(" "));
    let exclude_label = (!args.exclude.is_empty()).then(|| args.exclude.join(" "));
    let doc_scope = DocsScope {
        nodes: selected.as_ref(),
        select_label: select_label.as_deref(),
        exclude_label: exclude_label.as_deref(),
    };
    let files = docs_file_set(src, &resolved, scope, args.lineage_depth, &doc_scope)?;

    let out_dir = PathBuf::from(args.out);
    if args.check {
        return check_docs(&out_dir, &files, args.quiet);
    }

    for (rel_path, contents) in &files {
        let path = out_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if args.clean {
        let removed = remove_stale_docs(&out_dir, &files)?;
        if !args.quiet && !removed.is_empty() {
            println!("removed {} stale file(s)", removed.len());
        }
    }
    // The success summary is the only stdout `docs` writes; `--quiet` drops it
    // for CI. Errors are returned as `Err` and printed to stderr by `main`
    // either way, so silencing stdout never hides a failure.
    if !args.quiet {
        println!("wrote {} files to {}", files.len(), out_dir.display());
    }
    Ok(())
}

/// The dbtl-owned Markdown files under `out_dir`: every `*.md` file whose
/// contents start with [`dbtl::GENERATED_MARKER`], returned as sorted relative
/// paths (`/`-separated, matching the generated rel paths). Unmarked files —
/// hand-written docs sharing the dir — are invisible here, so `--clean` and
/// `--check` can never touch them.
pub(super) fn marked_docs_on_disk(out_dir: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, base, found);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !contents.starts_with(dbtl::GENERATED_MARKER) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(base) {
                found.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut found = Vec::new();
    walk(out_dir, out_dir, &mut found);
    found.sort();
    found
}

/// The in-memory file set `dbtl docs` would emit for `resolved`: the
/// provenance tag, the docs project name, the resolved lineage-diagram
/// depth (flag > `docs.lineage_depth` > default), the opt-in side artifacts
/// (`--catalog`/`--run-results`, or `.dbtl.yml`'s `docs.catalog` /
/// `docs.run_results` opting into the default `target/` sibling — the doc
/// pages NEVER auto-detect them, so a file the run wasn't asked about can't
/// change the output; an explicit request that isn't readable is an input
/// error, never a silent skip), the column resolver the `Upstream`/`Used
/// by` cells resolve through (that opt-in catalog inside it,
/// `--sql-dialect` pinning the parse dialect), and the SAME `run_rules`
/// vector `check --lint`, `dbtl lint`, and the TUI read — with the lint
/// baseline applied through the same path/`baseline_explicit` rules the
/// lint subcommand uses. `.dbtl.yml` is auto-detected beside the source, a
/// malformed file fatal: a committed doc set must never describe the wrong
/// rule set, draw the wrong depth, or silently claim an artifact it didn't
/// load. `check --docs-dir` compares exactly this set (no flag —
/// config/default depth and the config's own artifact opt-ins), so the
/// gate can never disagree with `docs --check` about what "current docs"
/// means. `src` supplies the aux inputs (`--sql-dialect`/`--catalog`/
/// `--run-results`); unlike `dbtl lint`/`check`, the lint feed and the
/// rendered pages share the run's ONE opt-in catalog — a sibling nobody
/// asked for can't leak findings (or types) into the generated set.
pub(super) fn docs_file_set(
    src: &DataSourceArgs,
    resolved: &ResolvedSource,
    scope: dbtl::PackageScope,
    lineage_depth: Option<&str>,
    sel: &DocsScope,
) -> Result<Vec<(String, String)>, CliError> {
    let provenance = if resolved.is_source {
        format!("project source ({})", resolved.label)
    } else {
        format!("manifest ({})", resolved.label)
    };
    let project_name = docs_project_name(&resolved.dag, &resolved.label);
    // `.dbtl.yml` once — it supplies BOTH the lint config and
    // `docs.lineage_depth`.
    let (manifest_arg, project_arg) = resolved_config_args(resolved);
    let pc = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?;
    let project_cfg = &pc.config;
    // Precedence: the flag wins, then `docs.lineage_depth`, then the
    // shipped default (2 hops).
    let depth = match lineage_depth {
        Some(text) => dbtl::LineageDepth::parse(text).ok_or_else(|| {
            CliError::usage(anyhow::anyhow!(
                "invalid --lineage-depth '{text}' (expected an integer >= 1 or 'all')"
            ))
        })?,
        None => project_cfg
            .docs
            .lineage_depth
            .unwrap_or(dbtl::LineageDepth::DEFAULT),
    };
    // A missing conventional baseline file freezes nothing; a
    // named-but-absent file warns; an unreadable/malformed one is an input
    // error (the same policy — a frozen set we can't read must never
    // silently un-freeze every violation in the record).
    // The opt-in catalog: an explicit flag wins; a `docs.catalog: true`
    // config key opts into the shared default sibling (`resolved_config_args`
    // supplies the same (manifest, project) pair `.dbtl.yml` discovery
    // used). The SAME instance feeds the lint evaluation AND the rendered
    // pages — never the bare auto-detect `lint`/`check` apply, so a sibling
    // nobody asked for changes neither output: the generated set stays
    // byte-identical with or without the file on disk.
    let catalog: Option<std::rc::Rc<dbtl::Catalog>> = docs_catalog(
        src,
        &project_cfg.docs,
        manifest_arg.as_deref(),
        project_arg.as_deref(),
    )?
    .map(std::rc::Rc::new);
    // One resolver for both surfaces — the opt-in catalog inside it (or
    // none), so the warehouse rules `skip` exactly when the pages would
    // have nothing to render.
    let resolver = parse_resolver(resolved, catalog.clone(), src.sql_dialect);
    let mut lint_findings = dbtl::run_rules(
        dbtl::LintInput::new(&resolved.dag, &project_cfg.lint)
            .with_sql(Some(&resolver))
            .with_catalog(catalog.as_deref()),
    )
    .findings;
    let candidates =
        dbtl::project_config_candidates(manifest_arg.as_deref(), project_arg.as_deref());
    let baseline = load_lint_baseline_file(
        &lint_baseline_path(&project_cfg.lint.baseline, pc.path.is_some(), &candidates),
        project_cfg.baseline_explicit,
    )?;
    if let Some(b) = &baseline {
        b.apply(&mut lint_findings);
    }
    let run_results = docs_run_results(
        src,
        &project_cfg.docs,
        manifest_arg.as_deref(),
        project_arg.as_deref(),
    )?;
    Ok(dbtl::generate_docs_with_options(
        &resolved.dag,
        &project_name,
        &provenance,
        scope,
        &dbtl::DocsOptions {
            findings: Some(&lint_findings),
            lineage_depth: depth,
            resolver: Some(&resolver),
            catalog: catalog.as_deref(),
            run_results: run_results.as_ref(),
            nodes: sel.nodes,
            select_label: sel.select_label,
            exclude_label: sel.exclude_label,
        },
    ))
}

/// The `catalog.json` the doc set consults — loaded ONLY when explicitly
/// requested (`--catalog`, else `.dbtl.yml`'s `docs.catalog` opting into the
/// default `target/catalog.json` sibling through the shared
/// [`dbtl::default_catalog_path`] rule). Docs never auto-detects the
/// sibling: a run that wasn't asked for warehouse data emits none — not in
/// the pages, and not in the lint evaluation they quote (the bare
/// auto-detect `dbtl lint`/`check` apply is deliberately different: their
/// rules `skip` on a missing catalog, but generated docs must be
/// byte-identical either way). An enabled-but-unreadable/malformed artifact
/// is an input error — a committed doc set must never silently claim data
/// it didn't load.
fn docs_catalog(
    src: &DataSourceArgs,
    cfg: &dbtl::DocsConfig,
    manifest_arg: Option<&std::path::Path>,
    project_arg: Option<&std::path::Path>,
) -> Result<Option<dbtl::Catalog>, CliError> {
    let path = if let Some(explicit) = src.catalog {
        Some(PathBuf::from(explicit))
    } else if cfg.catalog {
        dbtl::default_catalog_path(manifest_arg, project_arg)
    } else {
        None
    };
    path.map(|p| {
        dbtl::Catalog::load(&p)
            .with_context(|| format!("failed to load catalog {}", p.display()))
            .input()
    })
    .transpose()
}

/// The `run_results.json` the doc set consults — the same explicit-only rule
/// as [`docs_catalog`] (`--run-results`, else `docs.run_results` opting into
/// the default `target/run_results.json` sibling). Never auto-detected; an
/// enabled-but-unreadable/malformed artifact is an input error.
fn docs_run_results(
    src: &DataSourceArgs,
    cfg: &dbtl::DocsConfig,
    manifest_arg: Option<&std::path::Path>,
    project_arg: Option<&std::path::Path>,
) -> Result<Option<dbtl::RunResults>, CliError> {
    let path = if let Some(explicit) = src.run_results {
        Some(PathBuf::from(explicit))
    } else if cfg.run_results {
        dbtl::default_run_results_path(manifest_arg, project_arg)
    } else {
        None
    };
    path.map(|p| dbtl::RunResults::load(&p).input()).transpose()
}

/// The pure comparison half of `check_docs` — the sorted `missing` /
/// `changed` / `stale` page lists between the generated set and what's on
/// disk. `dbtl docs --check` and `check --docs-dir` share it verbatim, so
/// both report exactly the same divergence.
pub(super) fn docs_differences(out_dir: &std::path::Path, files: &[(String, String)]) -> DocsDrift {
    let expected: std::collections::HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let mut drift = DocsDrift::default();
    for (rel, want) in files {
        match std::fs::read_to_string(out_dir.join(rel)) {
            Err(_) => drift.missing.push(rel.clone()),
            Ok(have) if have != *want => drift.changed.push(rel.clone()),
            Ok(_) => {}
        }
    }
    for rel in marked_docs_on_disk(out_dir) {
        if !expected.contains(rel.as_str()) {
            drift.stale.push(rel);
        }
    }
    drift.missing.sort();
    drift.changed.sort();
    drift.stale.sort();
    drift
}

/// `docs --check`: compare the in-memory generated set against what's on disk
/// without writing anything. Prints one sorted line per difference — a
/// `missing:` page, a `changed:` page, or a `stale:` marked file — then
/// returns a gate failure (the check ran and the invariant did not hold).
/// An exact match exits 0. The difference list is the command's actionable
/// output, so it prints even under `--quiet`: quiet suppresses only the
/// success summary, never the report a CI run exists to read.
pub(super) fn check_docs(
    out_dir: &std::path::Path,
    files: &[(String, String)],
    quiet: bool,
) -> Result<(), CliError> {
    let drift = docs_differences(out_dir, files);
    if drift.total() == 0 {
        if !quiet {
            println!("docs up to date ({} files)", files.len());
        }
        return Ok(());
    }
    for p in drift.lines() {
        println!("{p}");
    }
    Err(CliError::gate(anyhow::anyhow!(
        "docs are out of date: {} difference(s)",
        drift.total()
    )))
}

/// `docs --clean`: delete marked generated files under `out_dir` that this run
/// did NOT regenerate (the stale pages of removed/renamed models), then prune
/// directories the deletions left empty. Returns the removed relative paths
/// (sorted). Only files carrying the generated marker are ever considered.
pub(super) fn remove_stale_docs(
    out_dir: &std::path::Path,
    files: &[(String, String)],
) -> Result<Vec<String>> {
    let expected: std::collections::HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let mut removed = Vec::new();
    for rel in marked_docs_on_disk(out_dir) {
        if expected.contains(rel.as_str()) {
            continue;
        }
        let path = out_dir.join(&rel);
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove stale {}", path.display()))?;
        // Prune the (possibly nested) parent dirs the deletion emptied, bottom
        // up, stopping at `out_dir` — `remove_dir` is a no-op error on a dir
        // that still holds files, so a half-emptied tree stops cleanly.
        let mut dir = path.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            if d == out_dir || std::fs::remove_dir(&d).is_err() {
                break;
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        removed.push(rel);
    }
    Ok(removed)
}

/// A project title for the docs index. Prefers the dbt project name read from
/// the loaded graph itself ([`dbtl::Dag::project_name`] — the manifest's
/// `metadata.project_name`, else a `unique_id`-segment heuristic) —
/// authoritative and identical in manifest and source mode. Falls back to a
/// label-derived heuristic (the dir name of a `<root>/target/manifest.json`,
/// else the path's file name) only when the graph has no name at all, and
/// finally to a generic title so the heading is never blank.
pub(super) fn docs_project_name(dag: &dbtl::Dag, label: &str) -> String {
    if let Some(name) = dag.project_name().filter(|s| !s.trim().is_empty()) {
        return name.to_string();
    }
    let path = std::path::Path::new(label);
    // Manifest path `<root>/target/manifest.json` → `<root>` dir name.
    if path.file_name().and_then(|f| f.to_str()) == Some("manifest.json") {
        if let Some(root) = path.parent().and_then(|p| p.parent()) {
            if let Some(name) = root.file_name().and_then(|f| f.to_str()) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    path.file_name()
        .and_then(|f| f.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("dbt project")
        .to_string()
}
