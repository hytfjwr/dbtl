use super::*;

impl SqlParseResolver {
    /// Bind a `TableWithJoins`: the base relation, then each join's relation
    /// and its ON/USING constraint (evaluated with BOTH sides in scope).
    pub(super) fn bind_twj(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        scope: &mut Scope<'_>,
        res: &mut QueryRes,
        twj: &TableWithJoins,
    ) -> Option<()> {
        self.bind_relation(ctx, env, scope, res, &twj.relation)?;
        for j in &twj.joins {
            self.bind_relation(ctx, env, scope, res, &j.relation)?;
            let refs = ref_scope(env, scope);
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
                Some(JoinConstraint::Using(names)) | Some(JoinConstraint::UsingAlias(names, _)) => {
                    for on in names {
                        let parts = object_name_parts(on);
                        res.foj.extend(self.resolve_parts(ctx, refs, &parts));
                    }
                }
                Some(JoinConstraint::Natural) => {
                    res.foj.extend(self.natural_join_sources(ctx, scope));
                }
                _ => {}
            }
        }
        Some(())
    }

    /// Register one table factor in `scope` (and walk its expression args /
    /// inner factors for row-shaping refs).
    pub(super) fn bind_relation(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        scope: &mut Scope<'_>,
        res: &mut QueryRes,
        factor: &TableFactor,
    ) -> Option<()> {
        match factor {
            TableFactor::Table {
                name,
                alias,
                args,
                with_hints,
                ..
            } => {
                let refs = ref_scope(env, scope);
                for e in with_hints {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                if let Some(args) = args {
                    for a in &args.args {
                        res.foj.extend(self.function_arg_refs(ctx, refs, a)?);
                    }
                }
                let parts = object_name_parts(name);
                let last = parts.last().cloned().unwrap_or_default();
                let binding_name = alias
                    .as_ref()
                    .map(|a| norm_ident(&a.name))
                    .unwrap_or_else(|| last.clone());
                // A CTE is only ever addressable UNqualified — `sch.c` is a
                // real relation (compiled `ref('c')` → `analytics.c`), never
                // the CTE `c`.
                let cte = if parts.len() == 1 {
                    env.ctes.get(&last)
                } else {
                    None
                };
                let alias_cols = alias.as_ref().map(|a| a.columns.as_slice()).unwrap_or(&[]);
                let src = if let Some(q) = cte {
                    // The CTE's own row-shaping deps (WHERE/JOIN inside it)
                    // shape every row this select reads — merge them.
                    res.foj.extend(q.foj.iter().cloned());
                    TableSrc::Derived(rename_rc(q, alias_cols))
                } else if let Some(p) = Self::match_parent(ctx.parents, &parts) {
                    if let (Some(cols), false) =
                        (self.col_set(ctx.dag, &p.uid), alias_cols.is_empty())
                    {
                        // `FROM up t(a, b)` positionally renames the parent's
                        // outputs — same alias-list semantics as a CTE, so
                        // `t.b` reads the parent's second column.
                        let out = cols
                            .iter()
                            .map(|c| OutCol {
                                name: c.clone(),
                                bare: true,
                                sources: BTreeSet::from([(p.uid.clone(), c.clone())]),
                                synthetic: false,
                            })
                            .collect();
                        TableSrc::Derived(Rc::new(rename_outputs(
                            QueryRes {
                                out,
                                foj: ColSources::new(),
                            },
                            alias_cols,
                        )))
                    } else {
                        TableSrc::Parent(p.uid.clone())
                    }
                } else {
                    TableSrc::External
                };
                scope.bindings.push(Binding {
                    name: binding_name,
                    src,
                });
            }
            TableFactor::Derived {
                lateral,
                subquery,
                alias,
                ..
            } => {
                let sub = self.resolve_query(
                    ctx,
                    Env {
                        // LATERAL derived tables see the FROM items bound so
                        // far; a plain derived table sees only outer scopes.
                        outer: if *lateral { Some(&*scope) } else { env.outer },
                        ctes: env.ctes,
                        locals: env.locals,
                    },
                    subquery,
                )?;
                let sub = rename_outputs(
                    sub,
                    alias.as_ref().map(|a| a.columns.as_slice()).unwrap_or(&[]),
                );
                // Same propagation as CTE bindings: a derived table's inner
                // WHERE/JOIN shapes the rows this select reads.
                res.foj.extend(sub.foj.iter().cloned());
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| format!("__anon{}", scope.bindings.len())),
                    src: TableSrc::Derived(Rc::new(sub)),
                });
            }
            TableFactor::Function { args, alias, .. } => {
                let refs = ref_scope(env, scope);
                for a in args {
                    res.foj.extend(self.function_arg_refs(ctx, refs, a)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__func".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::TableFunction { expr, alias } => {
                res.foj
                    .extend(self.expr_refs(ctx, ref_scope(env, scope), expr)?);
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__func".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.bind_twj(ctx, env, scope, res, table_with_joins)?;
            }
            TableFactor::UNNEST {
                alias,
                array_exprs,
                with_offset_alias,
                with_ordinality,
                ..
            } => {
                // UNNEST explodes an array into rows — row-shaping input,
                // and its alias binds a derived relation of the exploded
                // value (`UNNEST(a) AS u` → `u` carries `a`'s refs).
                let refs = ref_scope(env, &*scope);
                let mut elem_sources = ColSources::new();
                for e in array_exprs {
                    elem_sources.extend(self.expr_refs(ctx, refs, e)?);
                }
                res.foj.extend(elem_sources.iter().cloned());
                let mut out: Vec<OutCol> = {
                    let names: Vec<String> = alias
                        .as_ref()
                        .map(|a| a.columns.iter().map(|c| norm_ident(&c.name)).collect())
                        .unwrap_or_else(|| vec!["unnest".to_string()]);
                    names
                        .into_iter()
                        .map(|name| OutCol {
                            name,
                            bare: false,
                            sources: elem_sources.clone(),
                            synthetic: false,
                        })
                        .collect()
                };
                // Offsets/ordinality are generated, not read — no sources.
                let ord_name = with_offset_alias
                    .as_ref()
                    .map(norm_ident)
                    .or_else(|| with_ordinality.then(|| "ordinality".to_string()));
                if let Some(name) = ord_name {
                    out.push(OutCol {
                        name,
                        bare: false,
                        sources: ColSources::new(),
                        synthetic: false,
                    });
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__unnest".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::Pivot {
                table,
                aggregate_functions,
                value_column,
                value_source,
                default_on_null,
                alias,
            } => {
                // The inner table still binds (its columns feed the pivot
                // values); the pivoted result itself binds un-nameable.
                self.bind_relation(ctx, env, scope, res, table)?;
                let refs = ref_scope(env, scope);
                for ef in aggregate_functions {
                    res.foj.extend(self.expr_refs(ctx, refs, &ef.expr)?);
                }
                for e in value_column {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                self.pivot_value_source_refs(ctx, refs, value_source, &mut res.foj)?;
                if let Some(e) = default_on_null {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__pivot".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::Unpivot {
                table,
                value,
                columns,
                alias,
                ..
            } => {
                self.bind_relation(ctx, env, scope, res, table)?;
                let refs = ref_scope(env, scope);
                res.foj.extend(self.expr_refs(ctx, refs, value)?);
                for c in columns {
                    res.foj.extend(self.expr_refs(ctx, refs, &c.expr)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__unpivot".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::UnpivotExpr {
                expression,
                value_alias,
                attribute_alias,
            } => {
                // `UNPIVOT expr AS v AT a` (Redshift SUPER): the exploded
                // value/attribute columns read the input expression.
                let sources = self.expr_refs(ctx, ref_scope(env, scope), expression)?;
                res.foj.extend(sources.iter().cloned());
                let mut out = vec![OutCol {
                    name: norm_ident(value_alias),
                    bare: false,
                    sources: sources.clone(),
                    synthetic: false,
                }];
                if let Some(a) = attribute_alias {
                    out.push(OutCol {
                        name: norm_ident(a),
                        bare: false,
                        sources,
                        synthetic: false,
                    });
                }
                scope.bindings.push(Binding {
                    name: norm_ident(value_alias),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::JsonTable {
                json_expr,
                columns,
                alias,
                ..
            } => {
                // JSON_TABLE: the document expr is row-shaping input; the
                // declared COLUMNS bind named outputs fed by it (paths and
                // error-handling clauses carry no column refs).
                let doc = self.expr_refs(ctx, ref_scope(env, scope), json_expr)?;
                res.foj.extend(doc.iter().cloned());
                let mut names: Vec<String> = Vec::new();
                let mut stack: Vec<&sqlparser::ast::JsonTableColumn> = columns.iter().collect();
                while let Some(c) = stack.pop() {
                    match c {
                        sqlparser::ast::JsonTableColumn::Named(n) => {
                            names.push(norm_ident(&n.name));
                        }
                        sqlparser::ast::JsonTableColumn::ForOrdinality(i) => {
                            names.push(norm_ident(i));
                        }
                        sqlparser::ast::JsonTableColumn::Nested(n) => {
                            stack.extend(n.columns.iter());
                        }
                    }
                }
                let out = names
                    .into_iter()
                    .map(|name| OutCol {
                        name,
                        bare: false,
                        sources: doc.clone(),
                        synthetic: false,
                    })
                    .collect();
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__json".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::OpenJsonTable {
                json_expr,
                columns,
                alias,
                ..
            } => {
                // Same shape as JSON_TABLE with the OpenJson column type.
                let doc = self.expr_refs(ctx, ref_scope(env, scope), json_expr)?;
                res.foj.extend(doc.iter().cloned());
                let out = columns
                    .iter()
                    .map(|c| OutCol {
                        name: norm_ident(&c.name),
                        bare: false,
                        sources: doc.clone(),
                        synthetic: false,
                    })
                    .collect();
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__json".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::MatchRecognize {
                table,
                partition_by,
                order_by,
                measures,
                symbols,
                alias,
                ..
            } => {
                self.bind_relation(ctx, env, scope, res, table)?;
                // DEFINE symbols are row variables (like lambda params):
                // local names, never bindings — `A.price` still reaches the
                // input column through the first-segment fallback.
                let mut locals = env.locals.clone();
                for sd in symbols {
                    locals.insert(norm_ident(&sd.symbol));
                }
                let refs = RefScope {
                    scope: Some(scope),
                    ctes: env.ctes,
                    locals: &locals,
                };
                for e in partition_by {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                for o in order_by {
                    res.foj.extend(self.expr_refs(ctx, refs, &o.expr)?);
                }
                for m in measures {
                    res.foj.extend(self.expr_refs(ctx, refs, &m.expr)?);
                }
                for sd in symbols {
                    res.foj.extend(self.expr_refs(ctx, refs, &sd.definition)?);
                }
                // `rows_per_match`/`after_match_skip`/`pattern` carry no
                // column exprs (row-count modes and a regex over symbols).
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__mr".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::XmlTable {
                namespaces,
                row_expression,
                passing,
                columns,
                alias,
            } => {
                // XMLTABLE: namespaces/passing args read outer columns; the
                // extracted COLUMNS bind named outputs fed by the passed
                // document (the PATH/default exprs stay in the row-shaping
                // pool — column-list lineage for them is unknowable).
                let refs = ref_scope(env, scope);
                res.foj.extend(self.expr_refs(ctx, refs, row_expression)?);
                let mut doc = ColSources::new();
                for ns in namespaces {
                    doc.extend(self.expr_refs(ctx, refs, &ns.uri)?);
                }
                for arg in &passing.arguments {
                    doc.extend(self.expr_refs(ctx, refs, &arg.expr)?);
                }
                res.foj.extend(doc.iter().cloned());
                for c in columns {
                    if let sqlparser::ast::XmlTableColumnOption::NamedInfo {
                        path, default, ..
                    } = &c.option
                    {
                        for e in [path, default].into_iter().flatten() {
                            res.foj.extend(self.expr_refs(ctx, refs, e)?);
                        }
                    }
                }
                let out = columns
                    .iter()
                    .map(|c| OutCol {
                        name: norm_ident(&c.name),
                        bare: false,
                        sources: doc.clone(),
                        synthetic: false,
                    })
                    .collect();
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__xml".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::SemanticView {
                dimensions,
                metrics,
                facts,
                where_clause,
                alias,
                ..
            } => {
                // SEMANTIC_VIEW names objects inside a Snowflake semantic
                // model — not lineage relations. Their refs can still land
                // on same-named scope columns (the general qualified-name
                // ambiguity), so they join the row-shaping pool and the
                // view binds External.
                let refs = ref_scope(env, scope);
                for e in dimensions.iter().chain(metrics).chain(facts) {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                if let Some(e) = where_clause {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__semantic".to_string()),
                    src: TableSrc::External,
                });
            }
        }
        Some(())
    }
}
