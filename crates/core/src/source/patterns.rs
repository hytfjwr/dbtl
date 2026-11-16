use super::*;
use std::ops::Range;

/// The dbt-syntax extractors, compiled once. The `regex` crate has no
/// backreferences, so each quote is matched independently (`['"]`) — a mismatched
/// `'x"` is vanishingly rare in real SQL and harmless if it slips through. The
/// leading `\b` stops `my_ref(...)` / `data_source(...)` identifiers producing
/// spurious edges. A versioned or kwarg-bearing `ref('x', v=2)` /
/// `ref('x', version=2)` IS matched — the trailing `(?:,[^)]*)?` absorbs any
/// extra args up to (but never past) the call's closing `)`. The `v=`/
/// `version=` kwarg rides the arg tail (`call_sites_masked` records it on
/// `CallKind::Ref`), and edge resolution reads it as a [`RefPin`]: a literal
/// pins the `.vN` member, a nonliteral kwarg approximates to the latest.
pub(super) struct Patterns {
    pub(super) ref_re: Regex,
    pub(super) source_re: Regex,
    pub(super) snapshot_re: Regex,
    pub(super) docs_block_re: Regex,
    pub(super) docs_tag_re: Regex,
}

/// One `{% docs name %}` or `{% enddocs %}` tag, in file order — the input to
/// the pairing state machine in [`collect_doc_blocks`].
pub(super) enum DocTag {
    Open(String),
    Close,
}

impl Patterns {
    pub(super) fn new() -> Self {
        Patterns {
            ref_re: Regex::new(
                r#"\bref\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*['"]([^'"]+)['"]\s*)?(?:,[^)]*)?\)"#,
            )
            .unwrap(),
            source_re: Regex::new(
                r#"\bsource\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#,
            )
            .unwrap(),
            snapshot_re: Regex::new(r"\{%-?\s*snapshot\s+([A-Za-z_][A-Za-z0-9_]*)\s*-?%\}")
                .unwrap(),
            // `{% docs name %}…{% enddocs %}` in .md files: Jinja whitespace
            // control (`{%- -%}`) allowed; non-greedy body up to the first
            // `{% enddocs %}`. Names follow dbt's rule (alnum + underscore,
            // never digit-leading).
            docs_block_re: Regex::new(
                r"(?s)\{%-?\s*docs\s+([A-Za-z_][A-Za-z0-9_]*)\s*-?%\}(.*?)\{%-?\s*enddocs\s*-?%\}",
            )
            .unwrap(),
            // Any `{% docs name %}` or `{% enddocs %}` tag, in order — the
            // pairing state machine (an unclosed or stray tag is a dbt error,
            // degraded to a warning here) walks these.
            docs_tag_re: Regex::new(r"\{%-?\s*(?:docs\s+([A-Za-z_][A-Za-z0-9_]*)|enddocs)\s*-?%\}")
                .unwrap(),
        }
    }

    /// The `(source_name, table_name)` pairs.
    pub(super) fn sources(&self, sql: &str) -> Vec<(String, String)> {
        self.source_re
            .captures_iter(sql)
            .filter_map(|c| {
                Some((
                    c.get(1)?.as_str().to_string(),
                    c.get(2)?.as_str().to_string(),
                ))
            })
            .collect()
    }

    /// The `ref()`/`source()`/`config()` calls in `sql`, ordered by position —
    /// the span-carrying entry point every dep extraction goes through.
    /// The SAME regexes recognize the calls
    /// (extraction semantics are identical: `my_ref(` can't match, an
    /// unquoted or unclosed call is not a call), then a balanced-paren,
    /// quote-aware rescan takes the call's TRUE end — the regex's `[^)]*`
    /// arg tail can stop early at a `)` inside an argument string. When the
    /// rescan fails (a malformed call the regex still recognized, e.g.
    /// `ref('x', 'unclosed)`), the match's own range is the span and the
    /// call reports no arg list.
    ///
    /// `masked` is [`mask_jinja_comments`] output — the callers (the
    /// loader's `node_sql`, lint's `Sites`) already hold it, so the mask
    /// runs once per file, not once per consumer. `{# #}` is the only
    /// thing masked — `--`/`/* */` comments still see calls (dbt renders
    /// Jinja before SQL comments exist) — and masking preserves byte
    /// length, so every returned range indexes the caller's ORIGINAL text.
    ///
    /// One `site_head` pass finds every `ref`/`source`/`config` callee;
    /// the balanced/quote-aware arg scan + top-level segment parse then
    /// validates each head without a second regex evaluation — a call is
    /// a ref/source site exactly when its first positional args are the
    /// quoted literals the kind regexes would have matched (`ref(var)`,
    /// `source('a')`, `ref('a' junk)` → not sites). The kind regexes only
    /// still run when the balanced scan fails (a malformed call the regex
    /// may still recognize, e.g. `ref('x', 'unclosed)` → the regex match
    /// becomes the span, args stay unknown).
    pub fn call_sites_masked(&self, masked: &str) -> Vec<CallSite> {
        let mut out = Vec::new();
        // `config_call_args_list`'s traversal: a `config(` head inside a
        // previous config call's args is skipped (`config_floor`), and an
        // unclosed config swallows the rest — for configs only; ref and
        // source heads inside either still resolve, as the independent
        // full-text scans did.
        let mut config_floor = 0usize;
        for m in site_head_re().find_iter(masked) {
            let head = &masked[m.start()..];
            if head.starts_with("config") {
                if m.start() < config_floor {
                    continue;
                }
                match scan_call_args(masked, m.end()) {
                    Some((args, end)) => {
                        out.push(CallSite {
                            kind: CallKind::Config,
                            span: m.start()..end,
                            args: Some(m.end()..m.end() + args.len()),
                        });
                        config_floor = end;
                    }
                    None => config_floor = usize::MAX,
                }
                continue;
            }
            let is_ref = head.starts_with("ref");
            // The balanced scan takes the call's TRUE arg list; the
            // positional-quoted-literal check below then accepts exactly
            // the calls `ref_re`/`source_re` would — without a second
            // regex pass per call.
            let Some((args_text, end)) = scan_call_args(masked, m.end()) else {
                // A malformed call the regex still recognized (e.g.
                // `ref('x', 'unclosed)`) keeps the regex match as its span
                // and reports no args — the only place the kind regexes
                // still run.
                let re = if is_ref {
                    &self.ref_re
                } else {
                    &self.source_re
                };
                let Some(c) = re.captures_at(masked, m.start()) else {
                    continue;
                };
                let cm = c.get(0).expect("a full match");
                if cm.start() != m.start() {
                    continue;
                }
                let kind = if is_ref {
                    CallKind::Ref {
                        name: c
                            .get(2)
                            .or_else(|| c.get(1))
                            .expect("ref_re's name group")
                            .range(),
                        package: c.get(2).map(|_| c.get(1).expect("ref_re group 1").range()),
                        version: None, // args could not be read
                    }
                } else {
                    CallKind::Source {
                        source: c.get(1).expect("source_re group 1").range(),
                        table: c.get(2).expect("source_re group 2").range(),
                    }
                };
                out.push(CallSite {
                    kind,
                    span: cm.start()..cm.end(),
                    args: None,
                });
                continue;
            };
            let span = m.start()..end;
            let args = m.end()..m.end() + args_text.len();
            let mut segs = top_level_segs(args_text);
            let kind = if is_ref {
                // `refs()`'s rule: the LAST quoted positional arg is the
                // model; when it exists, the first is the package. seg0
                // must be a quoted literal (a `ref(var)` call is not a
                // ref site); a quoted seg1 makes `ref('pkg','x')` →
                // (pkg, x) while `ref('x', v=2)` keeps name = seg0.
                let Some(first) = segs.next().and_then(|s| quoted_lit(masked, s)) else {
                    continue;
                };
                let (name, package) = match segs.next().and_then(|s| quoted_lit(masked, s)) {
                    Some(second) => (second, Some(first)),
                    None => (first, None),
                };
                // The trailing args hold the version kwargs — `ref('x', v=2)`
                // / `ref('x', version=2)`; `v` is dbt's short spelling. The
                // `=` gate skips the kwarg walks for the argless-`=` case
                // (`ref('x')`, `ref('pkg','x')` — most calls).
                let version = args_text
                    .contains('=')
                    .then(|| kwarg(args_text, "v").or_else(|| kwarg(args_text, "version")))
                    .flatten()
                    .and_then(|expr| lit_range(masked, expr));
                CallKind::Ref {
                    name,
                    package,
                    version,
                }
            } else {
                // `sources()`'s shape: EXACTLY two quoted literals.
                let (Some(s1), Some(s2), None) = (segs.next(), segs.next(), segs.next()) else {
                    continue;
                };
                let (Some(source), Some(table)) = (quoted_lit(masked, s1), quoted_lit(masked, s2))
                else {
                    continue;
                };
                CallKind::Source { source, table }
            };
            out.push(CallSite {
                kind,
                span,
                args: Some(args),
            });
        }
        out
    }

    /// The snapshot names declared by `{% snapshot X %}` blocks.
    pub(super) fn snapshots(&self, sql: &str) -> Vec<String> {
        self.snapshot_re
            .captures_iter(sql)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect()
    }

    /// Every `{% docs name %}` / `{% enddocs %}` tag in order, so
    /// [`collect_doc_blocks`] can detect tags dbt would reject (an open with
    /// no matching close, or a stray close).
    pub(super) fn doc_tags(&self, md: &str) -> Vec<DocTag> {
        self.docs_tag_re
            .captures_iter(md)
            .map(|c| match c.get(1) {
                Some(n) => DocTag::Open(n.as_str().to_string()),
                None => DocTag::Close,
            })
            .collect()
    }

    /// The `(name, body)` pairs declared by `{% docs name %}…{% enddocs %}`
    /// blocks. The body is trimmed, matching dbt's `.strip()` on
    /// `block_contents` (verified: interior whitespace/newlines survive).
    pub(super) fn doc_blocks(&self, md: &str) -> Vec<(String, String)> {
        self.docs_block_re
            .captures_iter(md)
            .map(|c| {
                (
                    c.get(1).unwrap().as_str().to_string(),
                    c.get(2).unwrap().as_str().trim().to_string(),
                )
            })
            .collect()
    }

    /// The materialization from `config(materialized='…')`, if present. Both
    /// this and [`enabled`](Patterns::enabled) read only `config(` calls
    /// inside Jinja tags, so a bare `materialized='x'` / `enabled=false`
    /// in SQL text, a `-- comment`, or a WHERE clause can never configure
    /// the node (dbt only honours them inside `config()`). dbt applies
    /// config() calls in order — the LAST call naming `materialized`
    /// wins (verified: `config(materialized='view')` then
    /// `config(materialized='table')` yields `table`) — so the last
    /// parseable string literal is taken; an unresolvable expression
    /// (`var()`) can't override an earlier literal. `scope` carries the
    /// file's branch index: a call in a provably-dead arm or a
    /// `{% macro %}`-style definition body is never read, while a call
    /// in an unselectable arm still contributes — the conservative
    /// read every non-`enabled` key takes.
    pub(super) fn materialized(&self, sql: &str, scope: &JinjaScope) -> Option<String> {
        config_kwarg_exprs(sql, scope, "materialized")
            .into_iter()
            .filter_map(|expr| match lit_value(expr) {
                Some(serde_json::Value::String(s)) => Some(s),
                _ => None,
            })
            .next_back()
    }

