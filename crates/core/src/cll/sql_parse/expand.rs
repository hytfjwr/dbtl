//! Provable root-project `{% macro %}` expansion.
//!
//! A model file's `{{ name(args) }}` — where `name` resolves to a root
//! `{% macro %}` def whose body is JUST literal text and provable
//! `{{ … }}` emissions (its params, `ref`/`source` calls with provable
//! args, other expandable project macros) — renders the same text dbt
//! emits instead of the generic placeholder. The whole shape has to
//! prove statically: argument binding follows dbt's own call rules
//! (positional-or-keyword, defaults, no duplicates or extras), the
//! body carries no `{% %}` at all (any control-flow tag leaves the
//! def unexpandable), and a missing required arg is a dbt error — all
//! leave the tag to the placeholder path. Non-root defs, qualified
//! calls outside the project namespace, unbounded arguments, and
//! chains past the depth cap do too.
//!
//! Expansion runs only for root-project NODES — a package node's bare
//! calls resolve against its own package's namespace, not the root's —
//! and only where a `{{ }}` is a clean token (top level, unglued from
//! identifier text): inside a string or comment the tag still renders
//! positionally.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use serde_json::Value;

use super::jinja::{jinja_inner, quoted_ident, JinjaMark, JINJA_MARK_PREFIX};
use super::withhold::{find_block_word, leading_block, leading_call};
use crate::dag::Dag;
use crate::source::{
    bind_slots, call_shape_valid, is_identifier, jinja_tag_close, lit_value, mask_jinja_comments,
    parse_params, split_kwarg, tag_word, top_level_segments, Param, SlotSource, MAX_MACRO_DEPTH,
};

/// The dag's expandable root `{% macro %}` defs — keyed by uid — plus
/// the package namespace a `proj.name(…)` call resolves through. Built
/// once per dag and shared (`Rc`) across every node render.
pub(crate) struct MacroTable {
    /// The root project's package segment — the namespace a
    /// `proj.name(…)` call and every `macro.<ns>.<name>` uid share.
    /// [`Dag::primary_package`] is the axis [`Dag::is_root_macro_def_in`]
    /// and the node's own `is_root_project` gate already use, so the
    /// def filter, the qualified-call match, and the uid lookup can
    /// never disagree (the `project_name`/`primary_package` fallbacks
    /// split on a metadata-less manifest — the uid segment is the one
    /// defs are keyed under).
    ns: Option<String>,
    defs: HashMap<String, Expandable>,
}

impl MacroTable {
    /// The expandable defs `dag`'s macro map carries — defs failing
    /// [`Expandable::parse`] (control flow, package/builtin calls,
    /// unprovable args) aren't in the table, so every lookup leaves
    /// the call unexpanded.
    pub(crate) fn for_dag(dag: &Dag) -> MacroTable {
        let ns = dag.primary_package();
        let mut defs = HashMap::new();
        for (uid, m) in dag.macros() {
            let Some(sql) = m.sql.as_deref() else {
                continue;
            };
            if !dag.is_root_macro_def_in(ns.as_deref(), uid) {
                continue;
            }
            if let Some(ex) = Expandable::parse(sql, ns.as_deref()) {
                defs.insert(uid.clone(), ex);
            }
        }
        MacroTable { ns, defs }
    }

    /// No expandable defs — the render takes its placeholder path.
    pub(crate) fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// The `Expandable` `name` resolves to — bare names and
    /// `<ns>.name(…)` both bind `macro.<ns>.<name>`; any other
    /// namespace is a package/builtin call the table never holds.
    fn lookup(&self, name: &str) -> Option<&Expandable> {
        let ns = self.ns.as_deref()?;
        let bare = match name.split_once('.') {
            None => name,
            Some((qual, bare)) if qual == ns => bare,
            Some(_) => return None,
        };
        self.defs.get(&format!("macro.{ns}.{bare}"))
    }

