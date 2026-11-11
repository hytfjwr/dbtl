use super::*;

// ---------------------------------------------------------------------------
// Small AST helpers
// ---------------------------------------------------------------------------

/// `Ident` → the normalized column-space name: quoted idents keep their
/// case (sans quotes), unquoted fold to lowercase — the same rule
/// [`normalize_column`] applies to hand-written names.
pub(super) fn norm_ident(i: &Ident) -> String {
    if i.quote_style.is_some() {
        i.value.clone()
    } else {
        i.value.to_lowercase()
    }
}

/// Case-fold a canonical `expr.to_string()` for cross-manifest comparison:
/// bytes inside single/double/back-quoted spans copy verbatim (string
/// literals AND quoted identifiers keep their case — the [`norm_ident`]
/// rule), everything else folds to lowercase so `STATUS`, `Status`, and
/// `status` compare equal — as do `SUM(x)`/`sum(x)`. Deterministic
/// byte-scanning; no re-lexing, so escapes can't desync the two sides.
pub(super) fn fold_expr_text(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                i = (i + 1).min(b.len());
                out.extend_from_slice(&b[start..i]);
            }
            c => {
                out.push(c.to_ascii_lowercase());
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `ObjectName` → normalized dotted segments.
pub(super) fn object_name_parts(on: &ObjectName) -> Vec<String> {
    on.0.iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(i) => Some(norm_ident(i)),
            _ => None,
        })
        .collect()
}

/// Is `e` a bare column reference (`x` / `t.x`), possibly through parens?
pub(super) fn is_bare_ref(e: &Expr) -> bool {
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        Expr::Nested(inner) => is_bare_ref(inner),
        _ => false,
    }
}

/// The normalized parts of a bare reference — `x` → `[x]`, `t.x` → `[t, x]`.
pub(super) fn bare_parts(e: &Expr) -> Option<Vec<String>> {
    match e {
        Expr::Identifier(i) => Some(vec![norm_ident(i)]),
        Expr::CompoundIdentifier(parts) => Some(parts.iter().map(norm_ident).collect()),
        Expr::Nested(inner) => bare_parts(inner),
        _ => None,
    }
}

/// The output name an un-aliased projection expr wants: bare refs → their
/// last segment; casts unwrap to the inner expr's name; a function → its
/// name; a CASE → `case`; field access → the last field; everything else →
/// no name (positional fallback).
pub(super) fn derive_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(i) => Some(norm_ident(i)),
        Expr::CompoundIdentifier(parts) => parts.last().map(norm_ident),
        Expr::CompoundFieldAccess {
            root, access_chain, ..
        } => access_chain
            .iter()
            .rev()
            .find_map(|a| match a {
                AccessExpr::Dot(e) => derive_name(e),
                _ => None,
            })
            .or_else(|| derive_name(root)),
        Expr::Nested(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Collate { expr: inner, .. } => derive_name(inner),
        // `expr AS name` in expression position (MySQL-derived syntax):
        // the declared name is the output name, like ExprWithAlias.
        Expr::Named { name, .. } => Some(norm_ident(name)),
        Expr::Function(f) => object_name_parts(&f.name).last().cloned(),
        Expr::Case { .. } => Some("case".to_string()),
        Expr::Prefixed { value, .. } => derive_name(value),
        _ => None,
    }
}

/// The `ON`/`USING` of a join operator, when it has one.
pub(super) fn join_constraint(j: &Join) -> Option<&JoinConstraint> {
    use JoinOperator::*;
    match &j.join_operator {
        Join(c) | Inner(c) | Left(c) | LeftOuter(c) | Right(c) | RightOuter(c) | FullOuter(c)
        | CrossJoin(c) | Semi(c) | LeftSemi(c) | RightSemi(c) | Anti(c) | LeftAnti(c)
        | RightAnti(c) | StraightJoin(c) => Some(c),
        AsOf { constraint, .. } => Some(constraint),
        _ => None,
    }
}

/// `EXCLUDE` item set → normalized names (the last segment of each).
pub(super) fn exclude_names(excl: &ExcludeSelectItem) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let names: Vec<&ObjectName> = match excl {
        ExcludeSelectItem::Single(on) => vec![on],
        ExcludeSelectItem::Multiple(ons) => ons.iter().collect(),
    };
    for on in names {
        if let Some(last) = object_name_parts(on).into_iter().last() {
            out.insert(last);
        }
    }
    out
}