    /// What the file's `config(enabled=…)` calls declare, split by
    /// whether the call provably executes — see [`EnabledScan`]. The
    /// value goes through the shared expression evaluator
    /// (`Vars::node_enabled`) — literals, `var()`, comparisons,
    /// `and`/`or`/`not`, `in`, `is` tests, parens, `| as_bool`, and
    /// value-shaped project-macro calls — with the file's bound-name
    /// table marking `{% set %}`-bound and parse-context names
    /// unprovable. An unresolvable `enabled=` on a call that always
    /// runs is a boolean-validation error in dbt — its line lands in
    /// `warnings` now and an [`EvalWarn`] records it so
    /// [`merge_eval_warnings`] can fold the repeats later; on an
    /// unselectable-arm call it stays silent inside the scan (the arm
    /// may never run — [`EnabledScan::resolve`] weighs it instead).
    pub(super) fn enabled(
        &self,
        sql: &str,
        scope: &JinjaScope,
        warnings: &mut Vec<String>,
        evals: &mut Vec<EvalWarn>,
        ctx: &str,
    ) -> EnabledScan {
        // `config()` only exists inside Jinja tags — `{{ }}` AND
        // `{% %}` (`{% do %}`/`{% set _ = %}` invoke it too, and an
        // `{% if config(…) %}`'s own call applies when the condition
        // evaluates). A bare `config(...)` in SQL text or a `--`/`/* */`
        // comment is SQL to dbt, never a call — while `{{ }}`/`{% %}`
        // inside a `--` comment DOES execute (Jinja renders first).
        let mut scan = EnabledScan::default();
        for call in config_calls(sql, scope) {
            let Some(expr) = config_arg(call.args, "enabled") else {
                continue;
            };
            let verdict =
                match scope
                    .vars
                    .node_enabled(expr, scope.branches.bound_names(), scope.macros)
                {
                    Some(b) => {
                        if b {
                            Enabled::On
                        } else {
                            Enabled::Off
                        }
                    }
                    None => {
                        if call.gate.is_none() {
                            evals.push(EvalWarn {
                                expr: expr.to_string(),
                                ctx: ctx.to_string(),
                                pos: warnings.len(),
                                kind: EvalWarnKind::Expr,
                            });
                            warnings.push(eval_warn_line(EvalWarnKind::Expr, expr, ctx));
                        }
                        Enabled::Unproven
                    }
                };
            match call.gate {
                None => {
                    scan.readable = Some(verdict);
                    // The unconditional call overrides every earlier
                    // gated call — they can never be the effective last.
                    scan.gated.clear();
                }
                Some(cond) => scan.gated.push((verdict, cond.to_string())),
            }
        }
        scan
    }
}

/// The merged `meta` dict of EVERY `config(meta={…})` call in `sql`, in call
/// order: dbt applies `config()` calls in sequence and merges each call's
/// meta dict onto the accumulated config PER TOP-LEVEL KEY — a later call
/// replaces only the keys it names (verified: `{'a':1,'b':1}` then `{'b':2}`
/// yields `{'a':1,'b':2}`). A non-dict or unresolvable expression (`var()`,
/// `env_var()`, an identifier) is skipped rather than guessed — an
/// unresolvable LATER call can't clear what an earlier literal set, so the
/// result is a subset of dbt's, never a fabrication.
pub(super) fn config_meta(
    sql: &str,
    scope: &JinjaScope,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    let mut found = false;
    for expr in config_kwarg_exprs(sql, scope, "meta") {
        if let Some(serde_json::Value::Object(m)) = lit_value(expr) {
            out.extend(m);
            found = true;
        }
    }
    found.then_some(out)
}

/// The `unique_key=` literal from the file's `config()` calls — the LAST
/// resolvable one wins (ordered config() application, same as `materialized`).
/// Kept as the raw `Value`: dbt accepts a bare string or a string list and
/// records the declared shape; `Dag::build` normalizes. An unresolvable
/// expression can't override an earlier literal.
pub(super) fn config_unique_key(sql: &str, scope: &JinjaScope) -> Option<serde_json::Value> {
    config_kwarg_exprs(sql, scope, "unique_key")
        .into_iter()
        .filter_map(lit_value)
        .next_back()
}

/// The relation-name override from `config(alias='…')`, if a string literal —
/// the LAST `alias=` across the file's config() calls wins, the same ordered
/// application `materialized`/`enabled` follow. An unresolvable expression
/// (`var()`, an identifier) can't override an earlier literal.
pub(super) fn config_alias(sql: &str, scope: &JinjaScope) -> Option<String> {
    config_kwarg_exprs(sql, scope, "alias")
        .into_iter()
        .filter_map(|expr| match lit_value(expr) {
            Some(serde_json::Value::String(s)) => Some(s),
            _ => None,
        })
        .next_back()
}

/// The governance/config kwargs of the file's `config(…)` calls — `access`,
/// `group`, `contract`, `incremental_strategy` — resolved in ONE pass so the
/// extraction cost stays flat as the kwarg set grows (each
/// [`config_kwarg_exprs`] reader re-scans the whole file). Per kwarg the
/// semantics are unchanged: calls apply in order, the LAST resolvable
/// literal wins, and an unresolvable expression can't override an earlier
/// literal (the same stance `config_meta`/`config_unique_key` take).
/// `contract` accepts the `{'enforced': bool}` dict or a bare bool.
#[derive(Debug, Default)]
pub(super) struct FileConfig {
    pub(super) access: Option<String>,
    pub(super) group: Option<String>,
    pub(super) contract: Option<RawContract>,
    pub(super) incremental_strategy: Option<String>,
}

pub(super) fn config_governance(sql: &str, scope: &JinjaScope) -> FileConfig {
    let mut out = FileConfig::default();
    for args in config_call_args_list(sql, scope) {
        for (name, slot) in [
            ("access", &mut out.access),
            ("group", &mut out.group),
            ("incremental_strategy", &mut out.incremental_strategy),
        ] {
            if let Some(serde_json::Value::String(s)) = config_arg(args, name).and_then(lit_value) {
                *slot = Some(s);
            }
        }
        if let Some(c) = config_arg(args, "contract")
            .and_then(lit_value)
            .and_then(|v| contract_value(&v))
        {
            out.contract = Some(c);
        }
    }
    out
}

/// The `\bconfig\s*\(` finder [`config_call_args_list`] uses.
fn config_re() -> &'static Regex {
    static CONFIG: OnceLock<Regex> = OnceLock::new();
    CONFIG.get_or_init(|| Regex::new(r"\bconfig\s*\(").unwrap())
}

/// Any `ref`/`source`/`config` callee — [`Patterns::call_sites_masked`]'s
/// single full-text pass; the kind regexes then re-validate each head.
fn site_head_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:ref|source|config)\s*\(").unwrap())
}

/// The Jinja-level context every `config_*` reader shares for one file —
/// the branch index [`jinja_branches`] built (arm paths, per-arm
/// conditions, bound names, deferred definition bodies) plus the node's
/// `var()` scope and the project macro table for condition evaluation.
/// Building it once per file keeps the readers a pure map over
/// [`config_calls`].
pub(super) struct JinjaScope<'v> {
    /// The file's arm/bound-name/deferred-body index.
    branches: JinjaBranches,
    /// The node's `var()` scope — `Vars::node_condition` resolves an
    /// arm's condition through it.
    vars: &'v Vars,
    /// The project's macro table — `Vars::node_condition`'s
    /// `name(…)`/`ns.name(…)` evaluation resolves through it.
    macros: &'v Macros,
}

impl<'v> JinjaScope<'v> {
    /// Index `text` (the Jinja-comment-masked file body) once for every
    /// `config_*` reader of the file.
    pub(super) fn new(text: &str, vars: &'v Vars, macros: &'v Macros) -> Self {
        JinjaScope {
            branches: jinja_branches(text),
            vars,
            macros,
        }
    }

    /// True when the position provably never runs in the model render —
    /// a dead arm or a `{% macro %}`-style deferred definition body. A
    /// merely unprovable arm is NOT dead: its calls may execute.
    pub(super) fn dead(&self, text: &str, off: usize) -> bool {
        // A call inside an `{% elif %}` condition — `chain.conds[k]` for
        // k > 0 is exactly the k-th elif's expression — evaluates when the
        // earlier arms were falsy, which `arm_path` alone can't say.
        let boundary = self.branches.chains.iter().any(|c| {
            c.conds
                .iter()
                .enumerate()
                .any(|(k, r)| k > 0 && r.contains(&off))
        });
        let verdict = if boundary {
            exec_arm_boundary(text, &self.branches, self.vars, self.macros, off)
        } else {
            exec_at(text, &self.branches, self.vars, self.macros, off)
        };
        matches!(verdict, Exec::Inactive)
    }
}

/// One `config(…)` call site inside a Jinja tag, with the render verdict
/// [`exec_at`] left it: `gate` names the unprovable arm condition guarding
/// the call (`None` — it executes on every render). Calls provably dead —
/// a never-taken arm, a `{% macro %}`-style definition body — never reach
/// the list.
struct ConfigCall<'a> {
    /// The text between the call's parens.
    args: &'a str,
    /// The unprovable condition of the arm gating the call, when gated.
    gate: Option<&'a str>,
}

/// Whether a source position executes on every render, sits behind an
/// arm whose condition source can't prove, or can never execute.
enum Exec<'a> {
    /// Unconditional — the position renders whenever the file does.
    Active,
    /// An unprovable arm guards the position — the `&str` is the
    /// condition's source text (for the warning naming it).
    Gated(&'a str),
    /// The position never executes in the model render — an arm whose
    /// condition provably failed (or whose earlier arm provably took the
    /// chain), or a `{% macro %}`/`{% test %}`/`{% docs %}`/
    /// `{% materialization %}`/`{% verification %}` definition body the
    /// render never runs top-level.
    Inactive,
}

/// [`exec_at`] for a position inside an `{% elif %}`/`{% else %}` tag —
/// the tag's own expressions evaluate when its chain's EARLIER arms were
/// all falsy. The arm path's last step names the arm about to close, so
/// the gate is that chain's `conds[0..=arm]` — a proven-true earlier arm
/// means the tag never runs; an unprovable one gates it. (`{% endif %}`/
/// `{% end… %}` positions keep the plain read — an end tag holds no
/// valid expression anyway.)
fn exec_arm_boundary<'a>(
    text: &'a str,
    branches: &JinjaBranches,
    vars: &Vars,
    macros: &Macros,
    off: usize,
) -> Exec<'a> {
    if branches.in_deferred(off) {
        return Exec::Inactive;
    }
    let path = branches.arm_path(off);
    let Some((last, rest)) = path.split_last() else {
        // An `elif`/`else` outside a chain — Jinja rejects it; nothing
        // provable, keep the conservative read.
        return Exec::Active;
    };
    let mut gate = None;
    for step in rest {
        match step_exec(text, branches, vars, macros, step) {
            Exec::Inactive => return Exec::Inactive,
            Exec::Gated(cond) => gate = gate.or(Some(cond)),
            Exec::Active => {}
        }
    }
    let chain = &branches.chains[last.chain as usize];
    if chain.kind == ArmKind::For {
        // A for-chain's `{% else %}` evaluates when the loop ran empty —
        // unprovable like the loop itself.
        gate = gate.or(Some(&text[chain.header.clone()]));
    } else {
        for r in &chain.conds[..(last.arm as usize + 1).min(chain.conds.len())] {
            match vars.node_condition(&text[r.clone()], &branches.bound, macros) {
                Some(true) => return Exec::Inactive,
                Some(false) => {}
                None => gate = gate.or(Some(&text[r.clone()])),
            }
        }
    }
    match gate {
        Some(g) => Exec::Gated(g),
        None => Exec::Active,
    }
}

