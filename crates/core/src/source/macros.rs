//! Project-local `{% macro %}` handling for source mode.
//!
//! dbt executes macros at parse time and writes each caller's resolved
//! `ref()`/`source()` targets into `depends_on.nodes` (and the callee's
//! unique_id into `depends_on.macros`). This module is the no-Jinja
//! counterpart: collect every `{% macro name(sig) %}…{% endmacro %}` under
//! `macro-paths` once, and for each `{{ name(…) }}`/`{% … name(…) %}`
//! invocation [`jinja_calls`] finds in a model or snapshot, bind the call's
//! arguments to the macro's parameters, evaluate the body's dep calls, and
//! emit the targets that reduce to string literals — recursing into
//! project-macro calls the body itself makes, under
//! [`MAX_MACRO_DEPTH`] for the degenerate cycles.
//!
//! What reduces to an edge: `ref('x')`/`source('s','t')` with literal
//! strings, or with parameters bound to literal strings by the invocation
//! (`{{ union_raw('orders') }}` → `ref(table_name)` resolves as
//! `ref('orders')`). What canNOT — a `var()`/`set`-var/computed argument, a
//! `ns.name()` call into a package (installed `dbt_packages/` sources are
//! deliberately never read — see `crate::source`), or a call past the depth
//! cap — resolves no edges but still counts in [`Macros::resolve`]'s
//! `incomplete` flag. The edge pass carries those counts into
//! `RawManifest::unresolved_adjacent` so the degree-judging lint rules
//! under-report instead of inventing "isolated" findings — the same
//! unprovable-adjacency record a package-scope prune writes.
//!
//! Conditional Jinja (`{% if %}`/`{% for %}`) is NOT branched on here: a
//! macro body's calls all resolve, whichever arm a render would take —
//! dbt's own parse records only the executed arm, but source mode can't
//! evaluate the guard, so over-covering the arms is the deliberate
//! approximation (missing an edge is the worse direction).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::manifest::{RawDependsOn, RawMacro};

use super::collect::{collect_with_root, mask_jinja_comments, read, rel_path};
use super::patterns::{
    find_subslice, jinja_calls, jinja_tag_close, lit_value, scan_call_args, tag_word,
    top_level_segments,
};

/// Deepest macro→macro chain an invocation resolves before the call
/// reports `incomplete` — real chains run 1–3 deep; the cap is for
/// self-recursive and mutually-recursive bodies, which a per-level
/// decrement bounds without cycle bookkeeping.
pub(super) const MAX_MACRO_DEPTH: usize = 8;

/// Bare-call names dbt/Jinja expose in every context — consulted only
/// AFTER the project's own `{% macro %}` table (a project macro always
/// shadows them, the way dbt's name resolution works). `ref`/`source`/
/// `config` are dependency/context calls the call-site pass already owns;
/// the rest are context builtins and Jinja globals — never dep-carrying
/// macros. Names NOT listed (package macros like `dbt_utils.star`, or a
/// misspelling) fall to the unresolvable bucket, which only under-reports
/// lint findings — the safe direction.
const BUILTIN_BARE: &[&str] = &[
    "as_bool",
    "as_native",
    "as_number",
    "as_text",
    "bool",
    "builtins",
    "config",
    "debug",
    "dict",
    "doc",
    "docs",
    "env_var",
    "exceptions",
    "float",
    "int",
    "is_first_run",
    "is_incremental",
    "joiner",
    "lipsum",
    "list",
    "load_agate_table",
    "load_result",
    "local_md5",
    "log",
    "namespace",
    "print",
    "range",
    "ref",
    "return",
    "run_hooks",
    "run_query",
    "set",
    "source",
    "statement",
    "store_raw_result",
    "store_result",
    "str",
    "submit_python_job",
    "try_or_error",
    "tuple",
    "var",
    "zip",
    // dbt-internal SQL macros commonly invoked unqualified — they carry no
    // project deps, so treating them as builtins is exact, and an internal
    // name missing here only under-reports.
    "cast",
    "concat",
    "date_trunc",
    "dateadd",
    "datediff",
    "escape_single_quotes",
    "last_day",
    "position",
    "replace",
    "split_part",
    "string_literal",
];