    /// The text `{{ name(args) }}` emits when `name` binds an
    /// expandable def — plus the rendered ranges inside it standing
    /// for a `ref`/`source` relation (generated names, not CTE reads).
    /// `None` leaves the tag to the placeholder: callee missing or not
    /// expandable, an arg dbt's call rules reject or can't prove, or a
    /// nested chain the depth cap cuts.
    pub(crate) fn expand_call(
        &self,
        name: &str,
        args: &str,
        mark: JinjaMark,
    ) -> Option<(String, Vec<Range<usize>>)> {
        let ex = self.lookup(name)?;
        let (pos, kw) = parse_args(args, &[])?;
        let bound = bind_call(&ex.params, &pos, &kw, &[])?;
        let mut out = String::new();
        let mut refs = Vec::new();
        self.expand_parts(ex, &bound, mark, 0, &mut out, &mut refs)?;
        Some((out, refs))
    }

    /// Emit `ex`'s parts under `bound`, appending to `out` — `None`
    /// when a part can't prove its value. `refs` collects the emitted
    /// ranges standing for a relation. Whitespace control: a part's `l`
    /// flag trims this emission's trailing whitespace back to `start`,
    /// `r` eats the next text part's leading run; the def's own
    /// `{% macro -%}` / `{%- endmacro %}` dashes bound the body the
    /// same way.
    fn expand_parts(
        &self,
        ex: &Expandable,
        bound: &[Option<BoundVal>],
        mark: JinjaMark,
        depth: usize,
        out: &mut String,
        refs: &mut Vec<Range<usize>>,
    ) -> Option<()> {
        if depth >= MAX_MACRO_DEPTH {
            return None;
        }
        let start = out.len();
        let mut strip_next = ex.sig_dash;
        for part in &ex.parts {
            let (l, r) = part.flags();
            if l {
                let end = out.trim_end().len().max(start);
                out.truncate(end);
            }
            match part {
                Part::Text(text) => {
                    out.push_str(if strip_next { text.trim_start() } else { text });
                }
                Part::Expr { index, .. } => {
                    let v = bound.get(*index)?.clone()?;
                    let from = out.len();
                    out.push_str(&v.render(mark));
                    if v.is_relation() {
                        refs.push(from..out.len());
                    }
                }
                Part::Dep { call, .. } => {
                    let v = call.eval(bound)?;
                    let from = out.len();
                    out.push_str(&v.render(mark));
                    refs.push(from..out.len());
                }
                Part::Call { name, pos, kw, .. } => {
                    // A nested call's name resolves in the DEF's
                    // context, never the caller's — dbt compiles
                    // `{% set m = 'x' %}` + `{{ join_on() }}` (whose
                    // body calls `{{ m() }}`) to the macro's `m`, not
                    // the bound 'x'. `bound_names().shadows` therefore
                    // gates only the model-level tag in
                    // `Scan::expand_call`; a body's own names keep the
                    // project-table resolution Jinja gives them.
                    let callee = self.lookup(name)?;
                    let nb = bind_call(&callee.params, pos, kw, bound)?;
                    self.expand_parts(callee, &nb, mark, depth + 1, out, refs)?;
                }
                Part::Comment { .. } => {}
            }
            strip_next = r;
        }
        if ex.end_dash {
            let end = out.trim_end().len().max(start);
            out.truncate(end);
        }
        Some(())
    }
}

/// One node's expansion context — the dag's expandable table plus the
/// node's own root-project membership.
#[derive(Clone)]
pub(crate) struct ExpandScope {
    table: Rc<MacroTable>,
    /// The rendered node belongs to the root project — a package
    /// node's bare calls resolve against its own package, never this
    /// table.
    root: bool,
}

impl ExpandScope {
    /// The context for `uid` under `dag` — builds the table itself;
    /// `None` when the dag carries no expandable root macros (the
    /// common fast path decides every render at once). Every
    /// production caller shares a resolver's memoized table instead —
    /// this form is the tests' one-off.
    #[cfg(test)]
    pub(crate) fn for_node(dag: &Dag, uid: &str) -> Option<ExpandScope> {
        Self::with_table(Rc::new(MacroTable::for_dag(dag)), dag, uid)
    }

