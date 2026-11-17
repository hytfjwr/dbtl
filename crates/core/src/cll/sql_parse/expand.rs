//! Provable root-project `{% macro %}` expansion.
//!
//! A model file's `{{ name(args) }}` — where `name` resolves to a root
//! `{% macro %}` def whose body is literal text, provable `{{ … }}`
//! emissions, and statically provable control flow (`{% set %}`,
//! `{% if %}`/`{% elif %}`/`{% else %}`, `{% for %}`/`{% else %}`) —
//! renders the same text dbt emits instead of the generic placeholder.
//! The whole shape has to prove statically: argument binding follows
//! dbt's own call rules (positional-or-keyword, defaults evaluated in
//! the callee frame left to right, no duplicates or extras), every
//! expression the body reaches must evaluate under the render context
//! ([`eval_render`] — frame names, literals, `ref`/`source`/`range`,
//! the shared operators, `| join`/`| length`, `is` tests), and a
//! missing required arg is a dbt error — all leave the tag to the
//! placeholder path. Non-root defs, qualified calls outside the project
//! namespace, unsupported tags or expressions, a loop past the
//! iteration cap, output past the size cap, and chains past the depth
//! cap do too.
//!
//! Expansion runs only for root-project NODES — a package node's bare
//! calls resolve against its own package's namespace, not the root's —
//! and only where a `{{ }}` is a clean token (top level, unglued from
//! identifier text): inside a string or comment the tag still renders
//! positionally.
//!
//! Scope rules follow Jinja: `{% set %}` writes the innermost frame —
//! an `{% if %}` arm shares its enclosing frame (the binding leaks),
//! a `{% for %}` body and its `{% else %}` arm each run in a fresh
//! frame (a `{% set %}` there never persists), `loop` binds the
//! post-filter sequence metadata, and a `for...if` filter sees the
//! OUTER loop context. A macro call's frame is just its parameters —
//! the caller's names never reach it.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use super::jinja::{jinja_inner, quoted_ident, JinjaMark, JINJA_MARK_PREFIX};
use super::withhold::{find_block_word, leading_block, leading_call};
use crate::dag::Dag;
use crate::source::{
    bind_slots, call_shape_valid, eval_render, is_identifier, jinja_tag_close, mask_jinja_comments,
    parse_params, py_float_repr, split_assign, split_kwarg, tag_word, top_level_segments, truthy,
    ArgVals, LoopInfo, Param, Relation, SlotSource, Val, MAX_MACRO_DEPTH,
};

/// Loop iterations one expansion runs before giving up — a `{% for %}`
/// over a 100k `range` or a pathological nest can't run the render
/// unbounded.
const MAX_ITERS: usize = 10_000;

/// Rendered bytes one expansion emits before giving up — the same
/// order as the model render's own headroom.
const OUT_CAP: usize = 64 * 1024;

/// `{% if %}`/`{% for %}` nesting inside one body — past this the def
/// stays a placeholder (Jinja has no such limit; this one bounds the
/// executor's recursion).
const MAX_NEST: usize = 64;

