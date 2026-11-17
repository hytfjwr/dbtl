// ---------------------------------------------------------------------------
// Per-tag withholding — which absence-facts each replaced `{{ }}` shadows
// ---------------------------------------------------------------------------
//
// A `{{ }}` the winning render replaced — whitespace at a clause boundary
// or `(NULL)` on an `IN`/`ANY`/`SOME`/`ALL` right-hand side — stands for
// SQL the parse never saw. The old fact layer emptied every
// absence-finding model-wide on the mark; [`Withhold`] judges each
// [`ReplacedTag`] against each candidate instead: withhold only what the
// tag could invalidate, keep the rest reportable.
//
// - `unused_cte`: a tag can read a CTE through the clause or subquery it
//   hides. The tag's reach is judged per callee — a root-project macro's
//   body yields the word names it can emit (nested root macros included);
//   an unresolvable callee or an unprovable argument could emit anything;
//   and the tag's string literals match a CTE name by substring
//   (`{{ m('stg') }}` bound into `{{ prefix }}_orders` reaches
//   `stg_orders`). A package macro's body stays unproven without a
//   name-carrying argument — callers' CTE names can't reach it. That
//   last judgment is a rule of thumb, not a guarantee: a package macro
//   COULD coincidentally read a same-named table, so the approximation
//   may under-withhold there — acceptable, since over-withholding is
//   the worse direction.
// - `cartesian_join`: only a clause-boundary tag can add the missing
//   `ON`/`WHERE`, and only to the SELECT it sits in — the last `select`
//   keyword at the tag's own parenthesis depth. A tag with no
//   attributing SELECT withholds the whole model (the D3 fallback).
// - `order_by_without_limit`: only a clause-boundary tag positioned
//   after the outer ORDER BY can emit the bound the parse saw none of.
// - `unknown_column_reference`: unchanged — one clause-boundary tag
//   withholds the whole set, as before.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use sqlparser::tokenizer::Span;

use crate::dag::{Dag, MacroInfo};
use crate::source::{
    call_head_re, is_identifier, jinja_tag_close, lit_value, mask_jinja_comments, scan_call_args,
    split_kwarg, tag_word, top_level_segments, BUILTIN_BARE, BUILTIN_NS,
};

use super::jinja::{jinja_inner, jinja_string_literals, RenderedSql, TagShape};

/// The depth cap on the macro→macro body walk — a chain deeper than
/// this is judged unprovable (opaque), the same direction a cycle takes.
const BODY_DEPTH: usize = 16;

/// The model's per-tag withholding table — built once inside
/// `compute_analyzed`'s memoized pass, then consulted by each
/// absence-fact's candidate loop.
pub(super) struct Withhold<'a> {
    /// The winning parse render — the only one whose marks count.
    rendered: &'a RenderedSql,
    /// One judgment per [`ReplacedTag`], in tag order.
    tags: Vec<TagJudgment>,
}

/// One replaced `{{ }}` reduced to what each rule's filter reads.
struct TagJudgment {
    /// Clause-hiding shape: boundary whitespace vs `(NULL)` right-hand
    /// side. Only `ClauseBoundary` reaches `cartesian`/`order_by`;
    /// `unused_cte` consults both.
    shape: TagShape,
    /// The replacement's rendered byte range — the position checks.
    out: Range<usize>,
    /// The tag's raw-SQL byte range — the range a withheld candidate
    /// cites as its culprit.
    src: Range<usize>,
    /// The owning SELECT — `(paren depth, last `select` keyword end at
    /// that depth)`; `None` when no `select` at that depth precedes the
    /// tag (unattributable → withhold the candidate class model-wide).
    /// `ClauseBoundary` tags only — an `(NULL)` RHS can't add a clause.
    select: Option<(usize, usize)>,
    /// The tag could emit arbitrary SQL — a `{% set %}` variable or
    /// expression-shaped body (b), or a callee the classifier can't
    /// reach (c) — so it withholds every `unused_cte` candidate.
    opaque: bool,
    /// Lowercase word names the resolved root-project macro body chain
    /// may emit — an `unused_cte` candidate matching one may be read
    /// through the hidden clause (a).
    emit: BTreeSet<String>,
    /// The tag's lowercase string literals — a literal containing a
    /// CTE name reads it through the hidden clause, whichever callee
    /// holds it (d). Case-insensitive like the literal reads
    /// `unread_ctes` already excuses.
    frags: Vec<String>,
}

