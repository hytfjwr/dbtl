use super::*;

// ---------------------------------------------------------------------------
// ColumnReport — the shared `dbtl columns` / `yl` output shape
// ---------------------------------------------------------------------------

/// Which way a column-closure query walks. Shared by the `yl` yank and
/// `dbtl columns --upstream|--downstream`; serializes `snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureDirection {
    /// Columns the target feeds (its transitive blast radius).
    Downstream,
    /// Columns feeding the target (its provenance).
    Upstream,
}

impl std::fmt::Display for ClosureDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Downstream => "downstream",
            Self::Upstream => "upstream",
        })
    }
}

/// One row of a [`ColumnReport`]: a closure-reached column and how the walk
/// reaches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnRow {
    /// `model.column` (model = the node's display name — uids are noise in a
    /// pasted report; falls back to the uid for unknown nodes).
    pub column: String,
    /// The closure-adjacent `model.column`s of this row: for a downstream
    /// report, its in-closure feeders; for upstream, the in-closure columns
    /// it feeds — the row's next hop on the lineage path. Sorted, deduped;
    /// non-empty in practice since the target sits inside the scope.
    pub via: Vec<String>,
    /// The deduplicated edge kinds on those links, enum order.
    pub kinds: Vec<ColumnEdgeKind>,
    /// The most conservative confidence on the links — `stale` when ANY link
    /// read an old compiled file (the same rule as the report's own
    /// `confidence`, so a row can never look fresher than the header claim).
    pub confidence: Confidence,
}

/// The `dbtl columns` / `yl` payload — `Display`s as the deterministic
/// Markdown table the yank copies, and `Serialize`s for `--format json`.
/// Rows are sorted by `column`, so two runs emit identical bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnReport {
    /// `model.column` of the queried column.
    pub target: String,
    /// The queried node's `unique_id` (for tooling).
    pub unique_id: String,
    /// Which closure the report lists.
    pub direction: ClosureDirection,
    /// The most conservative confidence in the report — `stale` when ANY row
    /// came off an old compiled file (the conservative flag).
    pub confidence: Confidence,
    /// The reached columns, sorted by `column`.
    pub columns: Vec<ColumnRow>,
}

/// Build the closure report for `target` — walks `direction` from `target`
/// through the lazy [`ColumnLineage`] (resolves only the touched nodes).
/// Deterministic: rows sort by `column`; the report's own `confidence` is the
/// weakest link's (`Stale` if any edge read an old compiled file).
pub fn closure_report(
    dag: &Dag,
    resolver: &dyn ColumnResolver,
    target: &ColumnRef,
    direction: ClosureDirection,
) -> ColumnReport {
    let lin = ColumnLineage::new(dag, resolver);
    let name = |uid: &str| {
        dag.get(uid)
            .map_or_else(|| uid.to_string(), |n| n.name.clone())
    };
    let addr = |c: &ColumnRef| format!("{}.{}", name(&c.node), c.column);
    let reached = match direction {
        ClosureDirection::Downstream => lin.closure_down(target),
        ClosureDirection::Upstream => lin.closure_up(target),
    };
    let in_scope = |c: &ColumnRef| c == target || reached.contains(c);
    let mut columns: Vec<ColumnRow> = Vec::with_capacity(reached.len());
    for t in &reached {
        // The row's links are the in-closure edges incident on it — upstream
        // feeders for a downstream walk, downstream consumers for an upstream
        // one — so a row always shows the next hop of the lineage path.
        let links: Vec<ColumnEdge> = match direction {
            ClosureDirection::Downstream => lin
                .upstream(t)
                .into_iter()
                .filter(|e| in_scope(&e.from))
                .collect(),
            ClosureDirection::Upstream => lin
                .downstream(t)
                .into_iter()
                .filter(|e| in_scope(&e.to))
                .collect(),
        };
        let mut via: Vec<String> = links
            .iter()
            .map(|e| {
                addr(match direction {
                    ClosureDirection::Downstream => &e.from,
                    ClosureDirection::Upstream => &e.to,
                })
            })
            .collect();
        via.sort();
        via.dedup();
        let mut kinds: Vec<ColumnEdgeKind> = links.iter().map(|e| e.kind).collect();
        kinds.sort();
        kinds.dedup();
        let confidence = links
            .iter()
            .map(|e| e.confidence)
            .max()
            // An isolated column has no edges — its row inherits the
            // report-level default (Exact = "v1 ran", not the v0 label).
            .unwrap_or(Confidence::Exact);
        columns.push(ColumnRow {
            column: addr(t),
            via,
            kinds,
            confidence,
        });
    }
    columns.sort_by(|a, b| a.column.cmp(&b.column));
    let confidence = columns
        .iter()
        .map(|r| r.confidence)
        .max()
        .unwrap_or(Confidence::Exact);
    ColumnReport {
        target: addr(target),
        unique_id: target.node.clone(),
        direction,
        confidence,
        columns,
    }
}

impl std::fmt::Display for ColumnReport {
    /// The deterministic Markdown table — the `yl` yank payload AND the
    /// `dbtl columns` default text (one shape, two surfaces).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let title = match self.direction {
            ClosureDirection::Downstream => "Downstream",
            ClosureDirection::Upstream => "Upstream",
        };
        writeln!(
            f,
            "# Column lineage: `{}` ({})\n",
            self.target, self.direction
        )?;
        let method = match self.confidence {
            Confidence::Exact => "column-level lineage v1: SQL parser",
            Confidence::Stale => "column-level lineage v1: SQL parser, stale compiled SQL",
            Confidence::NameMatch => "column-level lineage v0: name-match approximation",
        };
        writeln!(f, "Confidence: `{}` ({method})\n", self.confidence)?;
        writeln!(f, "## {title} columns ({})\n", self.columns.len())?;
        writeln!(f, "| column | via | kind | confidence |")?;
        writeln!(f, "| --- | --- | --- | --- |")?;
        for r in &self.columns {
            let via = r
                .via
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let kinds = r
                .kinds
                .iter()
                .map(ColumnEdgeKind::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                f,
                "| `{}` | {} | {} | {} |",
                r.column, via, kinds, r.confidence
            )?;
        }
        Ok(())
    }
}
