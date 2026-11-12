use super::*;
use std::ops::Range;

/// The dbt-syntax extractors, compiled once. The `regex` crate has no
/// backreferences, so each quote is matched independently (`['"]`) — a mismatched
/// `'x"` is vanishingly rare in real SQL and harmless if it slips through. The
/// leading `\b` stops `my_ref(...)` / `data_source(...)` identifiers producing
/// spurious edges. A versioned or kwarg-bearing `ref('x', v=2)` /
/// `ref('x', version=2)` IS matched — the trailing `(?:,[^)]*)?` absorbs any
/// extra args up to (but never past) the call's closing `)`. Edge resolution
/// is name-only — the version kwargs ride the arg tail (`call_sites` records
/// them on `CallKind::Ref` so a position can still pick the versioned call);
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
    /// `{# #}` comments are masked first, LENGTH-PRESERVING, so a comment
    /// hides a call like it does for the name-only extractors AND every
    /// returned byte range indexes the caller's original text. `{# #}` is
    /// the only thing masked — `--`/`/* */` comments still see calls (dbt
    /// renders Jinja before SQL comments exist).
    pub(crate) fn call_sites(&self, sql: &str) -> Vec<CallSite> {
        let masked = mask_jinja_comments(sql);
        let mut out = Vec::new();
        for c in self.ref_re.captures_iter(&masked) {
            let m = c.get(0).expect("a full match");
            let (span, args) = call_span(&masked, m.start(), m.end());
            // The trailing args hold the version kwargs — `ref('x', v=2)` /
            // `ref('x', version=2)`; `v` is dbt's short spelling.
            let version = args
                .as_ref()
                .and_then(|a| {
                    let args = &masked[a.clone()];
                    kwarg(args, "v").or_else(|| kwarg(args, "version"))
                })
                .and_then(lit_value)
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });
            out.push(CallSite {
                kind: CallKind::Ref {
                    // `refs()`'s rule: the LAST quoted positional arg is the
                    // model; when it exists, the first is the package.
                    name: c
                        .get(2)
                        .or_else(|| c.get(1))
                        .expect("ref_re's name group")
                        .as_str()
                        .to_string(),
                    package: c
                        .get(2)
                        .map(|_| c.get(1).expect("ref_re group 1").as_str().to_string()),
                    version,
                },
                span,
                args,
            });
        }
        for c in self.source_re.captures_iter(&masked) {
            let m = c.get(0).expect("a full match");
            let (span, args) = call_span(&masked, m.start(), m.end());
            out.push(CallSite {
                kind: CallKind::Source {
                    source: c.get(1).expect("source_re group 1").as_str().to_string(),
                    table: c.get(2).expect("source_re group 2").as_str().to_string(),
                },
                span,
                args,
            });
        }
        // `config_call_args_list`'s traversal: the next `config(` is searched
        // AFTER the current call's `)`, so a config nested inside another's
        // args is never double-counted.
        let mut pos = 0usize;
        while let Some(m) = config_re().find_at(&masked, pos) {
            let Some((args, end)) = scan_call_args(&masked, m.end()) else {
                break; // an unclosed call swallows the rest of the file
            };
            out.push(CallSite {
                kind: CallKind::Config,
                span: m.start()..end,
                args: Some(m.end()..m.end() + args.len()),
            });
            pos = end;
        }
        out.sort_by_key(|c| c.span.start);
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
    /// `enabled=<bool literal>` decides, matching dbt's ordered config()
    /// application (verified: `config(enabled=false)` then
    /// `config(enabled=true)` leaves the node enabled). A non-bool
    /// `enabled=` expression doesn't change the outcome.
    pub(super) fn disabled(&self, sql: &str) -> bool {
        config_kwarg_exprs(sql, "enabled")
            .into_iter()
            .filter_map(|expr| match lit_value(expr) {
                Some(serde_json::Value::Bool(b)) => Some(b),
                _ => None,
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
            if let Some(serde_json::Value::String(s)) = kwarg(args, name).and_then(lit_value) {
                *slot = Some(s);
            }
        }
        if let Some(c) = kwarg(args, "contract")
            .and_then(lit_value)
            .and_then(|v| contract_value(&v))
        {
            out.contract = Some(c);
        }
    }
    out
}