impl<'a> Withhold<'a> {
    /// Judge every [`ReplacedTag`] on `rendered`. `node` decides whether
    /// the ROOT project's macro table answers bare calls: a node outside
    /// it (`--include-packages`) resolves its bare calls against its own
    /// package — a namespace `dag` can't model — so they stay
    /// unresolvable.
    pub(super) fn new(dag: &'a Dag, node: &str, rendered: &'a RenderedSql) -> Withhold<'a> {
        let proj = dag.project_name().map(str::to_string);
        let node_root = dag.get(node).is_some_and(|n| dag.is_root_project(n));
        let mut memo = BTreeMap::new();
        let tags = rendered
            .replaced_tags
            .iter()
            .map(|t| {
                let e = jinja_inner(&t.expr);
                let (mut opaque, mut emit) = (!provable(e), BTreeSet::new());
                if !opaque {
                    let class = leading_call(e)
                        .map(|(name, _, _)| call_class(dag, proj.as_deref(), node_root, name))
                        .unwrap_or(Class::NoCall);
                    match class {
                        Class::Project(uid) => {
                            let (w, op) =
                                body_reach(dag, proj.as_deref(), &uid, &mut memo, &mut Vec::new());
                            emit = w;
                            opaque = op;
                        }
                        Class::Opaque => opaque = true,
                        Class::Builtin | Class::Package | Class::NoCall => {}
                    }
                }
                TagJudgment {
                    shape: t.shape,
                    out: t.out.clone(),
                    src: t.src.clone(),
                    select: (t.shape == TagShape::ClauseBoundary)
                        .then(|| rendered.select_at(t.out.start))
                        .flatten(),
                    opaque,
                    emit,
                    frags: jinja_string_literals(e)
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_lowercase())
                        .collect(),
                }
            })
            .collect();
        Withhold { rendered, tags }
    }

    /// Could a replaced tag emit a reference to CTE `name`? Both shapes
    /// count — the hidden clause and the `(NULL)` subquery alike can
    /// read a CTE. The returned range is the first culprit tag's raw
    /// span.
    pub(super) fn may_emit_cte(&self, name: &str) -> Option<Range<usize>> {
        let name = name.to_lowercase();
        self.tags
            .iter()
            .find(|t| {
                t.opaque
                    || t.emit.contains(&name)
                    || t.frags.iter().any(|f| name.contains(f.as_str()))
            })
            .map(|t| t.src.clone())
    }

    /// Is the conditionless join arm at `span` inside a clause-boundary
    /// tag's reach — same owning SELECT, tag positioned after the joined
    /// relation, where it can emit the `ON`/`WHERE` the parse saw none
    /// of? A tag with no attributing SELECT withholds every candidate.
    /// The returned range is the culprit tag's raw span.
    pub(super) fn elides_cartesian(&self, span: Span) -> Option<Range<usize>> {
        let item = self.rendered.span_rendered(span);
        let owner = item.as_ref().and_then(|r| self.rendered.select_at(r.start));
        self.tags
            .iter()
            .find(|t| {
                t.shape == TagShape::ClauseBoundary
                    && match (t.select, owner, &item) {
                        (None, _, _) => true,
                        (Some(sel), Some(o), Some(r)) => sel == o && t.out.start >= r.end,
                        _ => false,
                    }
            })
            .map(|t| t.src.clone())
    }

    /// Does a clause-boundary tag sit after `ob` ( the outer ORDER BY's
    /// rendered span) — positioned to emit the LIMIT/FETCH the parse
    /// saw none of? An ORDER BY whose span can't be placed (an
    /// `ORDER BY ALL` carries none) falls back to "any boundary tag".
    /// The returned range is the culprit tag's raw span.
    pub(super) fn elides_order_by(&self, ob: Span) -> Option<Range<usize>> {
        let r = self.rendered.span_rendered(ob);
        self.tags
            .iter()
            .find(|t| {
                t.shape == TagShape::ClauseBoundary
                    && r.as_ref().is_none_or(|r| t.out.start >= r.end)
            })
            .map(|t| t.src.clone())
    }

    /// The first clause-boundary tag — `unresolved`'s model-wide
    /// culprit (one boundary tag withholds the whole set, as before).
    pub(super) fn first_elider(&self) -> Option<Range<usize>> {
        self.tags
            .iter()
            .find(|t| t.shape == TagShape::ClauseBoundary)
            .map(|t| t.src.clone())
    }
}

