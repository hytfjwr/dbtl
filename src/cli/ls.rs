//! `dbtl ls <SELECTOR>...` — the non-interactive selector listing: evaluate
//! the selector(s) against the resolved dag and print the matching nodes. The
//! ONE strict grammar [`dbtl::validate_selector`] + evaluation
//! [`dbtl::select`] every `--select`/`--exclude` flag uses, so `ls` shows
//! exactly what another command's selection would pick.

use std::collections::BTreeSet;

use super::*;

/// The `state:` caveat `ls` prints to stderr when a `state:` spec runs —
/// the same sentence the `Ls` variant's `--help` doc carries, so the
/// warning and the help can't drift. stderr only: stdout stays pipeable.
const STATE_NOTE: &str = "note: state: selectors are resolved by dbtl's own \
     comparison (the SQL, config, column, and macro changes dbtl can read) — \
     the set may not match dbt's `state:modified`.";

/// Run `dbtl ls`: validate the specs, resolve the source (+ the `--diff`
/// baseline when `state:` is in play), evaluate, and print per `--output`.
/// `--diff` exists on `ls` ONLY to feed `state:` selectors — pairing either
/// half without the other is a usage error, never silently ignored.
pub fn run_ls(
    selector: &[String],
    exclude: &[String],
    output: LsFormat,
    src: &DataSourceArgs,
) -> Result<(), CliError> {
    validate_selector_specs(selector, "selector")?;
    validate_selector_specs(exclude, "--exclude")?;
    let any_state = specs_use_state(selector) || specs_use_state(exclude);
    require_diff_for_state(selector, src.diff.is_some())?;
    require_diff_for_state(exclude, src.diff.is_some())?;
    if src.diff.is_some() && !any_state {
        return Err(CliError::usage(anyhow::anyhow!(
            "--diff on 'dbtl ls' is only meaningful with a 'state:' selector"
        )));
    }
    let resolved = src.resolve()?;
    let dag = &resolved.dag;
    // The `state:` comparison runs the SAME resolver pair `impact`/`check`
    // use, so `state:modified` selects exactly the change set the impact
    // report counts.
    let diff = match src.diff {
        Some(d) => {
            let (base, _) = load_baseline(d, src.scope, &src.project_dir())?;
            Some(selector_diff(src, &resolved, &base)?)
        }
        None => None,
    };
    if any_state {
        eprintln!("{STATE_NOTE}");
    }
    let (mut selected, notes) = selector_set_report(dag, diff.as_ref(), selector);
    let (excluded, exclude_notes) = selector_set_report(dag, diff.as_ref(), exclude);
    // One `warning:` line per distinct deprecated spec across both lists.
    let notes: BTreeSet<String> = notes.into_iter().chain(exclude_notes).collect();
    warn_each("", &notes.into_iter().collect::<Vec<_>>());
    for uid in excluded {
        selected.remove(&uid);
    }
    let mut out = String::new();
    match output {
        LsFormat::Names => {
            // (name, unique_id) rows — same-name nodes both print, in a
            // deterministic order. A versioned model shows its `name.vseg`
            // (`list_name` — the spelling lint/TUI/docs share) so the
            // versions never read as identical rows.
            let mut rows: Vec<(String, &str)> = selected
                .iter()
                .filter_map(|uid| {
                    dag.get(uid)
                        .map(|n| (dbtl::list_name(uid, &n.name), uid.as_str()))
                })
                .collect();
            rows.sort();
            for (name, _) in rows {
                out.push_str(&name);
                out.push('\n');
            }
        }
        LsFormat::Uids => {
            for uid in &selected {
                out.push_str(uid);
                out.push('\n');
            }
        }
        LsFormat::Selector => {
            // Paste-ready for `dbt build --select`: buildable resources
            // only, deduplicated, sorted. A versioned model emits its
            // `name.vseg` (dbt selects `fct_orders.v1` to that one
            // version) so the output round-trips the exact selection —
            // the bare name would re-select every version.
            let names: BTreeSet<String> = selected
                .iter()
                .filter_map(|uid| dag.get(uid))
                .filter(|n| matches!(n.resource_type.as_str(), "model" | "seed" | "snapshot"))
                .map(|n| dbtl::list_name(&n.unique_id, &n.name))
                .collect();
            if !names.is_empty() {
                out.push_str(&names.into_iter().collect::<Vec<_>>().join(" "));
                out.push('\n');
            }
        }
        LsFormat::Json => {
            let rows: Vec<serde_json::Value> = selected
                .iter()
                .map(|uid| {
                    let node = dag.get(uid);
                    serde_json::json!({
                        "unique_id": uid,
                        "name": node.map(|n| n.name.as_str()).unwrap_or_default(),
                        "resource_type": node.map(|n| n.resource_type.as_str()).unwrap_or_default(),
                        "materialized": node.and_then(|n| n.materialized.as_deref()),
                        "path": dag
                            .detail(uid)
                            .and_then(|d| d.original_file_path.as_deref()),
                    })
                })
                .collect();
            out.push_str(&serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()));
            out.push('\n');
        }
    }
    write_stdout(&out, anstream::ColorChoice::Auto)
}