/// The `\bconfig\s*\(` finder [`config_call_args_list`] and
/// [`Patterns::call_sites`] share.
fn config_re() -> &'static Regex {
    static CONFIG: OnceLock<Regex> = OnceLock::new();
    CONFIG.get_or_init(|| Regex::new(r"\bconfig\s*\(").unwrap())
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
    let mut depth = 0usize;
    let mut chars = sql[start..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    return (c == ')').then(|| (&sql[start..start + i], start + i + 1));
                }
                depth -= 1;
            }
            '\'' | '"' => {
                let it = chars.by_ref();
                while let Some((_, d)) = it.next() {
                    if d == '\\' {
                        it.next(); // consume the escaped char
                    } else if d == c {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    None // unclosed call
}

/// The expression bound to kwarg `name` in EVERY `config(…)` call in `sql`,
/// in call order — the input to last-wins scalar kwargs (`materialized`,
/// `enabled`) and per-key-merged dict kwargs (`meta`).
pub(super) fn config_kwarg_exprs<'a>(sql: &'a str, name: &str) -> Vec<&'a str> {
    config_call_args_list(sql)
        .into_iter()
        .filter_map(|args| kwarg(args, name))
        .collect()
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
    let mut out = Vec::new();
    let mut last = 0;
    let mut depth = 0usize;
    let mut chars = s.char_indices().peekable();
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
            ',' if depth == 0 => {
                out.push(&s[last..i]);
                last = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[last..]);
    out
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
/// [`Patterns::call_sites`]'s output. `span` covers the callee name through
/// the call's closing `)` (`{{ }}`/`{% %}` wrappers excluded); `args` covers
/// the argument list between the parens (`None` when the balanced rescan
/// couldn't read the call — a malformed shape the recognizing regex still
/// accepted). `{# #}` comments are masked length-preserving before matching,
/// so every range indexes the caller's ORIGINAL text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallSite {
    pub kind: CallKind,
    pub span: Range<usize>,
    /// The text between the call's parens, when the balanced rescan read it.
    pub args: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallKind {
    /// `ref(…)` — `name` is the resolved model (the LAST quoted positional
    /// arg, `ref('pkg','x')` → `x`), the same rule [`Patterns::refs`] applies.
    Ref {
        name: String,
        /// `ref('pkg','x')` → `pkg` (the first quoted arg of a two-arg call).
        package: Option<String>,
        /// The `v=`/`version=` kwarg's literal as text (`v=2` → `"2"`,
        /// `v='v2'` → `"v2"`) — `None` when absent or unresolvable.
        version: Option<String>,
    },
    /// `source('source_name', 'table_name')`.
    Source { source: String, table: String },
    /// `config(…)` — `args` carries the argument list.
    Config,
}

/// The `ref()`/`source()`/`config()` calls in `sql` without needing a
/// [`Patterns`] instance — the lint call-site consumer's entry point,
/// sharing one compiled `Patterns`.
pub(crate) fn call_sites(sql: &str) -> Vec<CallSite> {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(Patterns::new).call_sites(sql)
}

/// The full `(span, args)` pair for a callee match that starts at `ref`/
/// `source`/`config`: rescan balanced/quote-aware from the `(` so a `)`
/// nested in an argument can't truncate the span. When the rescan fails
/// (a malformed call the regex still recognized — e.g. `ref('x', 'unclosed)`),
/// the match's own range is the span and `args` is `None`.
fn call_span(sql: &str, start: usize, regex_end: usize) -> (Range<usize>, Option<Range<usize>>) {
    let Some(open) = sql[start..regex_end].find('(').map(|o| start + o) else {
        return (start..regex_end, None); // unreachable: every callee regex matches `(`
    };
    match scan_call_args(sql, open + 1) {
        Some((args, end)) => (start..end, Some(open + 1..open + 1 + args.len())),
        None => (start..regex_end, None),
    }
}