/// Attribute-call namespaces dbt/Jinja expose (`adapter.dispatch(…)` takes
/// its own branch — the string argument names a macro that may be one of
/// the project's own). A `pkg.name()` call whose `pkg` isn't listed and
/// isn't the project itself is a package macro — unresolvable by design.
const BUILTIN_NS: &[&str] = &[
    "adapter",
    "api",
    "builtins",
    "config",
    "context",
    "dbt",
    "doc",
    "docs",
    "env_var",
    "exceptions",
    "flags",
    "graph",
    "local_md5",
    "log",
    "macro",
    "model",
    "modules",
    "project",
    "ref",
    "return",
    "run_query",
    "schema",
    "source",
    "sql",
    "target",
    "this",
    "var",
];

/// One argument/parameter expression reduced to what edge resolution can
/// use: a string literal, a bound parameter, or "not provably a string".
#[derive(Debug, Clone)]
enum ArgExpr {
    /// The unquoted text of a quoted string literal.
    Lit(String),
    /// The enclosing macro's parameter at this index — evaluated through
    /// the call's bindings.
    Param(usize),
    /// `var()`, an expression, a subscript, a non-string literal —
    /// anything a string target can't be proven from.
    Other,
}

/// A call's arguments, classified against an optional parameter scope
/// (empty for a model-level call).
#[derive(Debug, Default)]
struct CallArgs {
    pos: Vec<ArgExpr>,
    kw: Vec<(String, ArgExpr)>,
}

