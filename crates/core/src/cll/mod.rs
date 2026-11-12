//! Column-level lineage (CLL) v0: the shared data model, the replaceable
//! [`ColumnResolver`] boundary, and the name-match approximation
//! ([`NameMatchResolver`]).
//!
//! The API is the contract later stages rely on: the real SQL parser
//! implements the same [`ColumnResolver`] trait and drops into every consumer
//! (the lazy [`ColumnLineage`] engine, the structure modal, the `y` menu, and
//! `dbtl columns`) without a signature change. Only [`NameMatchResolver`]'s
//! heuristics are v0-specific; everything above it is meant to be permanent.
//!
//! Laziness is a requirement, not an optimisation: nothing is resolved at load
//! time. [`ColumnLineage`] resolves a node's in-edges the first time a query
//! touches them and memoises the result, so a one-column closure walks only the
//! nodes it reaches — never the whole graph.

mod sql_parse;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde::Serialize;

pub(crate) use sql_parse::trailing_semicolon;
pub use sql_parse::{MarkedRelations, SqlDialect, SqlFacts, SqlParseResolver, SqlPositions};

use crate::catalog::Catalog;
use crate::dag::Dag;

/// Normalize a column identifier for comparison: an unquoted name folds to
/// lowercase (Snowflake uppercases, Postgres/Redshift lowercase — v0 picks one
/// canonical form rather than guessing the dialect); a double-quoted,
/// backticked, or bracketed name keeps its written case, sans quotes.
/// Dialect-aware casing is the parser stage's problem.
pub fn normalize_column(name: &str) -> String {
    let t = name.trim();
    let b = t.as_bytes();
    if b.len() >= 2 {
        let (a, z) = (b[0], b[b.len() - 1]);
        if (a == b'"' && z == b'"') || (a == b'`' && z == b'`') || (a == b'[' && z == b']') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_lowercase()
}

/// A column of a [`Dag`] node, addressed by `unique_id` + normalized name.
/// `Ord` so column sets are `BTreeSet`-stable — closures print sorted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ColumnRef {
    /// The node's `unique_id` (same key space as [`Dag::nodes`]).
    pub node: String,
    /// The normalized column name (see [`normalize_column`]).
    pub column: String,
}

impl ColumnRef {
    /// A column reference; the column is normalized on the way in so callers
    /// never have to remember to fold case.
    pub fn new(node: impl Into<String>, column: &str) -> Self {
        ColumnRef {
            node: node.into(),
            column: normalize_column(column),
        }
    }
}

/// First-occurrence-order dedupe on a NORMALIZED column list: two declared
/// names can fold to one key (`Status` vs `status`, `x` vs `"x"`), and every
/// downstream key space (`column_counts`, the modal's `col_counts`) is per
/// normalized name — a duplicate row would just read the same counts twice.
fn dedup_columns(cols: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    cols.retain(|c| seen.insert(c.clone()));
}

/// How a column edge was derived. `Ord` so `BTreeSet<ColumnEdge>` output is
/// deterministic; `Serialize` (snake_case) for `--format json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnEdgeKind {
    /// Same-named column carried through (a bare `x`, `x.*`, or `*` select).
    Passthrough,
    /// `x AS c` where `x` is a column of a direct parent.
    Rename,
    /// A computed expression feeding the output (`sum(x)`, `upper(x)`, …):
    /// the edge records that the parent's column *influences* the value.
    Transform,
    /// The parent column appears only in the FROM/JOIN/WHERE/GROUP BY tail —
    /// it shapes which rows exist, not any output column's value. v0 attaches
    /// it to every output column of the node.
    FilterOrJoin,
}

impl std::fmt::Display for ColumnEdgeKind {
    /// The `snake_case` wire name — same spelling `--format json` emits, so
    /// the Markdown table and the JSON payload can never disagree.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Passthrough => "passthrough",
            Self::Rename => "rename",
            Self::Transform => "transform",
            Self::FilterOrJoin => "filter_or_join",
        })
    }
}