    /// The shared-table form — `None` on an empty table so the
    /// placeholder path runs unchanged.
    pub(crate) fn with_table(table: Rc<MacroTable>, dag: &Dag, uid: &str) -> Option<ExpandScope> {
        if table.is_empty() {
            return None;
        }
        let root = dag.get(uid).is_some_and(|n| dag.is_root_project(n));
        Some(ExpandScope { table, root })
    }

    /// May this node's renders expand — root-project only.
    pub(crate) fn enabled(&self) -> bool {
        self.root
    }

    /// The text `{{ name(args) }}` emits — see [`MacroTable::expand_call`].
    pub(crate) fn expand_call(
        &self,
        name: &str,
        args: &str,
        mark: JinjaMark,
    ) -> Option<(String, Vec<Range<usize>>)> {
        self.table.expand_call(name, args, mark)
    }
}

/// A `{% macro %}` def reduced to its provable shape — the param
/// binding spec plus the body's emittable parts.
struct Expandable {
    /// `{% macro %}` params — positions + default texts.
    params: Vec<Param>,
    /// `{% macro -%}` — eats the body's leading whitespace.
    sig_dash: bool,
    /// `{%- endmacro %}` — eats the body's trailing whitespace.
    end_dash: bool,
    parts: Vec<Part>,
}

/// One emittable piece of a macro body.
enum Part {
    /// Verbatim body text — `{# … #}` comments never reach it (they're
    /// [`Part::Comment`]s instead).
    Text(String),
    /// `{{ param }}` — the bound value emits (`l`/`r` are the tag's
    /// `-` whitespace controls).
    Expr { l: bool, r: bool, index: usize },
    /// `{{ ref(…) }}`/`{{ source(…) }}` — provable args only.
    Dep { l: bool, r: bool, call: DepCall },
    /// `{{ name(…) }}` — resolved through the table at emit (nested
    /// def ordering doesn't matter).
    Call {
        l: bool,
        r: bool,
        name: String,
        pos: Vec<Arg>,
        kw: Vec<(String, Arg)>,
    },
    /// `{# … #}` — emits nothing; whitespace control still applies.
    Comment { l: bool, r: bool },
}

impl Part {
    /// The tag's `-` whitespace controls — `(l, r)`; `Text` has none.
    fn flags(&self) -> (bool, bool) {
        match self {
            Part::Text(_) => (false, false),
            Part::Expr { l, r, .. }
            | Part::Dep { l, r, .. }
            | Part::Call { l, r, .. }
            | Part::Comment { l, r } => (*l, *r),
        }
    }
}

/// Which relation-resolving call a [`DepCall`] wraps.
#[derive(Clone, Copy)]
enum DepKind {
    Ref,
    Source,
}

/// A `ref(…)`/`source(…)` call — its rendered relation resolves at
/// emit since the args can name params.
struct DepCall {
    kind: DepKind,
    pos: Vec<Arg>,
    kw: Vec<(String, Arg)>,
}