/// The literal strings a call's arguments carry — the edge pass's
/// unresolvable-call record keeps these as table-name candidates (matched
/// to declared sources once `self.sources` exists).
fn literal_args(args: &CallArgs) -> Vec<String> {
    args.pos
        .iter()
        .chain(args.kw.iter().map(|(_, e)| e))
        .filter_map(|e| match e {
            ArgExpr::Lit(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// A `ref()`/`source()` call in a macro body — the dep kind dbt records in
/// `depends_on.nodes`.
enum BodyDep {
    /// `ref(args)` — the last positional is the model name (the first a
    /// package when two are given), the same rule model-level `ref` uses.
    Ref(CallArgs),
    /// `source(args)` — `source_name` then `table_name`, positionally or
    /// by the canonical kwarg names.
    Source(CallArgs),
}

/// A non-`ref`/`source` call a macro body makes — reclassified at resolve
/// time (project macro → recurse, builtin → skip, otherwise →
/// `incomplete`).
struct BodyCall {
    /// The package qualifier as spelled (`shipkit.audited_select` →
    /// `Some("shipkit")`); `None` for a bare call.
    ns: Option<String>,
    /// The callee name after any qualifier.
    name: String,
    /// The call's arguments, classified against this body's params.
    args: CallArgs,
}

/// One `{% macro %}` parameter — the name, plus its default expression
/// (itself classified against the parameters declared before it, the only
/// scope Jinja lets a default reference).
struct Param {
    name: String,
    default: Option<ArgExpr>,
}

/// One collected `{% macro %}`: its signature and the dependency-relevant
/// calls in its body.
pub(super) struct MacroDef {
    /// The declared parameters in order (variadics like `*args`/`**kwargs`
    /// are not named parameters — an arg landing there binds nothing).
    params: Vec<Param>,
    /// The body's `ref()`/`source()` calls.
    deps: Vec<BodyDep>,
    /// The body's every other call (`config` excluded — a context call,
    /// never a dep carrier).
    calls: Vec<BodyCall>,
    /// The macro's body text — `RawMacro.macro_sql`'s source-mode value.
    sql: String,
}

/// Evaluate one argument expression through a binding frame — `None` is
/// "not provably a string".
fn eval(expr: &ArgExpr, bound: &[Option<String>]) -> Option<String> {
    match expr {
        ArgExpr::Lit(s) => Some(s.clone()),
        ArgExpr::Param(i) => bound.get(*i).cloned().flatten(),
        ArgExpr::Other => None,
    }
}

/// Is `s` a bare Jinja identifier (what a kwarg/param name must be)?
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Reduce `expr` against `scope` (the enclosing macro's parameter names,
/// empty at model level): quoted string literal → `Lit`, bare parameter
/// name → `Param`, everything else → `Other`.
fn classify_expr(expr: &str, scope: &[String]) -> ArgExpr {
    let e = expr.trim();
    if let Some(serde_json::Value::String(s)) = lit_value(e) {
        return ArgExpr::Lit(s);
    }
    if let Some(i) = scope.iter().position(|p| p == e) {
        return ArgExpr::Param(i);
    }
    ArgExpr::Other
}

/// `name = expr` at top level → `Some((name, expr))`. The name must be a
/// bare identifier and the `=` a single assignment — `==`/`!=`/`<=`/`>=`
/// are comparisons, a parenthesized/subscripted name is not a kwarg.
fn split_kwarg(seg: &str) -> Option<(&str, &str)> {
    let b = seg.as_bytes();
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
            b'=' if depth == 0 => {
                if b.get(i + 1) == Some(&b'=')
                    || i.checked_sub(1).is_some_and(|p| {
                        matches!(
                            b[p],
                            b'=' | b'!' | b'<' | b'>' | b'~' | b'+' | b'-' | b'*' | b'/'
                        )
                    })
                {
                    return None;
                }
                let name = seg[..i].trim();
                return is_ident(name).then(|| (name, seg[i + 1..].trim()));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Classify a call's argument text — each top-level segment is a kwarg or
/// positional, each expression a literal/parameter/other.
fn classify_args(args_text: &str, scope: &[String]) -> CallArgs {
    let mut out = CallArgs::default();
    for seg in top_level_segments(args_text) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        match split_kwarg(seg) {
            Some((name, expr)) => out.kw.push((name.to_string(), classify_expr(expr, scope))),
            None => out.pos.push(classify_expr(seg, scope)),
        }
    }
    out
}

/// A kwarg's expression by name — `None` when undeclared.
fn kw_value<'a>(args: &'a CallArgs, name: &str) -> Option<&'a ArgExpr> {
    args.kw.iter().find(|(k, _)| k == name).map(|(_, e)| e)
}

/// The value a call's argument carries — positional `i` if present, else
/// the kwarg of `name`.
fn arg_value(args: &CallArgs, i: usize, name: &str, bound: &[Option<String>]) -> Option<String> {
    args.pos
        .get(i)
        .or_else(|| kw_value(args, name))
        .and_then(|e| eval(e, bound))
}

/// The resolved shape of one `ref`/`source` body dep under `bound` —
/// `None` when a needed argument isn't provably a string.
enum DepTarget {
    Ref(String),
    Source(String, String),
}

fn dep_target(dep: &BodyDep, bound: &[Option<String>]) -> Option<DepTarget> {
    match dep {
        BodyDep::Ref(args) => args
            .pos
            .last()
            .and_then(|e| eval(e, bound))
            .or_else(|| kw_value(args, "name").and_then(|e| eval(e, bound)))
            .map(DepTarget::Ref),
        BodyDep::Source(args) => match (
            arg_value(args, 0, "source_name", bound),
            arg_value(args, 1, "table_name", bound),
        ) {
            (Some(s), Some(t)) => Some(DepTarget::Source(s, t)),
            _ => None,
        },
    }
}

/// The outcome of resolving one macro invocation: the dep targets that
/// reduced to literals, the project macros invoked along the way, and
/// whether any part stayed unresolvable.
#[derive(Debug, Default)]
pub(super) struct Resolution {
    /// Model names `ref()` reduced to (the name_index resolves them like
    /// a model-level `ref('x')`).
    pub refs: Vec<String>,
    /// `(source_name, table_name)` pairs `source()` reduced to.
    pub sources: Vec<(String, String)>,
    /// The bare name of the node-level invoked project macro — direct
    /// calls only, matching what dbt writes into a node's
    /// `depends_on.macros` (nested callees belong to the callee's own
    /// `depends_on.macros`, the [`raw_macros`] map).
    pub macros: BTreeSet<String>,
    /// A dep arg that stayed non-literal, a callee that isn't a project
    /// macro (a package `ns.name`, a bare unknown, an `adapter.dispatch`
    /// of an unknown name), or the depth cap — the invocation may hide
    /// edges the graph doesn't show.
    pub incomplete: bool,
}

/// What a callee spelling resolves to.
pub(super) enum Callee<'a> {
    /// `ref`/`source`/`config` — dep/context calls the call-site pass
    /// owns (skips at node level; in a body they're `BodyDep`s already).
    DepCall,
    /// A dbt/Jinja builtin — never a dep carrier.
    Builtin,
    /// `adapter.dispatch` — the first string argument names the macro;
    /// the dispatched call's own arguments (a second call group) can't be
    /// bound.
    Dispatch,
    /// A project-local macro.
    Project(&'a MacroDef),
    /// A package-namespaced or unknown call — unresolvable reach.
    Foreign,
}

/// Every `{% macro %}` under the project's `macro-paths`, keyed by name.
pub(super) struct Macros {
    /// Macro name → definition. `BTreeMap` so the `macros` manifest map
    /// (and its diff output) is deterministically ordered.
    defs: BTreeMap<String, MacroDef>,
}

impl Macros {
    /// Read every `.sql` under `macro_paths` once and reduce each
    /// `{% macro name(sig) %}…{% endmacro %}` to its parameters and the
    /// dependency-relevant calls in its body. A name defined twice keeps
    /// the LAST definition (dbt's namespacing overwrites on re-parse too)
    /// with a warning; `{% test %}`/`{% materialization %}` blocks aren't
    /// macro calls — only the literal `macro` keyword counts.
    pub(super) fn load(dir: &Path, macro_paths: &[String], warnings: &mut Vec<String>) -> Self {
        let mut defs: BTreeMap<String, MacroDef> = BTreeMap::new();
        for (file, _) in collect_with_root(dir, macro_paths, "sql") {
            let masked = mask_jinja_comments(&read(&file)).into_owned();
            for (name, def) in macro_defs(&masked) {
                if defs.insert(name.clone(), def).is_some() {
                    warnings.push(format!(
                        "duplicate macro '{name}' — keeping the last definition: {}",
                        rel_path(dir, &file)
                    ));
                }
            }
        }
        Macros { defs }
    }

    /// How a callee spelling from [`jinja_calls`] resolves against this
    /// table — the shared classifier for the node edge pass and for the
    /// calls nested inside macro bodies. Order matters: `ref`/`source`/
    /// `config` first (context calls a definition can't shadow), then the
    /// project's own table (a project macro shadows a same-named builtin,
    /// dbt's resolution), then the builtin lists, then the project-name
    /// qualifier, and whatever is left is a package macro or an unknown
    /// name — unresolvable either way.
    fn classify<'a>(&'a self, name: &str, proj: &str) -> Callee<'a> {
        if matches!(name, "ref" | "source" | "config") {
            return Callee::DepCall;
        }
        match name.split_once('.') {
            // Bare call: a project macro shadows a same-named builtin
            // (dbt's resolution), a listed builtin resolves nothing, and
            // anything else is a package-exposed or unknown name.
            None => {
                if let Some(def) = self.defs.get(name) {
                    return Callee::Project(def);
                }
                if BUILTIN_BARE.contains(&name) {
                    Callee::Builtin
                } else {
                    Callee::Foreign
                }
            }
            // `<this project>.name` — resolves only into the project's own
            // table; `proj.unknown` is a dbt error upstream, counted here.
            Some((ns, bare)) if ns == proj => match self.defs.get(bare) {
                Some(def) => Callee::Project(def),
                None => Callee::Foreign,
            },
            Some(("adapter", "dispatch")) => Callee::Dispatch,
            Some((ns, _)) if BUILTIN_NS.contains(&ns) => Callee::Builtin,
            // `pkg.name` — a package macro: `dbt_packages/` is never read,
            // so its reach is unprovable by construction.
            Some(_) => Callee::Foreign,
        }
    }

    /// The project's own macro table entry — `RawManifest.macros`'s
    /// source-mode content: one `RawMacro` per collected definition, its
    /// `depends_on.macros` the project callees its body names (the
    /// macro→macro call graph `Dag::macros` walks for shared-macro
    /// impact).
    pub(super) fn raw_macros(&self, proj: &str) -> BTreeMap<String, RawMacro> {
        self.defs
            .iter()
            .map(|(name, def)| {
                let mut callees: Vec<String> = def
                    .calls
                    .iter()
                    .filter_map(|c| match self.classify_named(&c.ns, &c.name, proj) {
                        Callee::Project(_) => Some(format!("macro.{proj}.{}", c.name)),
                        // `adapter.dispatch('name')` with a provable literal
                        // name is a dep of the calling macro, the same shape
                        // dbt writes.
                        Callee::Dispatch => c
                            .args
                            .pos
                            .first()
                            .and_then(|e| eval(e, &[]))
                            .filter(|n| self.defs.contains_key(n))
                            .map(|n| format!("macro.{proj}.{n}")),
                        _ => None,
                    })
                    .collect();
                callees.sort();
                callees.dedup();
                (
                    format!("macro.{proj}.{name}"),
                    RawMacro {
                        name: name.clone(),
                        macro_sql: Some(def.sql.clone()),
                        depends_on: RawDependsOn {
                            nodes: Vec::new(),
                            macros: callees,
                        },
                    },
                )
            })
            .collect()
    }

    /// `classify` on a stored (`ns`, `name`) pair — the body-call shape.
    fn classify_named<'a>(
        &'a self,
        ns: &'a Option<String>,
        name: &'a str,
        proj: &str,
    ) -> Callee<'a> {
        match ns {
            Some(ns) => self.classify(&format!("{ns}.{name}"), proj),
            None => self.classify(name, proj),
        }
    }

    /// Evaluate a call's args against the caller `frame` and bind them to
    /// `def`'s parameters: positional by index, keyword by name, defaults
    /// filling the leftovers.
    fn bind(def: &MacroDef, args: &CallArgs, frame: &[Option<String>]) -> Vec<Option<String>> {
        let mut bound: Vec<Option<String>> = vec![None; def.params.len()];
        for (i, expr) in args.pos.iter().enumerate() {
            if let Some(slot) = bound.get_mut(i) {
                *slot = eval(expr, frame);
            }
        }
        for (name, expr) in &args.kw {
            if let Some(i) = def.params.iter().position(|p| p.name == *name) {
                bound[i] = eval(expr, frame);
            }
        }
        // Defaults are evaluated inside the callee frame (Jinja semantics):
        // a `b=a` default's `Param(i)` refers to this def's own parameters,
        // so it must read `bound` — the slots already filled above — not
        // the caller's frame, which would fabricate an edge from the wrong
        // argument.
        for (i, p) in def.params.iter().enumerate() {
            if bound[i].is_none() {
                bound[i] = p.default.as_ref().and_then(|d| eval(d, &bound));
            }
        }
        bound
    }

    /// Resolve one invocation of `def` under `bound` — evaluate the
    /// body's dep calls, then recurse into its project-macro calls (their
    /// own args bound through THIS call's frame). `depth` counts DOWN —
    /// at 0 the body's dep calls still resolve but a nested invocation is
    /// reported `incomplete` rather than followed forever.
    fn resolve(
        &self,
        def: &MacroDef,
        bound: &[Option<String>],
        proj: &str,
        depth: usize,
    ) -> Resolution {
        let mut out = Resolution::default();
        for dep in &def.deps {
            match dep_target(dep, bound) {
                Some(DepTarget::Ref(name)) => out.refs.push(name),
                Some(DepTarget::Source(s, t)) => out.sources.push((s, t)),
                None => out.incomplete = true,
            }
        }
        if depth == 0 {
            // The cap hit — a call the descent can't follow counts unless
            // it classifies a proven no-dep builtin.
            out.incomplete |= def.calls.iter().any(|c| {
                !matches!(
                    self.classify_named(&c.ns, &c.name, proj),
                    Callee::Builtin | Callee::DepCall
                )
            });
            return out;
        }
        for call in &def.calls {
            match self.classify_named(&call.ns, &call.name, proj) {
                Callee::Project(inner) => {
                    let inner_bound = Self::bind(inner, &call.args, bound);
                    let inner_res = self.resolve(inner, &inner_bound, proj, depth - 1);
                    out.refs.extend(inner_res.refs);
                    out.sources.extend(inner_res.sources);
                    // The nested callee is NOT recorded on the node —
                    // dbt writes it on the callee's own
                    // `depends_on.macros` (the [`raw_macros`] map), and
                    // consumers reach it through that macro→macro graph.
                    out.incomplete |= inner_res.incomplete;
                }
                // `adapter.dispatch('name')` — the name is data, the real
                // call args live in a second group we can't bind: resolve
                // the dispatched project macro with its frame unbound — a
                // dep needing an unseen param stays `incomplete` on its own.
                Callee::Dispatch => {
                    match call
                        .args
                        .pos
                        .first()
                        .and_then(|e| eval(e, bound))
                        .and_then(|n| self.defs.get(&n))
                    {
                        Some(inner) => {
                            let inner_bound: Vec<Option<String>> = vec![None; inner.params.len()];
                            let inner_res = self.resolve(inner, &inner_bound, proj, depth - 1);
                            out.refs.extend(inner_res.refs);
                            out.sources.extend(inner_res.sources);
                            out.incomplete |= inner_res.incomplete;
                        }
                        None => out.incomplete = true,
                    }
                }
                Callee::DepCall | Callee::Builtin => {}
                Callee::Foreign => out.incomplete = true,
            }
        }
        out
    }

    /// The node-level entry: bind `call`'s arguments against an empty
    /// frame (a model has no enclosing params) and resolve. `call` must
    /// have classified [`Callee::Project`] — enforced by taking the def
    /// that classification already produced.
    pub(super) fn resolve_call(
        &self,
        name: &str,
        def: &MacroDef,
        args_text: &str,
        proj: &str,
    ) -> Resolution {
        let args = classify_args(args_text, &[]);
        let bound = Self::bind(def, &args, &[]);
        let mut res = self.resolve(def, &bound, proj, MAX_MACRO_DEPTH);
        res.macros.insert(name.to_string());
        res
    }

    /// Like [`resolve_call`](Macros::resolve_call) for an
    /// `adapter.dispatch('name')` spelling: the name's literal resolves
    /// the project macro with no arg bindings — `incomplete` only when a
    /// dep needs an unseen parameter (the dispatched call's own args are
    /// unbindable); a dispatched name that isn't a project macro is a
    /// package/internal reach and counts unprovable.
    pub(super) fn resolve_dispatch(&self, args_text: &str, proj: &str) -> Resolution {
        let args = classify_args(args_text, &[]);
        let empty: &[Option<String>] = &[];
        match args.pos.first().and_then(|e| eval(e, empty)) {
            Some(name) if self.defs.contains_key(&name) => {
                let def = &self.defs[&name];
                let mut res = self.resolve(def, empty, proj, MAX_MACRO_DEPTH);
                res.macros.insert(name);
                res
            }
            _ => Resolution {
                incomplete: true,
                ..Default::default()
            },
        }
    }

    /// The callee classification the node edge pass uses — same dispatch
    /// as the body path.
    pub(super) fn classify_callee(&self, name: &str, proj: &str) -> Callee<'_> {
        self.classify(name, proj)
    }

    /// The literal strings a node-level call's argument text carries —
    /// the unresolvable-call record's table-name candidates (a Foreign or
    /// an `incomplete` invocation may still name the source table it
    /// would have read).
    pub(super) fn literal_args_of(args_text: &str) -> Vec<String> {
        literal_args(&classify_args(args_text, &[]))
    }
}

