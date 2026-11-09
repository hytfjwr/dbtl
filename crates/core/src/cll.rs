//! Column-level lineage (CLL) v0: the shared data model, the replaceable
//! [`ColumnResolver`] boundary, and Issue 07's name-match approximation
//! ([`NameMatchResolver`]).
//!
//! The API is the contract later stages rely on: Issue 08's real SQL parser
//! implements the same [`ColumnResolver`] trait and drops into every consumer
//! (the lazy [`ColumnLineage`] engine, the structure modal, the `y` menu, and
//! `dbtl columns`) without a signature change. Only [`NameMatchResolver`]'s
//! heuristics are v0-specific; everything above it is meant to be permanent.
//!
//! Laziness is a requirement, not an optimisation: nothing is resolved at load
//! time. [`ColumnLineage`] resolves a node's in-edges the first time a query
//! touches them and memoises the result, so a one-column closure walks only the
//! nodes it reaches — never the whole graph.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde::Serialize;

use crate::catalog::Catalog;
use crate::dag::Dag;

/// Normalize a column identifier for comparison: an unquoted name folds to
/// lowercase (Snowflake uppercases, Postgres/Redshift lowercase — v0 picks one
/// canonical form rather than guessing the dialect); a double-quoted,
/// backticked, or bracketed name keeps its written case, sans quotes.
/// Dialect-aware casing is Issue 08's problem.
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
/// `min()` over a node's edges reports the freshest basis it used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Resolved by a real SQL parser — Issue 08 emits this; v0 never does.
    Exact,
    /// Issue 07's name-match approximation: directionally right, can
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

