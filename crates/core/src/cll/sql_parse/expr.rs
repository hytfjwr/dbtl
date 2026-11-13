use super::*;

impl SqlParseResolver {
    // -- expression references -----------------------------------------------

    /// Every parent-column reference inside `expr`. `None` propagates the
    /// depth-abort sentinel.
    pub(super) fn expr_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        expr: &Expr,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        match expr {
            Expr::Identifier(i) => {
                s.extend(self.resolve_parts(ctx, refs, &[norm_ident(i)]));
            }
            Expr::CompoundIdentifier(parts) => {
                let parts: Vec<String> = parts.iter().map(norm_ident).collect();
                s.extend(self.resolve_parts(ctx, refs, &parts));
            }
            Expr::CompoundFieldAccess {
                root, access_chain, ..
            } => {
                s.extend(self.expr_refs(ctx, refs, root)?);
                for a in access_chain {
                    if let AccessExpr::Subscript(sub) = a {
                        s.extend(self.subscript_refs(ctx, refs, sub)?);
                    }
                }
            }
            Expr::JsonAccess { value, .. } => {
                s.extend(self.expr_refs(ctx, refs, value)?);
            }
            Expr::IsFalse(e)
            | Expr::IsNotFalse(e)
            | Expr::IsTrue(e)
            | Expr::IsNotTrue(e)
            | Expr::IsNull(e)
            | Expr::IsNotNull(e)
            | Expr::IsUnknown(e)
            | Expr::IsNotUnknown(e)
            | Expr::Nested(e)
            | Expr::Prior(e)
            | Expr::OuterJoin(e)
            | Expr::IsJson { expr: e, .. }
            | Expr::IsNormalized { expr: e, .. }
            | Expr::Collate { expr: e, .. }
            | Expr::Named { expr: e, .. }
            | Expr::Prefixed { value: e, .. }
            | Expr::UnaryOp { expr: e, .. }
            | Expr::Ceil { expr: e, .. }
            | Expr::Floor { expr: e, .. }
            | Expr::Extract { expr: e, .. } => {
                s.extend(self.expr_refs(ctx, refs, e)?);
            }
            Expr::AtTimeZone {
                timestamp,
                time_zone,
                ..
            } => {
                s.extend(self.expr_refs(ctx, refs, timestamp)?);
                s.extend(self.expr_refs(ctx, refs, time_zone)?);
            }
            Expr::IsDistinctFrom(a, b)
            | Expr::IsNotDistinctFrom(a, b)
            | Expr::BinaryOp {
                left: a, right: b, ..
            }
            | Expr::AnyOp {
                left: a, right: b, ..
            }
            | Expr::AllOp {
                left: a, right: b, ..
            }
            | Expr::Position {
                expr: a, r#in: b, ..
            } => {
                s.extend(self.expr_refs(ctx, refs, a)?);
                s.extend(self.expr_refs(ctx, refs, b)?);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                for e in [expr, low, high] {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Like { expr, pattern, .. }
            | Expr::ILike { expr, pattern, .. }
            | Expr::SimilarTo { expr, pattern, .. }
            | Expr::RLike { expr, pattern, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                s.extend(self.expr_refs(ctx, refs, pattern)?);
            }
            Expr::In { expr, target, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                match target {
                    InTarget::List(exprs) => {
                        for e in exprs {
                            s.extend(self.expr_refs(ctx, refs, e)?);
                        }
                    }
                    InTarget::Subquery(q) => {
                        s.extend(self.subquery_refs(ctx, refs, q)?);
                    }
                    InTarget::Unnest(e) => {
                        s.extend(self.expr_refs(ctx, refs, e)?);
                    }
                    InTarget::Table { name, args } => {
                        // `x IN table_name` (SQLite) reads every row of the
                        // named relation — like `IN (SELECT * FROM name)`:
                        // all of that table's columns feed the test.
                        for a in args.iter().flatten() {
                            s.extend(self.function_arg_refs(ctx, refs, a)?);
                        }
                        let parts = object_name_parts(name);
                        if let Some(last) = parts.last() {
                            // Same rule as FROM position: a CTE is only ever
                            // addressable UNqualified (`sch.c` is a real
                            // relation, never the CTE `c`).
                            let cte = if parts.len() == 1 {
                                refs.ctes.get(last)
                            } else {
                                None
                            };
                            if let Some(q) = cte {
                                s.extend(q.foj.iter().cloned());
                                for o in &q.out {
                                    s.extend(o.sources.iter().cloned());
                                }
                            } else if let Some(p) = Self::match_parent(ctx.parents, &parts) {
                                if let Some(cs) = self.col_set(ctx.dag, &p.uid) {
                                    s.extend(cs.iter().map(|c| (p.uid.clone(), c.clone())));
                                }
                            }
                        }
                    }
                }
            }
            Expr::Convert { expr, styles, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                for st in styles {
                    s.extend(self.expr_refs(ctx, refs, st)?);
                }
            }
            Expr::Cast { expr, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                // CastFormat's Value/ValueAtTimeZone carry format-string
                // literals, not column refs — nothing to walk.
            }
            Expr::Substring {
                expr,
                substring_from,
                substring_for,
                ..
            } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                for e in [substring_from.as_ref(), substring_for.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Trim {
                expr,
                trim_what,
                trim_characters,
                ..
            } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                if let Some(e) = trim_what {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
                for e in trim_characters.iter().flatten() {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Overlay {
                expr,
                overlay_what,
                overlay_from,
                overlay_for,
            } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                s.extend(self.expr_refs(ctx, refs, overlay_what)?);
                s.extend(self.expr_refs(ctx, refs, overlay_from)?);
                if let Some(e) = overlay_for {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Function(f) => {
                s.extend(self.function_refs(ctx, refs, f)?);
            }
            Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                if let Some(op) = operand {
                    s.extend(self.expr_refs(ctx, refs, op)?);
                }
                for cw in conditions {
                    s.extend(self.expr_refs(ctx, refs, &cw.condition)?);
                    s.extend(self.expr_refs(ctx, refs, &cw.result)?);
                }
                if let Some(e) = else_result {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
                s.extend(self.subquery_refs(ctx, refs, subquery)?);
            }
            Expr::GroupingSets(sets) | Expr::Cube(sets) | Expr::Rollup(sets) => {
                for set in sets {
                    for e in set {
                        s.extend(self.expr_refs(ctx, refs, e)?);
                    }
                }
            }
            Expr::Tuple(exprs) => {
                for e in exprs {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Struct { values, .. } => {
                for e in values {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Dictionary(fields) => {
                for f in fields {
                    s.extend(self.expr_refs(ctx, refs, &f.value)?);
                }
            }
            Expr::Map(m) => {
                for e in &m.entries {
                    s.extend(self.expr_refs(ctx, refs, &e.key)?);
                    s.extend(self.expr_refs(ctx, refs, &e.value)?);
                }
            }
            Expr::Array(a) => {
                for e in &a.elem {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Interval(i) => {
                s.extend(self.expr_refs(ctx, refs, &i.value)?);
            }
            Expr::MatchAgainst { columns, .. } => {
                for c in columns {
                    let parts = object_name_parts(c);
                    s.extend(self.resolve_parts(ctx, refs, &parts));
                }
                // `match_value` is a ValueWithSpan literal — no column refs.
            }
            Expr::MemberOf(m) => {
                s.extend(self.expr_refs(ctx, refs, &m.value)?);
                s.extend(self.expr_refs(ctx, refs, &m.array)?);
            }
            Expr::Lambda(l) => {
                // Lambda params are LOCAL names: `x -> x > 0` never reads a
                // column `x`. The body still sees every other in-scope
                // column (`filter(arr, x -> x > y)` does read `y`).
                let mut locals = refs.locals.clone();
                for p in l.params.iter() {
                    locals.insert(norm_ident(&p.name));
                }
                s.extend(self.expr_refs(
                    ctx,
                    RefScope {
                        scope: refs.scope,
                        ctes: refs.ctes,
                        locals: &locals,
                    },
                    &l.body,
                )?);
            }
            // Literals and wildcards contribute no column refs (wildcards
            // are expanded by the projection path, not walked as exprs).
            // The exhaustive match means a NEW sqlparser variant fails the
            // build rather than silently under-resolving.
            Expr::Value(_)
            | Expr::TypedString { .. }
            | Expr::Wildcard(_)
            | Expr::QualifiedWildcard(..) => {}
        }
        Some(s)
    }

    /// A scalar/EXISTS subquery's refs: resolve it with `refs.scope` as the
    /// outer scope (correlated refs work) and `refs.ctes` still in force
    /// (a `WITH` name is visible inside a scalar subquery), then report
    /// every source it consumed — its output columns' sources AND its own
    /// row-shaping ones.
    pub(super) fn subquery_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        q: &Query,
    ) -> Option<ColSources> {
        let res = self.resolve_query(
            ctx,
            Env {
                outer: refs.scope,
                ctes: refs.ctes,
                locals: refs.locals,
            },
            q,
        )?;
        let mut s = res.foj;
        for o in &res.out {
            s.extend(o.sources.iter().cloned());
        }
        Some(s)
    }

    /// `Expr::Subscript` inside CompoundFieldAccess chains (`a[expr]`).
    pub(super) fn subscript_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        sub: &sqlparser::ast::Subscript,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        match sub {
            sqlparser::ast::Subscript::Index { index } => {
                s.extend(self.expr_refs(ctx, refs, index)?);
            }
            sqlparser::ast::Subscript::Slice {
                lower_bound,
                upper_bound,
                stride,
            } => {
                for e in [lower_bound, upper_bound, stride].into_iter().flatten() {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
        }
        Some(s)
    }

    /// A function call's refs: parameters + args + arg-clause exprs +
    /// WITHIN GROUP + FILTER + OVER (window specs).
    pub(super) fn function_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        f: &Function,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        let fname = object_name_parts(&f.name)
            .last()
            .cloned()
            .unwrap_or_default();
        for args in [&f.parameters, &f.args] {
            match args {
                FunctionArguments::List(l) => {
                    for (i, a) in l.args.iter().enumerate() {
                        // `dateadd(day, 1, d)`'s `day` — a bare unit word
                        // at a documented date-part slot is the keyword,
                        // never a column reference.
                        if date_part_arg_position(&fname, i, self.dialect) && is_date_part_arg(a) {
                            continue;
                        }
                        s.extend(self.function_arg_refs(ctx, refs, a)?);
                    }
                    for c in &l.clauses {
                        match c {
                            FunctionArgumentClause::OrderBy(ob) => {
                                for e in ob {
                                    s.extend(self.expr_refs(ctx, refs, &e.expr)?);
                                }
                            }
                            FunctionArgumentClause::Limit(e) | FunctionArgumentClause::Where(e) => {
                                s.extend(self.expr_refs(ctx, refs, e)?);
                            }
                            // `HAVING MIN/MAX <expr>` on `ANY_VALUE`.
                            FunctionArgumentClause::Having(bound) => {
                                s.extend(self.expr_refs(ctx, refs, &bound.1)?);
                            }
                            // IgnoreOrRespectNulls/OnOverflow/Separator/
                            // JsonNullClause/JsonReturningClause carry no
                            // column exprs — documented no-walk.
                            _ => {}
                        }
                    }
                }
                FunctionArguments::Subquery(q) => {
                    s.extend(self.subquery_refs(ctx, refs, q)?);
                }
                _ => {}
            }
        }
        for ob in &f.within_group {
            s.extend(self.expr_refs(ctx, refs, &ob.expr)?);
        }
        if let Some(flt) = &f.filter {
            s.extend(self.expr_refs(ctx, refs, flt)?);
        }
        if let Some(over) = &f.over {
            match over {
                WindowType::WindowSpec(spec) => {
                    s.extend(self.window_spec_refs(ctx, refs, spec)?);
                }
                WindowType::NamedWindow(name) => {
                    // `OVER w` resolves against the scope chain's WINDOW
                    // defs (select-local — nested selects get their own).
                    let key = norm_ident(name);
                    let mut sc = refs.scope;
                    while let Some(scope) = sc {
                        if let Some(spec) = scope.windows.get(&key) {
                            let spec = spec.clone();
                            s.extend(self.window_spec_refs(ctx, refs, &spec)?);
                            break;
                        }
                        sc = scope.outer;
                    }
                }
            }
        }
        Some(s)
    }

    /// PARTITION BY + ORDER BY of a window spec (frame bounds are
    /// literals/rows — no column refs).
    pub(super) fn window_spec_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        spec: &WindowSpec,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        for e in &spec.partition_by {
            s.extend(self.expr_refs(ctx, refs, e)?);
        }
        for ob in &spec.order_by {
            s.extend(self.expr_refs(ctx, refs, &ob.expr)?);
        }
        Some(s)
    }

    /// A function argument's refs (wildcards skipped — `count(*)` depends on
    /// the rows, not any column).
    pub(super) fn function_arg_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        arg: &FunctionArg,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        match arg {
            FunctionArg::Named { arg, .. } | FunctionArg::Unnamed(arg) => {
                if let FunctionArgExpr::Expr(e) = arg {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            FunctionArg::ExprNamed { arg, .. } => {
                // `name => value` / `name := value`: the LHS expr is an
                // argument NAME, not data — `named_struct(name => 'v')`
                // never reads a column `name`. Only the value walks.
                if let FunctionArgExpr::Expr(e) = arg {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
        }
        Some(s)
    }

    // -- column reference resolution -----------------------------------------

    /// Resolve a (normalized, dotted) column reference → the parent
    /// `(uid, column)` sources it provably draws from.
    pub(super) fn resolve_parts(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        parts: &[String],
    ) -> ColSources {
        let Some(scope) = refs.scope else {
            return ColSources::new();
        };
        if parts.len() >= 2 {
            let col = parts.last().expect("len >= 2");
            let qual = &parts[parts.len() - 2];
            // `qual` names a binding in this scope or an outer one — but a
            // LOCAL name (lambda param, MATCH_RECOGNIZE symbol) shadows it.
            let mut sc = Some(scope);
            while let Some(s) = sc {
                // `rfind`: a duplicated binding name (`FROM a t, b t`) —
                // the rightmost binding shadows, per SQL semantics.
                if let Some(b) = s.bindings.iter().rfind(|b| b.name == *qual) {
                    if !refs.locals.contains(qual) {
                        return self.resolve_in_binding(ctx, b, col);
                    }
                }
                sc = s.outer;
            }
            if parts.len() >= 3 {
                // `a.b.c…` — two readings, in order: `a` a bound table and
                // `b` its struct column (deeper segments are field access),
                // or `a` itself a column (a struct/map chain — `db.tbl.c`
                // can't be told apart from `tbl.c` once `tbl` is unbound,
                // documented).
                let mut sc = Some(scope);
                while let Some(s) = sc {
                    if let Some(b) = s.bindings.iter().rfind(|b| b.name == parts[0]) {
                        if !refs.locals.contains(&parts[0]) {
                            return self.resolve_in_binding(ctx, b, &parts[1]);
                        }
                    }
                    sc = s.outer;
                }
            }
            if refs.locals.contains(&parts[0]) {
                return ColSources::new();
            }
            return self.resolve_unqualified(ctx, scope, &parts[0]);
        }
        let Some(first) = parts.first() else {
            return ColSources::new();
        };
        if refs.locals.contains(first) {
            return ColSources::new();
        }
        self.resolve_unqualified(ctx, scope, first)
    }

    /// `t.col` against a bound `t`: the parent uid + col (trusting the
    /// qualification when the parent's list is unknown), the derived
    /// output's transitive sources, or nothing for External bindings.
    /// A qualified miss against a KNOWN parent list is recorded into
    /// `ctx.unresolved` — the SQL still names a column the parent provably
    /// doesn't emit (the `--diff` dangling-reference signal).
    pub(super) fn resolve_in_binding(&self, ctx: &Ctx<'_>, b: &Binding, col: &str) -> ColSources {
        let mut s = ColSources::new();
        match &b.src {
            TableSrc::Parent(uid) => match self.col_set(ctx.dag, uid) {
                Some(cols) => {
                    if cols.iter().any(|c| c == col) {
                        s.insert((uid.clone(), col.to_string()));
                    } else if !generated_name(col) {
                        // A KNOWN column list without `col` → no edge (the
                        // precision win over v0's blind name-match) — but
                        // the ref WAS written, so record the provable miss.
                        // A `__dbtl_` name is Jinja-generated
                        // (`{{ target.col }}` renders a marked column) —
                        // nothing was written, so nothing is proven.
                        ctx.unresolved
                            .borrow_mut()
                            .insert((uid.clone(), col.to_string()));
                    }
                }
                // Unknown column list → trust the qualification — but a
                // `__dbtl_` name is Jinja-generated (`t.{{ m() }}` renders
                // a marked column): the parent provably has no such
                // column, so crediting it would invent an edge.
                None => {
                    if !generated_name(col) {
                        s.insert((uid.clone(), col.to_string()));
                    }
                }
            },
            TableSrc::Derived(q) => {
                if let Some(o) = q.out.iter().find(|o| o.name == *col) {
                    s.extend(o.sources.iter().cloned());
                }
            }
            TableSrc::External => {}
        }
        s
    }

    /// The enumerable output names a binding exposes — `None` when its
    /// column list is unknowable (a parent nothing names, an External
    /// relation).
    pub(super) fn binding_col_names(&self, ctx: &Ctx<'_>, b: &Binding) -> Option<BTreeSet<String>> {
        match &b.src {
            TableSrc::Parent(uid) => self
                .col_set(ctx.dag, uid)
                .map(|cs| cs.iter().cloned().collect()),
            TableSrc::Derived(q) => Some(
                q.out
                    .iter()
                    .filter(|o| !o.synthetic)
                    .map(|o| o.name.clone())
                    .collect(),
            ),
            TableSrc::External => None,
        }
    }

    /// `NATURAL JOIN` keys on every column name BOTH sides share — so both
    /// sides' shared columns are row-shaping. Computable only where the
    /// just-bound (right) relation and an earlier binding each expose a
    /// known column list.
    pub(super) fn natural_join_sources(&self, ctx: &Ctx<'_>, scope: &Scope<'_>) -> ColSources {
        let mut s = ColSources::new();
        let Some((right, prior)) = scope.bindings.split_last() else {
            return s;
        };
        let Some(right_cols) = self.binding_col_names(ctx, right) else {
            return s;
        };
        for lb in prior {
            let Some(left_cols) = self.binding_col_names(ctx, lb) else {
                continue;
            };
            for c in left_cols.intersection(&right_cols) {
                s.extend(self.resolve_in_binding(ctx, lb, c));
                s.extend(self.resolve_in_binding(ctx, right, c));
            }
        }
        s
    }

    /// An unqualified `col`: every binding in the innermost scope that HAS
    /// it contributes (ambiguous → all candidates, the documented
    /// over-connection). Unknown-crediting is LEVEL-scoped: a binding with
    /// an UNKNOWN column list is credited only when no KNOWN binding at its
    /// own level — or any level OUTER of it — resolves the ref. (An inner
    /// unknown can't poison a provable outer hit, and vice versa.)
    pub(super) fn resolve_unqualified(
        &self,
        ctx: &Ctx<'_>,
        scope: &Scope<'_>,
        col: &str,
    ) -> ColSources {
        // Collect per-level (known hits, resolved?, unknown uids), innermost
        // first — and every KNOWN-list parent the ref was checked against.
        let mut levels: Vec<(ColSources, bool, BTreeSet<String>)> = Vec::new();
        let mut checked: BTreeSet<String> = BTreeSet::new();
        let mut sc = Some(scope);
        while let Some(s) = sc {
            let mut found = ColSources::new();
            let mut resolved = false;
            let mut unknown = BTreeSet::new();
            for b in &s.bindings {
                match &b.src {
                    TableSrc::Parent(uid) => match self.col_set(ctx.dag, uid) {
                        Some(cols) => {
                            checked.insert(uid.clone());
                            if cols.iter().any(|c| c == col) {
                                found.insert((uid.clone(), col.to_string()));
                            }
                        }
                        // An unknown-list parent is creditable only for a
                        // name that was actually written — a `__dbtl_`
                        // generated name exists in no column list, so
                        // crediting it would invent an edge.
                        None => {
                            if !generated_name(col) {
                                unknown.insert(uid.clone());
                            }
                        }
                    },
                    TableSrc::Derived(q) => {
                        if let Some(o) = q.out.iter().find(|o| o.name == *col) {
                            // A matched output resolves the name even when
                            // it carries no sources (a literal-derived or
                            // set-derived column): the name is bound, just
                            // lineage-free — an outer `col` stays shadowed.
                            resolved = true;
                            found.extend(o.sources.iter().cloned());
                        }
                    }
                    TableSrc::External => {}
                }
            }
            resolved |= !found.is_empty();
            levels.push((found, resolved, unknown));
            sc = s.outer;
        }
        match levels.iter().position(|(_, resolved, _)| *resolved) {
            Some(k) => {
                let mut result = levels[k].0.clone();
                // Unknowns OUTER of the outermost known level still can't be
                // disproven — credit them; everything at or inside a
                // resolving level is settled.
                let m = levels
                    .iter()
                    .rposition(|(_, resolved, _)| *resolved)
                    .unwrap_or(k);
                for (_, _, unknown) in &levels[m + 1..] {
                    for uid in unknown {
                        result.insert((uid.clone(), col.to_string()));
                    }
                }
                result
            }
            // Nothing provable anywhere → every unknown stays a candidate.
            None => {
                let mut result = ColSources::new();
                for (_, _, unknown) in &levels {
                    for uid in unknown {
                        result.insert((uid.clone(), col.to_string()));
                    }
                }
                if result.is_empty() && !generated_name(col) {
                    // A bare `col` absent from EVERY bound parent's known
                    // column list — an unresolvable name (like `t.gone`,
                    // just unqualified). Record it against every checked
                    // parent — any of them could have been the intended
                    // home; the --diff scan intersects with actually-dead
                    // columns, so a wrong guess can't misfire. Skip
                    // `__dbtl_` names — Jinja-generated, not written refs.
                    for uid in &checked {
                        ctx.unresolved
                            .borrow_mut()
                            .insert((uid.clone(), col.to_string()));
                    }
                }
                result
            }
        }
    }

    /// Whether a projection stays *bare*: its expression is a column
    /// reference AND every binding it can resolve to serves a bare column.
    /// A `t.c` where `c` was itself computed inside the CTE/derived table is
    /// not bare — the transform happened upstream of this reference but
    /// still belongs to the value.
    pub(super) fn projection_bare(&self, ctx: &Ctx<'_>, refs: RefScope<'_>, e: &Expr) -> bool {
        is_bare_ref(e)
            && bare_parts(e)
                .and_then(|parts| self.bare_through_binding(ctx, refs, &parts))
                .unwrap_or(true)
    }

    /// The bare-ness of the column `parts` resolves to — mirrors
    /// [`resolve_parts`](Self::resolve_parts)' scope walk. `None` when the
    /// ref resolves to nothing bare-meaningful (External binding, no match).
    pub(super) fn bare_through_binding(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        parts: &[String],
    ) -> Option<bool> {
        let scope = refs.scope?;
        if parts.len() >= 2 {
            let col = parts.last()?;
            let qual = &parts[parts.len() - 2];
            let mut sc = Some(scope);
            while let Some(s) = sc {
                // `rfind` like `resolve_parts` — a duplicated binding name
                // resolves through the rightmost `FROM a t, b t` binding.
                if let Some(b) = s.bindings.iter().rfind(|b| b.name == *qual) {
                    if !refs.locals.contains(qual) {
                        return Self::bare_in_binding(b, col);
                    }
                }
                sc = s.outer;
            }
            if parts.len() >= 3 {
                // `a.b.c…` — mirrors `resolve_parts`: a bound `a` reads its
                // column `b`; else `a` is the struct column itself.
                let mut sc = Some(scope);
                while let Some(s) = sc {
                    if let Some(b) = s.bindings.iter().rfind(|b| b.name == parts[0]) {
                        if !refs.locals.contains(&parts[0]) {
                            return Self::bare_in_binding(b, &parts[1]);
                        }
                    }
                    sc = s.outer;
                }
            }
            if refs.locals.contains(&parts[0]) {
                return None;
            }
            return self.bare_unqualified(ctx, scope, &parts[0]);
        }
        let first = parts.first()?;
        if refs.locals.contains(first) {
            return None;
        }
        self.bare_unqualified(ctx, scope, first)
    }

    /// `t.col`'s bare-ness: a parent column is always a raw value (any
    /// transform inside the parent is that model's own edge set); a derived
    /// column carries its producing projection's flag.
    pub(super) fn bare_in_binding(b: &Binding, col: &str) -> Option<bool> {
        match &b.src {
            TableSrc::Parent(_) => Some(true),
            TableSrc::Derived(q) => q.out.iter().find(|o| o.name == col).map(|o| o.bare),
            TableSrc::External => None,
        }
    }

    /// Unqualified bare-ness — mirrors [`resolve_unqualified`](Self::resolve_unqualified):
    /// the innermost RESOLVING scope's candidates win (with the same
    /// level-scoped unknown crediting), and the ref stays bare only when
    /// EVERY candidate is a bare column (ambiguous mixed sources keep the
    /// conservative transform flag).
    pub(super) fn bare_unqualified(
        &self,
        ctx: &Ctx<'_>,
        scope: &Scope<'_>,
        col: &str,
    ) -> Option<bool> {
        let mut levels: Vec<(Vec<bool>, bool)> = Vec::new();
        let mut sc = Some(scope);
        while let Some(s) = sc {
            let mut bares: Vec<bool> = Vec::new();
            let mut unknown = false;
            for b in &s.bindings {
                match &b.src {
                    TableSrc::Parent(uid) => match self.col_set(ctx.dag, uid) {
                        Some(cols) => {
                            if cols.iter().any(|c| c == col) {
                                bares.push(true);
                            }
                        }
                        None => unknown = true,
                    },
                    TableSrc::Derived(q) => {
                        if let Some(o) = q.out.iter().find(|o| o.name == col) {
                            bares.push(o.bare);
                        }
                    }
                    TableSrc::External => {}
                }
            }
            levels.push((bares, unknown));
            sc = s.outer;
        }
        match levels.iter().position(|(bares, _)| !bares.is_empty()) {
            Some(k) => {
                let mut all = levels[k].0.clone();
                // The same outer-of-outermost-known unknowns get credited —
                // they'd resolve raw, so they vote bare.
                let m = levels
                    .iter()
                    .rposition(|(bares, _)| !bares.is_empty())
                    .unwrap_or(k);
                let outer_unknown = levels[m + 1..].iter().any(|(_, u)| *u);
                if outer_unknown {
                    all.push(true);
                }
                Some(all.iter().all(|b| *b))
            }
            None => {
                // No provable hit anywhere: any unknown binding resolves
                // raw; a fully-known miss resolves nothing.
                let any_unknown = levels.iter().any(|(_, u)| *u);
                any_unknown.then_some(true)
            }
        }
    }

    /// `*` / `t.*` expansion: each binding's known columns become bare
    /// outputs; wildcard options (`EXCEPT`/`EXCLUDE`/`RENAME`/`REPLACE`/
    /// `ILIKE`) then reshape the list.
    pub(super) fn expand_wildcard(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        scope: &Scope<'_>,
        qualifier: Option<String>,
        opts: &WildcardAdditionalOptions,
        out: &mut Vec<OutCol>,
    ) -> Option<()> {
        let mut cols: Vec<OutCol> = Vec::new();
        for b in &scope.bindings {
            if let Some(q) = &qualifier {
                if &b.name != q {
                    continue;
                }
            }
            match &b.src {
                TableSrc::Parent(uid) => {
                    if let Some(cs) = self.col_set(ctx.dag, uid) {
                        for c in cs.iter() {
                            cols.push(OutCol {
                                name: c.clone(),
                                bare: true,
                                sources: BTreeSet::from([(uid.clone(), c.clone())]),
                                synthetic: false,
                            });
                        }
                    }
                }
                TableSrc::Derived(q) => {
                    cols.extend(q.out.iter().cloned());
                }
                TableSrc::External => {}
            }
        }

        // `* ILIKE '<pattern>'` — Snowflake's column-name filter.
        if let Some(ilike) = &opts.opt_ilike {
            cols.retain(|o| ilike_match(&ilike.pattern, &o.name));
        }
        // `* EXCEPT col` — BigQuery/ClickHouse.
        if let Some(except) = &opts.opt_except {
            let drop: BTreeSet<String> = std::iter::once(&except.first_element)
                .chain(except.additional_elements.iter())
                .map(norm_ident)
                .collect();
            cols.retain(|o| !drop.contains(&o.name));
        }
        // `* EXCLUDE col` — Snowflake.
        if let Some(exclude) = &opts.opt_exclude {
            let names = exclude_names(exclude);
            cols.retain(|o| !names.contains(&o.name));
        }
        // `* RENAME col AS alias` — the output name changes (the edge still
        // resolves to Rename downstream since `bare` stays true).
        if let Some(rename) = &opts.opt_rename {
            let pairs: Vec<(String, String)> = match rename {
                RenameSelectItem::Single(ia) => {
                    vec![(norm_ident(&ia.ident), norm_ident(&ia.alias))]
                }
                RenameSelectItem::Multiple(ias) => ias
                    .iter()
                    .map(|ia| (norm_ident(&ia.ident), norm_ident(&ia.alias)))
                    .collect(),
            };
            for o in &mut cols {
                if let Some((_, to)) = pairs.iter().find(|(from, _)| from == &o.name) {
                    o.name = to.clone();
                    // An explicit rename names the output — no longer a
                    // positional placeholder.
                    o.synthetic = false;
                }
            }
        }
        // `* REPLACE(expr AS col)` — the column's value is recomputed:
        // sources = the replacement expr's refs, and it's no longer bare.
        if let Some(repl) = &opts.opt_replace {
            for item in &repl.items {
                let target = norm_ident(&item.column_name);
                if let Some(o) = cols.iter_mut().find(|o| o.name == target) {
                    o.sources = self.expr_refs(ctx, refs, &item.expr)?;
                    o.bare = false;
                }
            }
        }
        out.extend(cols);
        Some(())
    }
}