/// Extract `{% macro name(sig) %}…{% endmacro %}` definitions from a
/// Jinja-comment-masked macro file. A definition ends at the NEXT
/// `{% endmacro %}` — Jinja forbids nesting, so no depth tracking. The
/// `depends_on`-relevant calls are collected from the body's tags exactly
/// like a model's would be.
fn macro_defs(text: &str) -> Vec<(String, MacroDef)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        // Find the next `{%` tag.
        let Some(open) = find_subslice(bytes, i, b"{%") else {
            break;
        };
        let Some(close) = jinja_tag_close(bytes, open) else {
            break;
        };
        let body = &text[open + 2..close];
        let (word, rest) = tag_word(body);
        if word != "macro" {
            i = close + 2;
            continue;
        }
        // ` name(sig) ` — the macro name is the leading identifier of the
        // rest; the sig is the `(...)` group that follows (absent for a
        // degenerate `{% macro name %}`).
        let decl = rest.trim_start();
        let nend = decl
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(decl.len());
        let name = &decl[..nend];
        if name.is_empty() {
            i = close + 2;
            continue;
        }
        let after = decl[nend..].trim_start();
        let params = if after.starts_with('(') {
            scan_call_args(after, 1)
                .map(|(sig, _)| parse_params(sig))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // The body runs to the matching `{% endmacro %}`; the next macro
        // scan resumes after that tag (a body can never contain a macro
        // declaration — Jinja forbids nesting).
        let mut body_end = text.len();
        let mut next_i = text.len();
        let mut j = close + 2;
        while let Some(rel) = find_subslice(bytes, j, b"{%") {
            let Some(eclose) = jinja_tag_close(bytes, rel) else {
                break;
            };
            let (eword, _) = tag_word(&text[rel + 2..eclose]);
            if eword == "endmacro" {
                body_end = rel;
                next_i = eclose + 2;
                break;
            }
            j = eclose + 2;
        }
        out.push((
            name.to_string(),
            macro_def(&text[close + 2..body_end], params),
        ));
        i = next_i;
    }
    out
}