/// The [`Exec`] verdict of the position `off` — first the deferred-body
/// check (a definition body's config applies only when the definition is
/// invoked, which a parse can't prove), then the arm path folded against
/// each chain's conditions. A chain's arm `k` requires `conds[0..k]` all
/// false and `conds[k]` true; a proven-true earlier cond parks the arm
/// (`Inactive`), a proven-false own cond the same, and any unprovable
/// cond gates it — the FIRST unprovable one names the gate. A `{% for %}`
/// chain's arms can never be selected from source — the loop header
/// (`x in expr`) is the gate text.
fn exec_at<'a>(
    text: &'a str,
    branches: &JinjaBranches,
    vars: &Vars,
    macros: &Macros,
    off: usize,
) -> Exec<'a> {
    if branches.in_deferred(off) {
        return Exec::Inactive;
    }
    let mut gate = None;
    for step in branches.arm_path(off) {
        match step_exec(text, branches, vars, macros, step) {
            Exec::Inactive => return Exec::Inactive,
            Exec::Gated(cond) => gate = gate.or(Some(cond)),
            Exec::Active => {}
        }
    }
    match gate {
        Some(g) => Exec::Gated(g),
        None => Exec::Active,
    }
}

/// One arm step's contribution to [`exec_at`]/[`exec_arm_boundary`]:
/// `Inactive` when the arm can never be selected, `Gated` carrying the
/// first unprovable condition text when selection is open but unproven.
fn step_exec<'a>(
    text: &'a str,
    branches: &JinjaBranches,
    vars: &Vars,
    macros: &Macros,
    step: &ArmStep,
) -> Exec<'a> {
    let chain = &branches.chains[step.chain as usize];
    if chain.kind == ArmKind::For {
        return Exec::Gated(&text[chain.header.clone()]);
    }
    let mut gate = None;
    for r in &chain.conds[..(step.arm as usize).min(chain.conds.len())] {
        match vars.node_condition(&text[r.clone()], &branches.bound, macros) {
            Some(true) => return Exec::Inactive,
            Some(false) => {}
            None => gate = gate.or(Some(&text[r.clone()])),
        }
    }
    if (step.arm as usize) < chain.conds.len() {
        match vars.node_condition(
            &text[chain.conds[step.arm as usize].clone()],
            &branches.bound,
            macros,
        ) {
            Some(false) => return Exec::Inactive,
            Some(true) => {}
            None => gate = gate.or(Some(&text[chain.conds[step.arm as usize].clone()])),
        }
    }
    match gate {
        Some(g) => Exec::Gated(g),
        None => Exec::Active,
    }
}

/// Every `config(…)` call inside `sql`'s Jinja tags — `{{ }}` AND
/// `{% %}` bodies (`{% do config(…) %}` and `{% set _ = config(…) %}`
/// invoke it identically at render) — in source order, each carrying its
/// [`Exec`] verdict. The bare `\bconfig\s*\(` finder inside the tag body
/// is balanced-paren/quote rescanned like everywhere else, so a nested
/// `(`/`)` inside dicts or macro calls can't end the call early.
fn config_calls<'a>(sql: &'a str, scope: &JinjaScope) -> Vec<ConfigCall<'a>> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < b.len() {
        if b[i] == b'{' && matches!(b[i + 1], b'{' | b'%') {
            if let Some(end) = jinja_tag_close(b, i) {
                let body = &sql[i + 2..end];
                // `{% elif %}` bodies sit at their chain's boundary —
                // their expressions evaluate when the earlier arms were
                // falsy, which the arm index alone can't say. The
                // declaration/raw-payload tags (`{% macro config(…) %}`
                // is a SIGNATURE, not a call) and every `{% end… %}`
                // close carry no call.
                let boundary = if b[i + 1] == b'%' {
                    let (word, _) = tag_word(body);
                    if word.starts_with("end") || NO_CALL_TAGS.contains(&word) {
                        i = end + 2;
                        continue;
                    }
                    matches!(word, "elif" | "elseif")
                } else {
                    false
                };
                let mut pos = 0usize;
                while let Some(m) = config_re().find_at(body, pos) {
                    match scan_call_args(body, m.end()) {
                        Some((args, call_end)) => {
                            // The call head's offset in `sql` — the arm
                            // index and deferred ranges are keyed on it.
                            let head = i + 2 + m.start();
                            let verdict = if boundary {
                                exec_arm_boundary(
                                    sql,
                                    &scope.branches,
                                    scope.vars,
                                    scope.macros,
                                    head,
                                )
                            } else {
                                exec_at(sql, &scope.branches, scope.vars, scope.macros, head)
                            };
                            match verdict {
                                Exec::Active => out.push(ConfigCall { args, gate: None }),
                                Exec::Gated(cond) => out.push(ConfigCall {
                                    args,
                                    gate: Some(cond),
                                }),
                                Exec::Inactive => {}
                            }
                            pos = call_end;
                        }
                        // An unclosed call swallows the rest of the tag
                        // body — nothing follows it inside.
                        None => break,
                    }
                }
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The argument text of every `config(…)` call `sql`'s render may execute,
/// in call order — [`config_calls`] with the branch bookkeeping dropped,
/// for the readers whose keys take the conservative "an unselectable arm
/// still contributes" stance (every key but `enabled`).
fn config_call_args_list<'a>(sql: &'a str, scope: &JinjaScope) -> Vec<&'a str> {
    config_calls(sql, scope)
        .into_iter()
        .map(|c| c.args)
        .collect()
}

/// What a file's `config(enabled=…)` calls declare, split by whether the
/// call provably executes — [`Patterns::enabled`]'s return.
///
/// `readable` is the last-wins verdict over the calls that run on every
/// render — unconditional positions plus provably-taken arms. `gated`
/// is every call sitting behind a condition source can't prove that
/// could be the EFFECTIVE last call — those after the last unconditional
/// `enabled=` call (an unconditional call overrides every earlier gated
/// one, so pre-`readable` gated calls can never win). Calls in a
/// provably-dead arm or a `{% macro %}`-style definition body reach
/// neither. A non-`config()` channel — a `.py` `dbt.config` literal, a
/// YAML `config.enabled:` — has no arms; [`EnabledScan::plain`] wraps it.
#[derive(Debug, Clone, Default)]
pub(super) struct EnabledScan {
    /// The verdict over calls that provably execute — `None` when no
    /// such call declares `enabled=` (the unselectable-arm calls alone
    /// do not fill this).
    readable: Option<Enabled>,
    /// `(verdict, gate condition)` per unselectable-arm call that could
    /// be the effective last `enabled=` call, in source order.
    gated: Vec<(Enabled, String)>,
}

impl EnabledScan {
    /// A channel with no Jinja branch context — the one verdict stands.
    pub(super) fn plain(v: Option<Enabled>) -> EnabledScan {
        EnabledScan {
            readable: v,
            gated: Vec::new(),
        }
    }

    /// Did the file declare `enabled=` at all?
    pub(super) fn declared(&self) -> bool {
        self.readable.is_some() || !self.gated.is_empty()
    }

    /// The file's own `enabled=false` is PROVEN — every call that may
    /// execute agrees (a verdict leaning on an unselectable arm is never
    /// proven) — so the resource parks like dbt's `disabled` whatever the
    /// lower channels say.
    pub(super) fn proven_off(&self) -> bool {
        self.readable == Some(Enabled::Off) && self.gated.iter().all(|(v, _)| *v == Enabled::Off)
    }

    /// The in-file channel's verdict folded over `fallback` — the lower-
    /// precedence levels' already-resolved verdict (schema
    /// `config.enabled:` then the tree's `+enabled`). Any gated call
    /// whose verdict differs from the unconditional outcome can be the
    /// effective last call on some render, so the verdict is
    /// [`Enabled::Unproven`] — the node stays enabled and marked, never
    /// silently disabled — and a [`EvalWarn`] names that call's
    /// unprovable condition (`ctx` names the file the branch lives in)
    /// so [`merge_eval_warnings`] can fold the repeats later.
    pub(super) fn resolve(
        &self,
        fallback: Option<Enabled>,
        warnings: &mut Vec<String>,
        evals: &mut Vec<EvalWarn>,
        ctx: &str,
    ) -> Option<Enabled> {
        let without = self.readable.or(fallback);
        let base = verdict_value(without);
        for (v, gate) in &self.gated {
            if *v != base {
                evals.push(EvalWarn {
                    expr: gate.clone(),
                    ctx: ctx.to_string(),
                    pos: warnings.len(),
                    kind: EvalWarnKind::Gate,
                });
                warnings.push(eval_warn_line(EvalWarnKind::Gate, gate, ctx));
                return Some(Enabled::Unproven);
            }
        }
        // No divergence — every gated call agrees with the unconditional
        // outcome, so its declaration still counts as the file's verdict.
        without.or_else(|| self.gated.first().map(|(v, _)| *v))
    }
}

/// One "cannot be evaluated" report at its file — the record
/// [`merge_eval_warnings`] folds so the same expression across N files
/// lands as one line. The report's own line is pushed to the warnings
/// vec at record time (`pos` marks it); the fold rewrites the group's
/// first line and drops the rest.
pub(super) struct EvalWarn {
    /// The raw expression text — the `enabled=` argument for
    /// [`EvalWarnKind::Expr`], the branch gate condition for
    /// [`EvalWarnKind::Gate`].
    pub(super) expr: String,
    /// The file the evaluation ran in — the single-file message's
    /// subject and the folded line's `(first: …)`.
    pub(super) ctx: String,
    /// The report's line's index in the warnings vec — where the folded
    /// line lands (the warnings-order contract keeps first position).
    pub(super) pos: usize,
    /// Which "cannot be evaluated" wording this record folds into.
    pub(super) kind: EvalWarnKind,
}

/// The two "cannot be evaluated" wordings [`merge_eval_warnings`]
/// folds — they never share a line.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum EvalWarnKind {
    /// `config(enabled={expr}) could not be evaluated as a boolean`.
    Expr,
    /// `… sits in a Jinja branch whose condition cannot be evaluated
    /// ({expr})`.
    Gate,
}

/// The single-file spelling an [`EvalWarn`]'s own line takes — what a
/// lone report keeps.
pub(super) fn eval_warn_line(kind: EvalWarnKind, expr: &str, ctx: &str) -> String {
    match kind {
        EvalWarnKind::Expr => format!(
            "{ctx}: config(enabled={expr}) could not be evaluated as a boolean; the resource stays enabled"
        ),
        EvalWarnKind::Gate => format!(
            "{ctx}: config(enabled=…) sits in a Jinja branch whose condition cannot be evaluated ({expr}); the resource stays enabled"
        ),
    }
}