impl DepCall {
    /// The relation the call binds — `None` when any arg can't prove
    /// (dbt evaluates them all) or the shape isn't one dbt accepts.
    fn eval(&self, bound: &[Option<BoundVal>]) -> Option<BoundVal> {
        for a in self.pos.iter().chain(self.kw.iter().map(|(_, a)| a)) {
            a.eval(bound)?;
        }
        let kw_arg = |name: &str| self.kw.iter().find(|(k, _)| k == name).map(|(_, a)| a);
        match self.kind {
            // The rendered relation is the LAST positional's name —
            // `name=` when no positional binds it. `v`/`version` pins
            // read fine but never reach the text; an unknown keyword —
            // or `name=` on top of a positional — is a compile error.
            DepKind::Ref => {
                if self.pos.len() > 2
                    || self
                        .kw
                        .iter()
                        .any(|(k, _)| !matches!(k.as_str(), "name" | "v" | "version"))
                    || (!self.pos.is_empty() && kw_arg("name").is_some())
                {
                    return None;
                }
                let name = match self.pos.last().or_else(|| kw_arg("name"))?.eval(bound)? {
                    BoundVal::Text(name) => name,
                    _ => return None,
                };
                Some(BoundVal::Ref(name))
            }
            // `source_name`/`table_name` — positionally or by keyword,
            // never both on the same slot.
            DepKind::Source => {
                let dup = self.kw.iter().any(|(k, _)| {
                    (k == "source_name" && !self.pos.is_empty())
                        || (k == "table_name" && self.pos.len() > 1)
                });
                if self.pos.len() > 2
                    || self
                        .kw
                        .iter()
                        .any(|(k, _)| !matches!(k.as_str(), "source_name" | "table_name"))
                    || dup
                {
                    return None;
                }
                let at = |i: usize, name: &str| self.pos.get(i).or_else(|| kw_arg(name));
                match (
                    at(0, "source_name")?.eval(bound)?,
                    at(1, "table_name")?.eval(bound)?,
                ) {
                    (BoundVal::Text(s), BoundVal::Text(t)) => Some(BoundVal::Source(s, t)),
                    _ => None,
                }
            }
        }
    }
}

/// A call-argument expression — provable at parse into one of dbt's
/// own binding shapes.
enum Arg {
    /// A literal string — emits verbatim (never re-read as Jinja).
    Text(String),
    /// A plain integer/decimal literal — emitted as it renders.
    Num(String),
    /// The enclosing macro's parameter at this index.
    Param(usize),
    /// `ref(…)`/`source(…)` — its own args already proven.
    Dep(DepCall),
}

impl Arg {
    /// The value the argument binds under `bound` — `None` when a
    /// `Param` slot is unfilled or a `Dep` can't prove.
    fn eval(&self, bound: &[Option<BoundVal>]) -> Option<BoundVal> {
        match self {
            Arg::Text(s) => Some(BoundVal::Text(s.clone())),
            Arg::Num(n) => Some(BoundVal::Num(n.clone())),
            Arg::Param(i) => bound.get(*i)?.clone(),
            Arg::Dep(d) => d.eval(bound),
        }
    }
}

/// A bound argument's value — strings and numbers emit their text;
/// `ref`/`source` emit the relation name the top-level render would
/// have shaped.
#[derive(Clone)]
enum BoundVal {
    Text(String),
    Num(String),
    /// `ref(…)` — one relation name.
    Ref(String),
    /// `source(…)` — the source/table pair.
    Source(String, String),
}

impl BoundVal {
    /// The emitted text — a bound string/number verbatim; a relation
    /// the quoted (or `__dbtl_`-marked under [`JinjaMark::Marked`])
    /// name form the top-level render uses.
    fn render(&self, mark: JinjaMark) -> String {
        match (self, mark) {
            (BoundVal::Text(s), _) | (BoundVal::Num(s), _) => s.clone(),
            (BoundVal::Ref(n), JinjaMark::Plain) => quoted_ident(n),
            (BoundVal::Ref(n), JinjaMark::Marked) => {
                quoted_ident(&format!("{JINJA_MARK_PREFIX}ref__{n}"))
            }
            (BoundVal::Source(a, b), JinjaMark::Plain) => {
                format!("{}.{}", quoted_ident(a), quoted_ident(b))
            }
            (BoundVal::Source(a, b), JinjaMark::Marked) => format!(
                "{}.{}",
                quoted_ident(&format!("{JINJA_MARK_PREFIX}src__{a}")),
                quoted_ident(&format!("{JINJA_MARK_PREFIX}src__{b}"))
            ),
        }
    }