/// Reduce a macro body to its dep calls and its other calls — the body's
/// `jinja_calls` split on `ref`/`source`/`config`, each call's args
/// classified against the macro's parameters.
fn macro_def(body: &str, params: Vec<Param>) -> MacroDef {
    let scope: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
    let mut deps = Vec::new();
    let mut calls = Vec::new();
    for call in jinja_calls(body) {
        let args = classify_args(call.args, &scope);
        match call.name {
            "ref" => deps.push(BodyDep::Ref(args)),
            "source" => deps.push(BodyDep::Source(args)),
            "config" => {}
            _ => {
                let (ns, name) = match call.name.split_once('.') {
                    Some((ns, rest)) => (Some(ns.to_string()), rest.to_string()),
                    None => (None, call.name.to_string()),
                };
                calls.push(BodyCall { ns, name, args });
            }
        }
    }
    MacroDef {
        params,
        deps,
        calls,
        sql: body.to_string(),
    }
}

/// Parse a `{% macro %}` signature — `name` parameters in order, each
/// optional `= expr` default classified against the names declared so
/// far.
fn parse_params(sig: &str) -> Vec<Param> {
    let mut raw: Vec<(String, Option<&str>)> = Vec::new();
    for seg in top_level_segments(sig) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        match split_kwarg(seg) {
            Some((name, def)) if is_ident(name) => raw.push((name.to_string(), Some(def))),
            None if is_ident(seg) => raw.push((seg.to_string(), None)),
            _ => {} // `*args`/`**kwargs` — not named parameters
        }
    }
    let names: Vec<String> = raw.iter().map(|(n, _)| n.clone()).collect();
    raw.into_iter()
        .map(|(name, default)| Param {
            name,
            default: default.map(|e| classify_expr(e, &names)),
        })
        .collect()
}

/// The unresolvable-call record `resolve_edges` fills: how many calls on
/// a node stayed unresolvable (`parents` — the node may have more parents
/// than the edges show) and the literal argument strings those calls
/// carried (`candidates` — matched to declared source tables at `finish`,
/// when `self.sources` exists).
#[derive(Debug, Default)]
pub(super) struct UnresolvedCalls {
    /// uid → counted call sites.
    pub(super) parents: HashMap<String, usize>,
    /// Literal string args of counted calls, in call order.
    pub(super) candidates: Vec<String>,
}