/// Fold the "cannot be evaluated" reports by (wording, whitespace-
/// normalized expression): DISTINCT files carrying the same expression
/// land as ONE line — `… in {N} files (first: {first})` — while a
/// report group in one file keeps its per-file wording (repeats drop
/// like the exact-dedup did before). The folded line sits where its
/// first report did, so first-seen order survives (the warnings-order
/// contract).
pub(super) fn merge_eval_warnings(warnings: &mut Vec<String>, evals: &[EvalWarn]) {
    // (wording, normalized expression) -> its reports, in push order
    let mut slots: HashMap<(EvalWarnKind, String), usize> = HashMap::new();
    let mut groups: Vec<((EvalWarnKind, String), Vec<&EvalWarn>)> = Vec::new();
    for e in evals {
        let key = (e.kind, normalize_ws(&e.expr));
        match slots.get(&key) {
            Some(&i) => groups[i].1.push(e),
            None => {
                slots.insert(key.clone(), groups.len());
                groups.push((key, vec![e]));
            }
        }
    }
    let mut drop: BTreeSet<usize> = BTreeSet::new();
    let mut rewrite: HashMap<usize, String> = HashMap::new();
    for ((kind, expr), members) in groups {
        // Every report after the first folds away — what the first
        // becomes depends on whether the group spans files.
        drop.extend(members[1..].iter().map(|e| e.pos));
        let n = members
            .iter()
            .map(|e| e.ctx.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if n == 1 {
            continue; // same file — the first keeps its per-file line
        }
        let first = &members[0].ctx;
        let line = match kind {
            EvalWarnKind::Expr => format!(
                "config(enabled={expr}) could not be evaluated as a boolean in {n} files (first: {first}); those resources stay enabled"
            ),
            EvalWarnKind::Gate => format!(
                "config(enabled=…) sits in a Jinja branch whose condition cannot be evaluated ({expr}) in {n} files (first: {first}); those resources stay enabled"
            ),
        };
        rewrite.insert(members[0].pos, line);
    }
    if drop.is_empty() {
        return;
    }
    let mut i = 0usize;
    warnings.retain_mut(|w| {
        let cur = i;
        i += 1;
        if let Some(line) = rewrite.get(&cur) {
            *w = line.clone();
        }
        !drop.contains(&cur)
    });
}

/// Whitespace-collapsed spelling for the warning-fold key — every run
/// of whitespace (newlines included) becomes one space, edges trimmed.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The outcome an `enabled` verdict leaves on the node — an absent
/// channel leaves the default (enabled), so the `readable`/`gated`
/// compare ignores which level declared it.
fn verdict_value(v: Option<Enabled>) -> Enabled {
    v.unwrap_or(Enabled::On)
}

/// Scan the call whose `(` ends at `start`: returns the text between the
/// parens and the position just past the closing `)`. A bare `]`/`}` at
/// depth 0 or an unclosed call is malformed → `None`.
pub(super) fn scan_call_args(sql: &str, start: usize) -> Option<(&str, usize)> {
    // Byte-wise: every delimiter is ASCII and UTF-8 continuation bytes are
    // >= 0x80, so a multi-byte char can never fake a `(`, `)`, or quote.
    let bytes = sql.as_bytes();
    let mut depth = 0usize;
    let mut pos = start;
    while pos < bytes.len() {
        match bytes[pos] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return (bytes[pos] == b')').then(|| (&sql[start..pos], pos + 1));
                }
                depth -= 1;
            }
            q @ (b'\'' | b'"') => {
                pos += 1;
                while pos < bytes.len() {
                    let b = bytes[pos];
                    pos += 1;
                    if b == b'\\' {
                        pos += 1; // consume the escaped byte
                    } else if b == q {
                        break;
                    }
                }
                continue;
            }
            _ => {}
        }
        pos += 1;
    }
    None // unclosed call
}

/// The expression bound to kwarg `name` in EVERY `config(…)` call in `sql`,
/// in call order — the input to last-wins scalar kwargs (`materialized`,
/// `enabled`) and per-key-merged dict kwargs (`meta`). Each call's binding
/// comes from [`config_arg`], so a positional-dict call's `"name": v`
/// entries and a trailing `**{…}` spread's entries take part in the same
/// ordered sequence as the `name=v` kwargs. `scope` filters the calls to
/// what the render may execute — the conservative read a non-`enabled`
/// key takes (an unselectable arm still contributes).
pub(super) fn config_kwarg_exprs<'a>(sql: &'a str, scope: &JinjaScope, name: &str) -> Vec<&'a str> {
    config_call_args_list(sql, scope)
        .into_iter()
        .filter_map(|args| config_arg(args, name))
        .collect()
}

/// Every `config(tags=…)` entry in `sql`, in call order — the in-file
/// channel of a node's tag list, which dbt places between the project
/// `+tags` and the schema `config.tags` (the wire `config.tags` order is
/// the same list with the two channels swapped — verified on 1.12.6).
/// Multiple calls accumulate; each literal extends the list.
pub(super) fn config_tags(sql: &str, scope: &JinjaScope) -> Vec<String> {
    let mut out = Vec::new();
    for expr in config_kwarg_exprs(sql, scope, "tags") {
        if let Some(v) = lit_value(expr) {
            out.extend(tag_values_of(&v));
        }
    }
    out
}

/// A tag list's string form — dbt's `config.tags`/`+tags` coercion,
/// shared by the in-file readers here and the YAML readers in
/// [`super::yaml`]: a bare scalar wraps to a one-element list, each list
/// element stringifies (`42` → `"42"`, `true` → `"true"` — verified on
/// 1.12.6), and a non-scalar element is dropped.
pub(crate) fn tag_values_of(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::Array(items) => items.iter().filter_map(scalar_to_string).collect(),
        scalar => scalar_to_string(scalar).into_iter().collect(),
    }
}

/// The expression bound to `name` in ONE `config(…)` call's argument list.
/// Only the shapes dbt itself accepts contribute (verified on 1.12.6 —
/// every other argument shape is a Jinja or "Invalid inline model config"
/// error, so the call applies NOTHING):
///
/// - `config(name=expr, …)` — keyword arguments, at most one trailing
///   `**{literal}` spread appended. Jinja's call grammar allows a single
///   `**` and only LAST (`config(**{…}, a=1)` and `config(**{…}, **{…})`
///   are both "invalid syntax for function call expression" — verified);
///   a repeated kwarg, a key the spread shares with an explicit kwarg, or
///   a non-string literal key in the spread (`**{1: 'x'}` — Python's
///   "keywords must be strings" TypeError) are likewise errors. The
///   spread's `"name":` entries bind like a kwarg — they can never
///   collide with an explicit `name=` in a call that survived validation.
/// - `config({…})` — a single positional dict literal, the dict spelling
///   of the same kwargs. `config(*[{'k': v}])`/`config(*({'k': v},))` —
///   a `*` spread unpacking a sequence of exactly one dict — resolves to
///   the same dict shape.
///
/// A `*`/`**` over a NON-literal (`**var('cfg')`) is none of these: dbt
/// evaluates the operand at render, so the call either applies whole or
/// errors there. What a static extractor CAN still prove is the explicit
/// kwargs, so those still bind ([`ConfigShape::RenderSpread`]) and the
/// caller warns once — matching what the pre-validation readers did.
///
/// A positional arg that isn't the sole dict (`config('x')`, `config(None)`),
/// a dict anywhere alongside other args, `*{…}` (unpacks a dict's KEYS
/// into positional strings) — all rejected outright. A bare `config()`
/// binds nothing but is not an error.
pub(crate) fn config_arg<'a>(args: &'a str, name: &str) -> Option<&'a str> {
    match config_args(args) {
        ConfigShape::Kwargs { spread } => {
            kwarg(args, name).or_else(|| spread.and_then(|s| dict_key(s, name)))
        }
        ConfigShape::Dict(inner) => dict_key(inner, name),
        // The non-literal operand resolves at render; the explicit
        // kwargs — and a trailing `**{…}` literal, whose entries a `*`
        // can never shadow — are statically provable.
        ConfigShape::RenderSpread { spread } => {
            kwarg(args, name).or_else(|| spread.and_then(|s| dict_key(s, name)))
        }
        ConfigShape::Invalid => None,
    }
}

/// Warn once per `config(…)` call in `sql`'s Jinja tags whose argument
/// shape is not one dbt accepts (the call contributes no config — a parse
/// error in dbt) or whose `*`/`**` operand isn't a literal (dbt evaluates
/// it at render; only the call's explicit kwargs can be read statically).
/// Scoping to the Jinja tags — `{{ }}` and `{% %}` alike — keeps a
/// `config(` appearing in plain SQL text — a column or function name —
/// from warning like a real config call; a call provably never executed
/// (a dead arm, a definition body) doesn't warn either.
pub(super) fn config_shape_warnings(
    sql: &str,
    scope: &JinjaScope,
    warnings: &mut Vec<String>,
    ctx: &str,
) {
    for call in config_calls(sql, scope) {
        match config_args(call.args) {
            ConfigShape::Invalid => warnings.push(format!(
                "{ctx}: a config(...) call's arguments are not a shape dbt accepts (kwargs or a single dict); its config is ignored"
            )),
            ConfigShape::RenderSpread { .. } => warnings.push(format!(
                "{ctx}: a config(...) call's * or ** operand is not a literal; only the call's keyword arguments are applied"
            )),
            _ => {}
        }
    }
}

