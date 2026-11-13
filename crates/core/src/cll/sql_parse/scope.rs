use super::*;

// ---------------------------------------------------------------------------
// Scope model
// ---------------------------------------------------------------------------

/// `(parent uid, parent column)` — the resolved provenance of a reference.
pub(super) type ColSources = BTreeSet<(String, String)>;

/// No lambda parameters in force — the shared empty `locals` set for
/// every [`RefScope`]/[`Env`] outside a lambda body.
pub(super) static NO_LOCALS: BTreeSet<String> = BTreeSet::new();

/// The placeholder name prefix for unnamed outputs — see
/// [`OutCol::synthetic`]. (A parent column literally named `__col1` still
/// resolves normally; the marker only suppresses INVENTED names.)
pub(super) const ANON_COL_PREFIX: &str = "__col";

/// One output column of a resolved query.
#[derive(Debug, Clone)]
pub(super) struct OutCol {
    /// Normalized output name.
    pub(super) name: String,
    /// The output is a bare column reference (`x`, `t.x`) — possibly renamed
    /// by `AS`. `false` for every computed expression (→ Transform).
    pub(super) bare: bool,
    /// Parent columns feeding this output.
    pub(super) sources: ColSources,
    /// `true` when `name` is an invented `__colN` placeholder for an
    /// unnameable output (a literal, `(expr).*`, …). Synthetic names never
    /// surface in `columns()` or the FilterOrJoin fan — they're positional
    /// bookkeeping only — and they don't pair in `UNION BY NAME` merges.
    pub(super) synthetic: bool,
}

/// What a SELECT-level construct resolved to: positional output columns
/// plus every parent column consumed in a *row-shaping* position
/// (join/filter/group/sort — the `FilterOrJoin` pool).
#[derive(Debug, Clone, Default)]
pub(super) struct QueryRes {
    pub(super) out: Vec<OutCol>,
    pub(super) foj: ColSources,
}

/// What a name in the FROM scope stands for.
#[derive(Debug, Clone)]
pub(super) enum TableSrc {
    /// A direct parent node of the model being resolved (its uid).
    Parent(String),
    /// A CTE or derived table — resolved to its own output columns, each
    /// carrying its parent-column sources transitively.
    Derived(Rc<QueryRes>),
    /// A name we can't map to a parent (an un-resolved relation, a table
    /// function…): columns off it contribute nothing.
    External,
}

/// One FROM-visible name → what it binds.
#[derive(Debug, Clone)]
pub(super) struct Binding {
    /// The name SQL sees: the alias when present, else the relation's last
    /// dotted segment.
    pub(super) name: String,
    pub(super) src: TableSrc,
}

/// The FROM bindings of one query level, chained to the enclosing query's
/// scope for correlated subqueries, plus its `WINDOW w AS (…)` definitions
/// (select-local, so they live per scope level).
#[derive(Debug, Default)]
pub(super) struct Scope<'a> {
    pub(super) bindings: Vec<Binding>,
    /// `WINDOW name AS spec` — looked up by `OVER name`.
    pub(super) windows: HashMap<String, WindowSpec>,
    pub(super) outer: Option<&'a Scope<'a>>,
}

/// The environment visible while resolving a query: the outer scope for
/// correlation, the CTE names in force, and the enclosing lambda
/// parameters (so a lambda body inside a subquery still shadows).
#[derive(Clone, Copy)]
pub(super) struct Env<'a> {
    pub(super) outer: Option<&'a Scope<'a>>,
    pub(super) ctes: &'a HashMap<String, Rc<QueryRes>>,
    pub(super) locals: &'a BTreeSet<String>,
}

/// The context threaded through *expression* resolution: the scope chain
/// (column binding + correlation), the CTE table for subqueries that
/// reference `WITH` names, and the lambda-parameter names in force — those
/// shadow same-named columns AND binding names inside the lambda body.
#[derive(Clone, Copy)]
pub(super) struct RefScope<'a> {
    pub(super) scope: Option<&'a Scope<'a>>,
    pub(super) ctes: &'a HashMap<String, Rc<QueryRes>>,
    pub(super) locals: &'a BTreeSet<String>,
}

