use super::*;

/// Append a linked Mermaid diagram for `sg` followed by a plain-Markdown legend
/// (node name → page link). Two redundant navigation paths on purpose: the
/// Mermaid `click` lines work where Mermaid renders them, and the legend works
/// everywhere Markdown renders — so a reader can always reach a node's page even
/// if `click` is stripped (GitHub historically restricts `click` for security).
///
/// `href_of` maps a target node's stored relative file path (from `files`) to
/// the href to emit, so callers in different directories (`models/` vs the repo
/// root) get a correct relative link. Deterministic: the diagram order is fixed
/// by [`subgraph_mermaid_linked`] and the legend walks `sg.nodes` (already sorted
/// by `unique_id`).
pub(super) fn push_linked_diagram(
    s: &mut String,
    dag: &Dag,
    sg: &Subgraph,
    files: &BTreeMap<String, String>,
    href_of: impl Fn(&str) -> String,
) {
    s.push_str(&subgraph_mermaid_linked(sg, |uid| {
        files.get(uid).map(|rel| href_of(rel))
    }));
    // Legend: every node in the diagram, linked to its page. Survives anywhere
    // the Mermaid `click` does not.
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

/// Append a "direct dependency" bullet list (each entry linked to its node
/// page), or a "None." line when empty. The `names` are already sorted.
pub(super) fn push_dep_list(
    s: &mut String,
    dag: &Dag,
    deps: &[String],
    files: &BTreeMap<String, String>,
) {
    if deps.is_empty() {
        s.push_str("None.\n");
        return;
    }
    for uid in deps {
        let name = dag.get(uid).map(|n| n.name.as_str()).unwrap_or(uid);
        match files.get(uid) {
            Some(rel) => s.push_str(&format!("- [{}]({})\n", name, link_between_nodes(rel))),
            None => s.push_str(&format!("- {name}\n")),
        }
    }
}