    /// Stands for a relation — the emitted range joins `expanded_refs`.
    fn is_relation(&self) -> bool {
        matches!(self, BoundVal::Ref(_) | BoundVal::Source(..))
    }
}

impl Expandable {
    /// Reduce a complete `{% macro %}…{% endmacro %}` def to its
    /// emittable parts — `None` when the body holds anything a
    /// placeholder can't safely stand in for: any `{% %}` tag at all
    /// (control flow or not), a `{{ }}` that isn't a param/`ref`/
    /// `source`/nested provable call, a `ref`/`source`/call arg that
    /// isn't a literal/parameter/`ref`-`source`, or a qualified call
    /// outside the project namespace.
    fn parse(sql: &str, proj: Option<&str>) -> Option<Expandable> {
        let masked = mask_jinja_comments(sql);
        let (_, span) = leading_block(&masked)?;
        let inner = jinja_inner(&masked[span.start + 2..span.end - 2]);
        let (word, rest) = tag_word(inner);
        if word != "macro" {
            return None;
        }
        let (name, sig, tail) = leading_call(rest.trim())?;
        if !is_identifier(name) || !tail.is_empty() {
            return None;
        }
        let params = parse_params(sig);
        let sig_dash = masked[..span.end].ends_with("-%}");
        let open = span.end;
        let end_rel = find_block_word(&masked[open..], "endmacro")?;
        let end_open = open + end_rel;
        let end_dash = masked[end_open..].starts_with("{%-");

        let body = &sql[open..end_open];
        let b = body.as_bytes();
        let mut parts = Vec::new();
        let mut last = 0usize;
        let mut i = 0usize;
        while i < b.len() {
            if !(b[i] == b'{' && matches!(b.get(i + 1), Some(b'{') | Some(b'%') | Some(b'#'))) {
                i += 1;
                continue;
            }
            let kind = b[i + 1];
            let close = jinja_tag_close(b, i)?;
            let l = b.get(i + 2) == Some(&b'-');
            let r = b.get(close.wrapping_sub(1)) == Some(&b'-');
            if i > last {
                parts.push(Part::Text(body[last..i].to_string()));
            }
            match kind {
                b'#' => parts.push(Part::Comment { l, r }),
                // ANY `{% %}` — if/for/set/macro/test/call/… — leaves the
                // def unexpandable.
                b'%' => return None,
                _ => {
                    let e = jinja_inner(&body[i + 2..close]);
                    if e.is_empty() {
                        parts.push(Part::Comment { l, r });
                    } else if is_identifier(e) {
                        let index = params.iter().position(|p| p.name == e)?;
                        parts.push(Part::Expr { l, r, index });
                    } else if let Some((cname, cargs, ctail)) = leading_call(e) {
                        if !ctail.is_empty() {
                            return None;
                        }
                        // A param can never be CALLED — `{{ p(…) }}` is
                        // a method call on an unproven value.
                        let first = cname.split('.').next()?;
                        if params.iter().any(|p| p.name == first) {
                            return None;
                        }
                        match cname {
                            "ref" | "source" => {
                                let (pos, kw) = parse_args(cargs, &params)?;
                                parts.push(Part::Dep {
                                    l,
                                    r,
                                    call: DepCall {
                                        kind: if cname == "ref" {
                                            DepKind::Ref
                                        } else {
                                            DepKind::Source
                                        },
                                        pos,
                                        kw,
                                    },
                                });
                            }
                            _ => {
                                // A qualified call reaches the project
                                // table only through the project's own
                                // namespace — anything else is a
                                // package/builtin call.
                                if cname.contains('.') && cname.split('.').next() != proj {
                                    return None;
                                }
                                let (pos, kw) = parse_args(cargs, &params)?;
                                parts.push(Part::Call {
                                    l,
                                    r,
                                    name: cname.to_string(),
                                    pos,
                                    kw,
                                });
                            }
                        }
                    } else {
                        return None;
                    }
                }
            }
            i = close + 2;
            last = i;
        }
        if last < b.len() {
            parts.push(Part::Text(body[last..].to_string()));
        }
        Some(Expandable {
            params,
            sig_dash,
            end_dash,
            parts,
        })
    }
}

