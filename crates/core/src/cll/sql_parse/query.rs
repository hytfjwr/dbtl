use super::*;

impl SqlParseResolver {
    // -- query / select resolution ------------------------------------------

    /// Resolve a `Query` (WITH + body + ORDER BY), returning the body's
    /// result AND the outermost select's scope bindings (needed by the
    /// declared-column probe at the model level; nested calls drop them).
    pub(super) fn resolve_query_full(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        q: &Query,
    ) -> Option<(QueryRes, Vec<Binding>)> {
        // Cumulative sub-resolution budget per model — pathological nesting
        // can't spin forever; exceeding it fails the whole model into the
        // name-match fallback.
        ctx.depth.set(ctx.depth.get() + 1);
        if ctx.depth.get() > 256 {
            return None;
        }
        let mut ctes: HashMap<String, Rc<QueryRes>> = env.ctes.clone();
        if let Some(with) = &q.with {
            for cte in &with.cte_tables {
                // A CTE sees the env's CTEs plus the earlier ones — resolve
                // BEFORE inserting itself so `WITH r AS (SELECT … FROM r)`
                // can't resolve its own name (recursive CTE → External).
                let sub = self.resolve_query(
                    ctx,
                    Env {
                        outer: env.outer,
                        ctes: &ctes,
                        locals: env.locals,
                    },
                    &cte.query,
                )?;
                let sub = rename_outputs(sub, &cte.alias.columns);
                ctes.insert(norm_ident(&cte.alias.name), Rc::new(sub));
            }
        }
        let inner = Env {
            outer: env.outer,
            ctes: &ctes,
            locals: env.locals,
        };
        let (mut res, scope) = match &*q.body {
            SetExpr::Select(sel) => self.resolve_select(ctx, inner, sel)?,
            body => (self.resolve_set_expr(ctx, inner, body)?, Vec::new()),
        };
        // Query-level tail clauses all shape the row set → FilterOrJoin.
        // ORDER BY / LIMIT BY can see BOTH the body's outputs (an
        // unqualified `order by <alias>` binds the alias, shadowing a
        // same-named FROM column) and the body's FROM scope (a qualified
        // or never-projected column) — resolve exprs against a probe scope
        // of the returned bindings stacked over the body's declared-name
        // set (the same alias level the row-shaping tail resolves
        // against), with the output-name match as the last-word fallback.
        let alias = Scope {
            bindings: vec![Binding {
                name: String::new(),
                src: TableSrc::Derived(Rc::new(QueryRes {
                    out: res.out.clone(),
                    foj: ColSources::new(),
                })),
            }],
            windows: HashMap::new(),
            outer: env.outer,
        };
        let probe = Scope {
            bindings: scope.clone(),
            windows: HashMap::new(),
            outer: Some(&alias),
        };
        let probe_refs = RefScope {
            scope: Some(&probe),
            ctes: inner.ctes,
            locals: env.locals,
        };
        if let Some(ob) = &q.order_by {
            if let OrderByKind::Expressions(exprs) = &ob.kind {
                for obe in exprs {
                    let ex = &obe.expr;
                    // `ORDER BY <alias>` (a bare identifier naming an
                    // output) binds the alias — SQL shadows the FROM column.
                    if let Expr::Identifier(i) = ex {
                        if let Some(o) = res.out.iter().find(|o| o.name == norm_ident(i)) {
                            res.foj.extend(o.sources.iter().cloned());
                            continue;
                        }
                    }
                    let mut s = self.expr_refs(ctx, probe_refs, ex)?;
                    if s.is_empty() {
                        for name in expr_ident_names(ex) {
                            if let Some(o) = res.out.iter().find(|o| o.name == name) {
                                s.extend(o.sources.iter().cloned());
                            }
                        }
                    }
                    res.foj.extend(s);
                    if let Some(wf) = &obe.with_fill {
                        for e in [&wf.from, &wf.to, &wf.step].into_iter().flatten() {
                            res.foj.extend(self.expr_refs(ctx, probe_refs, e)?);
                        }
                    }
                }
            }
        }
        if let Some(lc) = &q.limit_clause {
            match lc {
                sqlparser::ast::LimitClause::LimitOffset {
                    limit,
                    offset,
                    limit_by,
                } => {
                    for e in limit.iter().chain(limit_by.iter()) {
                        let mut s = self.expr_refs(ctx, probe_refs, e)?;
                        if s.is_empty() {
                            // `LIMIT BY` groups on select aliases too —
                            // the same output-name fallback as ORDER BY.
                            for name in expr_ident_names(e) {
                                if let Some(o) = res.out.iter().find(|o| o.name == name) {
                                    s.extend(o.sources.iter().cloned());
                                }
                            }
                        }
                        res.foj.extend(s);
                    }
                    if let Some(off) = offset {
                        res.foj.extend(self.expr_refs(ctx, probe_refs, &off.value)?);
                    }
                }
                sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                    for e in [offset, limit] {
                        res.foj.extend(self.expr_refs(ctx, probe_refs, e)?);
                    }
                }
            }
        }
        if let Some(f) = &q.fetch {
            if let Some(qe) = &f.quantity {
                res.foj.extend(self.expr_refs(ctx, probe_refs, qe)?);
            }
        }
        if let Some(settings) = &q.settings {
            for s in settings {
                res.foj.extend(self.expr_refs(ctx, probe_refs, &s.value)?);
            }
        }
        // `for_clause`/`format_clause`/`locks` carry no column expressions
        // (output-format and lock-mode decorations) — nothing to walk.
        self.resolve_pipe_operators(ctx, inner, &mut res, &q.pipe_operators)?;
        Some((res, scope))
    }

    pub(super) fn resolve_query(&self, ctx: &Ctx<'_>, env: Env<'_>, q: &Query) -> Option<QueryRes> {
        self.resolve_query_full(ctx, env, q).map(|(r, _)| r)
    }

    pub(super) fn resolve_set_expr(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        e: &SetExpr,
    ) -> Option<QueryRes> {
        match e {
            SetExpr::Select(sel) => self.resolve_select(ctx, env, sel).map(|(r, _)| r),
            SetExpr::Query(q) => self.resolve_query(ctx, env, q),
            SetExpr::SetOperation {
                left,
                right,
                op,
                set_quantifier,
            } => {
                let l = self.resolve_set_expr(ctx, env, left)?;
                let r = self.resolve_set_expr(ctx, env, right)?;
                if !matches!(op, SetOperator::Union) {
                    // EXCEPT / INTERSECT / MINUS: the right arm only shapes
                    // WHICH of the left arm's rows survive — its refs join
                    // the row-shaping pool (never value edges) and the
                    // outputs come from the left arm alone.
                    let mut foj = l.foj;
                    foj.extend(r.foj.iter().cloned());
                    for o in &r.out {
                        foj.extend(o.sources.iter().cloned());
                    }
                    return Some(QueryRes { out: l.out, foj });
                }
                // UNION: positional merge — output i carries every arm's
                // column-i sources; name/bare come from the left arm when
                // present (SQL's own rule). `BY NAME` quantifiers pair on
                // output names instead of position (DuckDB) — a
                // single-arm name stands alone.
                let by_name = matches!(
                    set_quantifier,
                    SetQuantifier::ByName
                        | SetQuantifier::AllByName
                        | SetQuantifier::DistinctByName
                );
                let mut out = Vec::new();
                if by_name {
                    for a in &l.out {
                        match r
                            .out
                            .iter()
                            .find(|b| !a.synthetic && !b.synthetic && b.name == a.name)
                        {
                            Some(b) => out.push(union_out(a, b)),
                            None => out.push(a.clone()),
                        }
                    }
                    for b in &r.out {
                        if l.out
                            .iter()
                            .all(|a| a.synthetic || b.synthetic || a.name != b.name)
                        {
                            out.push(b.clone());
                        }
                    }
                } else {
                    for i in 0..l.out.len().max(r.out.len()) {
                        match (l.out.get(i), r.out.get(i)) {
                            (Some(a), Some(b)) => out.push(union_out(a, b)),
                            (Some(a), None) => out.push(a.clone()),
                            (None, Some(b)) => out.push(b.clone()),
                            (None, None) => {}
                        }
                    }
                }
                Some(QueryRes {
                    out,
                    foj: l.foj.union(&r.foj).cloned().collect(),
                })
            }
            SetExpr::Values(v) => {
                // VALUES rows: literal columns (columnN) with no sources; the
                // row expressions' refs still count as row-shaping inputs.
                let mut foj = ColSources::new();
                let refs = RefScope {
                    scope: env.outer,
                    ctes: env.ctes,
                    locals: env.locals,
                };
                for row in &v.rows {
                    for e in &row.content {
                        foj.extend(self.expr_refs(ctx, refs, e)?);
                    }
                }
                let width = v.rows.first().map(|r| r.content.len()).unwrap_or(0);
                let out = (1..=width)
                    .map(|i| OutCol {
                        name: format!("column{i}"),
                        bare: false,
                        sources: ColSources::new(),
                        synthetic: false,
                        derived: false,
                    })
                    .collect();
                Some(QueryRes { out, foj })
            }
            SetExpr::Table(t) => {
                // `TABLE [sch.]name` — the whole relation's column list is
                // the output (CTE names resolve like a FROM reference).
                let mut parts: Vec<String> = Vec::new();
                if let Some(s) = &t.schema_name {
                    parts.push(norm_ident(s));
                }
                if let Some(n) = &t.table_name {
                    parts.push(norm_ident(n));
                }
                let mut out = Vec::new();
                let mut foj = ColSources::new();
                if let Some(last) = parts.last() {
                    if parts.len() == 1 && env.ctes.get(last).is_some() {
                        // CTE names are unqualified — `TABLE c` reads `c`.
                        let q = env.ctes.get(last).expect("checked above");
                        out = q.out.clone();
                        foj.extend(q.foj.iter().cloned());
                    } else if let Some(p) = Self::match_parent(ctx.parents, &parts) {
                        if let Some(cs) = self.col_set(ctx.dag, &p.uid) {
                            for c in cs.iter() {
                                out.push(OutCol {
                                    name: c.clone(),
                                    bare: true,
                                    sources: BTreeSet::from([(p.uid.clone(), c.clone())]),
                                    synthetic: false,
                                    derived: false,
                                });
                            }
                        }
                    }
                }
                Some(QueryRes { out, foj })
            }
            // Insert/Update/Delete/Merge bodies don't describe a model's
            // SELECT — nothing to resolve (documented non-statement).
            SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => {
                Some(QueryRes::default())
            }
        }
    }

    /// Apply the `|>` pipe-operator chain (DuckDB/BigQuery pipe syntax) to
    /// the running result `res`, in order. The pipe input is the only
    /// visible table: a synthetic Derived binding exposes the current
    /// `res.out` so unqualified refs read the CURRENT outputs (renames /
    /// `SELECT` lists keep their sources); `|> AS` names that binding for
    /// qualified refs and `|> JOIN` adds real bindings beside it.
    ///
    /// Documented approximations: `|> PIVOT`/`UNPIVOT`/`CALL` reshape the
    /// result in ways no column list can describe, so the output collapses
    /// to one synthetic column carrying every prior source (the whole
    /// input provably feeds it); `|> TABLESAMPLE` carries no column exprs.
    pub(super) fn resolve_pipe_operators(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        res: &mut QueryRes,
        ops: &[sqlparser::ast::PipeOperator],
    ) -> Option<()> {
        use sqlparser::ast::PipeOperator as P;
        // Slot 0 is the pipe input; `|> JOIN` appends bindings beside it.
        let mut scope = Scope {
            bindings: vec![Binding {
                name: String::new(),
                src: TableSrc::Derived(Rc::new(res.clone())),
            }],
            windows: HashMap::new(),
            outer: env.outer,
        };
        for po in ops {
            let refs = ref_scope(env, &scope);
            match po {
                P::Where { expr } => {
                    res.foj.extend(self.expr_refs(ctx, refs, expr)?);
                }
                P::OrderBy { exprs } => {
                    for e in exprs {
                        res.foj.extend(self.expr_refs(ctx, refs, &e.expr)?);
                    }
                }
                P::Limit { expr, offset } => {
                    for e in [expr].into_iter().chain(offset.iter()) {
                        res.foj.extend(self.expr_refs(ctx, refs, e)?);
                    }
                }
                P::Select { exprs } => {
                    let mut out = Vec::new();
                    for item in exprs {
                        self.resolve_select_item(ctx, refs, &scope, item, &mut out)?;
                    }
                    res.out = out;
                }
                P::Extend { exprs } => {
                    for item in exprs {
                        self.resolve_select_item(ctx, refs, &scope, item, &mut res.out)?;
                    }
                }
                P::Set { assignments } => {
                    for a in assignments {
                        let sources = self.expr_refs(ctx, refs, &a.value)?;
                        let names: Vec<String> = match &a.target {
                            sqlparser::ast::AssignmentTarget::ColumnName(on) => {
                                object_name_parts(on)
                                    .into_iter()
                                    .last()
                                    .into_iter()
                                    .collect()
                            }
                            sqlparser::ast::AssignmentTarget::Tuple(ons) => ons
                                .iter()
                                .filter_map(|on| object_name_parts(on).into_iter().last())
                                .collect(),
                        };
                        for n in names {
                            if let Some(o) = res.out.iter_mut().find(|o| o.name == n) {
                                o.sources = sources.clone();
                                o.bare = false;
                            } else {
                                // `SET x = expr` on a non-output name: the
                                // refs were still evaluated — row-shaping.
                                res.foj.extend(sources.iter().cloned());
                            }
                        }
                    }
                }
                P::Drop { columns } => {
                    let drop: BTreeSet<String> = columns.iter().map(norm_ident).collect();
                    res.out.retain(|o| !drop.contains(&o.name));
                }
                P::As { alias } => {
                    scope.bindings[0].name = norm_ident(alias);
                }
                P::Aggregate {
                    full_table_exprs,
                    group_by_expr,
                } => {
                    let mut out = Vec::new();
                    for ewa in full_table_exprs.iter().chain(group_by_expr.iter()) {
                        let sources = self.expr_refs(ctx, refs, &ewa.expr.expr)?;
                        out.push(OutCol {
                            name: ewa
                                .expr
                                .alias
                                .as_ref()
                                .map(norm_ident)
                                .or_else(|| derive_name(&ewa.expr.expr))
                                .unwrap_or_default(),
                            bare: self.projection_bare(ctx, refs, &ewa.expr.expr),
                            sources,
                            synthetic: false,
                            derived: ewa.expr.alias.is_none() && derived_name(&ewa.expr.expr),
                        });
                    }
                    res.out = out;
                }
                P::Rename { mappings } => {
                    let pairs: Vec<(String, String)> = mappings
                        .iter()
                        .map(|ia| (norm_ident(&ia.ident), norm_ident(&ia.alias)))
                        .collect();
                    for o in &mut res.out {
                        if let Some((_, to)) = pairs.iter().find(|(from, _)| from == &o.name) {
                            o.name = to.clone();
                            o.synthetic = false;
                            o.derived = false;
                        }
                    }
                }
                P::Union {
                    set_quantifier,
                    queries,
                } => {
                    let by_name = matches!(
                        set_quantifier,
                        SetQuantifier::ByName
                            | SetQuantifier::AllByName
                            | SetQuantifier::DistinctByName
                    );
                    for sub in queries {
                        let r = self.resolve_query(ctx, env, sub)?;
                        res.foj.extend(r.foj.iter().cloned());
                        if by_name {
                            for a in &mut res.out {
                                if let Some(b) = r
                                    .out
                                    .iter()
                                    .find(|b| !a.synthetic && !b.synthetic && b.name == a.name)
                                {
                                    a.sources = a.sources.union(&b.sources).cloned().collect();
                                }
                            }
                            for b in r.out {
                                if res
                                    .out
                                    .iter()
                                    .all(|a| a.synthetic || b.synthetic || a.name != b.name)
                                {
                                    res.out.push(b);
                                }
                            }
                        } else {
                            for i in 0..res.out.len().max(r.out.len()) {
                                match (res.out.get(i), r.out.get(i)) {
                                    (Some(a), Some(b)) => res.out[i] = union_out(a, b),
                                    (None, Some(b)) => res.out.push(b.clone()),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                P::Intersect { queries, .. } | P::Except { queries, .. } => {
                    // Like the binary EXCEPT/INTERSECT arms: each right-hand
                    // query only shapes which pipe-input rows survive.
                    for sub in queries {
                        let r = self.resolve_query(ctx, env, sub)?;
                        res.foj.extend(r.foj.iter().cloned());
                        for o in &r.out {
                            res.foj.extend(o.sources.iter().cloned());
                        }
                    }
                }
                P::Join(j) => {
                    // `|> JOIN t ON …` — the relation binds beside the pipe
                    // input; the constraint's refs are row-shaping.
                    self.bind_relation(ctx, env, &mut scope, res, &j.relation)?;
                    let refs = ref_scope(env, &scope);
                    if let JoinOperator::AsOf {
                        match_condition, ..
                    } = &j.join_operator
                    {
                        res.foj.extend(self.expr_refs(ctx, refs, match_condition)?);
                    }
                    match join_constraint(j) {
                        Some(JoinConstraint::On(e)) => {
                            res.foj.extend(self.expr_refs(ctx, refs, e)?);
                        }
                        Some(JoinConstraint::Using(names))
                        | Some(JoinConstraint::UsingAlias(names, _)) => {
                            for on in names {
                                let parts = object_name_parts(on);
                                res.foj.extend(self.resolve_parts(ctx, refs, &parts));
                            }
                        }
                        Some(JoinConstraint::Natural) => {
                            res.foj.extend(self.natural_join_sources(ctx, &scope));
                        }
                        _ => {}
                    }
                }
                P::Call { function, .. } => {
                    res.foj.extend(self.function_refs(ctx, refs, function)?);
                    res.out = vec![collapse_pipe_output(res)];
                }
                P::Pivot {
                    aggregate_functions,
                    value_column,
                    value_source,
                    ..
                } => {
                    for ef in aggregate_functions {
                        res.foj.extend(self.expr_refs(ctx, refs, &ef.expr)?);
                    }
                    for i in value_column {
                        res.foj
                            .extend(self.resolve_parts(ctx, refs, &[norm_ident(i)]));
                    }
                    self.pivot_value_source_refs(ctx, refs, value_source, &mut res.foj)?;
                    res.out = vec![collapse_pipe_output(res)];
                }
                P::Unpivot {
                    value_column,
                    name_column,
                    unpivot_columns,
                    ..
                } => {
                    // The unpivoted columns' sources flow into the value
                    // output; `name_column` carries the names themselves.
                    let drop: BTreeSet<String> = unpivot_columns.iter().map(norm_ident).collect();
                    let mut sources = ColSources::new();
                    res.out.retain(|o| {
                        if drop.contains(&o.name) {
                            sources.extend(o.sources.iter().cloned());
                            false
                        } else {
                            true
                        }
                    });
                    res.out.push(OutCol {
                        name: norm_ident(value_column),
                        bare: false,
                        sources,
                        synthetic: false,
                        derived: false,
                    });
                    res.out.push(OutCol {
                        name: norm_ident(name_column),
                        bare: false,
                        sources: ColSources::new(),
                        synthetic: false,
                        derived: false,
                    });
                }
                P::TableSample { .. } => {
                    // Sampling quantities/seeds/buckets are numerics — no
                    // column refs to walk.
                }
            }
            // The input binding re-exposes the (possibly reshaped) outputs.
            scope.bindings[0].src = TableSrc::Derived(Rc::new(res.clone()));
        }
        Some(())
    }

    /// The refs inside a pivot `IN (...)`/`ANY`/`(subquery)` value source —
    /// shared by the `PIVOT` table factor and the `|> PIVOT` pipe operator.
    pub(super) fn pivot_value_source_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        vs: &sqlparser::ast::PivotValueSource,
        foj: &mut ColSources,
    ) -> Option<()> {
        use sqlparser::ast::PivotValueSource as PVS;
        match vs {
            PVS::List(items) => {
                for it in items {
                    foj.extend(self.expr_refs(ctx, refs, &it.expr)?);
                }
            }
            PVS::Any(order_by) => {
                for ob in order_by {
                    foj.extend(self.expr_refs(ctx, refs, &ob.expr)?);
                }
            }
            PVS::Subquery(q) => {
                foj.extend(self.subquery_refs(ctx, refs, q)?);
            }
        }
        Some(())
    }

    /// Resolve one SELECT: FROM bindings → the projection's output
    /// columns → the row-shaping tail (which sees both). Returns the FROM
    /// scope too (the model level reuses it for the declared-column
    /// probe).
    pub(super) fn resolve_select(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        sel: &Select,
    ) -> Option<(QueryRes, Vec<Binding>)> {
        let mut scope = Scope {
            bindings: Vec::new(),
            windows: HashMap::new(),
            outer: env.outer,
        };
        let mut res = QueryRes::default();

        // FROM: bind every relation + join, evaluating each ON/USING against
        // the scope *as it stands* (left side already bound).
        for twj in &sel.from {
            self.bind_twj(ctx, env, &mut scope, &mut res, twj)?;
        }

        // Named windows (`WINDOW w AS (…)`) visible to `OVER w` — they live
        // on this scope level so the function walker can find them. A
        // `WINDOW w AS prev` alias chases the earlier def it points at.
        for def in &sel.named_window {
            let spec = match &def.1 {
                NamedWindowExpr::WindowSpec(spec) => Some(spec.clone()),
                NamedWindowExpr::NamedWindow(other) => {
                    scope.windows.get(&norm_ident(other)).cloned()
                }
            };
            if let Some(spec) = spec {
                scope.windows.insert(norm_ident(&def.0), spec);
            }
        }

        // LATERAL VIEWs explode a column into rows — their input expressions
        // are row-shaping, and `… lv AS c1,c2` binds a derived relation of
        // the exploded values (so `lv.c1` / bare `c1` in the projection
        // resolve to the input expr's sources).
        for lv in &sel.lateral_views {
            let lv_refs = {
                let r = ref_scope(env, &scope);
                self.expr_refs(ctx, r, &lv.lateral_view)?
            };
            res.foj.extend(lv_refs.iter().cloned());
            if let Some(name) = object_name_parts(&lv.lateral_view_name).last().cloned() {
                let out = if lv.lateral_col_alias.is_empty() {
                    vec![OutCol {
                        name: String::new(),
                        bare: false,
                        sources: lv_refs,
                        synthetic: true,
                        derived: false,
                    }]
                } else {
                    lv.lateral_col_alias
                        .iter()
                        .map(|a| OutCol {
                            name: norm_ident(a),
                            bare: false,
                            sources: lv_refs.clone(),
                            synthetic: false,
                            derived: false,
                        })
                        .collect()
                };
                scope.bindings.push(Binding {
                    name,
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
        }

        // The projection → output columns. Items resolve LEFT TO RIGHT,
        // each against the FROM scope first and then the names the EARLIER
        // items declared — modeled as one extra scope level between FROM
        // and the outer scopes, holding an un-nameable derived binding of
        // the outputs so far. A same-SELECT name no FROM binding proves
        // therefore reads as the alias (`select a + 1 as c, c * 2` reaches
        // `a`), while a same-named FROM column still wins (innermost scope
        // resolves first). The level's bindings/windows are plain clones of
        // this select's — wildcard expansion and `OVER w` see exactly the
        // same surface.
        let mut out: Vec<OutCol> = Vec::new();
        for item in &sel.projection {
            // Nothing nameable declared yet — the alias level can't
            // contribute (a leading literal or other unnameable output
            // never aliases), so resolve straight against FROM and skip
            // the clones.
            if out.iter().all(|o| o.name.is_empty()) {
                let refs = ref_scope(env, &scope);
                self.resolve_select_item(ctx, refs, &scope, item, &mut out)?;
                continue;
            }
            let alias = Scope {
                bindings: vec![Binding {
                    name: String::new(),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out: out.clone(),
                        foj: ColSources::new(),
                    })),
                }],
                windows: HashMap::new(),
                outer: env.outer,
            };
            let item_scope = Scope {
                bindings: scope.bindings.clone(),
                windows: scope.windows.clone(),
                outer: Some(&alias),
            };
            let item_refs = RefScope {
                scope: Some(&item_scope),
                ctes: env.ctes,
                locals: env.locals,
            };
            self.resolve_select_item(ctx, item_refs, &item_scope, item, &mut out)?;
        }

        // SELECT-level EXCLUDE (Redshift `SELECT * EXCLUDE(col)` parses at
        // the Select level, not as wildcard options).
        if let Some(excl) = &sel.exclude {
            let names = exclude_names(excl);
            out.retain(|o| !names.contains(&o.name));
        }

        // Unnamed outputs (literals, unnameable exprs) get a positional
        // placeholder so positional merges stay aligned — but they're
        // marked `synthetic` and never surface in `columns()`, the
        // row-shaping fan, or `UNION BY NAME` matching (a real column
        // literally named `__col1` is never marked).
        let mut anon = 0usize;
        for o in &mut out {
            if o.name.is_empty() {
                anon += 1;
                o.name = format!("{ANON_COL_PREFIX}{anon}");
                o.synthetic = true;
            }
        }

        // The row-shaping tail resolves last, over the same FROM scope
        // with the complete declared-name set as the next level out:
        // WHERE/GROUP BY/HAVING/QUALIFY and the dialect tails read a
        // same-SELECT alias the FROM scope can't prove (`where c > 2`
        // after `a + b as c`). Dialects that reject an alias there stay
        // silent either way — flagging invalid SQL isn't this analysis'
        // job.
        let alias = Scope {
            bindings: vec![Binding {
                name: String::new(),
                src: TableSrc::Derived(Rc::new(QueryRes {
                    out: out.clone(),
                    foj: ColSources::new(),
                })),
            }],
            windows: HashMap::new(),
            outer: env.outer,
        };
        let tail_scope = Scope {
            bindings: scope.bindings.clone(),
            windows: scope.windows.clone(),
            outer: Some(&alias),
        };
        let tail_refs = RefScope {
            scope: Some(&tail_scope),
            ctes: env.ctes,
            locals: env.locals,
        };

        // `TOP (n)` — the quantity is row-shaping (usually a literal, but
        // `TOP (expr)` can carry a column ref).
        if let Some(top) = &sel.top {
            if let Some(sqlparser::ast::TopQuantity::Expr(e)) = &top.quantity {
                res.foj.extend(self.expr_refs(ctx, tail_refs, e)?);
            }
        }

        // The row-shaping tail: PREWHERE/WHERE/GROUP BY/HAVING/QUALIFY/
        // CONNECT BY/CLUSTER BY/DISTRIBUTE BY/SORT BY/DISTINCT ON.
        for e in [&sel.prewhere, &sel.selection, &sel.having, &sel.qualify]
            .into_iter()
            .flatten()
        {
            res.foj.extend(self.expr_refs(ctx, tail_refs, e)?);
        }
        if let GroupByExpr::Expressions(exprs, _) = &sel.group_by {
            for e in exprs {
                // `GROUP BY 1` positional ordinals stay unresolved — a
                // literal int never names a column (documented degenerate
                // case; the output at that position is untouched).
                res.foj.extend(self.expr_refs(ctx, tail_refs, e)?);
            }
        }
        for cb in &sel.connect_by {
            match cb {
                sqlparser::ast::ConnectByKind::ConnectBy { relationships, .. } => {
                    for e in relationships {
                        res.foj.extend(self.expr_refs(ctx, tail_refs, e)?);
                    }
                }
                sqlparser::ast::ConnectByKind::StartWith { condition, .. } => {
                    res.foj.extend(self.expr_refs(ctx, tail_refs, condition)?);
                }
            }
        }
        for e in sel
            .cluster_by
            .iter()
            .chain(&sel.distribute_by)
            .chain(sel.sort_by.iter().map(|o| &o.expr))
        {
            res.foj.extend(self.expr_refs(ctx, tail_refs, e)?);
        }
        if let Some(Distinct::On(exprs)) = &sel.distinct {
            for e in exprs {
                res.foj.extend(self.expr_refs(ctx, tail_refs, e)?);
            }
        }

        res.out = out;
        Some((res, scope.bindings))
    }

    /// Resolve one projection item into output columns — shared by
    /// `resolve_select` and the `|> SELECT`/`|> EXTEND` pipe operators.
    pub(super) fn resolve_select_item(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        scope: &Scope<'_>,
        item: &SelectItem,
        out: &mut Vec<OutCol>,
    ) -> Option<()> {
        match item {
            SelectItem::UnnamedExpr(e) => {
                let sources = self.expr_refs(ctx, refs, e)?;
                out.push(OutCol {
                    name: derive_name(e).unwrap_or_default(),
                    bare: self.projection_bare(ctx, refs, e),
                    sources,
                    synthetic: false,
                    derived: derived_name(e),
                });
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                let sources = self.expr_refs(ctx, refs, expr)?;
                out.push(OutCol {
                    name: norm_ident(alias),
                    bare: self.projection_bare(ctx, refs, expr),
                    sources,
                    synthetic: false,
                    derived: false,
                });
            }
            SelectItem::ExprWithAliases { expr, aliases } => {
                let sources = self.expr_refs(ctx, refs, expr)?;
                for a in aliases {
                    out.push(OutCol {
                        name: norm_ident(a),
                        bare: self.projection_bare(ctx, refs, expr),
                        sources: sources.clone(),
                        synthetic: false,
                        derived: false,
                    });
                }
            }
            SelectItem::Wildcard(opts) => {
                self.expand_wildcard(ctx, refs, scope, None, opts, out)?;
            }
            SelectItem::QualifiedWildcard(kind, opts) => match kind {
                SelectItemQualifiedWildcardKind::ObjectName(on) => {
                    let parts = object_name_parts(on);
                    if let Some(qual) = parts.last() {
                        self.expand_wildcard(ctx, refs, scope, Some(qual.clone()), opts, out)?;
                    }
                }
                SelectItemQualifiedWildcardKind::Expr(e) => {
                    // `(expr).*` — a struct expansion: the fields are
                    // un-nameable, so only the expr's refs register… as
                    // an unnamed output, skipped by name matching.
                    let sources = self.expr_refs(ctx, refs, e)?;
                    out.push(OutCol {
                        name: String::new(),
                        bare: false,
                        sources,
                        synthetic: false,
                        derived: false,
                    });
                }
            },
        }
        Some(())
    }
}
