use super::*;

/// The fenced Mermaid block for `sg` (the shared
/// [`subgraph_mermaid_linked`] output — `click` navigation lines included).
/// `href_of` maps a target node's stored relative file path to the href to
/// emit, so callers in different directories resolve correctly.
pub(super) fn mermaid_figure(
    sg: &Subgraph,
    files: &BTreeMap<String, String>,
    href_of: &dyn Fn(&str) -> String,
) -> String {
    subgraph_mermaid_linked(sg, |uid| files.get(uid).map(|rel| href_of(rel)))
}

/// The plain-Markdown legend that follows every drawn diagram (node name →
/// page link). Two redundant navigation paths on purpose: the Mermaid
/// `click` lines work where Mermaid renders them, and the legend works
/// everywhere Markdown renders — so a reader can always reach a node's page
/// even if `click` is stripped (GitHub historically restricts `click` for
/// security). Deterministic: the legend walks `sg.nodes` (already sorted by
/// `unique_id`).
pub(super) fn push_legend(
    s: &mut String,
    dag: &Dag,
    sg: &Subgraph,
    files: &BTreeMap<String, String>,
    href_of: &dyn Fn(&str) -> String,
) {
    s.push_str("\nNodes in this diagram:\n\n");
    for n in &sg.nodes {
        let name = dag
            .get(&n.unique_id)
            .map(|i| i.name.as_str())
            .unwrap_or(&n.name);
        match files.get(&n.unique_id) {
            Some(rel) => s.push_str(&format!("- [{}]({})\n", name, href_of(rel))),
            None => s.push_str(&format!("- {name}\n")),
        }
    }
}

/// Append an already-rendered `figure` plus its legend — the emitted pair
/// for every diagram that survives the budget check.
pub(super) fn push_figure_and_legend(
    s: &mut String,
    dag: &Dag,
    sg: &Subgraph,
    files: &BTreeMap<String, String>,
    figure: String,
    href_of: &dyn Fn(&str) -> String,
) {
    s.push_str(&figure);
    push_legend(s, dag, sg, files, href_of);
}

/// Append a "direct dependency" bullet list (each entry linked to its node
/// page relative to `page_rel`, the page the list lives on), or a "None."
/// line when empty. The `deps` are already sorted.
pub(super) fn push_dep_list(
    s: &mut String,
    dag: &Dag,
    deps: &[String],
    files: &BTreeMap<String, String>,
    page_rel: &str,
) {
    if deps.is_empty() {
        s.push_str("None.\n");
        return;
    }
    for uid in deps {
        let name = dag.get(uid).map(|n| n.name.as_str()).unwrap_or(uid);
        match files.get(uid) {
            Some(rel) => s.push_str(&format!("- [{}]({})\n", name, page_link(page_rel, rel))),
            None => s.push_str(&format!("- {name}\n")),
        }
    }
}