/// `args` split into the valid [`config_arg`] shapes.
enum ConfigShape<'a> {
    /// Keyword arguments only; `spread` is the inner text of the one
    /// allowed trailing `**{…}` argument.
    Kwargs { spread: Option<&'a str> },
    /// The inner text of the sole positional dict.
    Dict(&'a str),
    /// A `*`/`**` over a non-literal operand — dbt evaluates it at render
    /// (applying the whole call or erroring there); the explicit kwargs
    /// still bind, as does a trailing `**{…}` literal's entries (`*`
    /// operands produce only positional args — they can't collide).
    RenderSpread { spread: Option<&'a str> },
    /// A call shape dbt errors on — ignored entirely.
    Invalid,
}

fn config_args<'a>(args: &'a str) -> ConfigShape<'a> {
    // `config()` — no arguments — is a valid call that binds nothing.
    if args.trim().is_empty() {
        return ConfigShape::Kwargs { spread: None };
    }
    let segs = top_level_segments(args);
    // Python accepts a trailing comma (`config(a=1,)`): a whitespace-only
    // LAST segment is no argument. An empty one anywhere else is a syntax
    // error (`config(a=1,, b=2)`, `config(,a=1)`).
    let segs = match segs.split_last() {
        Some((last, rest)) if last.trim().is_empty() => rest,
        _ => &segs[..],
    };
    if segs.is_empty() || segs.iter().any(|s| s.trim().is_empty()) {
        return ConfigShape::Invalid;
    }
    let mut spread: Option<&str> = None;
    let mut spread_names: Vec<String> = Vec::new();
    let mut kwarg_names: Vec<&str> = Vec::new();
    let mut positionals: Vec<ConfigShape<'a>> = Vec::new();
    let mut render_spread = false;
    for (i, seg) in segs.iter().enumerate() {
        let seg = seg.trim();
        if let Some(rest) = seg.strip_prefix("**") {
            // Jinja's call grammar allows a single `**` and only LAST
            // (verified: `config(**{…}, a=1)` and a second `**` are both
            // "invalid syntax for function call expression"), and a
            // `{dict}` positional before it still mixes a positional with
            // kwargs.
            if i + 1 != segs.len() || spread.is_some() || !positionals.is_empty() {
                return ConfigShape::Invalid;
            }
            match dict_inner(rest.trim()) {
                Some(inner) => {
                    // Its string keys are readable like a positional
                    // dict's — a key an explicit kwarg already bound
                    // makes the call a Python "multiple values" error;
                    // a literal NON-string key makes it a "keywords must
                    // be strings" TypeError (verified); a key that isn't
                    // a literal can't be judged and is skipped the way
                    // `dict_key` skips.
                    for entry in top_level_segments(inner) {
                        if let Some((k, _)) = dict_entry(entry) {
                            match lit_value(k) {
                                Some(serde_json::Value::String(k)) => spread_names.push(k),
                                Some(_) => return ConfigShape::Invalid,
                                None => {}
                            }
                        }
                    }
                    spread = Some(inner);
                }
                // `**var('x')` resolves at render — dbt applies the whole
                // call or errors there; the literal kwargs still bind.
                None => render_spread = true,
            }
            continue;
        }
        if let Some(rest) = seg.strip_prefix('*') {
            // A `*` spread: `*[{'k': v}]`/`*({'k': v},)` unpacks a
            // one-dict sequence into the positional dict (verified);
            // `*{…}` unpacks a dict's keys into positional strings — a
            // dbt error — and `*var('x')` resolves at render like `**`.
            match seq_inner(rest.trim()) {
                Some(inner) => positionals.extend(
                    top_level_segments(inner)
                        .iter()
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            dict_inner(s)
                                .map(ConfigShape::Dict)
                                .unwrap_or(ConfigShape::Invalid)
                        }),
                ),
                None if rest.trim().starts_with('{') => {
                    positionals.push(ConfigShape::Invalid);
                }
                None => render_spread = true,
            }
            continue;
        }
        if let Some((kwname, _)) = kwarg_split(seg) {
            // `f(k=1, k=2)` is a Python "keyword argument repeated" error.
            if kwarg_names.contains(&kwname) {
                return ConfigShape::Invalid;
            }
            kwarg_names.push(kwname);
        } else {
            positionals.push(
                dict_inner(seg)
                    .map(ConfigShape::Dict)
                    .unwrap_or(ConfigShape::Invalid),
            );
        }
    }
    // The `**` spread's keys may not repeat an explicit kwarg name —
    // Python's "multiple values" error (verified).
    if spread_names
        .iter()
        .any(|k| kwarg_names.contains(&k.as_str()))
    {
        return ConfigShape::Invalid;
    }
    // A member that is itself invalid — a non-dict positional, a `*{…}` —
    // is a dbt error regardless of what a render-time spread resolves to.
    // So are ≥2 positional dicts: a `*` unpack can only ADD positional
    // arguments at render, never remove them.
    if positionals.len() > 1
        || positionals
            .iter()
            .any(|p| matches!(p, ConfigShape::Invalid))
    {
        return ConfigShape::Invalid;
    }
    if render_spread {
        return ConfigShape::RenderSpread { spread };
    }
    match positionals.as_slice() {
        [] => ConfigShape::Kwargs { spread },
        // One positional dict ALONE is the dict spelling — nothing else
        // may accompany it (`config({…}, a=1)` and `config(a=1, {…})` are
        // both "Invalid inline model config" in dbt).
        [ConfigShape::Dict(inner)] if kwarg_names.is_empty() && spread.is_none() => {
            ConfigShape::Dict(inner)
        }
        _ => ConfigShape::Invalid,
    }
}

/// The inner text of a `{…}` literal, or `None` for any other expression.
fn dict_inner(seg: &str) -> Option<&str> {
    seg.strip_prefix('{')?.strip_suffix('}')
}

/// The inner text of a `[…]`/`(…)` sequence literal — a `*` spread's
/// operand — or `None` for any other expression.
fn seq_inner(seg: &str) -> Option<&str> {
    if let Some(rest) = seg.strip_prefix('[') {
        return rest.strip_suffix(']');
    }
    seg.strip_prefix('(')?.strip_suffix(')')
}

/// Split a `name=expr` kwarg segment at its top-level `=`, returning the
/// bound name when it is a bare identifier (anything else — a comparison
/// `a==1`, a walrus `x:=v`, a `*`-expression — is not a kwarg binding).
fn kwarg_split(seg: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut chars = seg.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '\'' | '"' => {
                let it = chars.by_ref();
                while let Some((_, d)) = it.next() {
                    if d == '\\' {
                        it.next();
                    } else if d == c {
                        break;
                    }
                }
            }
            '=' if depth == 0 => {
                // `==`/`!=`/`<=`/`>=`/`:=` are operators, not bindings —
                // the identifier check rejects them along with every
                // non-identifier left side.
                if seg[..i].trim().ends_with(['<', '>', '!', ':']) {
                    return None;
                }
                if i + 1 < seg.len() && seg[i + 1..].starts_with('=') {
                    return None;
                }
                let name = seg[..i].trim();
                return is_identifier(name).then(|| (name, seg[i + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

/// The value text bound to `"name":` in a dict literal's INNER text —
/// used by the positional `{…}` spelling and the `**{…}` spread alike.
/// Entries are read leniently: a key that isn't a string literal, or an
/// entry whose `:` can't be read, is skipped rather than failing the
/// dict — the same per-key stance the kwarg readers take on unresolvable
/// expressions. A duplicate key yields its LAST entry, the Python-dict rule.
fn dict_key<'a>(inner: &'a str, name: &str) -> Option<&'a str> {
    let mut found = None;
    for seg in top_level_segments(inner) {
        let Some((key, value)) = dict_entry(seg) else {
            continue;
        };
        if lit_value(key).is_some_and(|k| k.as_str() == Some(name)) {
            found = Some(value);
        }
    }
    found
}

/// Split one positional-dict entry at its `key: value` colon — the FIRST
/// `:` at bracket-depth 0 outside quotes. An entry without a top-level
/// colon (a set member like `{1, 2}`, or a `**merge` spread) yields `None`.
fn dict_entry(seg: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut chars = seg.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '\'' | '"' => {
                let it = chars.by_ref();
                while let Some((_, d)) = it.next() {
                    if d == '\\' {
                        it.next();
                    } else if d == c {
                        break;
                    }
                }
            }
            ':' if depth == 0 => return Some((seg[..i].trim(), seg[i + 1..].trim())),
            _ => {}
        }
    }
    None
}

/// Parse `expr` as a complete literal: the whole text must be a single
/// [`LitParser`] value — trailing tokens mean it wasn't a clean literal
/// (`'x' or 'y'`-style expressions dbt evaluates but source mode can't).
pub(crate) fn lit_value(expr: &str) -> Option<serde_json::Value> {
    let mut p = LitParser { s: expr, i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return None;
    }
    Some(v)
}

/// Split `s` at commas sitting at bracket-depth 0 and outside string quotes —
/// the top-level kwargs of a `config(…)` call (a comma inside `{'a': 'b'}`,
/// `[x, y]`, or a quoted string must not split the argument list).
pub(super) fn top_level_segments(s: &str) -> Vec<&str> {
    top_level_segs(s).collect()
}

/// The lazy form of [`top_level_segments`] — callers that only need the
/// first few arguments (call-site name/literal checks) stop scanning early
/// and never allocate.
fn top_level_segs(s: &str) -> impl Iterator<Item = &str> {
    let mut last = 0usize;
    let mut depth = 0usize;
    let mut pos = 0usize;
    let mut quote = 0u8;
    let mut done = false;
    let bytes = s.as_bytes();
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        while pos < bytes.len() {
            let b = bytes[pos];
            if quote != 0 {
                pos += 1;
                if b == b'\\' {
                    pos += 1; // skip the escaped char
                } else if b == quote {
                    quote = 0;
                }
                continue;
            }
            match b {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'\'' | b'"' => quote = b,
                b',' if depth == 0 => {
                    let seg = &s[last..pos];
                    last = pos + 1;
                    pos += 1;
                    return Some(seg);
                }
                _ => {}
            }
            pos += 1;
        }
        done = true;
        Some(&s[last..])
    })
}

/// The expression text bound to keyword `name` inside a call's argument
/// list (`meta={…}` → `{…}`). `name` must match exactly — a `metadata=`
/// kwarg is not `meta`. The name/expr separator is the first `=` at
/// bracket-depth 0 outside quotes.
pub(crate) fn kwarg<'a>(args: &'a str, name: &str) -> Option<&'a str> {
    for seg in top_level_segments(args) {
        let seg = seg.trim();
        let mut depth = 0usize;
        let mut chars = seg.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                '\'' | '"' => {
                    let it = chars.by_ref();
                    while let Some((_, d)) = it.next() {
                        if d == '\\' {
                            it.next();
                        } else if d == c {
                            break;
                        }
                    }
                }
                '=' if depth == 0 => {
                    if seg[..i].trim() == name {
                        return Some(seg[i + 1..].trim_start());
                    }
                    break; // a different kwarg — its expr can't contain `meta=`
                }
                _ => {}
            }
        }
    }
    None
}

/// A `ref()` call's `v=`/`version=` pin rendered the way dbt spells the
/// version segment — a number's `str()` (`1.10` becomes `"1.1"`), a
/// string verbatim (`'v3'` → `"v3"`, `'1.10'` stays `"1.10"`). `None`
/// when the pin kwarg is absent or not a literal (a dynamic pin the
/// loader resolves to the family's latest).
pub(crate) fn call_version(args: &str) -> Option<String> {
    kwarg(args, "v")
        .or_else(|| kwarg(args, "version"))
        .and_then(lit_value)
        .and_then(|v| scalar_to_string(&v))
}

/// Recursive-descent parser for the Python/Jinja literals a `config()` kwarg
/// holds: `{'k': v}` dicts, `[…]`/`(…)` sequences, quoted strings, `True` /
/// `False` / `None`, and numbers. Anything else — `var('x')`, an identifier,
/// a Jinja expression — is unresolvable from source and returns `None`.
pub(super) struct LitParser<'a> {
    pub(super) s: &'a str,
    pub(super) i: usize,
}