/// Can `e` — a `{{ }}` body — be proven literal-bound: a literal whole
/// (`{{ 'u' }}`), or a leading call whose EVERY argument is a literal
/// or a `ref`/`source` call? Anything else — a `{% set %}` variable, a
/// `var()`/`env_var()` callee (the var's value can be a whole clause),
/// an expression, a call after the call — leaves its emitted text
/// unprovable: (b).
fn provable(e: &str) -> bool {
    if lit_value(e).is_some() {
        return true;
    }
    let Some((name, args, tail)) = leading_call(e) else {
        return false;
    };
    if !tail.is_empty() || matches!(name, "var" | "env_var") {
        return false;
    }
    top_level_segments(args).iter().all(|s| arg_literal(s))
}

/// One top-level argument — kwarg or positional — is literal when its
/// expression parses as a literal or a `ref`/`source` call (a relation
/// name: resolvable, never a clause-emitting value). An empty segment
/// (`m(,)`) carries nothing.
fn arg_literal(seg: &str) -> bool {
    let seg = seg.trim();
    if seg.is_empty() {
        return true;
    }
    let expr = split_kwarg(seg).map_or(seg, |(_, e)| e);
    lit_value(expr).is_some()
        || matches!(leading_call(expr), Some(("ref" | "source", _, t)) if t.trim().is_empty())
}

/// The callee class a `{{ }}` body's leading call resolves to — dbt's
/// resolution order, mode-independent (the same order
/// [`crate::source::Macros`]'s `classify` follows, against
/// [`Dag::macros`]'s table): a root-project macro shadows builtins, then
/// the builtin lists, `adapter.dispatch` as unresolvable (its string
/// arg can reach a project macro), any other `ns.m` a package macro,
/// and a bare unknown name unresolvable.
enum Class {
    /// `macro.<root>.<name>` — a `{% macro %}` def (a `{% test %}` or
    /// materialization body is never a macro).
    Project(String),
    /// Unresolvable — an unknown bare name, `<root>.unknown`,
    /// `adapter.dispatch(…)`, or a `{% test %}`-shaped entry.
    Opaque,
    /// A listed builtin — only the literal-fragment match applies.
    Builtin,
    /// `<other pkg>.m` — a package macro. Its body isn't scanned: the
    /// rule of thumb is that it can't name the caller's CTEs, so only a
    /// literal argument carrying the name counts (an approximation — a
    /// coincidental same-named read slips through).
    Package,
    /// No leading call at all (`{{ 'u' }}`) — only the literal check.
    NoCall,
}

/// Classify `name` (a `{{ }}` callee as spelled) under `dag`'s macro
/// table. `node_root` gates the root-project lookup: a node outside the
/// root project binds bare names against its own package, which `dag`
/// can't model — so a root macro never answers a non-root node's call.
fn call_class(dag: &Dag, proj: Option<&str>, node_root: bool, name: &str) -> Class {
    match name.split_once('.') {
        None => {
            if node_root {
                if let Some(uid) = proj.and_then(|p| project_macro(dag, p, name)) {
                    return Class::Project(uid);
                }
            }
            if BUILTIN_BARE.contains(&name) {
                Class::Builtin
            } else {
                Class::Opaque
            }
        }
        Some((ns, bare)) => {
            if node_root && proj.is_some_and(|p| ns == p) {
                return match project_macro(dag, ns, bare) {
                    Some(uid) => Class::Project(uid),
                    // `<root>.unknown` resolves nowhere.
                    None => Class::Opaque,
                };
            }
            if (ns, bare) == ("adapter", "dispatch") {
                return Class::Opaque;
            }
            if BUILTIN_NS.contains(&ns) {
                Class::Builtin
            } else {
                Class::Package
            }
        }
    }
}

/// `macro.<pkg>.<name>`'s uid when the map holds a `{% macro %}`
/// definition for it — `{% test %}`/`{% materialization %}` bodies and
/// body-less entries are never callable macros.
fn project_macro(dag: &Dag, pkg: &str, name: &str) -> Option<String> {
    let uid = format!("macro.{pkg}.{name}");
    dag.macros()
        .get(&uid)
        .filter(|m| is_macro_def(m))
        .map(|_| uid)
}

/// `e`'s leading `name(…)` call — the callee, the argument text, and
/// the remainder after `)`. `None` when `e` doesn't open with a call or
/// the parens don't balance.
fn leading_call(e: &str) -> Option<(&str, &str, &str)> {
    let m = call_head_re().find(e)?;
    if m.start() != 0 {
        return None;
    }
    let name = m.as_str()[..m.len() - 1].trim_end();
    let (args, end) = scan_call_args(e, m.end())?;
    Some((name, args, e[end..].trim()))
}

