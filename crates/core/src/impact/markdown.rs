//! The [`DiffReport`] → Markdown renderer: the file `D`→`e` writes and the
//! stdout `dbtl impact` prints — a pure formatter over the already-sorted
//! payload, so re-emitting the same diff is byte-identical and empty
//! sections are omitted to keep the report tight.

use super::report::DiffReport;
use crate::{ColumnChangeClass, ColumnChangeKind};

/// The PR Impact Report as Markdown — the file `D`→`e` writes and the stdout
/// `dbtl impact` prints. A pure formatter over the [`DiffReport`] payload (all
/// names already resolved and sorted) plus the project name for the title, so
/// the export is byte-identical to what the modal renders and re-emitting the
/// same diff is deterministic. Empty sections are omitted so the report stays
/// tight.
pub fn pr_impact_markdown(project: &str, report: &DiffReport) -> String {
    let pr = &report.pr;
    let mut out = format!("# PR Impact Report: {project}\n\n");
    out.push_str(&format!("- baseline: `{}`\n", report.baseline));
    out.push_str(&format!("- scope: {}\n", report.scope.describe()));
    out.push_str(&format!(
        "- changed: +{} added, ~{} modified, -{} removed\n",
        report.added.len(),
        report.modified.len(),
        report.removed.len()
    ));
    out.push_str(&format!(
        "- edges: +{} / -{}\n",
        report.edges_added.len(),
        report.edges_removed.len()
    ));
    out.push_str(&format!("- affected models: {}\n", pr.affected_models));
    if !pr.affected_marts.is_empty() {
        out.push_str(&format!("- affected marts: {}\n", pr.affected_marts.len()));
    }
    if !pr.affected_exposures.is_empty() {
        out.push_str(&format!(
            "- affected exposures: {}\n",
            pr.affected_exposures.len()
        ));
    }
    if !pr.affected_owners.is_empty() {
        out.push_str(&format!(
            "- affected owners: {}\n",
            pr.affected_owners.len()
        ));
    }

    // The changed-node listings, mirroring the `D` modal sections. A list of
    // `(title, rows)`; empty sections are skipped (a clean diff prints none).
    let node_row = |name: &str, kind: &str| {
        if kind.is_empty() {
            format!("- {name}")
        } else {
            format!("- {name} ({kind})")
        }
    };
    let sections: [(&str, Vec<String>); 5] = [
        (
            "Added",
            report.added.iter().map(|(n, k)| node_row(n, k)).collect(),
        ),
        (
            "Modified",
            report
                .modified
                .iter()
                .map(|(n, reasons)| format!("- {n}: {reasons}"))
                .collect(),
        ),
        (
            "Removed",
            report.removed.iter().map(|(n, k)| node_row(n, k)).collect(),
        ),
        (
            "Edges added",
            report
                .edges_added
                .iter()
                .map(|(p, c)| format!("- {p} -> {c}"))
                .collect(),
        ),
        (
            "Edges removed",
            report
                .edges_removed
                .iter()
                .map(|(p, c)| format!("- {p} -> {c}"))
                .collect(),
        ),
    ];
    for (title, rows) in &sections {
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {title} ({})\n", rows.len()));
        for row in rows {
            out.push_str(row);
            out.push('\n');
        }
    }

    // The column-level refinement — the reviewer's "which downstream columns
    // / exposures break" section. One bullet per classified change, with the
    // downstream columns (the via-list), affected models/marts, and
    // exposures it reaches indented under it.
    if !report.column_changes.is_empty() {
        out.push_str(&format!(
            "\n## Column changes ({})\n",
            report.column_changes.len()
        ));
        for ch in &report.column_changes {
            let name = match &ch.renamed_from {
                Some(old) => format!("`{}.{} -> {}`", ch.node_name, old, ch.column),
                None => format!("`{}.{}`", ch.node_name, ch.column),
            };
            let what = match ch.kind {
                ColumnChangeKind::Removed => "removed".to_string(),
                ColumnChangeKind::Renamed => "renamed".to_string(),
                ColumnChangeKind::TypeChanged => format!(
                    "type changed ({} -> {})",
                    ch.type_from.as_deref().unwrap_or("(unknown)"),
                    ch.type_to.as_deref().unwrap_or("(unknown)")
                ),
                ColumnChangeKind::ExprChanged => "expression changed".to_string(),
                ColumnChangeKind::Added => "added".to_string(),
            };
            let class = match ch.class {
                ColumnChangeClass::Breaking => "**breaking**".to_string(),
                ColumnChangeClass::Partial => "*partial-breaking*".to_string(),
                ColumnChangeClass::NonBreaking => "non-breaking".to_string(),
            };
            out.push_str(&format!("- {name}: {what} - {class}\n"));
            if !ch.affected_columns.is_empty() {
                let cols = ch
                    .affected_columns
                    .iter()
                    .map(|c| format!("`{c}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "  - downstream cols ({}): {cols}\n",
                    ch.affected_columns.len()
                ));
            }
            if !ch.affected_models.is_empty() {
                out.push_str(&format!(
                    "  - affected models: {}\n",
                    ch.affected_models.join(", ")
                ));
            }
            if !ch.affected_marts.is_empty() {
                out.push_str(&format!(
                    "  - affected marts: {}\n",
                    ch.affected_marts.join(", ")
                ));
            }
            if !ch.affected_exposures.is_empty() {
                out.push_str(&format!(
                    "  - exposures: {}\n",
                    ch.affected_exposures.join(", ")
                ));
            }
        }
    }

    out.push_str(&format!(
        "\n## Blast radius ({} affected models)\n",
        pr.affected_models
    ));
    if pr.affected_marts.is_empty() {
        out.push_str("- no affected marts\n");
    } else {
        out.push_str(&format!(
            "\n### Affected marts ({})\n",
            pr.affected_marts.len()
        ));
        for name in &pr.affected_marts {
            out.push_str(&format!("- {name}\n"));
        }
    }

    if !pr.affected_exposures.is_empty() {
        out.push_str(&format!(
            "\n## Affected exposures ({})\n",
            pr.affected_exposures.len()
        ));
        for e in &pr.affected_exposures {
            out.push_str(&format!("- {e}\n"));
        }
    }

    if !pr.affected_owners.is_empty() {
        out.push_str(&format!(
            "\n## Affected owners ({})\n",
            pr.affected_owners.len()
        ));
        for o in &pr.affected_owners {
            out.push_str(&format!("- {o}\n"));
        }
    }

    out.push_str("\n## Suggested CI command\n");
    match &pr.ci_command {
        Some(cmd) => out.push_str(&format!("```sh\n{cmd}\n```\n")),
        None => out.push_str("- no buildable changes\n"),
    }
    if let Some(cmd) = &pr.ci_command_columns {
        // The column-narrowed selector: the changed nodes plus ONLY the
        // models their changed columns reach — printed even when it equals
        // the wide one, so the readout always shows the column analysis.
        out.push_str(&format!(
            "- column-level ({} model{} reached via changed columns): `{cmd}`\n",
            pr.column_affected_models,
            if pr.column_affected_models == 1 {
                ""
            } else {
                "s"
            }
        ));
    }

    // Risk flags — each subsection only when it has entries.
    let has_risk = !pr.untested_changes.is_empty()
        || !pr.changed_hubs.is_empty()
        || !pr.new_layer_violations.is_empty();
    if has_risk {
        out.push_str("\n## Risk flags\n");
        if !pr.untested_changes.is_empty() {
            out.push_str(&format!(
                "\n### Untested changes ({})\n",
                pr.untested_changes.len()
            ));
            for name in &pr.untested_changes {
                out.push_str(&format!("- {name}\n"));
            }
        }
        if !pr.changed_hubs.is_empty() {
            out.push_str(&format!("\n### Changed hubs ({})\n", pr.changed_hubs.len()));
            for (name, count) in &pr.changed_hubs {
                out.push_str(&format!("- {name} ({count} downstream)\n"));
            }
        }
        if !pr.new_layer_violations.is_empty() {
            out.push_str(&format!(
                "\n### New layer violations ({})\n",
                pr.new_layer_violations.len()
            ));
            for (p, c) in &pr.new_layer_violations {
                out.push_str(&format!("- {p} -> {c}\n"));
            }
        }
    }

    out
}