impl LitParser<'_> {
    pub(super) fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }
    pub(super) fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }
    pub(super) fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }
    pub(super) fn value(&mut self) -> Option<serde_json::Value> {
        self.ws();
        match self.peek()? {
            '{' => self.dict(),
            '[' | '(' => self.list(),
            '\'' | '"' => self.string().map(serde_json::Value::from),
            'T' | 'F' | 'N' | 't' | 'f' | 'n' => self.keyword(),
            _ => self.number(),
        }
    }
    /// `{'k': v, …}` — keys may be any literal; non-string keys stringify
    /// to their compact-JSON form (the same rendering a non-string YAML
    /// mapping key gets in [`YamlBuilder`]).
    pub(super) fn dict(&mut self) -> Option<serde_json::Value> {
        self.bump();
        let mut obj = serde_json::Map::new();
        loop {
            self.ws();
            if self.peek() == Some('}') {
                self.bump();
                return Some(obj.into());
            }
            let key = match self.value()? {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            self.ws();
            if self.bump() != Some(':') {
                return None;
            }
            obj.insert(key, self.value()?);
            self.ws();
            match self.bump()? {
                ',' => continue,
                '}' => return Some(obj.into()),
                _ => return None,
            }
        }
    }
    /// `[a, b]` / `(a, b)` — a tuple maps to a JSON array (JSON has no tuple).
    pub(super) fn list(&mut self) -> Option<serde_json::Value> {
        let close = if self.bump()? == '[' { ']' } else { ')' };
        let mut items = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some(close) {
                self.bump();
                return Some(items.into());
            }
            items.push(self.value()?);
            self.ws();
            match self.bump()? {
                ',' => continue,
                c if c == close => return Some(items.into()),
                _ => return None,
            }
        }
    }
    pub(super) fn string(&mut self) -> Option<String> {
        let quote = self.bump()?;
        let mut out = String::new();
        loop {
            match self.bump()? {
                '\\' => out.push(match self.bump()? {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other, // \' \" \\ and any other escaped char
                }),
                c if c == quote => return Some(out),
                c => out.push(c),
            }
        }
    }
    /// `True` / `False` / `None`, delimited — a `Truex` identifier is not
    /// the literal (and can't be resolved anyway). Jinja also accepts the
    /// lowercase spellings (`true`/`false`/`none` are its canonical names).
    pub(super) fn keyword(&mut self) -> Option<serde_json::Value> {
        for (tok, v) in [
            ("True", serde_json::Value::from(true)),
            ("true", serde_json::Value::from(true)),
            ("False", serde_json::Value::from(false)),
            ("false", serde_json::Value::from(false)),
            ("None", serde_json::Value::Null),
            ("none", serde_json::Value::Null),
            ("null", serde_json::Value::Null),
        ] {
            if self.s[self.i..].starts_with(tok) {
                let end = self.i + tok.len();
                if self.s[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    return None;
                }
                self.i = end;
                return Some(v);
            }
        }
        None
    }
    pub(super) fn number(&mut self) -> Option<serde_json::Value> {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
        {
            self.bump();
        }
        let tok = &self.s[start..self.i];
        if let Ok(i) = tok.parse::<i64>() {
            Some(i.into())
        } else if let Ok(u) = tok.parse::<u64>() {
            Some(u.into())
        } else {
            tok.parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Into::into)
        }
    }
}

/// One `ref(…)`, `source(…)` or `config(…)` call found in `sql` —
/// [`call_sites_masked`]'s output. `span` covers the callee name through
/// the call's closing `)` (`{{ }}`/`{% %}` wrappers excluded); `args` covers
/// the argument list between the parens (`None` when the balanced rescan
/// couldn't read the call — a malformed shape the recognizing regex still
/// accepted). `{# #}` comments are masked length-preserving before matching,
/// so every range indexes the caller's ORIGINAL text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub kind: CallKind,
    pub span: Range<usize>,
    /// The text between the call's parens, when the balanced rescan read it.
    pub args: Option<Range<usize>>,
}

/// The byte range of `seg`'s INNER text when `seg` — a subslice of
/// `text` — is a quoted literal: the `['"]([^'"]+)['"]` shape
/// `ref_re`/`source_re` require of ref/source name args (a kwarg or an
/// unquoted expression is never a name). Either quote char may open/close
/// — the regexes check them independently — and the inner text must
/// carry no quote at all.
fn quoted_lit(text: &str, seg: &str) -> Option<Range<usize>> {
    let s = seg.trim();
    let b = s.as_bytes();
    if b.len() < 3 || !matches!(b[0], b'\'' | b'"') || !matches!(b[b.len() - 1], b'\'' | b'"') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    if inner.contains(['\'', '"']) {
        return None;
    }
    let start = inner.as_ptr() as usize - text.as_ptr() as usize;
    Some(start..start + inner.len())
}

