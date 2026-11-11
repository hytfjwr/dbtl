use super::*;

/// Run the `columns` subcommand: resolve the data source (shared with the
/// TUI), split `<model>.<column>`, and print the column's upstream or
/// downstream [`dbtl::ColumnReport`] — Markdown by default (the SAME bytes
/// the `yl` yank writes), `--format json` for automation. Everything is
/// `Err`-shaped like the other subcommands: a bad model or column name
/// reports as plain text with a non-zero exit, never a panic or a TTY probe.
#[allow(clippy::too_many_arguments)]
pub fn run_columns(
    target: &str,
    upstream: bool,
    format: ImpactFormat,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    include_packages: bool,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> Result<()> {
    let (model, column) = target
        .rsplit_once('.')
        .with_context(|| format!("columns: expected <model>.<column>, got '{target}'"))?;
    anyhow::ensure!(
        !model.is_empty() && !column.is_empty(),
        "columns: expected <model>.<column>, got '{target}'"
    );
    let scope = if include_packages {
        dbtl::PackageScope::All
    } else {
        dbtl::PackageScope::Root
    };
    let resolved = resolve_source(manifest, source, project, scope)?;
    let dag = &resolved.dag;

    // The model segment: an exact unique_id first, else the unique node NAME
    // (dbt names are project-unique; two matches → ambiguous, so bail and
    // tell the caller to pass the uid).
    let uid = if dag.contains(model) {
        model.to_string()
    } else {
        let mut hits: Vec<&str> = dag
            .nodes()
            .values()
            .filter(|n| n.name == model)
            .map(|n| n.unique_id.as_str())
            .collect();
        hits.sort();
        match hits.as_slice() {
            [one] => (*one).to_string(),
            [] => anyhow::bail!("columns: no node named '{model}'"),
            _ => anyhow::bail!(
                "columns: '{model}' is ambiguous ({}); pass the unique_id",
                hits.join(", ")
            ),
        }
    };

    // The CLL resolver, built exactly like the app's: the auto-detected
    // `target/catalog.json` (when present) is the top column-list tier. The
    // parser-backed resolver resolves per model and falls back to the
    // name-match approximation whenever that model's SQL is missing or
    // unparseable — the count is reported on stderr below.
    let resolver = cll_resolver(&resolved, sql_dialect);

    let col = dbtl::ColumnRef::new(&uid, column);
    let known = resolver.columns(dag, &uid);
    anyhow::ensure!(
        known.contains(&col.column),
        "columns: '{model}' has no column '{column}' (known: {})",
        if known.is_empty() {
            "(none — the node has no resolvable columns)".to_string()
        } else {
            known.join(", ")
        }
    );

    let direction = if upstream {
        dbtl::ClosureDirection::Upstream
    } else {
        dbtl::ClosureDirection::Downstream
    };
    let report = dbtl::closure_report(dag, &resolver, &col, direction);
    match format {
        ImpactFormat::Markdown => print!("{report}"),
        ImpactFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    // Parser coverage transparency: how many models ended up on the Issue 07
    // name-match approximation (missing/unparseable SQL). Status goes to
    // stderr so stdout stays machine-clean for both formats.
    let fallbacks = resolver.fallback_count();
    if fallbacks > 0 {
        eprintln!(
            "columns: {fallbacks} model{} resolved by name-match fallback (missing/unparseable SQL)",
            if fallbacks == 1 { "" } else { "s" }
        );
    }
    Ok(())
}
