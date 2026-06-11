//! The yank/export text producers: Mermaid / Graphviz DOT / plain-text lineage
//! diagrams, the raw-SQL yank, and the Markdown impact report. All are pure
//! string builders over the current selection — deterministic byte-for-byte.

use crate::NodeInfo;

use super::App;

impl App {
    /// Render the current lineage subgraph as a Mermaid `graph LR` diagram
    /// (deterministic: the subgraph's nodes/edges are already sorted). Node IDs
    /// are the sanitized `unique_id`; labels carry the name + materialization.
    /// `None` when nothing is selected / the subgraph is empty.
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
        let mut out = String::from("```mermaid\ngraph LR\n");
        for n in &sg.nodes {
            let kind = node_kind(n);
            let mark = if n.unique_id == sg.selected { " *" } else { "" };
            out.push_str(&format!(
                "  {}[\"{} ({}){}\"]\n",
                mermaid_id(&n.unique_id),
                mermaid_label(&n.name),
                kind,
                mark,
            ));
        }
        for e in &sg.edges {
            out.push_str(&format!(
                "  {} --> {}\n",
                mermaid_id(&e.parent),
                mermaid_id(&e.child)
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
                kind,
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
        Some(crate::layout_mode(&sg, self.glyph_mode).grid.to_text())
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
fn node_kind(n: &NodeInfo) -> &str {
    n.materialized.as_deref().unwrap_or(&n.resource_type)
}

/// A Mermaid-safe node id: non-alphanumeric chars (the `.` in a `unique_id`)
/// become `_`, so `model.p.x` → `model_p_x`.
fn mermaid_id(uid: &str) -> String {
    uid.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Escape a label for a Mermaid `["..."]` node (double quotes would close it).
fn mermaid_label(name: &str) -> String {
    name.replace('"', "'")
}

/// Escape a string for a Graphviz DOT quoted id/label (`\` and `"`).
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
