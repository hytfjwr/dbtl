use super::*;
use std::ops::Range;

/// The dbt-syntax extractors, compiled once. The `regex` crate has no
/// backreferences, so each quote is matched independently (`['"]`) — a mismatched
/// `'x"` is vanishingly rare in real SQL and harmless if it slips through. The
/// leading `\b` stops `my_ref(...)` / `data_source(...)` identifiers producing
/// spurious edges. A versioned or kwarg-bearing `ref('x', v=2)` /
/// `ref('x', version=2)` IS matched — the trailing `(?:,[^)]*)?` absorbs any
/// extra args up to (but never past) the call's closing `)`. Edge resolution
/// is name-only — the version kwargs ride the arg tail (`call_sites_masked`
/// records them on `CallKind::Ref` so a position can still pick the versioned
/// call);
/// the `.vN` node a `versions:` block would define is not synthesized.
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

    /// The referenced model names. `ref('pkg','x')` uses the 2nd arg (`x`);
    /// `ref('x')` uses the 1st.
    pub(super) fn refs(&self, sql: &str) -> Vec<String> {
        self.ref_re
            .captures_iter(sql)
            .filter_map(|c| {
                c.get(2)
                    .or_else(|| c.get(1))
                    .map(|m| m.as_str().to_string())
            })
            .collect()
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
    /// the span-carrying extension of [`refs`](Patterns::refs) /
    /// [`sources`](Patterns::sources). The SAME regexes recognize the calls
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
    /// this and [`disabled`](Patterns::disabled) are scoped to `config(`
    /// calls, so a bare `materialized='x'` / `enabled=false` in SQL text, a
    /// `-- comment`, or a WHERE clause can never configure the node (dbt
    /// only honours them inside `config()`). dbt applies config() calls in
    /// order — the LAST call naming `materialized` wins (verified:
    /// `config(materialized='view')` then `config(materialized='table')`
    /// yields `table`) — so the last parseable string literal is taken; an
    /// unresolvable expression (`var()`) can't override an earlier literal.
    pub(super) fn materialized(&self, sql: &str) -> Option<String> {
        config_kwarg_exprs(sql, "materialized")
            .into_iter()
            .filter_map(|expr| match lit_value(expr) {
                Some(serde_json::Value::String(s)) => Some(s),
                _ => None,
            })
            .next_back()
    }

    /// Whether the file's config() calls leave it disabled — the LAST
    /// resolvable `enabled=` decides, matching dbt's ordered config()
    /// application (verified: `config(enabled=false)` then
    /// `config(enabled=true)` leaves the node enabled). Resolvable means a
    /// bool literal or a `var('name'[, default])` (`| as_bool` allowed)
    /// the node's var scope resolves to a bool — every other `enabled=`
    /// value fails dbt's boolean validation; it's reported once per site
    /// (`ctx` names the file) and can't change the outcome, so the node
    /// stays enabled unless another value decides.
    pub(super) fn disabled(
        &self,
        sql: &str,
        vars: &Vars,
        warnings: &mut Vec<String>,
        ctx: &str,
    ) -> bool {
        config_kwarg_exprs(sql, "enabled")
            .into_iter()
            .filter_map(|expr| {
                vars.node_enabled(expr).or_else(|| {
                    warnings.push(format!(
                        "{ctx}: config(enabled={expr}) could not be evaluated as a boolean; the resource stays enabled"
                    ));
                    None
                })
            })
            .next_back()
            == Some(false)
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
pub(super) fn config_meta(sql: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    let mut found = false;
    for expr in config_kwarg_exprs(sql, "meta") {
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
pub(super) fn config_unique_key(sql: &str) -> Option<serde_json::Value> {
    config_kwarg_exprs(sql, "unique_key")
        .into_iter()
        .filter_map(lit_value)
        .next_back()
}

/// The relation-name override from `config(alias='…')`, if a string literal —
/// the LAST `alias=` across the file's config() calls wins, the same ordered
/// application `materialized`/`enabled` follow. An unresolvable expression
/// (`var()`, an identifier) can't override an earlier literal.
pub(super) fn config_alias(sql: &str) -> Option<String> {
    config_kwarg_exprs(sql, "alias")
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

pub(super) fn config_governance(sql: &str) -> FileConfig {
    let mut out = FileConfig::default();
    for args in config_call_args_list(sql) {
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

/// The argument text of EVERY `config(…)` call in `sql`, in order — each
/// string is the text between that call's parens, found by a balanced-
/// paren/quote scan so nested `(`/`)` inside dicts or macro calls can't end
/// the call early (the `[^)]*` approach can't). dbt applies config() calls
/// in order — each call's kwargs override the earlier ones per key
/// (verified: `config(materialized='v')` then `config(materialized='t')`
/// yields `t`), so kwarg readers take the LAST resolvable value.
pub(super) fn config_call_args_list(sql: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(m) = config_re().find_at(sql, pos) {
        match scan_call_args(sql, m.end()) {
            Some((args, end)) => {
                out.push(args);
                pos = end;
            }
            // An unclosed call swallows the rest of the file — no more
            // config() calls can follow it.
            None => break,
        }
    }
    out
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
/// entries take part in the same ordered sequence as the `name=v` kwargs.
pub(super) fn config_kwarg_exprs<'a>(sql: &'a str, name: &str) -> Vec<&'a str> {
    config_call_args_list(sql)
        .into_iter()
        .filter_map(|args| config_arg(args, name))
        .collect()
}

/// The expression bound to `name` in ONE `config(…)` call's argument list:
/// the `name=` kwarg, else the `"name":` entry of a positional dict arg —
/// `config({…})` is dbt's dict spelling of the same kwargs (verified:
/// `{"materialized": "table"}` materializes the model, `{"enabled": false}`
/// disables it, and the dict joins the ordered per-key last-wins/merge
/// sequence like a kwargs call). dbt rejects a single call mixing a dict
/// with kwargs — a shape resolved kwargs-first here by convention.
pub(crate) fn config_arg<'a>(args: &'a str, name: &str) -> Option<&'a str> {
    kwarg(args, name).or_else(|| dict_arg(args, name))
}

/// The value text bound to `"name":` in the call's positional DICT arg —
/// only a FIRST top-level argument that is itself a `{…}` literal counts
/// (`config({…})`); a dict in any other position is a kwarg value, not the
/// dict spelling. Entries are read leniently: a key that isn't a string
/// literal, or an entry whose `:` can't be read, is skipped rather than
/// failing the dict — the same per-key stance the kwarg readers take on
/// unresolvable expressions. A duplicate key yields its LAST entry, the
/// Python-dict rule.
fn dict_arg<'a>(args: &'a str, name: &str) -> Option<&'a str> {
    let first = top_level_segments(args).first()?.trim();
    let inner = first.strip_prefix('{')?.strip_suffix('}')?;
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

/// Recursive-descent parser for the Python/Jinja literals a `config()` kwarg
/// holds: `{'k': v}` dicts, `[…]`/`(…)` sequences, quoted strings, `True` /
/// `False` / `None`, and numbers. Anything else — `var('x')`, an identifier,
/// a Jinja expression — is unresolvable from source and returns `None`.
pub(super) struct LitParser<'a> {
    s: &'a str,
    i: usize,
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
        serde_json::Value::Number(_) => Some(start..start + e.len()),
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
        /// `ref('pkg','x')` → the `pkg` range (the first quoted arg of a
        /// two-arg call).
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
/// [`call_sites_masked`] spans point into.
#[derive(Debug, Default)]
pub struct JinjaBranches {
    /// `(offset, path)` cuts in ascending order — the path in effect from
    /// `offset` until the next cut. Always non-empty (the empty path at
    /// offset 0).
    cuts: Vec<(usize, Vec<ArmStep>)>,
}

impl JinjaBranches {
    /// The arm path `offset` sits inside — `[]` outside any
    /// `{% if %}`/`{% for %}` (an unconditional position — and every
    /// position, when the template's structure couldn't be read).
    pub fn arm_path(&self, offset: usize) -> &[ArmStep] {
        let i = self.cuts.partition_point(|(o, _)| *o <= offset);
        self.cuts.get(i.wrapping_sub(1)).map_or(&[], |(_, p)| p)
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
/// A malformed structure — an unclosed block, a stray `{% end… %}`, an
/// `{% else %}`/`{% elif %}` outside `if`/`for` — yields a map where
/// every offset is unconditional: Jinja rejects it too, so nothing is
/// provably exclusive there. Mutually exclusive only ever claims what
/// the structure itself proves — under-reporting is the safe direction.
pub fn jinja_branches(text: &str) -> JinjaBranches {
    /// An open `{% %}` block — `If`/`For` carry the arm bookkeeping the
    /// signature reads; `Block` any other paired tag, holding only the
    /// `{% end… %}` word it must close with.
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
        Block(&'static str),
    }
    /// `{% word %}…{% end<tail> %}` pairs other than `if`/`for`/`raw` —
    /// their interior still composes arms; only the `end` word differs.
    const BLOCK_TAGS: &[(&str, &str)] = &[
        ("macro", "endmacro"),
        ("block", "endblock"),
        ("filter", "endfilter"),
        ("call", "endcall"),
        ("with", "endwith"),
        ("trans", "endtrans"),
        ("autoescape", "endautoescape"),
        ("debug", "enddebug"),
        ("docs", "enddocs"),
        ("snapshot", "endsnapshot"),
        ("test", "endtest"),
        ("materialization", "endmaterialization"),
        ("verification", "endverification"),
        ("embed", "endembed"),
    ];

    let b = text.as_bytes();
    let mut cuts: Vec<(usize, Vec<ArmStep>)> = vec![(0, Vec::new())];
    let mut stack: Vec<Frame> = Vec::new();
    let mut next_chain = 0u32;
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
                    stack.push(Frame::If {
                        chain: next_chain,
                        arm: 0,
                        else_seen: false,
                    });
                    next_chain += 1;
                }
                "for" => {
                    stack.push(Frame::For {
                        chain: next_chain,
                        arm: 0,
                        else_seen: false,
                    });
                    next_chain += 1;
                }
                "elif" | "elseif" => match stack.last_mut() {
                    Some(Frame::If { arm, else_seen, .. }) if !*else_seen => *arm += 1,
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
                "raw" => {
                    // `{% raw %}…{% endraw %}` — the body is inert text
                    // (its `{…}`s are literal). Scan to the `{% endraw %}`
                    // tag; no frame — calls inside keep the enclosing arm.
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
                        if tag_word(&text[open + 2..end]).0 == "endraw" {
                            break end + 2;
                        }
                        j = end + 2;
                    };
                    continue;
                }
                "set" if !jinja_set_assigns(rest) => stack.push(Frame::Block("endset")),
                _ => {
                    if let Some((_, end)) = BLOCK_TAGS.iter().find(|(w, _)| *w == word) {
                        stack.push(Frame::Block(end));
                    } else if word.starts_with("end") {
                        let expected = stack.last().map(|f| match f {
                            Frame::If { .. } => "endif",
                            Frame::For { .. } => "endfor",
                            Frame::Block(end) => end,
                        });
                        match expected {
                            Some(w) if w == word => {
                                stack.pop();
                            }
                            _ => malformed = true,
                        }
                    }
                    // Anything else (`do`, `include`, `import`, `print`, a
                    // statement `{% set x = … %}`…) executes once — no frame.
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
                Frame::Block(_) => None,
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
    JinjaBranches { cuts }
}

/// The close-pair offset of the Jinja tag opened at `b[i]` (`{{`/`{%`/`{#`)
/// — `None` when unterminated. `{{ }}` tracks bracket depth like the
/// render (a `}}` inside `{{ f({'x':1}) }}` doesn't close); `{% %}` and
/// `{# #}` end at the first tag close.
///
/// Approximation (safe direction): `'…'`/`"…"` strings inside the tag
/// body are not skipped, so a `%}`/`}}` inside one closes early —
/// `{% if x == 'a%}' %}` reads as a closed `{% if %}` plus tail text.
/// The arm index then goes malformed → every offset reports
/// unconditional, and the collapse render keeps more text than one arm —
/// both directions only ever under-suppress.
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
            b'(' | b'[' | b'{' if tracked => depth += 1,
            b')' | b']' | b'}' if tracked => depth = depth.saturating_sub(1),
            _ => {}
        }
        j += 1;
    }
    None
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
/// parens.
#[derive(Debug, Clone)]
pub(super) struct JinjaCall<'a> {
    pub name: &'a str,
    pub args: &'a str,
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
                });
            }
        }
        i = close + 2;
    }
    calls
}
