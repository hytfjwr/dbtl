use super::*;

/// Run the `columns` subcommand: resolve the data source (shared with the
/// TUI), split `<model>.<column>`, and print the column's upstream or
/// downstream [`dbtl::ColumnReport`] — Markdown by default (the SAME bytes
/// the `yl` yank writes), `--format json` for automation. A bad model or
/// column name is a usage error — the invocation named something that does
/// not exist; a named input failing to load is an input error.
pub fn run_columns(
    target: &str,
    upstream: bool,
    format: ImpactFormat,
    src: &DataSourceArgs,
) -> Result<(), CliError> {
    let (model, column) = target
        .rsplit_once('.')
        .with_context(|| format!("columns: expected <model>.<column>, got '{target}'"))
        .usage()?;
    if model.is_empty() || column.is_empty() {
        return Err(CliError::usage(anyhow::anyhow!(
            "columns: expected <model>.<column>, got '{target}'"
        )));
    }
    let resolved = src.resolve()?;
    let dag = &resolved.dag;

    // The model segment: an exact unique_id first, else the unique node NAME
    // (dbt names are project-unique; two matches → ambiguous, so bail and
    // tell the caller to pass the uid). The `list_name` spelling matches
    // too — `columns fct_orders.v1.id` addresses that one version, while
    // `fct_orders` still reports the family as ambiguous.
    let uid = if dag.contains(model) {
        model.to_string()
    } else {
        let mut hits: Vec<&str> = dag
            .nodes()
            .values()
            .filter(|n| n.name == model || dbtl::list_name(&n.unique_id, &n.name) == model)
            .map(|n| n.unique_id.as_str())
            .collect();
        hits.sort();
        match hits.as_slice() {
            [one] => (*one).to_string(),
            [] => {
                return Err(CliError::usage(anyhow::anyhow!(
                    "columns: no node named '{model}'"
                )))
            }
            _ => {
                return Err(CliError::usage(anyhow::anyhow!(
                    "columns: '{model}' is ambiguous ({}); pass the unique_id",
                    hits.join(", ")
                )))
            }
        }
    };

    // The CLL resolver, built exactly like the app's: an explicit `--catalog`
    // wins over the auto-detected `target/catalog.json` sibling, and the
    // catalog is the top column-list tier. The parser-backed resolver
    // resolves per model and falls back to the name-match approximation
    // whenever that model's SQL is missing or unparsable — the count is
    // reported on stderr below.
    let resolver = src.cll_resolver(&resolved)?;

    let col = dbtl::ColumnRef::new(&uid, column);
    let known = resolver.columns(dag, &uid);
    if !known.contains(&col.column) {
        return Err(CliError::usage(anyhow::anyhow!(
            "columns: '{model}' has no column '{column}' (known: {})",
            if known.is_empty() {
                "(none — the node has no resolvable columns)".to_string()
            } else {
                known.join(", ")
            }
        )));
    }

    let direction = if upstream {
        dbtl::ClosureDirection::Upstream
    } else {
        dbtl::ClosureDirection::Downstream
    };
    let report = dbtl::closure_report(dag, &resolver, &col, direction);
    match format {
        ImpactFormat::Markdown => write_stdout(&report.to_string(), anstream::ColorChoice::Auto)?,
        ImpactFormat::Json => write_stdout(
            &format!(
                "{}\n",
                serde_json::to_string_pretty(&report).map_err(|e| CliError::new(e.into()))?
            ),
            anstream::ColorChoice::Auto,
        )?,
    }
    // Parser coverage transparency: how many models ended up on the
    // name-match approximation (missing/unparsable SQL). Status goes to
    // stderr so stdout stays machine-clean for both formats.
    let fallbacks = resolver.fallback_count();
    if fallbacks > 0 {
        eprintln!(
            "columns: {fallbacks} model{} resolved by name-match fallback (missing/unparsable SQL)",
            if fallbacks == 1 { "" } else { "s" }
        );
    }
    Ok(())
}
