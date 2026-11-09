//! Mermaid text export shared by the interactive yank (`dbtl-tui`'s
//! `App::lineage_mermaid`) and the static docs generator (`dbtl-docs`):
//! identical node-id assignment ([`mermaid_ids`]) and label escaping
//! ([`mermaid_label`]), so both surfaces produce the same (and deterministic —
//! the subgraph is already sorted) Mermaid for a given node set.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::{NodeInfo, Subgraph};

/// Render a [`Subgraph`] as a fenced Mermaid `graph LR` diagram. The pure core
/// shared by the interactive yank (`App::lineage_mermaid`) and the static docs
/// generator (`dbtl-docs`): identical node-id assignment ([`mermaid_ids`])
/// and label escaping ([`mermaid_label`]), so both surfaces produce the same
/// (and deterministic — the subgraph is already sorted) Mermaid for a given
/// node set. The selected node is marked with a trailing ` *` in its label.
///
/// The diagram is wrapped in a ```` ```mermaid ```` fenced code block so pasting
/// it straight into a Markdown surface renders an actual diagram. Callers that
/// want the raw body without the fence strip the first/last line.
pub fn subgraph_mermaid(sg: &Subgraph) -> String {
    // The interactive yank carries no page links: pass a resolver that never
    // links, leaving the body byte-identical to the original (frozen) shape.
    subgraph_mermaid_linked(sg, |_uid| None)
}

/// Like [`subgraph_mermaid`], but emits a Mermaid `click <id> "<href>"` line for
/// every node whose `unique_id` resolves to a relative href (`link(uid)`),
/// turning the diagram into clickable navigation where Mermaid renders `click`
/// (GitHub, Mermaid Live, …). The diagram body is identical to
/// [`subgraph_mermaid`]; the `click` lines sit AFTER all node/edge statements,
/// in the same sorted-`unique_id` node order, so the output stays deterministic.
///
/// `click` is best-effort decoration: where it is unsupported the lines render as
/// inert text and the diagram is unaffected — the docs generator pairs this with
/// a plain Markdown legend so navigation never depends on `click` working. The
/// href is wrapped in quotes (Mermaid's relative-link form) and run through
/// [`mermaid_label`] so an exotic path can never break the fence.
pub fn subgraph_mermaid_linked(sg: &Subgraph, link: impl Fn(&str) -> Option<String>) -> String {
    let ids = mermaid_ids(
        sg.nodes
            .iter()
            .map(|n| n.unique_id.as_str())
            .chain(sg.edges.iter().map(|e| e.parent.as_str()))
            .chain(sg.edges.iter().map(|e| e.child.as_str())),
    );
    let mut out = String::from("```mermaid\ngraph LR\n");
    for n in &sg.nodes {
        let kind = node_kind(n);
        let mark = if n.unique_id == sg.selected { " *" } else { "" };
        // The kind comes from the manifest too, so the WHOLE label text is
        // escaped as one unit — not just the name.
        out.push_str(&format!(
            "  {}[\"{}\"]\n",
            ids[n.unique_id.as_str()],
            mermaid_label(&format!("{} ({kind}){mark}", n.name)),
        ));
    }
    for e in &sg.edges {
        out.push_str(&format!(
            "  {} --> {}\n",
            ids[e.parent.as_str()],
            ids[e.child.as_str()]
        ));
    }
    // Click targets last, in the same node order (already sorted by uid).
    for n in &sg.nodes {
        if let Some(href) = link(&n.unique_id) {
            out.push_str(&format!(
                "  click {} \"{}\"\n",
                ids[n.unique_id.as_str()],
                mermaid_label(&href),
            ));
        }
    }
    out.push_str("```\n");
    out
}

/// A node's display kind in an export: its materialization if recorded, else its
/// resource type. Shared by the Mermaid and DOT exporters so the policy is single.
/// Untrusted (straight from the manifest) — callers must escape it like the name.
pub fn node_kind(n: &NodeInfo) -> &str {
    n.materialized.as_deref().unwrap_or(&n.resource_type)
}

/// Mermaid words that must not appear as a bare node id: `end` closes a
/// `subgraph` block, and the rest open statements/blocks of their own, so a
/// `unique_id` that happens to sanitize to one of them would corrupt the graph.
const MERMAID_RESERVED: &[&str] = &[
    "end",
    "subgraph",
    "graph",
    "flowchart",
    "direction",
    "style",
    "linkStyle",
    "classDef",
    "class",
    "click",
];

/// A per-export `unique_id -> Mermaid node id` map.
///
/// Each id is the sanitized uid (non-alphanumeric chars — the `.` in a
/// `unique_id` — become `_`, so `model.p.x` → `model_p_x`). Sanitizing is
/// lossy: `model.p.x_y` and `model.p_x.y` both flatten to `model_p_x_y`, and
/// emitting that id for both would silently merge two distinct nodes in the
/// rendered diagram. So ids are assigned per export from a shared map: uids
/// are visited in sorted order (determinism — same subgraph, byte-identical
/// export) and a uid whose sanitized form is already taken gets the first free
/// `_2`, `_3`, … numeric suffix. Reserved Mermaid words are pre-claimed so a
/// uid can never sanitize to e.g. `end`, and an (unrealistic) empty uid falls
/// back to `_` rather than an empty — syntactically invalid — id.
pub fn mermaid_ids<'a>(uids: impl IntoIterator<Item = &'a str>) -> HashMap<String, String> {
    let sorted: BTreeSet<&str> = uids.into_iter().collect();
    let mut used: HashSet<String> = MERMAID_RESERVED.iter().map(|w| w.to_string()).collect();
    let mut ids = HashMap::with_capacity(sorted.len());
    for uid in sorted {
        let base: String = uid
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let base = if base.is_empty() {
            "_".to_string()
        } else {
            base
        };
        let mut id = base.clone();
        let mut n = 1usize;
        while used.contains(&id) {
            n += 1;
            id = format!("{base}_{n}");
        }
        used.insert(id.clone());
        ids.insert(uid.to_string(), id);
    }
    ids
}

/// Escape display text for a Mermaid `["..."]` quoted label. Inside the quotes
/// only two things can break the statement: a `"` (closes the string — emitted
/// as the `#quot;` entity so the character still renders) and line breaks
/// (start a new statement mid-label). Every control char collapses to a space,
/// which also keeps the label inside its line of the surrounding Markdown
/// fence — a smuggled newline followed by a backtick fence must never
/// terminate the code block.
pub fn mermaid_label(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '"' => out.push_str("#quot;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}
