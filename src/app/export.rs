//! The yank/export text producers: Mermaid / Graphviz DOT / plain-text lineage
//! diagrams, the raw-SQL yank, and the Markdown impact report. All are pure
//! string builders over the current selection — deterministic byte-for-byte.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::NodeInfo;

use super::App;

impl App {
    /// Render the current lineage subgraph as a Mermaid `graph LR` diagram
    /// (deterministic: the subgraph's nodes/edges are already sorted). Node IDs
    /// come from a per-export [`mermaid_ids`] map — the sanitized `unique_id`,
    /// with deterministic numeric suffixes when two uids sanitize to the same
    /// text; labels carry the name + materialization. `None` when nothing is
    /// selected / the subgraph is empty.
    ///
    /// The diagram is wrapped in a ```` ```mermaid ```` fenced code block so that
    /// pasting the yank straight into a Markdown surface (GitHub, Notion,
    /// Confluence, Slack, Obsidian, …) renders an actual diagram instead of plain
    /// text. The fence is the only thing standing between "valid Mermaid source"
    /// and "renders where people paste it". (Trade-off: a raw editor like
    /// mermaid.live wants the body without the fence — strip the first/last line
    /// there.)
    pub fn lineage_mermaid(&self) -> Option<String> {
        let sg = self.lineage_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
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
        out.push_str("```\n");
        Some(out)
    }

    /// Render the current lineage subgraph as a Graphviz DOT digraph (`rankdir=LR`).
    /// Node ids are the quoted `unique_id`; labels carry name + materialization.
    /// `None` when nothing is selected / the subgraph is empty.
    pub fn lineage_dot(&self) -> Option<String> {
        let sg = self.lineage_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
        let mut out = String::from("digraph lineage {\n  rankdir=LR;\n  node [shape=box];\n");
        for n in &sg.nodes {
            let kind = node_kind(n);
            out.push_str(&format!(
                "  \"{}\" [label=\"{}\\n({})\"];\n",
                dot_escape(&n.unique_id),
                dot_escape(&n.name),
                dot_escape(kind),
            ));
        }
        for e in &sg.edges {
            out.push_str(&format!(
                "  \"{}\" -> \"{}\";\n",
                dot_escape(&e.parent),
                dot_escape(&e.child)
            ));
        }
        out.push_str("}\n");
        Some(out)
    }

    /// Render the current lineage subgraph as plain text — the exact diagram
    /// the lineage pane draws (same `layout_mode` + glyph repertoire), minus
    /// colours. Pasteable into tickets/docs as a monospace block. `None` when
    /// nothing is selected / the subgraph is empty.
    pub fn lineage_ascii(&self) -> Option<String> {
        let sg = self.lineage_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
        Some(
            crate::layout_density(&sg, self.glyph_mode, self.ui_state.density())
                .grid
                .to_text(),
        )
    }

    /// The selected node's raw (uncompiled) SQL, cloned from the `Dag::sql`
    /// side map. `None` for nodes without source SQL — manifests record seeds
    /// with `raw_code: ""`, so blank SQL is treated as absent (an empty yank
    /// would silently clobber the clipboard with nothing).
    pub fn selected_raw_sql(&self) -> Option<String> {
        let uid = self.selected_unique_id()?;
        self.dag
            .raw_code(&uid)
            .filter(|code| !code.trim().is_empty())
            .map(str::to_string)
    }

    /// A Markdown blast-radius report for the selected node: direct +
    /// transitive up/down counts and the sorted member name lists. The closures
    /// come from `HashSet`s, so both lists are sorted here — two yanks of the
    /// same selection are byte-identical. `None` when nothing is selected.
    pub fn impact_report(&self) -> Option<String> {
        let uid = self.selected_unique_id()?;
        let node = self.dag.get(&uid)?;
        let names = |uids: std::collections::HashSet<String>| -> Vec<String> {
            let mut v: Vec<String> = uids
                .into_iter()
                .filter_map(|u| self.dag.get(&u).map(|n| n.name.clone()))
                .collect();
            v.sort();
            v
        };
        let down = names(self.dag.downstream(&uid));
        let up = names(self.dag.upstream(&uid));
        let mut out = format!("# Impact: {}\n\n", node.name);
        out.push_str(&format!("- unique_id: `{uid}`\n"));
        out.push_str(&format!(
            "- direct: {} upstream / {} downstream\n",
            node.direct_up, node.direct_down
        ));
        out.push_str(&format!(
            "- transitive: {} upstream / {} downstream\n",
            up.len(),
            down.len()
        ));
        for (title, list) in [("Downstream (blast radius)", &down), ("Upstream", &up)] {
            out.push_str(&format!("\n## {title} ({})\n", list.len()));
            for name in list {
                out.push_str(&format!("- {name}\n"));
            }
        }
        Some(out)
    }
}

/// A node's display kind in an export: its materialization if recorded, else its
/// resource type. Shared by the Mermaid and DOT exporters so the policy is single.
/// Untrusted (straight from the manifest) — callers must escape it like the name.
fn node_kind(n: &NodeInfo) -> &str {
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
pub(super) fn mermaid_ids<'a>(uids: impl IntoIterator<Item = &'a str>) -> HashMap<String, String> {
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
pub(super) fn mermaid_label(name: &str) -> String {
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

/// Escape a string for a Graphviz DOT quoted id/label (`\` and `"`).
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