/// The `{% word %}` openers needing a matching `{% end… %}` that leave
/// a def unexpandable wherever they sit — one-line unprovable tags go
/// in the tree instead (see [`Node::Unsupported`]).
const BLOCK_WORDS: &[&str] = &[
    "autoescape",
    "block",
    "call",
    "docs",
    "filter",
    "macro",
    "materialization",
    "pluralize",
    "raw",
    "snapshot",
    "test",
    "trans",
    "with",
];

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
    /// [`Expandable::parse`] (unsupported tags, unbalanced blocks,
    /// unprovable shapes) aren't in the table, so every lookup leaves
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
            if let Some(ex) = Expandable::parse(sql) {
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
    /// expandable, an arg dbt's call rules reject or can't prove, a
    /// body whose eval can't prove, or a nested chain the depth cap
    /// cuts.
    pub(crate) fn expand_call(
        &self,
        name: &str,
        args: &str,
        mark: JinjaMark,
    ) -> Option<(String, Vec<Range<usize>>)> {
        let ex = self.lookup(name)?;
        let (pos, kw) = eval_args(args, &[], true)?;
        let frame = bind_macro(&ex.params, pos, kw)?;
        let mut eval = Eval {
            table: self,
            mark,
            depth: 0,
            names: vec![frame],
            iters: 0,
            out: String::new(),
            refs: Vec::new(),
        };
        eval.run(&ex.body)?;
        Some((eval.out, eval.refs))
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
/// binding rules plus the body's node tree.
struct Expandable {
    /// `{% macro %}` params — positions + default texts.
    params: Vec<Param>,
    /// The body's executable nodes — `{% macro -%}`/`{%- endmacro %}`
    /// whitespace control already folded into the edge text.
    body: Vec<Node>,
}

/// One executable piece of a macro body — `{{ }}`/`{% %}` at any
/// depth; `{# #}` comments never make it here (they emit nothing).
enum Node {
    /// Verbatim template text — whitespace-control trims applied at
    /// parse.
    Text(String),
    /// `{{ expr }}` — evaluated under the render context and emitted.
    Expr(String),
    /// `{% set a[, b, …] = expr %}` — binds the innermost frame.
    Set { targets: Vec<String>, expr: String },
    /// `{% if %}`/`{% elif %}`/`{% else %}` — arms in order, then the
    /// `else` arm's nodes (empty when the chain has none).
    If {
        arms: Vec<(String, Vec<Node>)>,
        else_: Vec<Node>,
    },
    /// `{% for x in seq [if cond] %}` — `else_` runs on an empty
    /// post-filter sequence.
    For {
        targets: Vec<String>,
        iter: String,
        filter: Option<String>,
        body: Vec<Node>,
        else_: Vec<Node>,
    },
    /// A one-line tag the executor can't run (`{% do %}`/`{% include %}`/
    /// `{% import %}`/`{% from %}`/`{% break %}`/`{% continue %}`/…).
    /// It enters the tree — the def stays expandable while the tag
    /// sits in an arm never taken — and fails only on execution.
    Unsupported,
}

impl Expandable {
    /// Reduce a complete `{% macro %}…{% endmacro %}` def to its node
    /// tree — `None` when the body holds anything a placeholder can't
    /// safely stand in for: an unsupported `{% %}` tag, an unbalanced
    /// block, a `{% set %}`/`{% for %}` header that doesn't parse, or
    /// nesting past [`MAX_NEST`]. Expressions aren't checked here —
    /// the render context judges them at expansion.
    fn parse(sql: &str) -> Option<Expandable> {
        let masked = mask_jinja_comments(sql);
        let (_, span) = leading_block(&masked)?;
        let inner = tag_word(&masked[span.start + 2..span.end - 2]);
        let (word, rest) = inner;
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

        let mut body = build_body(&sql[open..end_open])?;
        if sig_dash {
            if let Some(Node::Text(t)) = body.first_mut() {
                *t = t.trim_start().to_string();
            }
        }
        if end_dash {
            if let Some(Node::Text(t)) = body.last_mut() {
                *t = t.trim_end().to_string();
            }
        }
        Some(Expandable { params, body })
    }
}

// -- body parsing -------------------------------------------------------------

/// One block level under construction — the `if`/`for` arms collected
/// so far; arm 0 carries the opener's condition (an `if`/`elif`) or
/// `None` (a `for` body).
struct Frame {
    kind: FrameKind,
    arms: Vec<Arm>,
}

/// What a [`Frame`] closes into.
enum FrameKind {
    /// `{% if %}` — arms hold `(cond, nodes)`; the `else` arm's cond
    /// is `None`.
    If,
    /// `{% for %}` — arm 0 is the loop body, arm 1 the `else`.
    For {
        targets: Vec<String>,
        iter: String,
        filter: Option<String>,
    },
}

/// One arm's nodes under a [`Frame`].
struct Arm {
    cond: Option<String>,
    nodes: Vec<Node>,
}

/// The body-tree builder — the node stack plus the pending
/// whitespace-control strip a `-%}`/`#}`/`}}` close asks the next text
/// node to apply.
struct Builder {
    root: Vec<Node>,
    stack: Vec<Frame>,
    /// The last tag's right-side `-` — only the text run directly
    /// after it left-trims; a tag in between voids the strip.
    strip: bool,
}

impl Builder {
    /// The node list the current position appends to — the innermost
    /// open arm's, else the body's.
    fn cur(&mut self) -> &mut Vec<Node> {
        match self.stack.last_mut() {
            Some(f) => &mut f.arms.last_mut().expect("frame holds an arm").nodes,
            None => &mut self.root,
        }
    }

    /// Template text between tags — the pending right-side strip
    /// applies to its head; dashes on the NEXT tag trim its tail. The
    /// strip reaches only the text directly after the tag — an empty
    /// run still consumes it, so a `-` can't reach across another tag
    /// (`{{ x -}}{{- y }} z` renders `xy z`, not `xyz`).
    fn text(&mut self, s: &str) {
        let s = if std::mem::take(&mut self.strip) {
            s.trim_start()
        } else {
            s
        };
        if s.is_empty() {
            return;
        }
        self.cur().push(Node::Text(s.to_string()));
    }

    /// A tag's left-side `-` — eats the trailing whitespace of the
    /// template text just emitted (a generated emission is never
    /// trimmed — only template text nodes are).
    fn ltrim(&mut self) {
        if let Some(Node::Text(t)) = self.cur().last_mut() {
            *t = t.trim_end().to_string();
        }
    }

    /// A `{% … %}` tag — routes to the block machinery or rejects the
    /// word outright (any tag outside `set`/`if`/`elif`/`else`/`endif`/
    /// `for`/`endfor` leaves the def unexpandable).
    fn tag(&mut self, inner: &str) -> Option<()> {
        let (word, rest) = tag_word(inner);
        match word {
            "set" => {
                let (lhs, expr) = split_assign(rest)?;
                let expr = expr.trim();
                if expr.is_empty() {
                    return None;
                }
                self.cur().push(Node::Set {
                    targets: target_names(lhs)?,
                    expr: expr.to_string(),
                });
            }
            "if" => {
                let cond = rest.trim();
                if cond.is_empty() || self.stack.len() >= MAX_NEST {
                    return None;
                }
                self.stack.push(Frame {
                    kind: FrameKind::If,
                    arms: vec![Arm {
                        cond: Some(cond.to_string()),
                        nodes: Vec::new(),
                    }],
                });
            }
            "elif" => {
                let cond = rest.trim();
                let f = self.stack.last_mut()?;
                if cond.is_empty()
                    || !matches!(f.kind, FrameKind::If)
                    || f.arms.last().is_some_and(|a| a.cond.is_none())
                {
                    return None;
                }
                f.arms.push(Arm {
                    cond: Some(cond.to_string()),
                    nodes: Vec::new(),
                });
            }
            "else" => {
                if !rest.trim().is_empty() {
                    return None;
                }
                let f = self.stack.last_mut()?;
                // One `else` per block, and it must be the last arm —
                // after every `if`/`elif` arm, or after the `for` body
                // (whose arm carries `cond: None` by construction).
                let reject = match f.kind {
                    FrameKind::If => f.arms.last().is_some_and(|a| a.cond.is_none()),
                    FrameKind::For { .. } => f.arms.len() > 1,
                };
                if reject {
                    return None;
                }
                f.arms.push(Arm {
                    cond: None,
                    nodes: Vec::new(),
                });
            }
            "endif" => {
                if !rest.trim().is_empty() {
                    return None;
                }
                let f = self.stack.pop()?;
                if !matches!(f.kind, FrameKind::If) {
                    return None;
                }
                let mut arms = f.arms;
                let else_ = if arms.last().is_some_and(|a| a.cond.is_none()) {
                    arms.pop().expect("else arm").nodes
                } else {
                    Vec::new()
                };
                let arms = arms
                    .into_iter()
                    .map(|a| a.cond.map(|c| (c, a.nodes)))
                    .collect::<Option<Vec<_>>>()?;
                self.cur().push(Node::If { arms, else_ });
            }
            "for" => {
                let (targets, iter, filter) = for_header(rest)?;
                if self.stack.len() >= MAX_NEST {
                    return None;
                }
                self.stack.push(Frame {
                    kind: FrameKind::For {
                        targets,
                        iter,
                        filter,
                    },
                    arms: vec![Arm {
                        cond: None,
                        nodes: Vec::new(),
                    }],
                });
            }
            "endfor" => {
                if !rest.trim().is_empty() {
                    return None;
                }
                let f = self.stack.pop()?;
                let FrameKind::For {
                    targets,
                    iter,
                    filter,
                } = f.kind
                else {
                    return None;
                };
                let mut arms = f.arms.into_iter();
                let body = arms.next()?.nodes;
                let else_ = arms.next().map(|a| a.nodes).unwrap_or_default();
                self.cur().push(Node::For {
                    targets,
                    iter,
                    filter,
                    body,
                    else_,
                });
            }
            // A block tag needing a matching `{% end… %}` — `{% call %}`,
            // `{% filter %}`, `{% raw %}`, a nested `{% macro %}`, the
            // `{% set %}…{% endset %}` block form, `{% materialization %}`,
            // `{% test %}`, `{% docs %}`, `{% snapshot %}`, and Jinja's
            // `block`/`with`/`autoescape`/`trans`/`pluralize`/`debug` —
            // leaves the whole DEF unexpandable wherever it sits.
            _ if BLOCK_WORDS.contains(&word) => return None,
            // Any other tag — `{% do %}`/`{% include %}`/`{% break %}`/
            // `{% continue %}`/`{% endset %}`/a stray ender — enters the
            // tree and refuses only if an arm reaches it.
            _ => self.cur().push(Node::Unsupported),
        }
        Some(())
    }
}

/// The `{% … %}`/`{{ … }}`/`{# … #}` token stream → a node tree.
/// `None` on an unterminated tag, an unsupported `{% %}` word, or an
/// unbalanced block (an `end` with nothing open, a leftover frame at
/// EOF). Whitespace-control markers trim TEMPLATE text only — the
/// left `-` eats the previous text node's tail, the right `-` the next
/// one's head.
fn build_body(body: &str) -> Option<Vec<Node>> {
    let b = body.as_bytes();
    let mut bld = Builder {
        root: Vec::new(),
        stack: Vec::new(),
        strip: false,
    };
    let (mut last, mut i) = (0usize, 0usize);
    while i < b.len() {
        if !(b[i] == b'{' && matches!(b.get(i + 1), Some(b'{') | Some(b'%') | Some(b'#'))) {
            i += 1;
            continue;
        }
        let kind = b[i + 1];
        let close = jinja_tag_close(b, i)?;
        let l = b.get(i + 2) == Some(&b'-');
        let r = b.get(close.wrapping_sub(1)) == Some(&b'-');
        bld.text(&body[last..i]);
        if l {
            bld.ltrim();
        }
        match kind {
            // `{# … #}` emits nothing — its dashes still trim.
            b'#' => {}
            b'{' => {
                let expr = jinja_inner(&body[i + 2..close]);
                if expr.is_empty() {
                    return None;
                }
                bld.cur().push(Node::Expr(expr.to_string()));
            }
            _ => bld.tag(&body[i + 2..close])?,
        }
        if r {
            bld.strip = true;
        }
        i = close + 2;
        last = i;
    }
    bld.text(&body[last..]);
    if !bld.stack.is_empty() {
        return None;
    }
    Some(bld.root)
}

/// A `{% set %}`/`{% for %}` target list — `x`, `x, y`, `(x, y)` — as
/// bare identifiers. Anything else (an expression target, a nested
/// tuple, an empty name) is a shape the bind can't prove.
fn target_names(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    let s = s
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(s);
    let names: Vec<String> = top_level_segments(s)
        .iter()
        .map(|n| n.trim().to_string())
        .collect();
    (!names.is_empty() && names.iter().all(|n| is_identifier(n))).then_some(names)
}

/// `x in seq [if cond]` — a `{% for %}` header. `in`/`if` split only
/// at bracket depth 0 outside quotes (a `for x in f(a if b)`-style
/// arg can't fake the filter boundary).
fn for_header(rest: &str) -> Option<(Vec<String>, String, Option<String>)> {
    let (target, iter_part) = split_top_word(rest, "in")?;
    let targets = target_names(target)?;
    let (iter, filter) = match split_top_word(iter_part, "if") {
        Some((i, f)) => (i, Some(f)),
        None => (iter_part, None),
    };
    let iter = iter.trim();
    let filter = filter.map(str::trim);
    if iter.is_empty() || filter.is_some_and(str::is_empty) {
        return None;
    }
    Some((targets, iter.to_string(), filter.map(str::to_string)))
}

/// The first `w` standing alone at bracket depth 0 outside quotes —
/// `for`'s `in`/`if` separators. A word char on either side means it's
/// a longer identifier, not the keyword.
fn split_top_word<'a>(s: &'a str, w: &str) -> Option<(&'a str, &'a str)> {
    let b = s.as_bytes();
    let (mut depth, mut quote) = (0usize, 0u8);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') if quote == 0 => quote = q,
            _ if quote != 0 => {
                if b[i] == b'\\' {
                    i += 1;
                } else if b[i] == quote {
                    quote = 0;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && s[i..].starts_with(w) => {
                let word_char = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
                let before_ok = i == 0 || !word_char(b[i - 1]);
                let after = i + w.len();
                let after_ok = after >= b.len() || !word_char(b[after]);
                if before_ok && after_ok {
                    return Some((s[..i].trim(), s[after..].trim()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// -- expansion -----------------------------------------------------------------

/// `{{ … }}` argument list → positional + keyword [`Val`]s — each
/// expression evaluated in the CALLER's frame (empty at model level,
/// where `literals_only` narrows args to plain literals and
/// `ref`/`source` calls).
fn eval_args(args: &str, frame: &[HashMap<String, Val>], literals_only: bool) -> Option<ArgVals> {
    let mut pos = Vec::new();
    let mut kw: Vec<(String, Val)> = Vec::new();
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
                kw.push((name.to_string(), eval_render(expr, frame, literals_only)?));
            }
            None => {
                // `m(x=1, y)` — a positional after a keyword — is
                // Python's "positional argument follows keyword
                // argument" SyntaxError, the same refusal.
                if !kw.is_empty() {
                    return None;
                }
                pos.push(eval_render(seg, frame, literals_only)?);
            }
        }
    }
    Some((pos, kw))
}

/// dbt's call rules on `pos`/`kw` against `params`, then the callee's
/// frame — `None` on a shape dbt would compile-error (duplicate,
/// extra, missing-after-default) or a required param with no binding
/// (its `Undefined` interpolation isn't provable text). Defaults
/// evaluate inside the callee frame left to right — a `b=a` default
/// reads the params bound before it.
fn bind_macro(
    params: &[Param],
    pos: Vec<Val>,
    kw: Vec<(String, Val)>,
) -> Option<HashMap<String, Val>> {
    if !call_shape_valid(params, &pos, &kw) {
        return None;
    }
    let slots = bind_slots(params, &pos, &kw);
    let mut frame = HashMap::new();
    for (i, slot) in slots.iter().enumerate() {
        let v = match slot {
            Some(SlotSource::Arg(v)) => (*v).clone(),
            Some(SlotSource::Default(d)) => eval_render(d, std::slice::from_ref(&frame), false)?,
            None => return None,
        };
        frame.insert(params[i].name.clone(), v);
    }
    Some(frame)
}

/// `a, b = seq` / `for a, b in seq` — one name takes the value whole;
/// several destructure an iterable of matching length (a string gives
/// its chars, a dict its keys — Python's own rules).
fn bind_into(frame: &mut HashMap<String, Val>, targets: &[String], v: Val) -> Option<()> {
    let [single] = targets else {
        let seq = v.iterate()?;
        if seq.len() != targets.len() {
            return None;
        }
        for (i, t) in targets.iter().enumerate() {
            frame.insert(t.clone(), seq.item(i));
        }
        return Some(());
    };
    frame.insert(single.clone(), v);
    Some(())
}

/// One expansion — the bound frames (`names`, innermost last), the
/// loop/output budgets, the rendered text and the relation ranges it
/// carries.
struct Eval<'a> {
    table: &'a MacroTable,
    mark: JinjaMark,
    /// Nested `{{ macro(…) }}` depth — [`MAX_MACRO_DEPTH`]-bounded.
    depth: usize,
    /// The frame stack — a macro call starts it at just its params;
    /// `{% for %}` iterations and `for...else` push one each.
    names: Vec<HashMap<String, Val>>,
    /// Loop items iterated so far — [`MAX_ITERS`]-bounded.
    iters: usize,
    out: String,
    /// Ranges of `out` standing for a `ref`/`source` relation.
    refs: Vec<Range<usize>>,
}

impl Eval<'_> {
    /// Run `nodes` — `None` on anything the render can't prove.
    fn run(&mut self, nodes: &[Node]) -> Option<()> {
        for n in nodes {
            match n {
                Node::Text(t) => self.push(t)?,
                Node::Expr(e) => self.emit_expr(e)?,
                Node::Set { targets, expr } => {
                    let v = eval_render(expr, &self.names, false)?;
                    // A `ref()`/`source()` relation emits or passes
                    // through a call arg — it can't live in a `set`
                    // binding (dbt binds the object, not the text).
                    if matches!(v, Val::Relation(_)) {
                        return None;
                    }
                    bind_into(self.names.last_mut()?, targets, v)?;
                }
                Node::If { arms, else_ } => {
                    let mut taken = else_;
                    for (cond, nodes) in arms {
                        if truthy(&eval_render(cond, &self.names, false)?)? {
                            taken = nodes;
                            break;
                        }
                    }
                    // An `if` arm shares the enclosing frame — a `set`
                    // inside leaks outward, like Jinja.
                    self.run(taken)?;
                }
                Node::For {
                    targets,
                    iter,
                    filter,
                    body,
                    else_,
                } => self.run_for(targets, iter, filter.as_deref(), body, else_)?,
                Node::Unsupported => return None,
            }
        }
        Some(())
    }

    /// A `{% for %}` — each item iterates in a fresh frame holding the
    /// target bindings and `loop` (post-filter metadata, so
    /// `loop.index`/`loop.length` count what runs). The `else` arm —
    /// and the filter probe — get their own frames: a `set` inside
    /// either doesn't persist, and `loop` in a filter reads the OUTER
    /// context like Jinja.
    fn run_for(
        &mut self,
        targets: &[String],
        iter: &str,
        filter: Option<&str>,
        body: &[Node],
        else_: &[Node],
    ) -> Option<()> {
        let seq = eval_render(iter, &self.names, false)?.iterate()?;
        let mut kept = Vec::new();
        for i in 0..seq.len() {
            self.iters += 1;
            if self.iters > MAX_ITERS {
                return None;
            }
            let item = seq.item(i);
            match filter {
                Some(f) => {
                    let mut probe = HashMap::new();
                    bind_into(&mut probe, targets, item.clone())?;
                    self.names.push(probe);
                    let keep = eval_render(f, &self.names, false).and_then(|v| truthy(&v));
                    self.names.pop();
                    match keep {
                        Some(true) => kept.push(item),
                        Some(false) => {}
                        None => return None,
                    }
                }
                None => kept.push(item),
            }
        }
        if kept.is_empty() {
            self.names.push(HashMap::new());
            let r = self.run(else_);
            self.names.pop();
            return r;
        }
        let len = kept.len();
        for (index0, item) in kept.into_iter().enumerate() {
            let mut frame =
                HashMap::from([("loop".to_string(), Val::Loop(LoopInfo { index0, len }))]);
            // The target names bind AFTER `loop` — a `{% for loop in
            // … %}` target shadows the LoopContext like a normal name.
            bind_into(&mut frame, targets, item)?;
            self.names.push(frame);
            let r = self.run(body);
            self.names.pop();
            r?;
        }
        Some(())
    }

    /// `{{ expr }}` — a whole-expression `name(…)` resolves through
    /// the macro table first (a frame name holding a value is never
    /// callable — Jinja's "not a callable" error); anything else the
    /// render context evaluates and the emitter prints.
    fn emit_expr(&mut self, e: &str) -> Option<()> {
        if let Some((name, args, tail)) = leading_call(e) {
            if tail.is_empty() {
                let head = name.split('.').next().unwrap_or(name);
                if self.bound(head) {
                    return None;
                }
                if self.table.lookup(name).is_some() {
                    return self.expand_nested(name, args);
                }
            }
        }
        let v = eval_render(e, &self.names, false)?;
        self.emit_val(&v)
    }

    /// A nested `{{ name(…) }}` — the callee's args evaluate in THIS
    /// frame, its body runs in a fresh stack holding just its bound
    /// params (a macro sees its own names, never the caller's).
    fn expand_nested(&mut self, name: &str, args: &str) -> Option<()> {
        let ex = self.table.lookup(name)?;
        let (pos, kw) = eval_args(args, &self.names, false)?;
        let frame = bind_macro(&ex.params, pos, kw)?;
        if self.depth >= MAX_MACRO_DEPTH {
            return None;
        }
        self.depth += 1;
        let saved = std::mem::replace(&mut self.names, vec![frame]);
        let r = self.run(&ex.body);
        self.names = saved;
        self.depth -= 1;
        r
    }

    /// A frame name resolves — the innermost scope wins.
    fn bound(&self, name: &str) -> bool {
        self.names.iter().rev().any(|f| f.contains_key(name))
    }

    /// Append rendered text under the output cap.
    fn push(&mut self, s: &str) -> Option<()> {
        if self.out.len() + s.len() > OUT_CAP {
            return None;
        }
        self.out.push_str(s);
        Some(())
    }

    /// Emit a value — a relation prints its quoted (or `__dbtl_`-
    /// marked) name and records the range; a scalar prints its Python
    /// text. A container, `loop`, or `Undefined` has no provable text.
    fn emit_val(&mut self, v: &Val) -> Option<()> {
        match v {
            Val::Relation(r) => {
                let t = relation_text(r, self.mark);
                let from = self.out.len();
                self.push(&t)?;
                self.refs.push(from..self.out.len());
                Some(())
            }
            _ => self.push(&val_text(v)?),
        }
    }
}

/// `{{ }}`'s text for a scalar — a string verbatim, an int in decimal,
/// a float only through the literal spelling that already IS Python's
/// `repr` (`1.5`, `2.0`: `1.50`/`1e3`/`0.00001` stay unexpanded even
/// though Jinja would print `1.5`/`1000.0`/`1e-05`). A bool, `none`,
/// sequence, dict, `range`, or `loop` prints Python notation — text
/// that needn't read as SQL — so it never emits.
fn val_text(v: &Val) -> Option<String> {
    use serde_json::Value as J;
    match v {
        Val::V(J::String(s)) => Some(s.clone()),
        Val::V(J::Number(n)) if n.is_f64() => Some(py_float_repr(n.as_f64()?)),
        Val::V(J::Number(n)) => Some(n.to_string()),
        Val::Float(_, canon) => canon.clone(),
        _ => None,
    }
}

/// A bound `ref`/`source` emission — the quoted name the top-level
/// render would shape, `__dbtl_`-marked under [`JinjaMark::Marked`].
fn relation_text(r: &Relation, mark: JinjaMark) -> String {
    match (r, mark) {
        (Relation::Ref(n), JinjaMark::Plain) => quoted_ident(n),
        (Relation::Ref(n), JinjaMark::Marked) => {
            quoted_ident(&format!("{JINJA_MARK_PREFIX}ref__{n}"))
        }
        (Relation::Source(a, b), JinjaMark::Plain) => {
            format!("{}.{}", quoted_ident(a), quoted_ident(b))
        }
        (Relation::Source(a, b), JinjaMark::Marked) => format!(
            "{}.{}",
            quoted_ident(&format!("{JINJA_MARK_PREFIX}src__{a}")),
            quoted_ident(&format!("{JINJA_MARK_PREFIX}src__{b}"))
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `macro.t.<name>` table — `defs` are `(name, full_def)` pairs;
    /// a def that fails [`Expandable::parse`] drops out exactly like
    /// [`MacroTable::for_dag`]'s filter.
    fn table(defs: &[(&str, &str)]) -> MacroTable {
        MacroTable {
            ns: Some("t".to_string()),
            defs: defs
                .iter()
                .filter_map(|(name, sql)| {
                    Expandable::parse(sql).map(|ex| (format!("macro.t.{name}"), ex))
                })
                .collect(),
        }
    }

    /// `{{ name(args) }}` under `table` → the emitted text.
    fn render(t: &MacroTable, name: &str, args: &str) -> Option<String> {
        t.expand_call(name, args, JinjaMark::Plain).map(|(s, _)| s)
    }

    /// `{{ name(args) }}` under `table` → `(text, relation ranges)`.
    fn render_refs(t: &MacroTable, name: &str, args: &str) -> Option<(String, Vec<Range<usize>>)> {
        t.expand_call(name, args, JinjaMark::Plain)
    }

    /// The driving example — `set`/`if`/`for`/`loop.last` producing
    /// both an `IN` list and a whole query, verbatim.
    const STATUS_CODES: &str = concat!(
        "{% macro status_codes(as_rows=false) -%}\n",
        "    {%- set codes = ['a1', 'b2', 'c3'] -%}\n",
        "    {%- if as_rows %}\n",
        "        {%- for code in codes %}\n",
        "            SELECT '{{ code }}' AS code\n",
        "            {%- if not loop.last %}\n",
        "                UNION ALL\n",
        "            {%- endif %}\n",
        "        {%- endfor %}\n",
        "    {%- else -%}\n",
        "        (\n",
        "            {%- for code in codes %}\n",
        "                '{{ code }}'{% if not loop.last %},{% endif %}\n",
        "            {%- endfor %}\n",
        "        )\n",
        "    {%- endif %}\n",
        "{%- endmacro %}"
    );

    #[test]
    fn the_status_codes_body_expands_byte_for_byte() {
        // The `IN`-list and UNION-ALL forms, exactly as a real compile
        // emits them (verified against `compiled_code` from dbt-core
        // 1.12.7, whitespace included).
        let t = table(&[("status_codes", STATUS_CODES)]);
        assert_eq!(
            render(&t, "status_codes", ""),
            Some(
                "(\n                'a1',\n                'b2',\n                'c3'\n        )"
                    .to_string()
            )
        );
        let rows = "\n            SELECT 'a1' AS code\n                UNION ALL\n            SELECT 'b2' AS code\n                UNION ALL\n            SELECT 'c3' AS code";
        assert_eq!(
            render(&t, "status_codes", "as_rows=true"),
            Some(rows.to_string())
        );
        // …and the positional form binds the same slot.
        assert_eq!(render(&t, "status_codes", "true"), Some(rows.to_string()));
    }

    #[test]
    fn control_flow_expands_with_jinja_semantics() {
        for (name, def, want) in [
            // `for` over an empty sequence runs the `else` arm …
            (
                "empty_else",
                "{% macro m() %}{% for x in [] %}{{ x }}{% else %}none{% endfor %}{% endmacro %}",
                "none",
            ),
            // … but not over a kept one.
            (
                "kept_skips_else",
                "{% macro m() %}{% for x in [1] %}{{ x }}{% else %}none{% endfor %}{% endmacro %}",
                "1",
            ),
            // `for … if` — `loop` metadata counts the post-filter seq
            // (dbt-verified: `range(5) if i is odd` keeps 2 items).
            (
                "filtered",
                "{% macro m() %}{% for i in range(5) if i is odd %}{{ loop.index }}/{{ loop.length }}{% if not loop.last %},{% endif %}{% endfor %}{% endmacro %}",
                "1/2,2/2",
            ),
            // A filter that empties the seq runs `else`.
            (
                "filtered_out",
                "{% macro m() %}{% for i in [1, 2] if i > 9 %}{{ i }}{% else %}none{% endfor %}{% endmacro %}",
                "none",
            ),
            // `loop` rebinds per nest — the INNER `loop.last` bounds
            // the comma.
            (
                "nested_loop",
                "{% macro m() %}{% for a in ['x','y'] %}{% for b in [1,2] %}{{ a }}{{ b }}{% if not loop.last %},{% endif %}{% endfor %};{% endfor %}{% endmacro %}",
                "x1,x2;y1,y2;",
            ),
            // `revindex`/`revindex0`/`first` count the same way.
            (
                "revindex",
                "{% macro m() %}{% for i in [7,8] %}{{ loop.index }}{{ loop.index0 }}{{ loop.revindex }}{{ loop.revindex0 }}{% if loop.first %}F{% endif %}{% if loop.last %}L{% endif %};{% endfor %}{% endmacro %}",
                "1021F;2110L;",
            ),
            // An `if`-arm `{% set %}` leaks into the enclosing frame.
            (
                "if_set_leaks",
                "{% macro m() %}{% if true %}{% set k = 'v' %}{% endif %}{{ k }}{% endmacro %}",
                "v",
            ),
            // A `for` body's set stays inside its iteration's frame —
            // each pass starts from the enclosing frame (`g` reads `z`
            // again, never the previous pass's `1`), and nothing leaks
            // out of the loop.
            (
                "loop_set_resets",
                "{% macro m() %}{% set g = 'z' %}{% for i in [1,2] %}{% if g == 'z' %}Z{% set g = i %}{% else %}{{ g }}{% endif %}{% endfor %}{% endmacro %}",
                "ZZ",
            ),
            // `elif` chains take the first true arm.
            (
                "elif",
                "{% macro m(n) %}{% if n == 1 %}a{% elif n == 2 %}b{% else %}c{% endif %}{% endmacro %}",
                "b",
            ),
            // `is none`, `in`, `range`, `| join`, `| length`.
            (
                "is_none",
                "{% macro m() %}{% set x = none %}{% if x is none %}n{% endif %}{% endmacro %}",
                "n",
            ),
            (
                "in_list",
                "{% macro m() %}{% if 'a' in ['a','b'] %}y{% endif %}{% endmacro %}",
                "y",
            ),
            (
                "in_range",
                "{% macro m() %}{% if 1 in range(3) %}y{% endif %}{% endmacro %}",
                "y",
            ),
            (
                "range_args",
                "{% macro m() %}{% for i in range(1, 4) %}{{ i }}{% endfor %}{% endmacro %}",
                "123",
            ),
            (
                "range_step",
                "{% macro m() %}{% for i in range(10, 0, -5) %}{{ i }},{% endfor %}{% endmacro %}",
                "10,5,",
            ),
            (
                "join_sep",
                "{% macro m() %}{{ ['a','b'] | join(', ') }}{% endmacro %}",
                "a, b",
            ),
            (
                "join_default",
                "{% macro m() %}{{ ['a','b'] | join }}{% endmacro %}",
                "ab",
            ),
            // `| length` — code points on a string, elements elsewhere.
            (
                "len_str",
                "{% macro m() %}{{ 'é日' | length }}{% endmacro %}",
                "2",
            ),
            (
                "len_list",
                "{% macro m() %}{{ [1,2] | length }}{% endmacro %}",
                "2",
            ),
            (
                "len_range",
                "{% macro m() %}{{ range(3) | length }}{% endmacro %}",
                "3",
            ),
            (
                "len_dict",
                "{% macro m() %}{{ {'a':1} | length }}{% endmacro %}",
                "1",
            ),
            // `{% set a, b = %}` destructures a tuple OR a list.
            (
                "destr_tuple",
                "{% macro m() %}{% set a, b = (1, 2) %}{{ a }}{{ b }}{% endmacro %}",
                "12",
            ),
            (
                "destr_list",
                "{% macro m() %}{% set a, b = [1, 2] %}{{ a }}{{ b }}{% endmacro %}",
                "12",
            ),
            (
                "destr_for",
                "{% macro m() %}{% for a, b in [(1,'x'),(2,'y')] %}{{ a }}{{ b }}{% endfor %}{% endmacro %}",
                "1x2y",
            ),
            // A dict iterates its keys in declaration order.
            (
                "dict_iter",
                "{% macro m() %}{% for k in {'y2':1,'y1':2} %}{{ k }}{% endfor %}{% endmacro %}",
                "y2y1",
            ),
            // A string iterates its characters.
            (
                "str_iter",
                "{% macro m() %}{% for c in 'ab' %}{{ c }}.{% endfor %}{% endmacro %}",
                "a.b.",
            ),
            // An `{{ x }}` emission of an int prints decimal.
            (
                "emit_int",
                "{% macro m(n) %}{{ n }}{% endmacro %}",
                "42",
            ),
            // A `{% do %}` in an arm never taken stays a node — the
            // def still expands.
            (
                "do_untaken",
                "{% macro m() %}{% if false %}{% do 1 %}a{% else %}b{% endif %}{% endmacro %}",
                "b",
            ),
        ] {
            let t = table(&[("m", def)]);
            let args = if name == "elif" { "2" } else if name == "emit_int" { "42" } else { "" };
            assert_eq!(render(&t, "m", args).as_deref(), Some(want), "{name}");
        }
    }

    #[test]
    fn whitespace_control_trims_template_text_only() {
        // `x = 'a '` — `{{ x }}{{- 'y' }}` keeps the emitted `a `
        // tail: only template text trims, never generated output
        // (dbt renders `a y`; the pre-tree expander produced `ay`).
        let t = table(&[(
            "m",
            "{% macro m() %}{% set x = 'a ' %}{{ x }}{{- 'y' }}{% endmacro %}",
        )]);
        assert_eq!(render(&t, "m", ""), Some("a y".to_string()));
        // A loop pass's own trailing space survives.
        let t = table(&[(
            "m",
            "{% macro m() %}{% for i in [1,2] %}{{ i }} {% endfor %}{% endmacro %}",
        )]);
        assert_eq!(render(&t, "m", ""), Some("1 2 ".to_string()));
        // `-%}`/`{%-` on the def's own tags trim the edge text.
        let t = table(&[("m", "{% macro m() -%}\n    x\n{%- endmacro %}")]);
        assert_eq!(render(&t, "m", ""), Some("x".to_string()));
    }

    #[test]
    fn a_right_strip_reaches_only_the_text_after_the_tag() {
        // A `-%}`/`-}}` strip dies when another tag lands first — the
        // marker trims the run DIRECTLY after it, never across a tag
        // boundary (each `want` is real Jinja output).
        for (name, def, want) in [
            (
                "expr_between",
                "{% macro m() %}{{ 'a' -}}{{- 'b' }} c{% endmacro %}",
                "ab c",
            ),
            (
                "set_between",
                "{% macro m() %}{% set x = '1' -%}{{ x }} end{% endmacro %}",
                "1 end",
            ),
            (
                "block_between",
                "{% macro m() %}{{ 'a' -}}{% if true %} b{% endif %}{% endmacro %}",
                "a b",
            ),
            (
                "comment_between",
                "{% macro m() %}{{ 'a' -}}{# c #} b{% endmacro %}",
                "a b",
            ),
            // The strip still applies where text really follows —
            // including a run it empties completely.
            (
                "strips_when_text_follows",
                "{% macro m() %}{{ 'a' -}}  b{% endmacro %}",
                "ab",
            ),
            (
                "empty_run_eats_strip",
                "{% macro m() %}{{ 'a' -}}  {# c #} b{% endmacro %}",
                "a b",
            ),
        ] {
            let t = table(&[("m", def)]);
            assert_eq!(render(&t, "m", "").as_deref(), Some(want), "{name}");
        }
    }

    #[test]
    fn nested_calls_bind_their_own_frame() {
        // `show(t)` evaluates its arg in the caller's frame (the loop's
        // `t`), then runs with only `t` bound — the emitted `ref`s land
        // in the dep ranges.
        let t = table(&[
            ("show", "{% macro show(t) %}{{ ref(t) }}{% endmacro %}"),
            (
                "m",
                "{% macro m() %}{% set code = 'x' %}{% for t in ['a','b'] %}{{ show(t) }},{% endfor %}{% endmacro %}",
            ),
        ]);
        let (s, refs) = render_refs(&t, "m", "").unwrap();
        assert_eq!(s, "\"a\",\"b\",");
        assert_eq!(refs.len(), 2);
        assert_eq!(&s[refs[0].clone()], "\"a\"");
        assert_eq!(&s[refs[1].clone()], "\"b\"");
        // A callee reading a caller-local name is unprovable — `code`
        // never reaches `leak`'s frame.
        let t = table(&[
            ("leak", "{% macro leak() %}{{ code }}{% endmacro %}"),
            (
                "m",
                "{% macro m() %}{% set code = 'x' %}{{ leak() }}{% endmacro %}",
            ),
        ]);
        assert_eq!(render(&t, "m", ""), None);
        // A `source()` through the chain marks both segments.
        let t = table(&[
            (
                "show",
                "{% macro show(s, tbl) %}{{ source(s, tbl) }}{% endmacro %}",
            ),
            (
                "m",
                "{% macro m() %}{% for t in ['orders'] %}{{ show('raw', t) }}{% endfor %}{% endmacro %}",
            ),
        ]);
        let (s, refs) = render_refs(&t, "m", "").unwrap();
        assert_eq!(s, "\"raw\".\"orders\"");
        assert_eq!(&s[refs[0].clone()], "\"raw\".\"orders\"");
        // A `ref` passes through an argument binding.
        let t = table(&[
            ("emit", "{% macro emit(r) %}{{ r }}{% endmacro %}"),
            ("m", "{% macro m() %}{{ emit(ref('x')) }}{% endmacro %}"),
        ]);
        assert_eq!(render(&t, "m", ""), Some("\"x\"".to_string()));
    }

    #[test]
    fn call_binding_follows_dbt_rules() {
        // Defaults evaluate in the callee frame left to right — `b=a`
        // reads the slot bound just before it.
        let t = table(&[("m", "{% macro m(a, b=a) %}{{ b }}{% endmacro %}")]);
        assert_eq!(render(&t, "m", "'x'"), Some("x".to_string()));
        // A list literal arg iterates.
        let t = table(&[(
            "m",
            "{% macro m(xs) %}{% for x in xs %}{{ x }}{% endfor %}{% endmacro %}",
        )]);
        assert_eq!(render(&t, "m", "['x','y']"), Some("xy".to_string()));
        // Calls dbt rejects — or args the model level can't prove —
        // leave the placeholder.
        for (name, def, args) in [
            ("extra", "{% macro m(a) %}1{% endmacro %}", "'a','b'"),
            ("unknown_kw", "{% macro m(a) %}1{% endmacro %}", "x='a'"),
            ("dup", "{% macro m(a) %}1{% endmacro %}", "'a',a='b'"),
            ("dup_kw", "{% macro m(a) %}1{% endmacro %}", "a='x',a='y'"),
            // `m(x=1, y)` — Python's "positional argument follows
            // keyword argument" SyntaxError. `b=` names the slot the
            // binding alone would let stand, so only the ordering
            // check refuses the call.
            (
                "pos_after_kw",
                "{% macro m(a, b) %}1{% endmacro %}",
                "b='x', 'y'",
            ),
            ("missing", "{% macro m(a) %}1{% endmacro %}", ""),
            ("name_arg", "{% macro m(a) %}{{ a }}{% endmacro %}", "foo"),
            (
                "call_arg",
                "{% macro m(a) %}{{ a }}{% endmacro %}",
                "range(3)",
            ),
            ("op_arg", "{% macro m(a) %}{{ a }}{% endmacro %}", "1 + 1"),
        ] {
            let t = table(&[("m", def)]);
            assert_eq!(render(&t, "m", args), None, "{name}");
        }
        // The same ordering refusal inside a body — a nested call with
        // a positional after a keyword fails the whole expansion, and
        // so does a `ref` spelled `ref(v=…, 'x')`.
        let t = table(&[
            ("n", "{% macro n(a, b) %}{{ b }}{% endmacro %}"),
            ("m", "{% macro m() %}{{ n(b='x', 'y') }}{% endmacro %}"),
        ]);
        assert_eq!(render(&t, "m", ""), None);
        let t = table(&[("m", "{% macro m() %}{{ ref(v=2, 'x') }}{% endmacro %}")]);
        assert_eq!(render(&t, "m", ""), None);
        // A qualified call resolves only the project namespace.
        let t = table(&[("m", "{% macro m() %}ok{% endmacro %}")]);
        assert_eq!(render(&t, "t.m", ""), Some("ok".to_string()));
        assert_eq!(render(&t, "pkg.m", ""), None);
        assert_eq!(render(&t, "missing", ""), None);
    }

    #[test]
    fn unprovable_shapes_stay_placeholders() {
        for (name, def, args) in [
            // A context name the frame can't prove — `target`, `var`,
            // `execute`, `env_var`, `this`, `adapter`, `config`.
            (
                "target",
                "{% macro m() %}{% if target.name == 'prod' %}a{% endif %}{% endmacro %}",
                "",
            ),
            (
                "var",
                "{% macro m() %}{% if var('x') %}a{% endif %}{% endmacro %}",
                "",
            ),
            (
                "execute",
                "{% macro m() %}{% if execute %}a{% endif %}{% endmacro %}",
                "",
            ),
            (
                "env_var",
                "{% macro m() %}{{ env_var('X') }}{% endmacro %}",
                "",
            ),
            (
                "exceptions",
                "{% macro m() %}{{ exceptions.raise_compiler_error('x') }}{% endmacro %}",
                "",
            ),
            // `null` is a NAME under Jinja (no `null` literal) — unbound.
            ("null_name", "{% macro m() %}{% set x = null %}{{ x }}{% endmacro %}", ""),
            // `is_incremental` isn't a frame name — the render refuses
            // where dbt's context would answer.
            (
                "incremental",
                "{% macro m() %}{% if is_incremental is defined %}a{% endif %}{% endmacro %}",
                "",
            ),
            // `as_bool` is an enabled-pass filter — the render has none.
            (
                "as_bool",
                "{% macro m() %}{% if 'False' | as_bool %}a{% endif %}{% endmacro %}",
                "",
            ),
            // A `{% do %}`/`{% break %}` in the TAKEN arm executes.
            (
                "do_taken",
                "{% macro m() %}{% if true %}{% do 1 %}a{% endif %}{% endmacro %}",
                "",
            ),
            (
                "break_taken",
                "{% macro m() %}{% for i in [1] %}{% break %}{{ i }}{% endfor %}{% endmacro %}",
                "",
            ),
            // Emitting a bool / none / list / dict / range is Python
            // notation — never SQL the emit can prove.
            ("emit_bool", "{% macro m(f) %}{{ f }}{% endmacro %}", "true"),
            ("emit_none", "{% macro m() %}{{ none }}{% endmacro %}", ""),
            ("emit_list", "{% macro m() %}{{ [1,2] }}{% endmacro %}", ""),
            ("emit_dict", "{% macro m() %}{{ {'a':1} }}{% endmacro %}", ""),
            ("emit_range", "{% macro m() %}{{ range(3) }}{% endmacro %}", ""),
            ("emit_loop", "{% macro m() %}{% for i in [1] %}{{ loop }}{% endfor %}{% endmacro %}", ""),
            // Unknown filters / attributes / subscripts / calls.
            ("upper", "{% macro m() %}{{ 'a' | upper }}{% endmacro %}", ""),
            ("attr", "{% macro m() %}{% set d = {'x':1} %}{{ d.x }}{% endmacro %}", ""),
            ("subscript", "{% macro m() %}{% set l = [1] %}{{ l[0] }}{% endmacro %}", ""),
            ("method", "{% macro m() %}{{ 'a'.upper() }}{% endmacro %}", ""),
            ("loop_cycle", "{% macro m() %}{% for i in [1,2] %}{{ loop.cycle('a','b') }}{% endfor %}{% endmacro %}", ""),
            // Arithmetic / concatenation / conditional expressions.
            ("arith", "{% macro m() %}{% for i in [1] %}{{ loop.index + 1 }}{% endfor %}{% endmacro %}", ""),
            ("concat", "{% macro m() %}{{ 'a' ~ 'b' }}{% endmacro %}", ""),
            ("cond_expr", "{% macro m() %}{{ 'a' if true else 'b' }}{% endmacro %}", ""),
            // `range` misuses — 0 step, past the mint cap, iterating
            // past the loop cap.
            ("range_step0", "{% macro m() %}{% for i in range(1,5,0) %}{{ i }}{% endfor %}{% endmacro %}", ""),
            ("range_mint_cap", "{% macro m() %}{{ range(200000) | length }}{% endmacro %}", ""),
            ("range_iter_cap", "{% macro m() %}{% for i in range(10001) %}{% endfor %}{% endmacro %}", ""),
            // Python type errors.
            ("cmp_mixed", "{% macro m() %}{% if 1 < 'a' %}x{% endif %}{% endmacro %}", ""),
            ("destr_short", "{% macro m() %}{% set a, b = (1,) %}{{ a }}{% endmacro %}", ""),
            ("destr_for", "{% macro m() %}{% for a, b in [(1,)] %}{{ a }}{% endfor %}{% endmacro %}", ""),
            ("loop_outside", "{% macro m() %}{{ loop.index }}{% endmacro %}", ""),
            // A frame name is never callable — `{% set %}`, a param, a
            // loop variable.
            ("set_shadow", "{% macro m() %}{% set inner = 'x' %}{{ inner() }}{% endmacro %}", ""),
            ("param_shadow", "{% macro m(inner='x') %}{{ inner() }}{% endmacro %}", ""),
            ("loop_shadow", "{% macro m() %}{% for inner in ['x'] %}{{ inner() }}{% endfor %}{% endmacro %}", ""),
            // Literal spellings Jinja reads differently — an unknown
            // escape, a leading-zero int, an i64 overflow, and floats
            // whose spelling isn't Python's `repr`.
            ("escape", "{% macro m() %}{{ '^\\d+' }}{% endmacro %}", ""),
            ("lead_zero", "{% macro m() %}{{ 007 }}{% endmacro %}", ""),
            ("big_int", "{% macro m() %}{{ 99999999999999999999 }}{% endmacro %}", ""),
            ("float_150", "{% macro m() %}{{ 1.50 }}{% endmacro %}", ""),
            ("float_small", "{% macro m() %}{{ 0.00001 }}{% endmacro %}", ""),
            ("float_e", "{% macro m() %}{{ 1e3 }}{% endmacro %}", ""),
            // A relation can't condition, compare, or bind via `set`.
            ("ref_cond", "{% macro m(r) %}{% if r is string %}s{% endif %}{% endmacro %}", "ref('x')"),
            ("set_ref", "{% macro m() %}{% set r = ref('x') %}{{ r }}{% endmacro %}", ""),
            // Sequence-kind distinctions Python proves but the render
            // refuses to.
            ("range_eq", "{% macro m() %}{% if range(3) == [0,1,2] %}x{% endif %}{% endmacro %}", ""),
            ("tuple_eq", "{% macro m() %}{% if (1,2) == [1,2] %}x{% endif %}{% endmacro %}", ""),
            // A bare `a, b = 1, 2` — real Jinja, but the evaluator's
            // grammar has no bare tuple → unexpandable (documented
            // narrowing).
            ("bare_tuple", "{% macro m() %}{% set a, b = 1, 2 %}{{ a }}{% endmacro %}", ""),
        ] {
            let t = table(&[("m", def)]);
            assert_eq!(render(&t, "m", args), None, "{name}");
        }
        // Emission caps — >64 KiB of text, >10k iterations, a
        // self-call chain past the depth cap.
        let pad = "x".repeat(400);
        let t = table(&[(
            "m",
            &format!(
                "{{% macro m() %}}{{% for i in range(200) %}}{pad}{{% endfor %}}{{% endmacro %}}"
            ),
        )]);
        assert_eq!(render(&t, "m", ""), None);
        let t = table(&[(
            "m",
            "{% macro m() %}{% for i in range(10001) %}{% endfor %}{% endmacro %}",
        )]);
        assert_eq!(render(&t, "m", ""), None);
        let t = table(&[("m", "{% macro m() %}{{ m() }}{% endmacro %}")]);
        assert_eq!(render(&t, "m", ""), None);
    }

    #[test]
    fn block_tags_fail_the_def_at_parse() {
        // A paired block the tree can't represent — `call`, `filter`,
        // `raw`, a nested `macro`, the `{% set %}…{% endset %}` block
        // form, `materialization` — makes the whole def unexpandable
        // (never reaches the table), wherever it sits.
        for (name, def) in [
            (
                "call",
                "{% macro m() %}{% call f() %}a{% endcall %}{% endmacro %}",
            ),
            (
                "filter",
                "{% macro m() %}{% filter upper %}a{% endfilter %}{% endmacro %}",
            ),
            (
                "raw",
                "{% macro m() %}{% raw %}{{ x }}{% endraw %}{% endmacro %}",
            ),
            (
                "block_set",
                "{% macro m() %}{% set x %}a{% endset %}{{ x }}{% endmacro %}",
            ),
            (
                "nested_macro",
                "{% macro m() %}{% macro n() %}a{% endmacro %}{{ n() }}{% endmacro %}",
            ),
            (
                "materialization",
                "{% macro m() %}{% materialization x %}a{% endmaterialization %}{% endmacro %}",
            ),
            // …as do an unbalanced block and a stray ender.
            ("unclosed_if", "{% macro m() %}{% if true %}a{% endmacro %}"),
            ("stray_endif", "{% macro m() %}{% endif %}{% endmacro %}"),
            ("stray_elif", "{% macro m() %}{% elif true %}{% endmacro %}"),
            ("stray_else", "{% macro m() %}{% else %}{% endmacro %}"),
            ("elif_after_else", "{% macro m() %}{% if true %}a{% else %}b{% elif false %}c{% endif %}{% endmacro %}"),
            ("double_else", "{% macro m() %}{% if true %}a{% else %}b{% else %}c{% endif %}{% endmacro %}"),
        ] {
            assert!(Expandable::parse(def).is_none(), "{name}");
        }
    }
}