/// How much trust an edge deserves. `Ord`/`Serialize` like
/// [`ColumnEdgeKind`]. `Stale` sorts after `NameMatch` and `Exact`, so a
/// `max()` over a row's links / a report's rows reports the most conservative
/// basis used — the "weakest link" semantics both surfaces share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Resolved by a real SQL parser — the parser stage emits this; v0 never does.
    Exact,
    /// The name-match approximation: directionally right, can
    /// over-connect (a qualified `t.x` binds to every parent that has `x`).
    NameMatch,
    /// Resolved from a `target/compiled/` file older than its source `.sql` —
    /// name-match edges on text that may no longer describe the model.
    Stale,
}

impl std::fmt::Display for Confidence {
    /// The `snake_case` wire name — same spelling `--format json` emits.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Exact => "exact",
            Self::NameMatch => "name_match",
            Self::Stale => "stale",
        })
    }
}

/// One column→column dependency: `from` (a direct parent's column) feeds
/// `to` (a column of the resolved node). Both refs are normalized.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ColumnEdge {
    pub from: ColumnRef,
    pub to: ColumnRef,
    pub kind: ColumnEdgeKind,
    pub confidence: Confidence,
}

/// The SQL text a name-match resolution should read, plus the confidence its
/// edges carry: compiled SQL first (marked [`Confidence::Stale`] when the file
/// is older than the source), else the raw (Jinja-unexpanded) text. Shared by
/// [`NameMatchResolver`] and the structure modal's confidence tag so the two
/// never disagree about which text described the node.
pub fn resolution_sql<'a>(dag: &'a Dag, node: &str) -> Option<(&'a str, Confidence)> {
    if let Some(c) = dag.compiled(node) {
        if !c.code.trim().is_empty() {
            return Some((
                c.code.as_str(),
                if c.stale {
                    Confidence::Stale
                } else {
                    Confidence::NameMatch
                },
            ));
        }
    }
    dag.raw_code(node)
        .filter(|code| !code.trim().is_empty())
        .map(|code| (code, Confidence::NameMatch))
}

/// The replaceable boundary the parser stage plugs into. A resolver answers two
/// questions *per node*: what its output columns are called, and which parent
/// columns feed them. Everything else (lazy traversal, the TUI, the CLI) is
/// built on this and never sees the resolution strategy.
///
/// Contract: [`resolve`](ColumnResolver::resolve) returns the edges INTO
/// `node` — every `edge.to.node` equals `node` — so a node's edges live with
/// its own resolution and the lazy engine can answer "what feeds column c"
/// without touching downstream nodes. The engine filters out non-conforming
/// edges defensively.
pub trait ColumnResolver {
    /// The node's output column names (normalized). Default: the documented
    /// (`schema.yml` / manifest `columns`) list — the same names the structure
    /// modal shows. Resolvers override this when they can see deeper
    /// (warehouse catalog, SQL extraction, …).
    fn columns(&self, dag: &Dag, node: &str) -> Vec<String> {
        dag.detail(node)
            .map(|d| {
                let mut cols: Vec<String> = d
                    .columns
                    .iter()
                    .map(|c| normalize_column(&c.name))
                    .collect();
                dedup_columns(&mut cols);
                cols
            })
            .unwrap_or_default()
    }

    /// The column edges INTO `node`'s output columns (`to.node == node`),
    /// sorted and deduplicated. `dag` supplies the topology (direct parents),
    /// declared columns, catalog side-map, and SQL text.
    fn resolve(&self, dag: &Dag, node: &str) -> Vec<ColumnEdge>;