/// Positional + keyword arguments parsed into their [`Arg`] shapes —
/// the tuple [`parse_args`] hands the binding rules.
type CallArgs = (Vec<Arg>, Vec<(String, Arg)>);

/// `{{ (…) }}` argument list → positional + keyword [`Arg`]s, each
/// provable in the enclosing scope (`params` — the caller's own
/// params; empty at model level so a bare name never proves).
fn parse_args(args: &str, params: &[Param]) -> Option<CallArgs> {
    let mut pos = Vec::new();
    let mut kw: Vec<(String, Arg)> = Vec::new();
    for seg in top_level_segments(args) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        match split_kwarg(seg) {
            Some((name, expr)) => {
                // `m(x=1, x=2)` is a template-parse error in Jinja
                // (Python's "keyword argument repeated") — refuse
                // rather than last-wins a call the compile rejects.
                if kw.iter().any(|(n, _)| n == name) {
                    return None;
                }
                kw.push((name.to_string(), parse_arg(expr, params)?));
            }
            None => pos.push(parse_arg(seg, params)?),
        }
    }
    Some((pos, kw))
}

/// One argument expression → its [`Arg`] shape — `params` is the
/// ENCLOSING scope's params (a macro body's own signature; empty for
/// a model-level call).
fn parse_arg(e: &str, params: &[Param]) -> Option<Arg> {
    let e = e.trim();
    if let Some(i) = params.iter().position(|p| p.name == e) {
        return Some(Arg::Param(i));
    }
    match lit_value(e) {
        Some(Value::String(s)) => return Some(Arg::Text(s)),
        Some(v @ Value::Number(_)) if is_plain_number(e) => return Some(Arg::Num(v.to_string())),
        Some(_) => return None,
        None => {}
    }
    let (name, args, tail) = leading_call(e)?;
    if !tail.is_empty() || !matches!(name, "ref" | "source") {
        return None;
    }
    let (pos, kw) = parse_args(args, params)?;
    Some(Arg::Dep(DepCall {
        kind: if name == "ref" {
            DepKind::Ref
        } else {
            DepKind::Source
        },
        pos,
        kw,
    }))
}

/// A plain integer/decimal the arg slot takes — no `e`/`E` exponent:
/// `1e3` is a valid literal to Jinja's parser but renders `1000.0`
/// where the SQL almost surely meant something else, so it never
/// proves.
fn is_plain_number(e: &str) -> bool {
    !e.is_empty()
        && e.bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'+' | b'-'))
}

/// dbt's call rules on `pos`/`kw` against `params`, then the resolved
/// frame — `None` on a shape dbt would compile-error (duplicate,
/// extra, missing-after-default) or a required param with no binding
/// (its `Undefined` interpolation isn't provable text).
fn bind_call(
    params: &[Param],
    pos: &[Arg],
    kw: &[(String, Arg)],
    frame: &[Option<BoundVal>],
) -> Option<Vec<Option<BoundVal>>> {
    if !call_shape_valid(params, pos, kw) {
        return None;
    }
    let slots = bind_slots(params, pos, kw);
    let mut bound: Vec<Option<BoundVal>> = vec![None; params.len()];
    for (i, slot) in slots.iter().enumerate() {
        if let Some(SlotSource::Arg(a)) = slot {
            bound[i] = Some(a.eval(frame)?);
        }
    }
    // Defaults evaluate inside the callee frame, left to right — a
    // `b=a` default reads the slots bound before it.
    for (i, slot) in slots.iter().enumerate() {
        if let Some(SlotSource::Default(d)) = slot {
            bound[i] = Some(parse_arg(d, params)?.eval(&bound)?);
        }
    }
    if bound.iter().any(Option::is_none) {
        return None;
    }
    Some(bound)
}