/// The byte range of `expr` — a literal INSIDE `text` — as offsets into
/// `text`, unquoted when the literal is a string (`'v2'` → the `v2`
/// range). `expr` must be a subslice of `text` (the pointer delta is the
/// offset); anything that isn't a clean string or number literal is not a
/// resolvable value → `None`, mirroring [`lit_value`]'s accept set.
fn lit_range(text: &str, expr: &str) -> Option<Range<usize>> {
    let e = expr.trim();
    let start = e.as_ptr() as usize - text.as_ptr() as usize;
    match lit_value(e)? {
        serde_json::Value::String(_) => Some(start + 1..start + e.len() - 1),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(start..start + e.len()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallKind {
    /// `ref(…)` — `name` is the byte range of the resolved model name's
    /// literal text in the masked text (the LAST quoted positional arg,
    /// `ref('pkg','x')` → the `x` range), the same rule `Patterns::refs`
    /// applies. Ranges index the input text — no per-call allocation.
    Ref {
        name: Range<usize>,
        /// `ref('pkg','x')` → the `pkg` range — the first quoted arg of a
        /// call with more than one (a shape dbt rejects past two, which
        /// still takes the first and last).
        package: Option<Range<usize>>,
        /// The `v=`/`version=` kwarg's UNQUOTED literal range (`v=2` → the
        /// `2`, `v='v2'` → the `v2`) — `None` when absent or unresolvable.
        version: Option<Range<usize>>,
    },
    /// `source('source_name', 'table_name')` — both ranges.
    Source {
        source: Range<usize>,
        table: Range<usize>,
    },
    /// `config(…)` — `args` carries the argument list.
    Config,
}

/// `Patterns::call_sites_masked` without needing a `Patterns` instance —
/// the lint call-site consumer's entry point on text it masked itself,
/// sharing one compiled `Patterns`.
pub fn call_sites_masked(masked: &str) -> Vec<CallSite> {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(Patterns::new).call_sites_masked(masked)
}

// ---------------------------------------------------------------------------
// Jinja branch structure — which call sites can execute in one render
// ---------------------------------------------------------------------------

/// Which block kind an [`ArmStep`]'s chain is — an `{% if %}` arm split
/// and a `{% for %}` arm split mean different things to the SQL render
/// (an `if` chain's `{% else %}` can union select items; a `for`
/// chain's `{% else %}` is its empty-iteration fallback, never a union
/// member).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmKind {
    /// An `{% if %}`/`{% elif %}`/`{% else %}` chain arm.
    If,
    /// A `{% for %}` body arm or its `{% else %}` fallback arm.
    For,
}

/// One `{% if %}`/`{% for %}` arm in a template's branch structure:
/// `chain` is the block's id (its open tag's source order — unique per
/// block), `arm` the branch inside it — the `{% if %}`/`{% for %}` body is
/// 0, each `{% elif %}`/`{% else %}` arm +1. Two call sites whose paths
/// disagree on a shared chain sit in arms that never execute in one
/// render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmStep {
    /// Which `{% if %}`/`{% for %}` block the arm belongs to.
    pub chain: u32,
    /// The branch inside it — `{% if %}`/`{% for %}` is 0, each
    /// `{% elif %}`/`{% else %}` arm +1.
    pub arm: u32,
    /// Whether the step's chain is an `{% if %}` or a `{% for %}` block.
    pub kind: ArmKind,
}

/// Do the arm paths `a` and `b` disagree on a shared chain? Then the
/// calls they position sit in mutually exclusive arms — `if`/`elif`/`else`
/// alternatives, or a `{% for %}` body against its `{% else %}`. dbt
/// renders exactly one arm per chain, so the calls never execute in one
/// render. Paths sharing no chain — or agreeing on every chain — compose
/// freely (an empty path is unconditional and exclusive with nothing).
pub fn arms_exclusive(a: &[ArmStep], b: &[ArmStep]) -> bool {
    a.iter()
        .any(|x| b.iter().any(|y| y.chain == x.chain && y.arm != x.arm))
}

/// A Jinja template's `{% if %}`/`{% for %}` arm structure indexed by byte
/// offset — [`jinja_branches`]'s output. [`JinjaBranches::arm_path`]
/// answers the stack of enclosing arms a position sits in, outermost
/// first; the positions it indexes are the same text
/// [`call_sites_masked`] spans point into. `chains`/`bound`/`deferred`
/// extend the index for the branch-aware `config()` readers: the arm
/// conditions `Vars::node_condition` can prove, the template-bound
/// names it can't, and the definition bodies the model render never runs.
#[derive(Debug, Default)]
pub struct JinjaBranches {
    /// `(offset, path)` cuts in ascending order — the path in effect from
    /// `offset` until the next cut. Always non-empty (the empty path at
    /// offset 0).
    cuts: Vec<(usize, Vec<ArmStep>)>,
    /// Per-chain arm conditions — `chains[step.chain]` guards
    /// `step.arm`. Private to the branch-aware `config()` readers.
    chains: Vec<ChainArms>,
    /// The names the template binds — `{% set %}`/`{% for %}` targets,
    /// `{% macro %}`/`{% test %}` signatures, `{% import %}`/
    /// `{% from %}`/`{% with %}`/`{% call %}` bindings — defined but
    /// unreadable from source, so the expression evaluator treats them
    /// like dbt's parse-context names: unprovable.
    bound: BoundNames,
    /// Definition-body ranges the model render never executes top-level —
    /// `{% macro %}`/`{% test %}`/`{% docs %}`/`{% materialization %}`/
    /// `{% verification %}` interiors. A `config()` inside applies only
    /// when the definition is invoked, which a parse can't prove, so the
    /// model-level readers never see it.
    deferred: Vec<Range<usize>>,
}

impl JinjaBranches {
    /// The arm path `offset` sits inside — `[]` outside any
    /// `{% if %}`/`{% for %}` (an unconditional position — and every
    /// position, when the template's structure couldn't be read).
    pub fn arm_path(&self, offset: usize) -> &[ArmStep] {
        let i = self.cuts.partition_point(|(o, _)| *o <= offset);
        self.cuts.get(i.wrapping_sub(1)).map_or(&[], |(_, p)| p)
    }

    /// The file's template-bound name table — [`Vars::node_enabled`]/
    /// [`Vars::node_condition`]'s unprovable-name set.
    pub(super) fn bound_names(&self) -> &BoundNames {
        &self.bound
    }

    /// Is `offset` inside a definition body the model render never runs
    /// (`{% macro %}`/`{% test %}`/`{% docs %}`/`{% materialization %}`/
    /// `{% verification %}` interiors)? The `config()` and dependency
    /// passes filter those — an uninvoked definition's calls are not
    /// the node's (an invoked macro's calls arrive through `Macros`).
    pub(super) fn in_deferred(&self, offset: usize) -> bool {
        self.deferred.iter().any(|r| r.contains(&offset))
    }
}

/// One `{% if %}`/`{% elif %}`/`{% else %}`/`{% endif %}` or
/// `{% for %}`/`{% else %}`/`{% endfor %}` chain's arm bookkeeping —
/// [`ArmStep::chain`] indexes into this.
#[derive(Debug)]
struct ChainArms {
    /// The same `{% if %}`-vs-`{% for %}` split [`ArmStep::kind`] carries.
    kind: ArmKind,
    /// The `{% if %}`/`{% elif %}` condition text ranges in arm order —
    /// `conds[k]` guards arm `k`; the `{% else %}` arm (index
    /// `conds.len()`) has none. Empty for a `{% for %}` chain — its arms
    /// are never selectable from source.
    conds: Vec<Range<usize>>,
    /// The open tag's whole remainder — the `for` clause's display text
    /// (`x in expr`) naming the gate in warnings.
    header: Range<usize>,
}

/// The names a template binds at render — `{% set %}`/`{% for %}` targets,
/// `{% macro %}`/`{% test %}` names and parameters, `{% import %}`/
/// `{% from %}` aliases, `{% with %}`/`{% call %}` bindings — collected by
/// [`jinja_branches`] alongside the arm index. In the node render these
/// are DEFINED but unreadable from source (a `{% set x = expr %}`'s
/// `expr` evaluates at render), so the enabled/condition evaluator treats
/// them like dbt's parse-context names: unprovable, never `Undefined`.
#[derive(Debug, Default)]
pub(super) struct BoundNames {
    names: std::collections::BTreeSet<String>,
}

impl BoundNames {
    /// Extend the table with a binding position's identifiers.
    fn extend<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        self.names.extend(names.into_iter().map(str::to_string));
    }

    /// Is `name` bound anywhere in the template?
    pub(super) fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

/// Read a template's `{% if %}`/`{% elif %}`/`{% else %}`/`{% endif %}` and
/// `{% for %}`/`{% else %}`/`{% endfor %}` structure into a per-offset arm
/// index — the branch context a [`CallSite::span`] positions against.
/// Built on the same `mask_jinja_comments` text the call scan reads, so a
/// `{# #}`-commented `{% if %}` opens nothing (and would be skipped here
/// anyway). Nested chains compose; a `{% raw %}` body is inert; every
/// other paired tag (`{% macro %}`, `{% block %}`, `{% docs %}`…) nests
/// transparently — arms inside it still index.
///
/// The same pass records what the branch-aware `config()` readers need:
/// each chain's arm conditions (`chains`), the template-bound names
/// (`bound` — `{% set %}`/`{% for %}`/`{% macro %}`/`{% import %}`…
/// bindings, defined but unreadable from source), and the deferred
/// definition bodies (`deferred` — `{% macro %}`/`{% test %}`/
/// `{% docs %}`/`{% materialization %}`/`{% verification %}` interiors
/// the model render never runs top-level).
///
/// A malformed structure — an unclosed block, a stray `{% end… %}`, an
/// `{% else %}`/`{% elif %}` outside `if`/`for` — yields a map where
/// every offset is unconditional: Jinja rejects it too, so nothing is
/// provably exclusive there. Mutually exclusive only ever claims what
/// the structure itself proves — under-reporting is the safe direction.
pub fn jinja_branches(text: &str) -> JinjaBranches {
    /// An open `{% %}` block — `If`/`For` carry the arm bookkeeping the
    /// signature reads; `Block` any other paired tag — the `{% end… %}`
    /// word it must close with, plus the interior's start offset when
    /// the body is a definition the render defers (`Some`), so the close
    /// can record the deferred range.
    enum Frame {
        If {
            chain: u32,
            arm: u32,
            else_seen: bool,
        },
        For {
            chain: u32,
            arm: u32,
            else_seen: bool,
        },
        Block(&'static str, Option<usize>),
    }
    /// `{% word %}…{% end<tail> %}` pairs other than `if`/`for`/`raw` and
    /// the binding/deferred tags handled above the catch-all — their
    /// interior still composes arms; only the `end` word differs.
    /// (`debug` is deliberately absent — Jinja's `{% debug %}` is a
    /// single statement tag, not a pair.)
    const BLOCK_TAGS: &[(&str, &str)] = &[
        ("block", "endblock"),
        ("filter", "endfilter"),
        ("trans", "endtrans"),
        ("autoescape", "endautoescape"),
        ("snapshot", "endsnapshot"),
        ("embed", "endembed"),
    ];
    /// The paired tags whose interiors the model render never executes:
    /// they DEFINE a macro/test/docs block/materialization/verification
    /// invoked elsewhere, so a `config()` inside applies (at most) at
    /// the invocation a parse can't prove.
    const DEFERRED_TAGS: &[(&str, &str)] = &[
        ("macro", "endmacro"),
        ("docs", "enddocs"),
        ("test", "endtest"),
        ("materialization", "endmaterialization"),
        ("verification", "endverification"),
    ];

    let b = text.as_bytes();
    let mut cuts: Vec<(usize, Vec<ArmStep>)> = vec![(0, Vec::new())];
    let mut chains: Vec<ChainArms> = Vec::new();
    let mut bound = BoundNames::default();
    let mut deferred: Vec<Range<usize>> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut malformed = false;
    let mut i = 0usize;
    while i < b.len() && !malformed {
        if b[i] != b'{' || !matches!(b.get(i + 1), Some(b'{') | Some(b'%') | Some(b'#')) {
            i += 1;
            continue;
        }
        let kind = b[i + 1];
        let Some(e) = jinja_tag_close(b, i) else {
            // An unterminated tag: the render copies the remainder
            // verbatim — nothing after it can open an arm.
            break;
        };
        if kind == b'%' {
            let (word, rest) = tag_word(&text[i + 2..e]);
            match word {
                "if" => {
                    let r = range_of(text, rest);
                    chains.push(ChainArms {
                        kind: ArmKind::If,
                        conds: vec![r.clone()],
                        header: r,
                    });
                    stack.push(Frame::If {
                        chain: chains.len() as u32 - 1,
                        arm: 0,
                        else_seen: false,
                    });
                }
                "for" => {
                    bound.extend(for_bind_names(rest));
                    chains.push(ChainArms {
                        kind: ArmKind::For,
                        conds: Vec::new(),
                        header: range_of(text, rest),
                    });
                    stack.push(Frame::For {
                        chain: chains.len() as u32 - 1,
                        arm: 0,
                        else_seen: false,
                    });
                }
                "elif" | "elseif" => match stack.last_mut() {
                    Some(Frame::If {
                        chain,
                        arm,
                        else_seen,
                    }) if !*else_seen => {
                        *arm += 1;
                        chains[*chain as usize].conds.push(range_of(text, rest));
                    }
                    // An `elif`/`elseif` anywhere else is malformed Jinja.
                    _ => malformed = true,
                },
                "else" => match stack.last_mut() {
                    Some(Frame::If { arm, else_seen, .. })
                    | Some(Frame::For { arm, else_seen, .. })
                        if !*else_seen =>
                    {
                        *arm += 1;
                        *else_seen = true;
                    }
                    _ => malformed = true,
                },
                word @ ("raw" | "comment") => {
                    // `{% raw %}…{% endraw %}` — the body is inert text
                    // (its `{…}`s are literal). `{% comment %}…{% endcomment %}`
                    // is dead text Jinja never parses. Neither nests — the
                    // first matching end tag closes — and no frame opens:
                    // offsets inside keep the enclosing arm. (Both spans
                    // are already blanked on masked input; this read is
                    // the same one for a caller on unmasked text.)
                    let end_word = if word == "raw" {
                        "endraw"
                    } else {
                        "endcomment"
                    };
                    let mut j = e + 2;
                    i = loop {
                        let Some(open) = find_subslice(b, j, b"{%") else {
                            malformed = true;
                            break b.len();
                        };
                        let Some(end) = jinja_tag_close(b, open) else {
                            malformed = true;
                            break b.len();
                        };
                        if tag_word(&text[open + 2..end]).0 == end_word {
                            break end + 2;
                        }
                        j = end + 2;
                    };
                    continue;
                }
                "set" => {
                    bound.extend(set_bind_names(rest));
                    if !jinja_set_assigns(rest) {
                        stack.push(Frame::Block("endset", None));
                    }
                }
                word @ ("macro" | "test" | "docs" | "materialization" | "verification") => {
                    if word == "macro" || word == "test" {
                        bound.extend(sig_bind_names(rest));
                    }
                    let (_, end) = DEFERRED_TAGS.iter().find(|(w, _)| *w == word).unwrap();
                    stack.push(Frame::Block(end, Some(e + 2)));
                }
                "with" => {
                    bound.extend(with_bind_names(rest));
                    stack.push(Frame::Block("endwith", None));
                }
                "call" => {
                    // A `{% call f() %}…{% endcall %}` body is NOT a
                    // deferred definition: dbt's static extractor reads
                    // it as toplevel — a `config()` inside applies (an
                    // `enabled=false` parks the node) and a `ref` edges,
                    // even though the body only renders when the callee
                    // invokes `caller()`. Verified against the real
                    // manifest — keep the body unconditional (`None`).
                    bound.extend(call_bind_names(rest));
                    stack.push(Frame::Block("endcall", None));
                }
                "import" => bound.extend(import_bind_names(rest)),
                "from" => bound.extend(from_bind_names(rest)),
                _ => {
                    if let Some((_, end)) = BLOCK_TAGS.iter().find(|(w, _)| *w == word) {
                        stack.push(Frame::Block(end, None));
                    } else if word.starts_with("end") {
                        match stack.last() {
                            Some(Frame::If { .. }) if word == "endif" => {
                                stack.pop();
                            }
                            Some(Frame::For { .. }) if word == "endfor" => {
                                stack.pop();
                            }
                            Some(Frame::Block(w, _)) if *w == word => {
                                if let Some(Frame::Block(_, Some(start))) = stack.pop() {
                                    deferred.push(start..i);
                                }
                            }
                            _ => malformed = true,
                        }
                    }
                    // Anything else (`do`, `include`, `print`, a statement
                    // `{% set x = … %}`…) executes once — no frame.
                }
            }
        }
        // `{{ }}` expressions and `{# #}` comments carry no structure;
        // `{% %}` tags record a cut whenever the arm stack changed.
        let sig: Vec<ArmStep> = stack
            .iter()
            .filter_map(|f| match f {
                Frame::If { chain, arm, .. } => Some(ArmStep {
                    chain: *chain,
                    arm: *arm,
                    kind: ArmKind::If,
                }),
                Frame::For { chain, arm, .. } => Some(ArmStep {
                    chain: *chain,
                    arm: *arm,
                    kind: ArmKind::For,
                }),
                Frame::Block(..) => None,
            })
            .collect();
        if cuts.last().is_none_or(|(_, s)| *s != sig) {
            cuts.push((e + 2, sig));
        }
        i = e + 2;
    }
    if malformed || !stack.is_empty() {
        // Jinja rejects the structure too — nothing is provably
        // exclusive, so every offset reports unconditional.
        return JinjaBranches::default();
    }
    JinjaBranches {
        cuts,
        chains,
        bound,
        deferred,
    }
}

/// `sub`'s byte range inside `text` (`sub` must be a subslice — the
/// pointer delta is its offset).
fn range_of(text: &str, sub: &str) -> Range<usize> {
    let start = sub.as_ptr() as usize - text.as_ptr() as usize;
    start..start + sub.len()
}

/// Is `b` an identifier byte?
fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The identifiers a binding position's LHS carries — the bare names in
/// `x`, `x, y`, `(x, y)`, `[x, [y]]` destructuring — quotes skipped.
/// Over-collecting is the safe direction: a name marked bound that isn't
/// merely evaluates unprovable, never `Undefined`.
fn lhs_idents(lhs: &str) -> Vec<&str> {
    let b = lhs.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    let c = b[i];
                    i += 1;
                    if c == q {
                        break;
                    }
                }
            }
            c if is_ident(c) && !c.is_ascii_digit() => {
                let start = i;
                while i < b.len() && is_ident(b[i]) {
                    i += 1;
                }
                out.push(&lhs[start..i]);
            }
            _ => i += 1,
        }
    }
    out
}