    /// The best-known `data_type` for `column` (a normalized name) of `node`,
    /// following the same source priority as [`columns`](ColumnResolver::columns):
    /// catalog → declared. `None` when nothing records a type. Default: the
    /// declared (`schema.yml` / manifest `columns`) type only — resolvers
    /// holding a catalog overlay it. The `--diff` column analysis uses this
    /// for type-change classification.
    fn column_type(&self, dag: &Dag, node: &str, column: &str) -> Option<String> {
        dag.detail(node).and_then(|d| {
            d.columns
                .iter()
                .find(|c| normalize_column(&c.name) == column)
                .and_then(|c| c.data_type.clone())
        })
    }

    /// The node's output-column SELECT expressions, normalized for
    /// cross-manifest comparison: `output name -> canonical expression text`
    /// (identifier case folded like [`normalize_column`], formatting and
    /// comments erased by the parse). `None` when the resolver has no
    /// expression-level view of the node — missing/unparsable SQL, or a
    /// strategy without an AST (the [`NameMatchResolver`] default) — which
    /// callers must treat as "unknown", never "changed". The `--diff` column
    /// analysis uses this for expression-change classification; a
    /// formatting-only edit leaves every expression identical.
    fn column_exprs(&self, dag: &Dag, node: &str) -> Option<BTreeMap<String, String>> {
        let _ = (dag, node);
        None
    }

    /// The node's column references that provably resolve to NOTHING:
    /// `(parent uid, normalized column)` pairs the SQL names but the bound
    /// parent does not emit — a qualified `t.c` where `t` binds a parent
    /// whose KNOWN column list lacks `c`, or an unqualified `c` absent from
    /// every bound parent's known list. The `--diff` column analysis uses
    /// this to separate a consumer that still *references* a removed column
    /// (its query can't run — breaking) from one that dropped the reference
    /// deliberately (migrated): the two produce the same current edge set —
    /// the edge simply doesn't form — so only this signal tells them apart.
    /// Default: empty — a resolver without binding-level scope knowledge
    /// (the name-match approximation) can't prove a miss and must not
    /// guess.
    fn unresolved_refs(&self, dag: &Dag, node: &str) -> Vec<(String, String)> {
        let _ = (dag, node);
        Vec::new()
    }
}

/// Lazy, memoized column lineage over a [`Dag`] + [`ColumnResolver`]: resolves
/// each node's in-edges on first touch, caches them, and answers 1-hop and
/// transitive column queries against the memo. `BTreeMap`-backed so a full
/// dump iterates deterministically.
///
/// Borrows the `Dag` and resolver — build one per query pass (the detail modal,
/// a yank, a CLI run); construction is free, resolution is paid per node asked.
pub struct ColumnLineage<'a, R: ColumnResolver + ?Sized> {
    dag: &'a Dag,
    resolver: &'a R,
    /// In-edges resolved so far, keyed by the `to` node's uid. Interior
    /// mutability keeps query methods `&self`; `Rc` lets callers hold a node's
    /// answer while the map keeps growing.
    resolved: RefCell<BTreeMap<String, Rc<Vec<ColumnEdge>>>>,
}

impl<'a, R: ColumnResolver + ?Sized> ColumnLineage<'a, R> {
    /// A lineage engine over `dag` driven by `resolver` — explicitly supplied,
    /// so tests pin behaviour to a chosen strategy (the parser slots
    /// in behind the same trait).
    pub fn new(dag: &'a Dag, resolver: &'a R) -> Self {
        ColumnLineage {
            dag,
            resolver,
            resolved: RefCell::new(BTreeMap::new()),
        }
    }

    /// The node's output columns per the resolver's own column-list strategy.
    pub fn columns(&self, node: &str) -> Vec<String> {
        self.resolver.columns(self.dag, node)
    }

    /// The node's in-edges (`to.node == node`), resolved once then memoized.
    /// Sorted + deduplicated — two queries over the same graph agree
    /// byte-for-byte regardless of map iteration order.
    pub fn in_edges(&self, node: &str) -> Rc<Vec<ColumnEdge>> {
        if let Some(v) = self.resolved.borrow().get(node) {
            return v.clone();
        }
        let mut edges: Vec<ColumnEdge> = self
            .resolver
            .resolve(self.dag, node)
            .into_iter()
            .filter(|e| e.to.node == node)
            .collect();
        edges.sort();
        edges.dedup();
        let rc = Rc::new(edges);
        self.resolved
            .borrow_mut()
            .insert(node.to_string(), rc.clone());
        rc
    }