/// Does `sql` hold a `{% macro %}` definition — vs a `{% test %}`,
/// `{% materialization %}`, or `{% docs %}` body the manifest's `macros`
/// map also carries? Source mode's `macro_sql` is the def BODY (no
/// header) — its collector only keeps real `{% macro %}` defs, so a
/// body-shaped value counts as a macro.
fn is_macro_def(m: &MacroInfo) -> bool {
    let Some(sql) = &m.sql else { return false };
    match leading_block(sql) {
        Some((word, _)) => word == "macro",
        None => true,
    }
}

/// `sql`'s leading `{% … %}` tag — `(word, tag span)` — `None` when the
/// trimmed text doesn't open with a block tag.
fn leading_block(sql: &str) -> Option<(&str, Range<usize>)> {
    let lead = sql.len() - sql.trim_start().len();
    if !sql[lead..].starts_with("{%") {
        return None;
    }
    let close = jinja_tag_close(sql.as_bytes(), lead)?;
    let (word, _) = tag_word(&sql[lead + 2..close]);
    Some((word, lead..close + 2))
}

/// The offset of the first `{% … %}` tag whose word is `want` — the
/// `{% endmacro %}` close that ends a def (macros don't nest, so the
/// first match closes).
fn find_block_word(text: &str, want: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = 0usize;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'%' {
            if let Some(close) = jinja_tag_close(b, i) {
                let (word, _) = tag_word(&text[i + 2..close]);
                if word == want {
                    return Some(i);
                }
                i = close + 2;
                continue;
            }
        }
        i += 1;
    }
    None
}

/// `{% macro name(sig) %}`'s parameter names — recovered from the
/// signature when `MacroInfo::params` is empty (hand-built manifests
/// carrying `macro_sql` alone). `*args`/`**kw` aren't named parameters,
/// matching the loader's own convention.
fn signature_params(sig: &str) -> Vec<String> {
    let Some((_, args, _)) = leading_call(sig.trim()) else {
        return Vec::new();
    };
    top_level_segments(args)
        .iter()
        .filter_map(|seg| {
            let seg = seg.trim();
            if seg.starts_with('*') {
                return None;
            }
            let name = split_kwarg(seg).map_or(seg, |(n, _)| n);
            is_identifier(name).then(|| name.to_string())
        })
        .collect()
}

/// The emit-words + opacity of `macro.<pkg>.<name>`'s body — the
/// lowercase word names its definition can place in the query (its
/// plain text and its literals' contents), and whether it holds any
/// part the scan can't prove: a `{% %}` tag, a non-param `{{ }}`, a
/// nested call reaching an unresolvable callee, or a depth/cycle bail.
/// Memoized per `uid` over one `Withhold` build.
fn body_reach(
    dag: &Dag,
    proj: Option<&str>,
    uid: &str,
    memo: &mut BTreeMap<String, (BTreeSet<String>, bool)>,
    visiting: &mut Vec<String>,
) -> (BTreeSet<String>, bool) {
    if let Some(hit) = memo.get(uid) {
        return hit.clone();
    }
    let mut reach = (BTreeSet::new(), true);
    if visiting.len() < BODY_DEPTH && !visiting.iter().any(|u| u == uid) {
        if let Some(m) = dag.macros().get(uid) {
            reach = body_scan(dag, proj, uid, m, memo, visiting);
        }
    }
    memo.insert(uid.to_string(), reach.clone());
    reach
}

