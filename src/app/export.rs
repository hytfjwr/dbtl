//! The yank/export text producers: Mermaid / Graphviz DOT / plain-text lineage
//! diagrams, the raw-SQL yank, and the Markdown impact report. All are pure
//! string builders over the current selection — deterministic byte-for-byte.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::{NodeInfo, Subgraph};

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
        Some(subgraph_mermaid(&sg))
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

    /// A runnable `dbt build --select …` command for the current lineage view:
    /// the rooted node expressed through dbt's graph operators, mirroring the
    /// pane exactly — the upstream toggle becomes a `+` prefix, the downstream
    /// toggle a `+` suffix, and a hop-depth limit becomes the `N+`/`+N` bounded
    /// forms, so what the user yanks is what the pane shows. The root is always
    /// a model (the list holds only models, and `jump_to` re-roots only to list
    /// members), so the bare name is always a valid node selector — no
    /// `source:` method needed. `None` when nothing is selected.
    pub fn dbt_selector_command(&self) -> Option<String> {
        let base = self.selected_name()?;
        let view = &self.lineage_view;
        let prefix = match (view.upstream, view.depth) {
            (false, _) => String::new(),
            (true, None) => "+".to_string(),
            (true, Some(n)) => format!("{n}+"),
        };
        let suffix = match (view.downstream, view.depth) {
            (false, _) => String::new(),
            (true, None) => "+".to_string(),
            (true, Some(n)) => format!("+{n}"),
        };
        Some(format!("dbt build --select {prefix}{base}{suffix}"))
    }

    /// A Markdown blast-radius report for the selected node: direct +
    /// transitive up/down counts, the sorted member name lists, and — when any
    /// exist — the downstream exposures (the "who cares" endpoints), with kind
    /// and owner. Exposures are split OUT of the Downstream list into their own
    /// section, so the up/down numbers keep matching the status chip. The
    /// closures come from `HashSet`s, so every list is sorted here — two yanks
    /// of the same selection are byte-identical. `None` when nothing is selected.
    pub fn impact_report(&self) -> Option<String> {
        let uid = self.selected_unique_id()?;
        let node = self.dag.get(&uid)?;
        let names = |uids: std::collections::HashSet<String>| -> Vec<String> {
            let mut v: Vec<String> = uids
                .into_iter()
                .filter_map(|u| self.dag.get(&u))
                .filter(|n| !crate::is_exposure(n))
                .map(|n| n.name.clone())
                .collect();
            v.sort();
            v
        };
        let down = names(self.dag.downstream(&uid));
        let up = names(self.dag.upstream(&uid));
        let exposures = self.downstream_exposures(&uid);
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
        if !exposures.is_empty() {
            out.push_str(&format!("- affected exposures: {}\n", exposures.len()));
        }
        for (title, list) in [("Downstream (blast radius)", &down), ("Upstream", &up)] {
            out.push_str(&format!("\n## {title} ({})\n", list.len()));
            for name in list {
                out.push_str(&format!("- {name}\n"));
            }
        }
        if !exposures.is_empty() {
            out.push_str(&format!("\n## Affected exposures ({})\n", exposures.len()));
            for (info, payload) in &exposures {
                out.push_str(&format!("- {}{}\n", info.name, exposure_note(*payload)));
            }
        }
        Some(out)
    }

    /// Render the `--diff` baseline comparison as a reviewer-shaped "PR Impact
    /// Pack" in Markdown, ready to paste into a PR description: the changed
    /// models grouped by kind (the same listings the `D` modal shows), the
    /// aggregate blast radius (affected model count + the affected marts), the
    /// affected exposures, a suggested `dbt build --select …` command, and the
    /// risk flags.
    ///
    /// A PURE formatter over the [`DiffView`] snapshot (which already carries the
    /// resolved names, joined reasons, and the [`PrImpact`](crate::PrImpact)
    /// analysis) plus the project name for the title — so the exported file is
    /// byte-identical to what the modal renders, and re-exporting the same diff
    /// is deterministic. Empty sections are omitted so the report stays tight.
    pub fn pr_impact_markdown(&self, dv: &crate::DiffView) -> String {
        let project = self.dag.project_name().unwrap_or("dbt");
        let pr = &dv.pr;
        let mut out = format!("# PR Impact Report: {project}\n\n");
        out.push_str(&format!("- baseline: `{}`\n", dv.baseline));
        out.push_str(&format!(
            "- changed: +{} added, ~{} modified, -{} removed\n",
            dv.added.len(),
            dv.modified.len(),
            dv.removed.len()
        ));
        out.push_str(&format!(
            "- edges: +{} / -{}\n",
            dv.edges_added.len(),
            dv.edges_removed.len()
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
                dv.added.iter().map(|(n, k)| node_row(n, k)).collect(),
            ),
            (
                "Modified",
                dv.modified
                    .iter()
                    .map(|(n, reasons)| format!("- {n}: {reasons}"))
                    .collect(),
            ),
            (
                "Removed",
                dv.removed.iter().map(|(n, k)| node_row(n, k)).collect(),
            ),
            (
                "Edges added",
                dv.edges_added
                    .iter()
                    .map(|(p, c)| format!("- {p} -> {c}"))
                    .collect(),
            ),
            (
                "Edges removed",
                dv.edges_removed
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

        out.push_str("\n## Suggested CI command\n");
        match &pr.ci_command {
            Some(cmd) => out.push_str(&format!("```sh\n{cmd}\n```\n")),
            None => out.push_str("- no buildable changes\n"),
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
}

/// Render a [`Subgraph`] as a fenced Mermaid `graph LR` diagram. The pure core
/// shared by the interactive yank ([`App::lineage_mermaid`]) and the static docs
/// generator ([`crate::docs`]): identical node-id assignment ([`mermaid_ids`])
/// and label escaping ([`mermaid_label`]), so both surfaces produce the same
/// (and deterministic — the subgraph is already sorted) Mermaid for a given
/// node set. The selected node is marked with a trailing ` *` in its label.
///
/// The diagram is wrapped in a ```` ```mermaid ```` fenced code block so pasting
/// it straight into a Markdown surface renders an actual diagram. Callers that
/// want the raw body without the fence strip the first/last line.
pub(crate) fn subgraph_mermaid(sg: &Subgraph) -> String {
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
pub(crate) fn subgraph_mermaid_linked(
    sg: &Subgraph,
    link: impl Fn(&str) -> Option<String>,
) -> String {
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
pub(crate) fn node_kind(n: &NodeInfo) -> &str {
    n.materialized.as_deref().unwrap_or(&n.resource_type)
}

/// The parenthesised kind/owner suffix for one impact-report exposure line:
/// `" (dashboard, owner: Finance <fin@example.com>)"`. Each part is optional;
/// the kind falls back to the bare `exposure` so the line always names what it
/// is. Owner shows `name <email>`, either half standing alone when the other
/// is missing. `pub(super)` so the PR-impact analysis ([`App::pr_impact_data`])
/// formats its affected-exposure lines through the SAME rule.
pub(super) fn exposure_note(payload: Option<&crate::ExposureInfo>) -> String {
    let kind = payload
        .and_then(|p| p.exposure_type.as_deref())
        .unwrap_or("exposure");
    let owner = payload.map_or_else(String::new, |p| {
        match (p.owner_name.as_deref(), p.owner_email.as_deref()) {
            (Some(n), Some(e)) => format!(", owner: {n} <{e}>"),
            (Some(n), None) => format!(", owner: {n}"),
            (None, Some(e)) => format!(", owner: <{e}>"),
            (None, None) => String::new(),
        }
    });
    format!(" ({kind}{owner})")
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
pub(crate) fn mermaid_ids<'a>(uids: impl IntoIterator<Item = &'a str>) -> HashMap<String, String> {
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
pub(crate) fn mermaid_label(name: &str) -> String {
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