/// One direct parent of the model being resolved: the names SQL can call it
/// (node name, dbt `alias`/`identifier` override, uid tail) and the
/// qualifiers (`schema`.`relation`, `database`.`schema`.`relation`, uid
/// segments) that disambiguate it when several parents share a relation
/// name.
#[derive(Debug)]
pub(super) struct Parent {
    pub(super) uid: String,
    /// Normalized names that match this parent in FROM position.
    pub(super) names: BTreeSet<String>,
    /// Normalized schema/database/uid segments usable as qualifier evidence.
    pub(super) qualifiers: BTreeSet<String>,
}

/// The result of analyzing one model's SQL: its top-level scope (for the
/// declared-column fallback probe), its output columns, and its
/// filter/join source set.
#[derive(Debug)]
pub(super) struct Analyzed {
    pub(super) scope: Vec<Binding>,
    pub(super) out: Vec<OutCol>,
    pub(super) foj: ColSources,
    /// `true` when an output-producing projection contained a `*`/`t.*` —
    /// the only way a declared column can be carried without appearing in
    /// `out` (behind a wildcard no binding's column list could expand).
    /// Without one, every output is explicitly named and probing an
    /// unproduced declared name can only fabricate edges.
    pub(super) unbounded_wildcard: bool,
    /// Occurrence count behind `unbounded_wildcard` (UNION arms and pipe
    /// steps can carry several) — the lint positions pass pairs wildcard
    /// items positionally only when its raw-render count matches.
    pub(super) wildcard_count: usize,
    /// Display text of every JOIN arm carrying no join condition
    /// (`CROSS JOIN`, or `JOIN`/`INNER`/`LEFT`/… with no ON/USING/NATURAL)
    /// — the `cartesian_join` lint input, collected by the same parse that
    /// produced the rest of `self` (the scan is join-blind to rendered
    /// relation names, so no marked re-parse is needed).
    pub(super) cartesian: Vec<String>,
    /// Occurrence count behind `cartesian` (the list dedups display text)
    /// — the lint positions pass compares its raw-render count against
    /// this before pairing arms positionally.
    pub(super) cartesian_count: usize,
    /// `(parent uid, column)` refs that bound a parent with a KNOWN column
    /// list and missed it — the model names a column that doesn't exist.
    /// Recorded, not resolved: the `--diff` dangling-consumer scan reads it
    /// (a still-named dead column means the query can't run); everywhere
    /// else an unresolved ref contributes no edge, same as before.
    pub(super) unresolved: ColSources,
    /// `(normalized name, occurrence count)` of every output column name
    /// the output-defining SELECT emits more than once — the
    /// `duplicate_output_columns` lint input, from the same judged query
    /// (`res.out` counts — wildcard-expanded where bindings resolve).
    pub(super) duplicate_outputs: Vec<(String, usize)>,
    /// Normalized names of `WITH` CTEs nothing reads — the `unused_cte`
    /// lint input, the complement of the relation scan's `cte_uses`
    /// (traversal order; nested queries' CTEs included).
    pub(super) unused_ctes: Vec<String>,
    /// The final query's `ORDER BY` lacks a `LIMIT`/`FETCH`/`TOP` bound —
    /// the `order_by_without_limit` lint input
    /// (`facts::outer_order_by_span`).
    pub(super) unlimited_order_by: bool,
    /// The final query is a `select * from <one all-explicit-columns CTE>`
    /// — `select_star`'s `allow_final_star_over_single_cte` opt-out input
    /// (`facts::star_over_single_explicit_cte`). Irrelevant when
    /// `unbounded_wildcard` is false (nothing would flag anyway).
    pub(super) star_over_single_explicit_cte: bool,
}

/// Per-query context: the `Dag`, the model's direct-parent matcher table,
/// and the recursion budget (a `Cell` so all walkers share `&Ctx`).
pub(super) struct Ctx<'a> {
    pub(super) dag: &'a Dag,
    pub(super) parents: &'a [Parent],
    pub(super) depth: Cell<usize>,
    /// Dead references collected while resolving this model — see
    /// [`Analyzed::unresolved`]. Interior mutability so every `&Ctx` walker
    /// records into the same set; [`compute_analyzed`](Self::compute_analyzed)
    /// drains it into the memoized result.
    pub(super) unresolved: RefCell<ColSources>,
}

/// The [`RefScope`] for expressions in `env` resolved against `scope`
/// (a one-line convenience — `scope` reborrows `&mut` as `&` here).
pub(super) fn ref_scope<'a>(env: Env<'a>, scope: &'a Scope<'a>) -> RefScope<'a> {
    RefScope {
        scope: Some(scope),
        ctes: env.ctes,
        locals: env.locals,
    }
}