/// The replaceable boundary Issue 08 plugs into. A resolver answers two
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
                d.columns
                    .iter()
                    .map(|c| normalize_column(&c.name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The column edges INTO `node`'s output columns (`to.node == node`),
    /// sorted and deduplicated. `dag` supplies the topology (direct parents),
    /// declared columns, catalog side-map, and SQL text.
    fn resolve(&self, dag: &Dag, node: &str) -> Vec<ColumnEdge>;
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
    /// so tests pin behaviour to a chosen strategy (the Issue 08 parser slots
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
    /// their `(upstream, downstream)` transitive column-closure sizes — the
    /// numbers the structure modal prints beside each column row. Resolves
    /// `node` and its direct children once (memoized). `BTreeMap` →
    /// deterministic iteration order (normalized name).
    pub fn column_counts(&self, node: &str) -> BTreeMap<String, (usize, usize)> {
        let mut out = BTreeMap::new();
        for c in self.columns(node) {
            let col = ColumnRef::new(node, &c);
            out.insert(
                c,
                (self.closure_up(&col).len(), self.closure_down(&col).len()),
            );
        }
        out
    }

    /// The explicitly-eager answer for callers that genuinely need the whole
    /// graph (the Issue 10 full-lineage compute): resolve every node once,
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

// ---------------------------------------------------------------------------
// NameMatchResolver — Issue 07's approximation
// ---------------------------------------------------------------------------

/// The v0 resolution strategy: column names + a naïve scan of the outermost
/// SELECT. Everything it produces is [`Confidence::NameMatch`] (or
/// [`Confidence::Stale`] when it had to read a stale compiled file) — the UI
/// can always distinguish approximation from Issue 08's `Exact`.
///
/// Internal memos (`col_memo`/`sql_memo`) make per-node answers cheap to reuse
/// across [`ColumnLineage`] passes. They are keyed by uid alone, so the
/// resolver belongs to ONE `Dag` lifetime: the `App` rebuilds it wherever the
/// `Dag` is replaced (reload / manifest adoption).
pub struct NameMatchResolver {
    /// Warehouse truth (`catalog.json`): the highest-priority column source.
    /// `Rc`-shared with the `App`'s copy so the detail modal and CLL read the
    /// same artifact.
    catalog: Option<Rc<Catalog>>,
    /// Memoized `columns` answers per uid.
    col_memo: RefCell<BTreeMap<String, Rc<Vec<String>>>>,
    /// Memoized masked-SQL parses per uid (select items + region identifiers).
    sql_memo: RefCell<BTreeMap<String, Rc<ParsedSql>>>,
}

impl NameMatchResolver {
    /// A resolver with an optional shared `catalog.json`. `None` drops the top
    /// priority tier (column lists then start at the documented ones).
    pub fn new(catalog: Option<Rc<Catalog>>) -> Self {
        NameMatchResolver {
            catalog,
            col_memo: RefCell::new(BTreeMap::new()),
            sql_memo: RefCell::new(BTreeMap::new()),
        }
    }

    /// `columns` with memoization + a `*`-expansion recursion guard. `visiting`
    /// tracks the expansion stack so a pathological column cycle degrades to
    /// "no columns" for the re-entered node instead of recursing forever.
    fn columns_rc(
        &self,
        dag: &Dag,
        node: &str,
        visiting: &mut BTreeSet<String>,
    ) -> Rc<Vec<String>> {
        if let Some(v) = self.col_memo.borrow().get(node) {
            return v.clone();
        }
        if !visiting.insert(node.to_string()) {
            return Rc::new(Vec::new());
        }
        let cols = self.compute_columns(dag, node, visiting);
        visiting.remove(node);
        let rc = Rc::new(cols);
        self.col_memo
            .borrow_mut()
            .insert(node.to_string(), rc.clone());
        rc
    }

    /// The column-list precedence — `catalog.json` columns → documented
    /// (`schema.yml`/manifest) columns → outermost-SELECT extraction — first
    /// non-empty wins. `*` / `t.*` items expand to the direct parents' column
    /// union (a `select *` staging model's columns are its parent's).
    fn compute_columns(
        &self,
        dag: &Dag,
        node: &str,
        visiting: &mut BTreeSet<String>,
    ) -> Vec<String> {
        if let Some(cat) = &self.catalog {
            if let Some(n) = cat.get(node) {
                if !n.columns.is_empty() {
                    return n.columns.keys().map(|k| normalize_column(k)).collect();
                }
            }
        }
        if let Some(d) = dag.detail(node) {
            if !d.columns.is_empty() {
                return d
                    .columns
                    .iter()
                    .map(|c| normalize_column(&c.name))
                    .collect();
            }
        }
        let Some((text, _)) = resolution_sql(dag, node) else {
            return Vec::new();
        };
        let parsed = self.parsed(node, text);
        let mut out: Vec<String> = Vec::new();
        for item in &parsed.items {
            match &item.kind {
                ItemKind::Wildcard => {
                    let mut parents = dag.direct_parents(node).to_vec();
                    parents.sort();
                    for p in parents {
                        for c in self.columns_rc(dag, &p, visiting).iter() {
                            if !out.contains(c) {
                                out.push(c.clone());
                            }
                        }
                    }
                }
                _ => {
                    if let Some(alias) = &item.alias {
                        if !out.contains(alias) {
                            out.push(alias.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// The masked-SQL parse for a node, computed once and memoized. The `text`
    /// argument is the resolution SQL (compiled-first); deterministic per uid.
    fn parsed(&self, node: &str, text: &str) -> Rc<ParsedSql> {
        if let Some(v) = self.sql_memo.borrow().get(node) {
            return v.clone();
        }
        let rc = Rc::new(parse_select(&mask_sql(text)));
        self.sql_memo
            .borrow_mut()
            .insert(node.to_string(), rc.clone());
        rc
    }
}

impl ColumnResolver for NameMatchResolver {
    fn columns(&self, dag: &Dag, node: &str) -> Vec<String> {
        self.columns_rc(dag, node, &mut BTreeSet::new())
            .as_ref()
            .clone()
    }

    /// Edges into `node`. Per output column `c` of the node, against each
    /// direct parent `P`:
    /// - a select item producing `c` decides the edge: bare `x` →
    ///   [`Passthrough`](ColumnEdgeKind::Passthrough), `x AS c` (`x` a parent
    ///   column) → [`Rename`](ColumnEdgeKind::Rename), any other expression's
    ///   identifiers that name parent columns →
    ///   [`Transform`](ColumnEdgeKind::Transform), and `*` / `t.*` →
    ///   passthrough for every parent column the node outputs.
    /// - when no select item produces `c` (a documented column the extraction
    ///   can't see), the spec's fallback applies: `c` present in the node's
    ///   SQL and `c` a parent column → `Passthrough`.
    /// - a parent column that appears in the FROM/JOIN/WHERE/GROUP BY tail and
    ///   carries no direct edge → [`FilterOrJoin`](ColumnEdgeKind::FilterOrJoin)
    ///   to every output column (it shapes the row set, not a value).
    fn resolve(&self, dag: &Dag, node: &str) -> Vec<ColumnEdge> {
        let Some((text, confidence)) = resolution_sql(dag, node) else {
            return Vec::new();
        };
        let parsed = self.parsed(node, text);
        let out_cols = self.columns_rc(dag, node, &mut BTreeSet::new());
        if out_cols.is_empty() {
            return Vec::new();
        }
        let out_set: BTreeSet<&String> = out_cols.iter().collect();
        let mut parents = dag.direct_parents(node).to_vec();
        parents.sort();
        let parent_cols: Vec<(String, Rc<Vec<String>>)> = parents
            .iter()
            .map(|p| (p.clone(), self.columns_rc(dag, p, &mut BTreeSet::new())))
            .collect();
        let has_col = |cols: &[String], c: &str| cols.iter().any(|x| x == c);

        let mut edges: BTreeSet<ColumnEdge> = BTreeSet::new();
        // Parent (uid, column) pairs that already carry a direct (value) edge —
        // FilterOrJoin is only for columns that appear in the tail and have
        // none ("appears ONLY there").
        let mut direct_sources: BTreeSet<(String, String)> = BTreeSet::new();
        // Output columns some select item claims to produce — the fallback
        // passthrough only covers columns no item named.
        let mut produced: BTreeSet<String> = BTreeSet::new();
        let mut edge =
            |from_p: &str, from_c: &str, to_c: &str, kind: ColumnEdgeKind| -> ColumnEdge {
                direct_sources.insert((from_p.to_string(), from_c.to_string()));
                ColumnEdge {
                    from: ColumnRef::new(from_p, from_c),
                    to: ColumnRef::new(node, to_c),
                    kind,
                    confidence,
                }
            };

        for item in &parsed.items {
            match &item.kind {
                ItemKind::Wildcard => {
                    for c in out_cols.iter() {
                        if parent_cols.iter().any(|(_, cols)| has_col(cols, c)) {
                            produced.insert(c.clone());
                            for (p, cols) in &parent_cols {
                                if has_col(cols, c) {
                                    edges.insert(edge(p, c, c, ColumnEdgeKind::Passthrough));
                                }
                            }
                        }
                    }
                }
                ItemKind::Ident(src) => {
                    let Some(alias) = &item.alias else { continue };
                    if !out_set.contains(alias) {
                        continue;
                    }
                    produced.insert(alias.clone());
                    let kind = if src == alias {
                        ColumnEdgeKind::Passthrough
                    } else {
                        ColumnEdgeKind::Rename
                    };
                    for (p, cols) in &parent_cols {
                        if has_col(cols, src) {
                            edges.insert(edge(p, src, alias, kind));
                        }
                    }
                }
                ItemKind::Expr(idents) => {
                    let Some(alias) = &item.alias else { continue };
                    if !out_set.contains(alias) {
                        continue;
                    }
                    produced.insert(alias.clone());
                    for (p, cols) in &parent_cols {
                        for ident in idents {
                            if has_col(cols, ident) {
                                edges.insert(edge(p, ident, alias, ColumnEdgeKind::Transform));
                            }
                        }
                    }
                }
            }
        }

        // Fallback passthrough (the spec's "identifier c appears in the SQL"):
        // a documented column no select item produced is assumed carried
        // through verbatim when its name appears in the SQL at all (e.g. a
        // column behind `select *` that the parent's column list can't see).
        for c in out_cols.iter() {
            if produced.contains(c) || !parsed.all_idents.contains(c) {
                continue;
            }
            for (p, cols) in &parent_cols {
                if has_col(cols, c) {
                    edges.insert(edge(p, c, c, ColumnEdgeKind::Passthrough));
                }
            }
        }

        // FilterOrJoin: parent columns named in the FROM/JOIN/WHERE/GROUP BY
        // tail with no value edge — they filter or join the row set, which is
        // why v0 attaches them to every output column.
        for (p, cols) in &parent_cols {
            for pc in cols.iter() {
                if !parsed.filter_idents.contains(pc)
                    || direct_sources.contains(&(p.clone(), pc.clone()))
                {
                    continue;
                }
                for c in out_cols.iter() {
                    edges.insert(ColumnEdge {
                        from: ColumnRef::new(p, pc),
                        to: ColumnRef::new(node, c),
                        kind: ColumnEdgeKind::FilterOrJoin,
                        confidence,
                    });
                }
            }
        }

        edges.into_iter().collect()
    }
}

// ---------------------------------------------------------------------------
// Naïve SQL scanning (deliberately scanner-grade; Issue 08 replaces it)
// ---------------------------------------------------------------------------

/// One outermost-select-list item: how it names its output column (`alias`)
/// and what it draws from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemKind {
    /// `*` or `t.*` — every output column it contributes is a passthrough of
    /// the same-named parent column.
    Wildcard,
    /// A single (possibly qualified) identifier — the normalized source name
    /// (last dotted segment).
    Ident(String),
    /// Any other expression: the normalized identifiers inside it (potential
    /// transform sources).
    Expr(BTreeSet<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectItem {
    /// The output column this item produces (normalized): `x AS c` → `c`,
    /// a bare `x` / `t.x` → `x`. `None` only on a wildcard item.
    alias: Option<String>,
    kind: ItemKind,
}

/// The masked-SQL parse result: outermost select items + identifier sets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSql {
    /// Outermost select-list items in order.
    items: Vec<SelectItem>,
    /// Identifiers appearing after the outermost FROM keyword — the
    /// JOIN/ON/WHERE/GROUP BY/HAVING/ORDER tail (name-match can't tell a join
    /// key from a filter; both are FilterOrJoin).
    filter_idents: BTreeSet<String>,
    /// Every identifier in the masked SQL — the fallback presence test.
    all_idents: BTreeSet<String>,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let mut s = b;
    while !s.is_empty() && s[0].is_ascii_whitespace() {
        s = &s[1..];
    }
    while !s.is_empty() && s[s.len() - 1].is_ascii_whitespace() {
        s = &s[..s.len() - 1];
    }
    s
}

/// Is `kw` at byte offset `i` of `b`, on an identifier boundary, ASCII
/// case-insensitive? (`select` inside `myselect` or `selection` does not count.)
fn keyword_at(b: &[u8], i: usize, kw: &[u8]) -> bool {
    let Some(end) = i.checked_add(kw.len()) else {
        return false;
    };
    end <= b.len()
        && b[i..end].eq_ignore_ascii_case(kw)
        && (i == 0 || !is_ident_byte(b[i - 1]))
        && (end == b.len() || !is_ident_byte(b[end]))
}

/// A set-operator that closes the outermost query for v0 purposes: a second
/// UNION/INTERSECT/EXCEPT arm's columns must not leak into the first arm's
/// select list nor its filter region (v0 answers from the first arm only).
fn is_set_operator(b: &[u8], i: usize) -> bool {
    keyword_at(b, i, b"union") || keyword_at(b, i, b"intersect") || keyword_at(b, i, b"except")
}

/// Mask comment / string / Jinja regions of `sql` to spaces (length preserved,
/// newlines kept). `--` / `/* */` comments, `'…'` string literals (with `''`
/// escapes), and `{{ }}` / `{% %}` / `{# #}` Jinja all blank out — what remains
/// is the SQL the scanner actually parses, so a column name inside a comment
/// or a quoted literal can never masquerade as a reference. Double-quoted and
/// backticked identifiers are NOT masked (they are identifiers).
fn mask_sql(sql: &str) -> Vec<u8> {
    let b = sql.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        let from = i;
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b'\'' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2; // '' escape stays inside the literal
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'{' if matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) => {
                let close = match b[i + 1] {
                    b'{' => b"}}",
                    b'%' => b"%}",
                    _ => b"#}",
                };
                i += 2;
                while i + 1 < b.len() && !(b[i] == close[0] && b[i + 1] == close[1]) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            _ => {
                i += 1;
                continue;
            }
        }
        for c in out.iter_mut().take(i).skip(from) {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    }
    out
}

/// All identifiers in `text`, normalized: unquoted words fold lowercase;
/// `"…"` / `` `…` `` keep their case (sans quotes). `[…]` bracket text is NOT
/// scanned — in dbt-targeted SQL it is overwhelmingly array indexing, not a
/// quoted identifier.
fn idents(text: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < text.len() {
        let b = text[i];
        if b == b'"' || b == b'`' {
            let start = i;
            i += 1;
            while i < text.len() && text[i] != b && text[i] != b'\n' {
                i += 1;
            }
            if i < text.len() {
                i += 1; // consume the closing quote
                let tok = std::str::from_utf8(&text[start..i]).unwrap_or("");
                out.insert(normalize_column(tok));
            }
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < text.len() && is_ident_byte(text[i]) {
                i += 1;
            }
            out.insert(
                String::from_utf8_lossy(&text[start..i])
                    .to_lowercase()
                    .to_string(),
            );
        } else {
            i += 1;
        }
    }
    out
}

/// The last depth-0 (outside parens) position of `kw` in `b`, on word
/// boundaries. `x AS c` is the ONLY alias syntax v0 honours (a bare `expr c`
/// alias is too ambiguous to name-match safely); the LAST depth-0 `as` splits
/// `cast(x as int) as c` correctly (the inner `as` nests inside parens).
fn last_top_keyword(b: &[u8], kw: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut found = None;
    for (i, &ch) in b.iter().enumerate() {
        match ch {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if depth == 0 && keyword_at(b, i, kw) => found = Some(i),
            _ => {}
        }
    }
    found
}

/// A whole-token identifier: `a`, `a.b.c`, `"Quoted"`, `` `quoted` `` —
/// returned as the normalized LAST segment (the column-name position).
fn ident_last_segment(t: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(t).ok()?.trim();
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"')
            || (b[0] == b'`' && b[b.len() - 1] == b'`')
            || (b[0] == b'[' && b[b.len() - 1] == b']'))
    {
        return (!s[1..s.len() - 1].is_empty()).then(|| normalize_column(s));
    }
    let mut last = "";
    for seg in s.split('.') {
        let sb = seg.as_bytes();
        if sb.is_empty()
            || !(sb[0].is_ascii_alphabetic() || sb[0] == b'_')
            || !sb.iter().all(|&c| is_ident_byte(c))
        {
            return None;
        }
        last = seg;
    }
    Some(normalize_column(last))
}

/// Parse one select-list item (already masked): wildcards, `x AS c`, bare
/// identifiers, and named expressions. Unnamed non-identifier expressions
/// (`count(*)` with no alias) are not nameable output columns — skipped.
fn parse_item(item: &[u8]) -> Option<SelectItem> {
    let t = trim_ascii(item);
    if t.is_empty() {
        return None;
    }
    if t == b"*" || (t.ends_with(b".*") && ident_last_segment(&t[..t.len() - 2]).is_some()) {
        return Some(SelectItem {
            alias: None,
            kind: ItemKind::Wildcard,
        });
    }
    if let Some(pos) = last_top_keyword(t, b"as") {
        if let Some(alias) = ident_last_segment(trim_ascii(&t[pos + 2..])) {
            let expr = trim_ascii(&t[..pos]);
            let kind = match ident_last_segment(expr) {
                Some(src) => ItemKind::Ident(src),
                None => ItemKind::Expr(idents(expr)),
            };
            return Some(SelectItem {
                alias: Some(alias),
                kind,
            });
        }
        // An `as` without a clean alias — fall through to whole-item tests.
    }
    if let Some(name) = ident_last_segment(t) {
        return Some(SelectItem {
            alias: Some(name.clone()),
            kind: ItemKind::Ident(name),
        });
    }
    None
}

/// Parse the masked SQL: the OUTERMOST select list (the model's output
/// columns) plus the identifier sets for the passthrough/fallback and
/// filter-or-join tests.
///
/// "Outermost" = the first `select` keyword at paren depth 0, which is the
/// model's own list — CTE bodies and subqueries nest inside parens and never
/// claim the top slot. The list runs to the first depth-0 `from`; the
/// filter/join region is everything after it up to a depth-0
/// `union`/`intersect`/`except` (a second UNION arm's columns would otherwise
/// leak false FilterOrJoin hits — v0 answers from the first arm only).
fn parse_select(masked: &[u8]) -> ParsedSql {
    let all_idents = idents(masked);
    let empty = || ParsedSql {
        items: Vec::new(),
        filter_idents: BTreeSet::new(),
        all_idents: all_idents.clone(),
    };
    // First depth-0 `select`.
    let mut depth = 0i32;
    let mut select_end = None;
    for (i, &ch) in masked.iter().enumerate() {
        match ch {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if depth == 0 && keyword_at(masked, i, b"select") => {
                select_end = Some(i + 6);
                break;
            }
            _ => {}
        }
    }
    let Some(mut list_start) = select_end else {
        return empty();
    };
    // A leading DISTINCT / ALL keyword is list decoration, not part of an item.
    {
        let ws = masked[list_start..]
            .iter()
            .take_while(|b| b.is_ascii_whitespace())
            .count();
        let w = list_start + ws;
        for kw in [b"distinct".as_slice(), b"all".as_slice()] {
            if keyword_at(masked, w, kw) {
                list_start = w + kw.len();
                break;
            }
        }
    }
    // List end = first depth-0 `from`, else the first depth-0 set-operator,
    // else end of text.
    let mut depth = 0i32;
    let mut from_pos = None;
    let mut boundary = masked.len();
    for (i, &ch) in masked.iter().enumerate().skip(list_start) {
        match ch {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if depth == 0 && keyword_at(masked, i, b"from") => {
                from_pos = Some(i);
                break;
            }
            _ if depth == 0 && is_set_operator(masked, i) => {
                boundary = i;
                break;
            }
            _ => {}
        }
    }
    let list_end = from_pos.unwrap_or(boundary);
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut start = list_start;
    for i in list_start..list_end {
        match masked[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                if let Some(item) = parse_item(&masked[start..i]) {
                    items.push(item);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if let Some(item) = parse_item(&masked[start..list_end]) {
        items.push(item);
    }
    // The filter region: after `from`, up to the first depth-0 set-operator.
    let filter_idents = match from_pos {
        Some(from) => {
            let mut depth = 0i32;
            let mut end = masked.len();
            for (i, &ch) in masked.iter().enumerate().skip(from + 4) {
                match ch {
                    b'(' | b'[' => depth += 1,
                    b')' | b']' => depth -= 1,
                    _ if depth == 0 && is_set_operator(masked, i) => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            idents(&masked[from..end])
        }
        None => BTreeSet::new(),
    };
    ParsedSql {
        items,
        filter_idents,
        all_idents,
    }
}

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
    /// The freshest confidence on the links (`Exact`/`NameMatch` before
    /// `Stale`).
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
            .min()
            .unwrap_or(Confidence::NameMatch);
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
        .unwrap_or(Confidence::NameMatch);
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
        writeln!(
            f,
            "Confidence: `{}` (column-level lineage v0: name-match approximation)\n",
            self.confidence
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RawManifest, RawNode};
    use std::collections::HashMap;

    /// A `RawNode` literal with a raw SQL body and optional declared columns.
    fn node(uid: &str, sql: &str, cols: &[&str]) -> (String, RawNode) {
        let columns: serde_json::Map<String, serde_json::Value> = cols
            .iter()
            .map(|c| {
                (
                    c.to_string(),
                    serde_json::json!({"name": c, "data_type": "int"}),
                )
            })
            .collect();
        let name = uid.rsplit('.').next().unwrap_or(uid).to_string();
        let file = format!("models/{name}.sql");
        (
            uid.to_string(),
            RawNode {
                name,
                resource_type: "model".to_string(),
                raw_code: Some(sql.to_string()),
                columns,
                original_file_path: Some(file),
                ..Default::default()
            },
        )
    }

    /// A minimal manifest of `uid -> (sql, declared cols)` models plus
    /// `parent -> child` topology.
    fn manifest(specs: &[(&str, &str, &[&str])], edges: &[(&str, &str)]) -> RawManifest {
        let mut nodes = HashMap::new();
        for (uid, sql, cols) in specs {
            let (k, v) = node(uid, sql, cols);
            nodes.insert(k, v);
        }
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        for (p, c) in edges {
            parent_map
                .entry(c.to_string())
                .or_default()
                .push(p.to_string());
            child_map
                .entry(p.to_string())
                .or_default()
                .push(c.to_string());
        }
        RawManifest {
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            parent_map,
            child_map,
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            metadata: Default::default(),
        }
    }

    /// `edge("model.p.stg.id", "model.p.cll.order_id", Rename)` — the uid is
    /// everything before the last `.` (uids themselves contain dots).
    fn edge(from: &str, to: &str, kind: ColumnEdgeKind) -> ColumnEdge {
        let (fn_, fc) = from.rsplit_once('.').unwrap();
        let (tn, tc) = to.rsplit_once('.').unwrap();
        ColumnEdge {
            from: ColumnRef::new(fn_, fc),
            to: ColumnRef::new(tn, tc),
            kind,
            confidence: Confidence::NameMatch,
        }
    }

    /// The acceptance scenario in miniature: `cll` reads `stg`'s `id` (rename),
    /// `status` (passthrough), `cc` (transform) and the ON-clause-only
    /// `agg.country` / WHERE-only `df` (filter-join); `count(*)` gets nothing.
    fn fixture_dag() -> Dag {
        Dag::build(&manifest(
            &[
                (
                    "model.p.stg",
                    "select * from up",
                    &["id", "status", "cc", "df"],
                ),
                ("model.p.agg", "select country, count(*) as n from x", &[]),
                (
                    "model.p.cll",
                    "select o.id as order_id, o.status, upper(o.cc) as cc, \
                     count(*) as n_orders \
                     from stg o left join agg a on o.cc = a.country \
                     where o.df = false \
                     group by o.id, o.status, o.cc",
                    &[],
                ),
            ],
            &[
                ("model.p.stg", "model.p.cll"),
                ("model.p.agg", "model.p.cll"),
            ],
        ))
    }

    #[test]
    fn resolves_all_four_edge_kinds() {
        let dag = fixture_dag();
        let r = NameMatchResolver::new(None);
        let edges: BTreeSet<ColumnEdge> = r.resolve(&dag, "model.p.cll").into_iter().collect();
        let mut want: BTreeSet<ColumnEdge> = [
            edge(
                "model.p.stg.id",
                "model.p.cll.order_id",
                ColumnEdgeKind::Rename,
            ),
            edge(
                "model.p.stg.status",
                "model.p.cll.status",
                ColumnEdgeKind::Passthrough,
            ),
            edge(
                "model.p.stg.cc",
                "model.p.cll.cc",
                ColumnEdgeKind::Transform,
            ),
        ]
        .into_iter()
        .collect();
        // FilterOrJoin fans out to every output column: `df` (WHERE-only) and
        // `country` (join-ON-only) each reach all four outputs.
        for c in ["order_id", "status", "cc", "n_orders"] {
            want.insert(edge(
                "model.p.stg.df",
                &format!("model.p.cll.{c}"),
                ColumnEdgeKind::FilterOrJoin,
            ));
            want.insert(edge(
                "model.p.agg.country",
                &format!("model.p.cll.{c}"),
                ColumnEdgeKind::FilterOrJoin,
            ));
        }
        assert_eq!(edges, want);
    }

    #[test]
    fn star_select_expands_to_parent_columns() {
        // stg has no declared columns: `select *` outputs the parent's whole
        // column list, each passthrough'd from the same-named parent column.
        let dag = Dag::build(&manifest(
            &[
                ("model.p.up", "select 1", &["a", "b"]),
                ("model.p.stg", "select * from up", &[]),
            ],
            &[("model.p.up", "model.p.stg")],
        ));
        let r = NameMatchResolver::new(None);
        assert_eq!(r.columns(&dag, "model.p.stg"), vec!["a", "b"]);
        let edges = r.resolve(&dag, "model.p.stg");
        assert_eq!(
            edges,
            vec![
                edge("model.p.up.a", "model.p.stg.a", ColumnEdgeKind::Passthrough),
                edge("model.p.up.b", "model.p.stg.b", ColumnEdgeKind::Passthrough),
            ]
        );
    }

    #[test]
    fn closures_walk_column_edges_transitively() {
        let dag = fixture_dag();
        let r = NameMatchResolver::new(None);
        let lin = ColumnLineage::new(&dag, &r);
        let stg_status = ColumnRef::new("model.p.stg", "status");
        let down = lin.closure_down(&stg_status);
        assert_eq!(
            down.iter()
                .map(|c| format!("{}.{}", c.node, c.column))
                .collect::<Vec<_>>(),
            vec!["model.p.cll.status".to_string()]
        );
        // `stg.df` is WHERE-only in cll: its downstream closure covers every
        // output column of cll.
        let df_down = lin.closure_down(&ColumnRef::new("model.p.stg", "df"));
        assert_eq!(df_down.len(), 4, "filter column reaches all outputs");
        // And the reverse closure answers the inverse question.
        let up = lin.closure_up(&ColumnRef::new("model.p.cll", "order_id"));
        assert_eq!(
            up.iter()
                .map(|c| format!("{}.{}", c.node, c.column))
                .collect::<Vec<_>>(),
            vec![
                "model.p.agg.country".to_string(),
                "model.p.stg.df".to_string(),
                "model.p.stg.id".to_string()
            ]
        );
        assert!(!up.contains(&ColumnRef::new("model.p.cll", "order_id")));
    }

    #[test]
    fn repeated_queries_are_deterministic_and_lazy() {
        let dag = fixture_dag();
        let r = NameMatchResolver::new(None);
        let lin = ColumnLineage::new(&dag, &r);
        let a = lin.downstream(&ColumnRef::new("model.p.stg", "cc"));
        let b = lin.downstream(&ColumnRef::new("model.p.stg", "cc"));
        assert_eq!(a, b);
        let all = lin.resolve_all();
        let mut sorted = all.clone();
        sorted.sort();
        assert_eq!(all, sorted, "resolve_all emits sorted edges");
    }

    #[test]
    fn normalization_rules_are_v0_canonical() {
        assert_eq!(normalize_column("  Status "), "status");
        assert_eq!(normalize_column("\"Quoted\""), "Quoted");
        assert_eq!(normalize_column("`Ticked`"), "Ticked");
        assert_eq!(normalize_column("[Bracketed]"), "Bracketed");
        // The same spelling unquoted vs quoted must NOT unify in v0.
        assert_ne!(normalize_column("Mixed"), normalize_column("\"Mixed\""));
    }

    /// The 08-swappability contract: a resolver returning hand-written edges
    /// works through the same engine untouched.
    struct StubResolver;
    impl ColumnResolver for StubResolver {
        fn resolve(&self, _dag: &Dag, _node: &str) -> Vec<ColumnEdge> {
            vec![edge(
                "model.p.up.a",
                "model.p.stg.a",
                ColumnEdgeKind::Passthrough,
            )]
        }
    }

    #[test]
    fn engine_runs_on_any_column_resolver() {
        let dag = fixture_dag();
        let stub = StubResolver;
        let lin = ColumnLineage::new(&dag, &stub);
        let up = lin.upstream(&ColumnRef::new("model.p.stg", "a"));
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].kind, ColumnEdgeKind::Passthrough);
    }

    #[test]
    fn stale_compiled_sql_marks_edges_stale() {
        // A node whose resolution text is a stale compiled file reports
        // Confidence::Stale on every edge — the column-level "you're looking
        // at old SQL" flag.
        let m = manifest(
            &[
                ("model.p.up", "select 1", &["a"]),
                ("model.p.stg", "select a from up", &[]),
            ],
            &[("model.p.up", "model.p.stg")],
        );
        let mut dag = Dag::build(&m);
        dag.set_compiled_for_test("model.p.stg", "select a from up", true);
        let r = NameMatchResolver::new(None);
        let edges = r.resolve(&dag, "model.p.stg");
        assert!(!edges.is_empty());
        assert!(
            edges.iter().all(|e| e.confidence == Confidence::Stale),
            "stale compiled text marks every edge Stale"
        );
    }

    #[test]
    fn masking_keeps_comments_and_literals_out_of_resolution() {
        // `--`-commented and string-embedded column names must not produce
        // edges — Jinja-visibility rules do NOT apply to column identifiers.
        let dag = Dag::build(&manifest(
            &[
                ("model.p.up", "select 1", &["a", "b"]),
                (
                    "model.p.stg",
                    "select a from up -- also b\nwhere 'b' = a",
                    &[],
                ),
            ],
            &[("model.p.up", "model.p.stg")],
        ));
        let r = NameMatchResolver::new(None);
        let edges = r.resolve(&dag, "model.p.stg");
        assert!(
            edges.iter().all(|e| e.from.column != "b"),
            "comment/literal columns invisible: {edges:?}"
        );
        assert!(edges.iter().any(|e| e.from.column == "a"));
    }
}
