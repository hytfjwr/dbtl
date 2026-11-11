use super::*;

// ---------------------------------------------------------------------------
// NameMatchResolver — the approximation
// ---------------------------------------------------------------------------

/// The v0 resolution strategy: column names + a naïve scan of the outermost
/// SELECT. Everything it produces is [`Confidence::NameMatch`] (or
/// [`Confidence::Stale`] when it had to read a stale compiled file) — the UI
/// can always distinguish approximation from the `Exact`.
///
/// Internal memos (`col_memo`/`sql_memo`) make per-node answers cheap to reuse
/// across [`ColumnLineage`] passes. They are keyed by uid alone, so the
/// resolver belongs to ONE `Dag` lifetime: the `App` rebuilds it wherever the
/// `Dag` is replaced (reload / manifest adoption).
pub struct NameMatchResolver {
    /// Warehouse truth (`catalog.json`): the highest-priority column source.
    /// `Rc`-shared with the `App`'s copy so the detail modal and CLL read the
    /// same artifact.
    pub(super) catalog: Option<Rc<Catalog>>,
    /// Memoized `columns` answers per uid.
    pub(super) col_memo: RefCell<BTreeMap<String, Rc<Vec<String>>>>,
    /// Memoized masked-SQL parses per uid (select items + region identifiers).
    pub(super) sql_memo: RefCell<BTreeMap<String, Rc<ParsedSql>>>,
    /// Memoized `resolve` answers per uid: a [`ColumnLineage`] engine memoizes
    /// only for its own lifetime, but each structure-modal open / `yl` yank /
    /// `dbtl columns` run builds a FRESH engine — without this the region
    /// walk + edge build would be paid per query, per node, again.
    /// Same single-Dag caveat as the other memos: uid-keyed, so the resolver
    /// belongs to one loaded `Dag` (the `App` rebuilds it on reload).
    pub(super) resolved_memo: RefCell<BTreeMap<String, Rc<Vec<ColumnEdge>>>>,
}

impl NameMatchResolver {
    /// A resolver with an optional shared `catalog.json`. `None` drops the top
    /// priority tier (column lists then start at the documented ones).
    pub fn new(catalog: Option<Rc<Catalog>>) -> Self {
        NameMatchResolver {
            catalog,
            col_memo: RefCell::new(BTreeMap::new()),
            sql_memo: RefCell::new(BTreeMap::new()),
            resolved_memo: RefCell::new(BTreeMap::new()),
        }
    }

    /// `columns` with memoization + a `*`-expansion recursion guard. `visiting`
    /// tracks the expansion stack so a pathological column cycle degrades to
    /// "no columns" for the re-entered node instead of recursing forever.
    pub(super) fn columns_rc(
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
    pub(super) fn compute_columns(
        &self,
        dag: &Dag,
        node: &str,
        visiting: &mut BTreeSet<String>,
    ) -> Vec<String> {
        if let Some(cat) = &self.catalog {
            if let Some(n) = cat.get(node) {
                if !n.columns.is_empty() {
                    let mut cols: Vec<String> =
                        n.columns.keys().map(|k| normalize_column(k)).collect();
                    dedup_columns(&mut cols);
                    return cols;
                }
            }
        }
        if let Some(d) = dag.detail(node) {
            if !d.columns.is_empty() {
                let mut cols: Vec<String> = d
                    .columns
                    .iter()
                    .map(|c| normalize_column(&c.name))
                    .collect();
                dedup_columns(&mut cols);
                return cols;
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
    pub(super) fn parsed(&self, node: &str, text: &str) -> Rc<ParsedSql> {
        if let Some(v) = self.sql_memo.borrow().get(node) {
            return v.clone();
        }
        let rc = Rc::new(parse_select(&mask_sql(text)));
        self.sql_memo
            .borrow_mut()
            .insert(node.to_string(), rc.clone());
        rc
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
    ///   can't see), the fallback applies: `c` present in the node's
    ///   SQL and `c` a parent column → `Passthrough`.
    /// - a parent column that appears in the FROM/JOIN/WHERE/GROUP BY tail and
    ///   carries no direct edge → [`FilterOrJoin`](ColumnEdgeKind::FilterOrJoin)
    ///   to every output column (it shapes the row set, not a value).
    pub(super) fn compute_resolve(&self, dag: &Dag, node: &str) -> Vec<ColumnEdge> {
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

        // Fallback passthrough (the "identifier c appears in the SQL" rule):
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

impl ColumnResolver for NameMatchResolver {
    fn columns(&self, dag: &Dag, node: &str) -> Vec<String> {
        self.columns_rc(dag, node, &mut BTreeSet::new())
            .as_ref()
            .clone()
    }

    /// `resolve` memoized per uid — fresh [`ColumnLineage`] engines (one per
    /// modal open / yank / CLI run) share the paid computation through the
    /// resolver. The trait returns an owned `Vec`, so the memo hands out a
    /// cheap clone of the shared answer rather than re-walking the region.
    fn resolve(&self, dag: &Dag, node: &str) -> Vec<ColumnEdge> {
        if let Some(v) = self.resolved_memo.borrow().get(node) {
            return v.as_ref().clone();
        }
        let rc = Rc::new(self.compute_resolve(dag, node));
        self.resolved_memo
            .borrow_mut()
            .insert(node.to_string(), rc.clone());
        rc.as_ref().clone()
    }

    /// Catalog warehouse type first (same top priority as `columns`), then
    /// the declared type — the diff's type-change classification reads it.
    fn column_type(&self, dag: &Dag, node: &str, column: &str) -> Option<String> {
        if let Some(cat) = &self.catalog {
            if let Some(ty) = cat.get(node).and_then(|n| n.column_type(column)) {
                return Some(ty.to_string());
            }
        }
        dag.detail(node).and_then(|d| {
            d.columns
                .iter()
                .find(|c| normalize_column(&c.name) == column)
                .and_then(|c| c.data_type.clone())
        })
    }
}
