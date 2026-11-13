//! The yank/export text producers: Mermaid / Graphviz DOT / plain-text lineage
//! diagrams, the raw-SQL yank, and the Markdown impact report. All are pure
//! string builders over the current selection — deterministic byte-for-byte.

use dbtl_core::mermaid::{node_kind, subgraph_mermaid};

use super::App;
use crate::action::Mode;
use crate::Subgraph;

impl App {
    /// The subgraph a lineage yank renders: the current view's — EXCEPT
    /// inside the `y` menu opened over the structure modal, where the modal's
    /// node is the subject: the yank then draws ITS rooted lineage under the
    /// same direction/depth view, not the list selection's. `pending_node`
    /// lives only while `Mode::Yank` is up, so this can't leak into a
    /// Selection-mode export (the `e` write / the docs yank path).
    fn yank_subgraph(&self) -> Subgraph {
        if matches!(self.mode, Mode::Yank { .. }) {
            if let Some(uid) = &self.pending_node {
                let v = &self.tab().lineage_view;
                return self
                    .dag
                    .subgraph_view(uid, v.upstream, v.downstream, v.depth);
            }
        }
        self.lineage_subgraph()
    }

    /// Render the yank-target lineage subgraph as a Mermaid `graph LR` diagram
    /// (deterministic: the subgraph's nodes/edges are already sorted). Node IDs
    /// come from a per-export [`crate::mermaid_ids`] map — the sanitized `unique_id`,
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
        let sg = self.yank_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
        Some(subgraph_mermaid(&sg))
    }

    /// Render the yank-target lineage subgraph as a Graphviz DOT digraph
    /// (`rankdir=LR`). Node ids are the quoted `unique_id`; labels carry name +
    /// materialization. `None` when nothing is selected / the subgraph is empty.
    pub fn lineage_dot(&self) -> Option<String> {
        let sg = self.yank_subgraph();
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

    /// Render the yank-target lineage as plain text — the exact diagram the
    /// lineage pane draws (same `layout_mode` + glyph repertoire AND the same
    /// sibling-collapse view: `+M more` markers and all), minus colours.
    /// Pasteable into tickets/docs as a monospace block. `None` when nothing
    /// is selected / the subgraph is empty.
    ///
    /// Policy: the ASCII export is SCREEN-EQUIVALENT — the existing
    /// "the yank IS the pane's diagram" rule — so collapse markers appear in
    /// the text exactly as on screen (expand first, then yank, to capture the
    /// members). The structured exports ([`Self::lineage_mermaid`]/[`Self::lineage_dot`])
    /// keep the SEMANTIC graph: every real node, no markers.
    ///
    /// When the yank targets the structure modal's node (`pending_node`), the
    /// base is rooted elsewhere: the keep-alive set must be that root's
    /// subject alone — the cursor/path/bookmark keeps belong to the CURRENT
    /// root's view and would otherwise draw nodes the screen hides.
    pub fn lineage_ascii(&self) -> Option<String> {
        let base = self.yank_subgraph();
        if base.nodes.is_empty() {
            return None;
        }
        let same_root = base.selected == self.selected_unique_id().unwrap_or_default();
        let sg = if same_root {
            self.collapse_view_of(&base)
        } else {
            self.collapse_view_bare(&base)
        }
        .subgraph;
        Some(
            crate::layout_density(&sg, self.glyph_mode, self.effective_density())
                .grid
                .to_text(),
        )
    }

    /// The selected node's raw (uncompiled) SQL — [`App::raw_sql_for`] applied
    /// to the current selection.
    pub fn selected_raw_sql(&self) -> Option<String> {
        self.selected_unique_id()
            .and_then(|uid| self.raw_sql_for(&uid))
    }

    /// A node's raw (uncompiled) SQL, cloned from the `Dag::sql` side map.
    /// `None` for nodes without source SQL — manifests record seeds with
    /// `raw_code: ""`, so blank SQL is treated as absent (an empty yank would
    /// silently clobber the clipboard with nothing). The explicit-uid form the
    /// `y` menu's modal target needs.
    pub fn raw_sql_for(&self, uid: &str) -> Option<String> {
        self.dag
            .raw_code(uid)
            .filter(|code| !code.trim().is_empty())
            .map(str::to_string)
    }

    /// A runnable `dbt build --select …` command for `uid`'s lineage view:
    /// the rooted node expressed through dbt's graph operators, mirroring the
    /// pane exactly — the upstream toggle becomes a `+` prefix, the downstream
    /// toggle a `+` suffix, and a hop-depth limit becomes the `N+`/`+N` bounded
    /// forms, so what the user yanks is what the pane shows. The root is almost
    /// always a model (the list holds only models, and `jump_to` re-roots only
    /// to list members), so the bare name is a valid node selector — for the
    /// yank menu's modal target (possibly a source/seed) it is still the
    /// nearest name form v0 emits. `None` for an unknown uid.
    pub fn dbt_selector_command_for(&self, uid: &str) -> Option<String> {
        let base = &self.dag.get(uid)?.name;
        let view = &self.tab().lineage_view;
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

    /// A Markdown blast-radius report for `uid`: direct + transitive up/down
    /// counts, the sorted member name lists, and — when any exist — the
    /// downstream exposures (the "who cares" endpoints), with kind and owner.
    /// Exposures are split OUT of the Downstream list into their own section,
    /// so the up/down numbers keep matching the status chip. The closures come
    /// from `HashSet`s, so every list is sorted here — two yanks of the same
    /// target are byte-identical. `None` for an unknown uid. The explicit-uid
    /// form the `y` menu's modal target needs.
    pub fn impact_report_for(&self, uid: &str) -> Option<String> {
        let node = self.dag.get(uid)?;
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
        let down = names(self.dag.downstream(uid));
        let up = names(self.dag.upstream(uid));
        let exposures = self.downstream_exposures(uid);
        // The distinct `meta[owner_meta_key]` values of the downstream
        // MODELS — the "who to page" set, same population the PR impact
        // pack's affected-owners section collects.
        let owners: Vec<String> = {
            let mut v: Vec<String> = self
                .dag
                .downstream(uid)
                .iter()
                .filter(|u| self.dag.get(u).is_some_and(|n| n.resource_type == "model"))
                .filter_map(|u| {
                    self.dag
                        .detail(u)
                        .and_then(|d| d.meta.get(self.owner_meta_key.as_str()))
                })
                .cloned()
                .collect();
            v.sort();
            v.dedup();
            v
        };
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
        if !owners.is_empty() {
            out.push_str(&format!("- affected owners: {}\n", owners.len()));
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
                out.push_str(&format!(
                    "- {}{}\n",
                    info.name,
                    crate::exposure_note(*payload)
                ));
            }
        }
        if !owners.is_empty() {
            out.push_str(&format!("\n## Affected owners ({})\n", owners.len()));
            for o in &owners {
                out.push_str(&format!("- {o}\n"));
            }
        }
        Some(out)
    }

    /// The `yl` payload: `col`'s column-level lineage closure as a
    /// deterministic Markdown table — literally
    /// [`closure_report`](crate::closure_report)'s `Display`, so the yank and
    /// `dbtl columns` print the same bytes for the same target. Drives the
    /// app-owned [`SqlParseResolver`](crate::SqlParseResolver); the report
    /// labels columns `model.column` (display names).
    pub fn column_lineage_markdown(
        &self,
        col: &crate::ColumnRef,
        direction: crate::ClosureDirection,
    ) -> String {
        crate::closure_report(&self.dag, &self.cll, col, direction).to_string()
    }

    /// Render the `--diff` baseline comparison as a reviewer-shaped "PR Impact
    /// Pack" in Markdown, ready to paste into a PR description: the changed
    /// models grouped by kind (the same listings the `D` modal shows), the
    /// aggregate blast radius (affected model count + the affected marts), the
    /// affected exposures, a suggested `dbt build --select …` command, and the
    /// risk flags.
    ///
    /// A PURE formatter over the [`crate::DiffView`] snapshot (which already carries the
    /// resolved names, joined reasons, and the [`PrImpact`](crate::PrImpact)
    /// analysis) plus the project name for the title — so the exported file is
    /// byte-identical to what the modal renders, and re-exporting the same diff
    /// is deterministic. Empty sections are omitted so the report stays tight.
    pub fn pr_impact_markdown(&self, dv: &crate::DiffView) -> String {
        let project = self.dag.project_name().unwrap_or("dbt");
        // The formatter lives in dbtl_core so `dbtl impact` prints the exact
        // same bytes the `e` export writes; DiffView is the same data plus a
        // scroll offset, so it maps back to DiffReport field-for-field.
        let report = crate::DiffReport {
            baseline: dv.baseline.clone(),
            // The snapshot's own scope field — the export must match what the
            // modal printed, so it never re-reads live App state.
            scope: dv.scope,
            added: dv.added.clone(),
            removed: dv.removed.clone(),
            modified: dv.modified.clone(),
            edges_added: dv.edges_added.clone(),
            edges_removed: dv.edges_removed.clone(),
            column_changes: dv.column_changes.clone(),
            pr: dv.pr.clone(),
        };
        crate::pr_impact_markdown(project, &report)
    }
}

/// Escape a string for a Graphviz DOT quoted id/label (`\` and `"`).
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