/// `rest` cut at the FIRST depth-0, quote-free keyword `kw` — word-
/// boundary on both sides. `None` when `kw` doesn't occur at top level.
fn split_top_kw<'a>(rest: &'a str, kw: &str) -> Option<(&'a str, &'a str)> {
    let b = rest.as_bytes();
    let (mut depth, mut quote) = (0usize, 0u8);
    let mut i = 0usize;
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
            _ if depth == 0
                && rest[i..].starts_with(kw)
                && (i == 0 || !is_ident(b[i - 1]))
                && !b.get(i + kw.len()).is_some_and(|&c| is_ident(c)) =>
            {
                return Some((&rest[..i], &rest[i + kw.len()..]));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `{% set … %}`'s bound names — the LHS identifiers before the top-level
/// `=` (`set x = e`, `set x, y = e`, the block form's bare `x`).
fn set_bind_names(rest: &str) -> Vec<&str> {
    let b = rest.as_bytes();
    let (mut depth, mut quote) = (0usize, 0u8);
    let mut i = 0usize;
    let mut eq = rest.len();
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
                eq = i;
                break;
            }
            _ => {}
        }
        i += 1;
    }
    lhs_idents(&rest[..eq])
}

/// `{% for … in … %}`'s bound names — the LHS identifiers before the
/// top-level `in` (`for x in e`, `for x, y in e`).
fn for_bind_names(rest: &str) -> Vec<&str> {
    let (lhs, _) = split_top_kw(rest, "in").unwrap_or((rest, ""));
    lhs_idents(lhs)
}

/// `{% macro name(…)`/`{% test name(…)`'s bound names — the signature
/// name plus every parameter identifier (`lhs_idents` over-collects a
/// `p='default'`'s contents — the safe direction).
fn sig_bind_names(rest: &str) -> Vec<&str> {
    let first = rest.trim_start();
    let end = first
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(first.len());
    let mut out = Vec::new();
    if end > 0 {
        out.push(&first[..end]);
    }
    if let Some(open) = rest.find('(') {
        if let Some((inner, _)) = scan_call_args(rest, open + 1) {
            out.extend(lhs_idents(inner));
        }
    }
    out
}

/// `{% call(…) %}`'s bound names — the leading parameter list's
/// identifiers (`{% call(a, b) f() %}` binds `a`, `b` for the body's
/// `caller` — `caller` itself binds too).
fn call_bind_names(rest: &str) -> Vec<&str> {
    let mut out = vec!["caller"];
    let rest = rest.trim_start();
    if rest.starts_with('(') {
        if let Some((inner, _)) = scan_call_args(rest, 1) {
            out.extend(lhs_idents(inner));
        }
    }
    out
}

/// `{% with a = e1, b = e2 %}`'s bound names — each top-level segment's
/// kwarg-style name. A bare `{% with %}` binds nothing.
fn with_bind_names(rest: &str) -> Vec<&str> {
    top_level_segments(rest)
        .iter()
        .filter_map(|seg| kwarg_split(seg).map(|(n, _)| n))
        .collect()
}

/// `{% import e as n %}`'s bound name — `n`. A bare `{% import e %}`
/// binds the module name (`lhs_idents` over-collects an expression's
/// identifiers — the safe direction).
fn import_bind_names(rest: &str) -> Vec<&str> {
    match split_top_kw(rest, "as") {
        Some((_, alias)) => lhs_idents(alias).into_iter().take(1).collect(),
        None => lhs_idents(rest),
    }
}

/// `{% from e import a, b as c %}`'s bound names — each item's `as`
/// alias, or the imported name itself.
fn from_bind_names(rest: &str) -> Vec<&str> {
    let Some((_, tail)) = split_top_kw(rest, "import") else {
        return Vec::new();
    };
    top_level_segments(tail)
        .iter()
        .filter_map(|seg| {
            let seg = seg.trim();
            match split_top_kw(seg, "as") {
                Some((_, alias)) => lhs_idents(alias).into_iter().next(),
                None => lhs_idents(seg).into_iter().next(),
            }
        })
        .collect()
}

/// The close-pair offset of the Jinja tag opened at `b[i]` (`{{`/`{%`/`{#`)
/// — `None` when unterminated. `{{ }}` tracks bracket depth like the
/// render (a `}}` inside `{{ f({'x':1}) }}` doesn't close); `{% %}` and
/// `{# #}` end at the first tag close. `'…'`/`"…"` spans inside `{{`/`{%`
/// are skipped like the render's `tag_close` (backslash-escaped and
/// doubled quotes stay inside), so a quoted `}}`/`%}`/`{` can't fake a
/// close or a depth step — `{{ "{" }}`, `{% if x == 'a%}' %}`. A `{# #}`
/// comment holds no string syntax, so its quotes never skip.
pub(crate) fn jinja_tag_close(b: &[u8], i: usize) -> Option<usize> {
    let close: &[u8] = match b.get(i + 1)? {
        b'{' => b"}}",
        b'%' => b"%}",
        _ => b"#}",
    };
    let tracked = close == b"}}";
    let mut depth = 0usize;
    let mut j = i + 2;
    while j + 1 < b.len() {
        if b[j] == close[0] && b[j + 1] == close[1] && (!tracked || depth == 0) {
            return Some(j);
        }
        match b[j] {
            q @ (b'\'' | b'"') if close != b"#}" => {
                j = quoted_tag_end(b, j, q);
                continue;
            }
            b'(' | b'[' | b'{' if tracked => depth += 1,
            b')' | b']' | b'}' if tracked => depth = depth.saturating_sub(1),
            _ => {}
        }
        j += 1;
    }
    None
}

/// One-past the quote that closes the `'…'`/`"…"` opened at `i` —
/// `b.len()` when unterminated. Jinja strings backslash-escape (`\'`
/// stays inside) and double-quote (`''`) escapes too — the same read the
/// render's `quoted_end` does.
fn quoted_tag_end(b: &[u8], i: usize, q: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == q {
            if b.get(j + 1) == Some(&q) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

/// The leading word of a `{% %}` tag body — whitespace-control dashes
/// and padding stripped (`{%- if x -%}` → `if`) — plus the remainder
/// after the word. A body whose first token isn't an identifier yields
/// `""`.
pub(crate) fn tag_word(body: &str) -> (&str, &str) {
    let body = body.trim_matches(|c: char| c.is_whitespace() || c == '-' || c == '+');
    let end = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(body.len());
    body.split_at(end)
}

/// Is `rest` (after `{% set %}`) the assignment form `name = expr`? The
/// block form `{% set x %}…{% endset %}` has no `=` at bracket depth 0
/// outside quotes.
pub(crate) fn jinja_set_assigns(rest: &str) -> bool {
    let b = rest.as_bytes();
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
            b'=' if depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// `needle`'s first offset in `hay[from..]` — the raw-tag body's
/// `{%`-finder.
pub(crate) fn find_subslice(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from.min(hay.len())..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

// ---------------------------------------------------------------------------
// Jinja call sites — `name(...)`/`ns.name(...)` invocations inside tags
// ---------------------------------------------------------------------------

/// `{% %}` words whose remainder is NOT an expression to scan for calls:
/// the declaration tags (`{% macro name(sig) %}`'s `name(sig)` is a
/// signature, `{% test %}`/`{% docs %}`/`{% snapshot %}`/`{% block %}`/
/// `{% embed %}`/`{% materialization %}`/`{% verification %}` name their
/// resource), the raw-payload tags (`{% filter name %}` applies a Jinja
/// filter — not a macro — and `{% import %}`/`{% from %}`/`{% include %}`/
/// `{% extends %}` take paths), `{% raw %}`, and `{% else %}` (no
/// payload). Every `end*` close is skipped by prefix, not listed.
const NO_CALL_TAGS: &[&str] = &[
    "block",
    "docs",
    "else",
    "embed",
    "extends",
    "filter",
    "from",
    "import",
    "include",
    "macro",
    "materialization",
    "raw",
    "snapshot",
    "test",
    "verification",
];

/// Any `name(...)`/`ns.name(...)` callee — [`jinja_calls`]'s per-tag pass.
/// Unlike `site_head_re` there is no `\b` needed: the leading character
/// class can only match at an identifier's start, so a longer identifier
/// always wins the leftmost match (`xunion_raw(` captures `xunion_raw`).
fn call_head_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*\s*\(").unwrap()
    })
}

/// One `name(...)`/`ns.name(...)` call inside a Jinja `{{ }}`/`{% %}` tag —
/// the shape macro invocations take. `name` is the callee as spelled
/// (`union_raw`, `shipkit.audited_select`); `args` is the text between the
/// parens; `head` is the callee's byte offset in `text` — the position the
/// deferred-body filter reads.
#[derive(Debug, Clone)]
pub(super) struct JinjaCall<'a> {
    pub name: &'a str,
    pub args: &'a str,
    pub head: usize,
}

/// The ordered `name(...)`/`ns.name(...)` calls inside `text`'s Jinja tags —
/// the scan that sees macro invocations. `{{ }}` bodies scan whole;
/// `{% %}` bodies scan after the tag word, so `{% set x = f() %}`,
/// `{% call f() %}`, `{% if f() %}`, `{% do f() %}` and `{% for x in f() %}`
/// all see `f` — except the [`NO_CALL_TAGS`] words and every `end*` close,
/// whose payloads are not calls. `ref`/`source`/`config` heads land in the
/// output like any other name — which callees a consumer accepts is its
/// business (the edge pass filters to macro names; the macro-body pass
/// turns `ref`/`source` into dependency calls).
///
/// `text` should be [`mask_jinja_comments`] output — a call inside a
/// `{# #}` comment is not a call. Only tag bodies are scanned, so plain
/// SQL text never produces a call; a head inside a Jinja string literal
/// (`'{{ m() }}'`-shaped text) still matches, the same simplification
/// [`call_sites_masked`] makes.
pub(super) fn jinja_calls(text: &str) -> Vec<JinjaCall<'_>> {
    let bytes = text.as_bytes();
    let mut calls = Vec::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let tag = match (bytes[i], bytes[i + 1]) {
            (b'{', b'{') => b'{',
            (b'{', b'%') => b'%',
            _ => {
                i += 1;
                continue;
            }
        };
        let Some(close) = jinja_tag_close(bytes, i) else {
            // An unterminated tag has no body to scan — keep walking so a
            // stray `{{` in a string can't hide the calls after it.
            i += 1;
            continue;
        };
        let body = &text[i + 2..close];
        // The byte offset inside `body` the call scan starts at: `{{ }}`
        // scans from 0; `{% %}` skips the tag word (and whole no-call tags).
        let scan_at = if tag == b'{' {
            Some(0usize)
        } else {
            let (word, _) = tag_word(body);
            // The scan starts after the tag word — measured in `body`'s
            // ORIGINAL coordinates (`tag_word` trims both ends, so `rest`
            // can't be used to derive the offset): left-pad width + the
            // word itself.
            let lead = body.len()
                - body
                    .trim_start_matches(|c: char| c.is_whitespace() || c == '-' || c == '+')
                    .len();
            (!word.is_empty() && !word.starts_with("end") && !NO_CALL_TAGS.contains(&word))
                .then_some(lead + word.len())
        };
        if let Some(off) = scan_at {
            let scan = &text[i + 2 + off..close];
            for m in call_head_re().find_iter(scan) {
                let Some((args, _)) = scan_call_args(text, i + 2 + off + m.end()) else {
                    continue;
                };
                calls.push(JinjaCall {
                    name: m.as_str().trim_end_matches('(').trim_end(),
                    args,
                    head: i + 2 + off + m.start(),
                });
            }
        }
        i = close + 2;
    }
    calls
}
