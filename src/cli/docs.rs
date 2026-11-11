use super::*;

/// Run the `docs` subcommand: resolve the data source (shared with the TUI),
/// generate the Markdown doc set in memory (the pure [`dbtl::generate_docs`]),
/// then write each file under `out` — creating the directory tree as needed and
/// overwriting existing pages. `--check` writes nothing and exits 1 when the
/// on-disk set differs; `--clean` additionally deletes marked generated files
/// the run did not regenerate (stale pages of removed models). Prints a
/// one-line summary to stdout on success. Every failure (load error, IO error)
/// returns `Err`, which `main` reports as plain text + the exit code its
/// [`ExitKind`] assigns — no panic, no terminal init.
pub fn run_docs(
    out: &str,
    src: &DataSourceArgs,
    lineage_depth: Option<&str>,
    quiet: bool,
    clean: bool,
    check: bool,
) -> Result<(), CliError> {
    let scope = src.scope;
    let resolved = src.resolve()?;
    let provenance = if resolved.is_source {
        format!("project source ({})", resolved.label)
    } else {
        format!("manifest ({})", resolved.label)
    };
    let project_name = docs_project_name(&resolved.dag, &resolved.label);
    // `.dbtl.yml` once — it supplies BOTH the lint config and
    // `docs.lineage_depth`. Malformed is fatal: a committed doc set must
    // never describe the wrong rule set or draw the wrong depth.
    let (manifest_arg, project_arg) = resolved_config_args(&resolved);
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
    // The README's Lint section + per-node violation lists quote the SAME
    // `run_rules` vector `check --lint`, `dbtl lint`, and the TUI read — and
    // the baseline resolves through the same path/`baseline_explicit` rules
    // the lint subcommand uses. A missing conventional file freezes nothing;
    // a named-but-absent file warns; an unreadable/malformed one is an input
    // error (the same policy — a frozen set we can't read must never
    // silently un-freeze every violation in the record).
    let mut lint_findings = dbtl::run_rules(dbtl::LintInput::new(&resolved.dag, &project_cfg.lint));
    let candidates =
        dbtl::project_config_candidates(manifest_arg.as_deref(), project_arg.as_deref());
    let baseline = load_lint_baseline_file(
        &lint_baseline_path(&project_cfg.lint.baseline, pc.path.is_some(), &candidates),
        project_cfg.baseline_explicit,
    )?;
    if let Some(b) = &baseline {
        b.apply(&mut lint_findings);
    }
    let files = dbtl::generate_docs_with_options(
        &resolved.dag,
        &project_name,
        &provenance,
        scope,
        &dbtl::DocsOptions {
            findings: Some(&lint_findings),
            lineage_depth: depth,
        },
    );

    let out_dir = PathBuf::from(out);
    if check {
        return check_docs(&out_dir, &files, quiet);
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
    if clean {
        let removed = remove_stale_docs(&out_dir, &files)?;
        if !quiet && !removed.is_empty() {
            println!("removed {} stale file(s)", removed.len());
        }
    }
    // The success summary is the only stdout `docs` writes; `--quiet` drops it
    // for CI. Errors are returned as `Err` and printed to stderr by `main`
    // either way, so silencing stdout never hides a failure.
    if !quiet {
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

/// `docs --check`: compare the in-memory generated set against what's on disk
/// without writing anything. Prints one sorted line per difference — a
/// `missing:` page, a `changed:` page, or a `stale:` marked file — then
/// returns a gate failure (the check ran and the invariant did not hold).
/// An exact match exits 0.
pub(super) fn check_docs(
    out_dir: &std::path::Path,
    files: &[(String, String)],
    quiet: bool,
) -> Result<(), CliError> {
    let expected: std::collections::HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let mut problems: Vec<String> = Vec::new();

    for (rel, want) in files {
        match std::fs::read_to_string(out_dir.join(rel)) {
            Err(_) => problems.push(format!("missing: {rel}")),
            Ok(have) if have != *want => problems.push(format!("changed: {rel}")),
            Ok(_) => {}
        }
    }
    for rel in marked_docs_on_disk(out_dir) {
        if !expected.contains(rel.as_str()) {
            problems.push(format!("stale: {rel}"));
        }
    }

    if problems.is_empty() {
        if !quiet {
            println!("docs up to date ({} files)", files.len());
        }
        return Ok(());
    }
    problems.sort();
    for p in &problems {
        println!("{p}");
    }
    Err(CliError::gate(anyhow::anyhow!(
        "docs are out of date: {} difference(s)",
        problems.len()
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