/// One body's emit-words + opacity. A `{% macro %}` header (manifest
/// shape) is stripped first — its signature params, when
/// `MacroInfo::params` doesn't already carry them — and a first
/// `{% endmacro %}` truncates the scan. Source-mode bodies carry
/// neither. `visiting` holds the recursion stack so a cycle and the
/// depth cap both judge opaque.
fn body_scan(
    dag: &Dag,
    proj: Option<&str>,
    uid: &str,
    m: &MacroInfo,
    memo: &mut BTreeMap<String, (BTreeSet<String>, bool)>,
    visiting: &mut Vec<String>,
) -> (BTreeSet<String>, bool) {
    let Some(sql) = &m.sql else {
        return (BTreeSet::new(), true);
    };
    let masked = mask_jinja_comments(sql);
    let mut text: &str = &masked;
    let mut params: Vec<String> = m.params.clone();
    if let Some((word, span)) = leading_block(text) {
        // `{% test %}`/`{% materialization %}`/… — `is_macro_def` keeps
        // these out of the call classifier already; a nested body
        // shaped like one is unprovable either way.
        if word != "macro" {
            return (BTreeSet::new(), true);
        }
        let (_, rest) = tag_word(&text[span.start + 2..span.end - 2]);
        if params.is_empty() {
            params = signature_params(rest);
        }
        text = &text[span.end..];
        if let Some(off) = find_block_word(text, "endmacro") {
            text = &text[..off];
        }
    }

    let mut emit = BTreeSet::new();
    let mut opaque = false;
    // Text outside any tag contributes its word names wholesale — the
    // macro emits it verbatim into the query.
    let mut plain = String::new();
    visiting.push(uid.to_string());
    let b = text.as_bytes();
    let mut last = 0usize;
    let mut i = 0usize;
    while i + 1 < b.len() {
        let expr = b[i] == b'{' && b[i + 1] == b'{';
        if !(expr || (b[i] == b'{' && b[i + 1] == b'%')) {
            i += 1;
            continue;
        }
        let Some(close) = jinja_tag_close(b, i) else {
            // An unterminated tag renders the rest unprovable.
            opaque = true;
            break;
        };
        plain.push_str(&text[last..i]);
        if expr {
            let inner = &text[i + 2..close];
            let e = jinja_inner(inner);
            if e.is_empty() || params.iter().any(|p| param_ref(e, p)) {
                // `{{ param }}` / `{{ param.attr }}` / `{{ p[k] }}` —
                // bound at the call site; the caller's own literals
                // carry whatever it emits.
            } else {
                // The tag's own string literals always contribute —
                // a `{{ 'u' }}` or the `'u'` inside `{{ ref('u') }}`
                // can spell the emitted name.
                for lit in jinja_string_literals(inner) {
                    collect_words(&lit, &mut emit);
                }
                if lit_value(e).is_some() {
                    // A pure literal emits only itself — counted above.
                } else if let Some((name, args, tail)) = leading_call(e) {
                    // The same provability a model-level tag gets:
                    // `var()`/`env_var()` can hold a whole clause, a
                    // `f()()` tail applies the result once more, and an
                    // argument that is neither a literal nor one of this
                    // body's bound params (`var()`, `this`, an
                    // expression) could feed the callee's `{{ param }}`
                    // slots a value this scan can't see.
                    let provable_call = tail.trim().is_empty()
                        && !matches!(name, "var" | "env_var")
                        && top_level_segments(args).iter().all(|s| {
                            arg_literal(s) || {
                                let seg = s.trim();
                                let arg = split_kwarg(seg).map_or(seg, |(_, e)| e).trim();
                                params.iter().any(|p| param_ref(arg, p))
                            }
                        });
                    if !provable_call {
                        opaque = true;
                    } else {
                        match call_class(dag, proj, true, name) {
                            Class::Project(next) => {
                                let (w, op) = body_reach(dag, proj, &next, memo, visiting);
                                emit.extend(w);
                                opaque |= op;
                            }
                            // A builtin/`ref`/`source` emits a value or a
                            // relation name; a package macro can't name
                            // the caller's CTEs — the same rule of thumb
                            // the model-level call applies. The
                            // literals already counted either way.
                            Class::Builtin | Class::Package | Class::NoCall => {}
                            Class::Opaque => opaque = true,
                        }
                    }
                } else {
                    // `{{ name }}` not bound to a param, or an
                    // expression — the value isn't provable from
                    // this side.
                    opaque = true;
                }
            }
        } else {
            // Any `{% %}` is an unprovable part: `{% set %}` binds a
            // value the tag can emit anywhere, `{% if %}`/`{% for %}`
            // pick text this scan can't see.
            opaque = true;
        }
        i = close + 2;
        last = i;
    }
    plain.push_str(&text[last..]);
    visiting.pop();
    collect_words(&plain, &mut emit);
    (emit, opaque)
}

/// Is `e` bound to param `p` — `{{ p }}`, `{{ p.attr }}`, `{{ p[k] }}`?
/// (`p(…)` isn't: it invokes the param's value, unprovable.)
fn param_ref(e: &str, p: &str) -> bool {
    e == p
        || e.strip_prefix(p)
            .is_some_and(|r| r.starts_with('.') || r.starts_with('['))
}

/// Fold `text`'s word runs (`[A-Za-z0-9_]+`, lowercased) into `out` —
/// the name set a body can emit, matched against `unused_cte`
/// candidates.
fn collect_words(text: &str, out: &mut BTreeSet<String>) {
    let mut w = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            w.push(c.to_ascii_lowercase());
        } else if !w.is_empty() {
            out.insert(std::mem::take(&mut w));
        }
    }
    if !w.is_empty() {
        out.insert(w);
    }
}