    /// The 1-hop upstream column edges of `c` (who feeds it).
    pub fn upstream(&self, c: &ColumnRef) -> Vec<ColumnEdge> {
        self.in_edges(&c.node)
            .iter()
            .filter(|e| e.to == *c)
            .cloned()
            .collect()
    }

    /// The 1-hop downstream column edges of `c` (who it feeds). Resolves each
    /// direct child of `c`'s node once (memoized), so a column in a fan-out
    /// model pays for its children, not the whole graph.
    pub fn downstream(&self, c: &ColumnRef) -> Vec<ColumnEdge> {
        let mut out = Vec::new();
        for child in self.dag.direct_children(&c.node) {
            for e in self.in_edges(child).iter() {
                if e.from == *c {
                    out.push(e.clone());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The transitive downstream column closure: every column `c` reaches,
    /// excluding `c` itself. Cycle-safe (a cyclic column can never report
    /// itself). Deterministic: `BTreeSet` order is `(node uid, column)`.
    pub fn closure_down(&self, c: &ColumnRef) -> BTreeSet<ColumnRef> {
        self.closure(c, true)
    }

    /// The transitive upstream column closure (every column feeding `c`),
    /// excluding `c` itself.
    pub fn closure_up(&self, c: &ColumnRef) -> BTreeSet<ColumnRef> {
        self.closure(c, false)
    }

    /// BFS over 1-hop column edges — each visited node resolves once via the
    /// memo, so the closure only ever computes the region it touches.
    fn closure(&self, c: &ColumnRef, down: bool) -> BTreeSet<ColumnRef> {
        let mut seen: BTreeSet<ColumnRef> = BTreeSet::new();
        let mut stack = vec![c.clone()];
        seen.insert(c.clone());
        while let Some(cur) = stack.pop() {
            let nexts = if down {
                self.downstream(&cur)
            } else {
                self.upstream(&cur)
            };
            for e in nexts {
                let tgt = if down { e.to } else { e.from };
                if seen.insert(tgt.clone()) {
                    stack.push(tgt);
                }
            }
        }
        seen.remove(c);
        seen
    }

    /// All of `node`'s output columns (per the resolver's column list) with
    /// their `(upstream, downstream)` ONE-HOP column-edge counts — the direct
    /// parent columns feeding each output, and the direct child columns it
    /// feeds (the direct-edge `↑u ↓d` counts, not transitive closure sizes).
    /// Resolves `node` and its direct children once (memoized). `BTreeMap` →
    /// deterministic iteration order (normalized name).
    pub fn column_counts(&self, node: &str) -> BTreeMap<String, (usize, usize)> {
        let mut out = BTreeMap::new();
        for c in self.columns(node) {
            let col = ColumnRef::new(node, &c);
            out.insert(c, (self.upstream(&col).len(), self.downstream(&col).len()));
        }
        out
    }

    /// The explicitly-eager answer for callers that genuinely need the whole
    /// graph (the full-lineage compute): resolve every node once,
    /// concatenate in-edges, sort + dedup.
    pub fn resolve_all(&self) -> Vec<ColumnEdge> {
        let mut uids: Vec<&String> = self.dag.nodes().keys().collect();
        uids.sort();
        let mut out = Vec::new();
        for uid in uids {
            out.extend(self.in_edges(uid).iter().cloned());
        }
        out.sort();
        out.dedup();
        out
    }
}

mod name_match;
mod report;
mod scan;
#[cfg(test)]
mod tests;

pub use name_match::NameMatchResolver;
pub use report::{closure_report, ClosureDirection, ColumnReport, ColumnRow};
use scan::*;