/// The bare identifier names appearing at the top level of `e` — the
/// `ORDER BY <output-name>` probe (Query-level ORDER BY can't see the
/// select's FROM scope, so only the output-name match is available).
pub(super) fn expr_ident_names(e: &Expr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    match e {
        Expr::Identifier(i) => {
            out.insert(norm_ident(i));
        }
        Expr::CompoundIdentifier(parts) => {
            if let Some(last) = parts.last() {
                out.insert(norm_ident(last));
            }
        }
        Expr::Nested(inner) => out.extend(expr_ident_names(inner)),
        _ => {}
    }
    out
}

/// Could this query's output carry columns no projection enumerated? A
/// `*`/`t.*` over a binding whose column list is unknown leaves carried
/// columns invisible to `Analyzed::out` — the only case the declared-column
/// probe can legitimately fill (without a wildcard every output is named,
/// so an unproduced declared name was never selected).
pub(super) fn query_has_output_wildcard(q: &Query) -> bool {
    set_expr_has_wildcard(&q.body)
        || q.pipe_operators.iter().any(|po| {
            use sqlparser::ast::PipeOperator as P;
            match po {
                P::Select { exprs } | P::Extend { exprs } => {
                    exprs.iter().any(select_item_is_wildcard)
                }
                P::Union { queries, .. } => queries.iter().any(query_has_output_wildcard),
                _ => false,
            }
        })
}

pub(super) fn set_expr_has_wildcard(e: &SetExpr) -> bool {
    match e {
        SetExpr::Select(sel) => sel.projection.iter().any(select_item_is_wildcard),
        SetExpr::SetOperation {
            left, right, op, ..
        } => {
            // Only UNION arms merge outputs; an EXCEPT/INTERSECT right arm
            // shapes rows, never output names.
            set_expr_has_wildcard(left)
                || (*op == SetOperator::Union && set_expr_has_wildcard(right))
        }
        SetExpr::Query(q) => query_has_output_wildcard(q),
        _ => false,
    }
}

pub(super) fn select_item_is_wildcard(i: &SelectItem) -> bool {
    matches!(
        i,
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
    )
}

/// A resolved query whose outputs are positionally renamed by a
/// `WITH x(a,b) AS` / `(sub) AS t(a,b)` column-alias list.
pub(super) fn rename_outputs(mut res: QueryRes, cols: &[TableAliasColumnDef]) -> QueryRes {
    if cols.is_empty() {
        return res;
    }
    for (o, c) in res.out.iter_mut().zip(cols.iter()) {
        o.name = norm_ident(&c.name);
        // `WITH c(a, b)` / `t(a, b)` gives the output a real name — a
        // positional placeholder it lands on becomes declared.
        o.synthetic = false;
    }
    res
}

/// Merge two positional (or `BY NAME`-matched) UNION arms' output columns:
/// the left arm's name/bare when the left exists (SQL's naming rule), the
/// union of both sides' sources. An arm contributing no sources (a literal
/// `null` arm) doesn't vote on bareness.
pub(super) fn union_out(a: &OutCol, b: &OutCol) -> OutCol {
    OutCol {
        name: a.name.clone(),
        bare: match (a.sources.is_empty(), b.sources.is_empty()) {
            (false, true) => a.bare,
            (true, false) => b.bare,
            _ => a.bare && b.bare,
        },
        sources: a.sources.union(&b.sources).cloned().collect(),
        // The kept name is `a`'s — its synthetic flag answers "is this name
        // invented?" regardless of what the right arm called the column.
        synthetic: a.synthetic,
    }
}

/// The collapsed output of a `|> PIVOT`/`|> CALL` pipe step — one synthetic
/// column carrying every source the input consumed (the reshape's real
/// column list is unknowable).
pub(super) fn collapse_pipe_output(res: &QueryRes) -> OutCol {
    let mut sources: ColSources = res
        .out
        .iter()
        .flat_map(|o| o.sources.iter().cloned())
        .collect();
    sources.extend(res.foj.iter().cloned());
    OutCol {
        name: String::new(),
        bare: false,
        sources,
        synthetic: true,
    }
}

/// `rename_outputs` over an `Rc<QueryRes>` (clones the inner value — CTE
/// results are shared, the rename only applies at this binding).
pub(super) fn rename_rc(q: &Rc<QueryRes>, cols: &[TableAliasColumnDef]) -> Rc<QueryRes> {
    if cols.is_empty() {
        return q.clone();
    }
    Rc::new(rename_outputs((**q).clone(), cols))
}

/// Case-insensitive `LIKE` match for `* ILIKE '<pat>'`: `%` matches any
/// run, every other char (including `_` — the approximation noted in the
/// module docs) matches itself, compared on lowercase.
pub(super) fn ilike_match(pattern: &str, name: &str) -> bool {
    pub(super) fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(&'%') => (0..=n.len()).any(|k| go(&p[1..], &n[k..])),
            Some(&c) => n.first() == Some(&c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    go(&p, &n)
}
