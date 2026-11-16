//! The `.py` model file's static read — a text-level counterpart of the
//! AST pass real dbt runs over a python model. dbt scans the file for
//! `dbt.ref`/`dbt.source`/`dbt.config` attribute calls (in ANY scope —
//! module level included), literal-evals each call's arguments (any
//! non-literal arg — a variable, an f-string, a `*`/`**` spread — is a
//! dbt parse error), and rejects a file whose `def model` isn't declared
//! exactly once or that carries Jinja markup. This module mirrors that
//! over text: a Python lexer masks `#` comments and string bodies —
//! EXCEPT the `{…}` expression inside an f-string, whose Call nodes
//! dbt's AST scan still visits (verified: `f"{dbt.ref('x')}"` produces a
//! real edge) — then finds the `dbt.*` calls, balances their parens, and
//! literal-evals the arguments.
//!
//! A call that can't be read cleanly fabricates NO edge: it counts into
//! the unprovable-adjacency record (`unresolved_adjacent`) instead, the
//! same under-reporting stance the macro pass takes, so degree-judging
//! lint rules can't claim the node has parents or children the graph
//! can't show.

use super::*;

/// The calls and file-level facts one `.py` scan produces — `calls` are
/// `dbt.ref`/`dbt.source`/`dbt.config` invocations in file order.
pub(crate) struct PyScan {
    pub(crate) calls: Vec<PyCall>,
    /// `def model(` count — dbt requires exactly one.
    pub(super) model_defs: usize,
    /// `{{`/`{%`/`{#` anywhere in the file — Jinja in a python model is a
    /// dbt parse error (the file renders before its AST is checked).
    pub(super) has_jinja: bool,
}

/// One `dbt.ref`/`dbt.source`/`dbt.config` call the scan found.
pub(crate) struct PyCall {
    pub(crate) kind: PyCallKind,
    /// The call's literal-evaluated arguments — `None` when the balanced
    /// paren scan couldn't read the call (a dbt syntax error).
    pub(super) args: Option<PyArgs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PyCallKind {
    Ref,
    Source,
    Config,
}

/// A `dbt.*` call's arguments after literal evaluation — the text-level
/// counterpart of dbt's `_get_call_literals` (`ast.literal_eval` on each
/// positional and keyword value).
#[derive(Debug, Default)]
pub(super) struct PyArgs {
    /// Positional args in order — `None` = a `*` spread or a non-literal
    /// expression (either way an `ast.literal_eval` failure in dbt).
    pub(super) positional: Vec<Option<serde_json::Value>>,
    /// `(name, value)` kwargs in order — `name` `None` is a `**` spread;
    /// `value` `None` is a non-literal expression.
    pub(super) kwargs: Vec<(Option<String>, Option<serde_json::Value>)>,
    /// Every plain string literal in the args text — the unverifiable
    /// call's source-table candidates (matching `literal_args_of`'s role
    /// on the macro path).
    pub(super) literals: Vec<String>,
}

/// The model name a literal `dbt.ref(...)` call resolves to — the LAST
/// positional arg's string (dbt's `ref(*args)` reads the last as the
/// name; `ref('pkg','x')` → `x`). More than two positionals or a
/// non-string name is a dbt error → `None` = unverifiable.
pub(super) fn ref_name(args: &PyArgs) -> Option<&str> {
    if args.positional.len() > 2 {
        return None;
    }
    match args.positional.last()? {
        Some(serde_json::Value::String(s)) => Some(s),
        _ => None,
    }
}

/// The `(source, table)` a literal `dbt.source("s", "t")` names — exactly
/// two positional strings, no kwargs (dbt's `source` takes precisely
/// those). Anything else is a dbt error → `None` = unverifiable.
pub(super) fn source_name(args: &PyArgs) -> Option<(&str, &str)> {
    if args.positional.len() == 2 && args.kwargs.is_empty() {
        match (&args.positional[0], &args.positional[1]) {
            (Some(serde_json::Value::String(s)), Some(serde_json::Value::String(t))) => {
                Some((s, t))
            }
            _ => None,
        }
    } else {
        None
    }
}

impl PyArgs {
    /// Every argument literal-evals the way dbt's `_get_call_literals`
    /// requires — no `*`/`**` spreads, no non-literal expressions.
    pub(super) fn is_literal(&self) -> bool {
        self.positional.iter().all(Option::is_some)
            && self.kwargs.iter().all(|(n, v)| n.is_some() && v.is_some())
    }

    /// The `name=` kwarg's literal — the LAST matching kwarg, Python's
    /// kwargs order — else the `"name":` entry of a leading positional
    /// dict literal (`dbt.config({'k': v})` is dbt's dict spelling of the
    /// same kwargs, verified on 1.12.6). `Some(None)` = declared but
    /// unreadable.
    pub(super) fn value_of(&self, name: &str) -> Option<Option<serde_json::Value>> {
        for (k, v) in self.kwargs.iter().rev() {
            if k.as_deref() == Some(name) {
                return Some(v.clone());
            }
        }
        if let Some(Some(serde_json::Value::Object(m))) = self.positional.first() {
            if let Some(v) = m.get(name) {
                return Some(Some(v.clone()));
            }
        }
        None
    }

    /// One top-level segment of a call's args (masked/uncut pair). A
    /// segment is "no argument" — a trailing `,`'s tail — only when the
    /// masked text holds no token AND no string literal hides in the
    /// original (a pure-string arg like `"x"` masks entirely to
    /// whitespace but IS a real argument; a comment-only tail is not).
    fn push_arg(&mut self, masked_seg: &str, seg: &str) {
        if masked_seg.trim().is_empty() && py_string_literals(seg).is_empty() {
            return;
        }
        if let Some(i) = kwarg_eq(masked_seg) {
            let lhs = masked_seg[..i].trim();
            if is_identifier(lhs) {
                self.kwargs
                    .push((Some(lhs.to_string()), py_lit(seg[i + 1..].trim())));
            } else {
                // `*x`-spread or a `=`-bearing expression — either way not
                // a clean literal argument.
                self.positional.push(None);
            }
        } else if masked_seg.trim_start().starts_with('*') {
            // `*expr`/`**expr` spreads — `ast.literal_eval` rejects them
            // (and a `**` spread breaks dbt's kwargs dispatch).
            self.positional.push(None);
        } else {
            self.positional.push(py_lit(seg.trim()));
        }
    }
}

/// The merged `dbt.config(...)` view of a scanned file — the python
/// spelling of the in-file config surface the SQL `config_*` extractors
/// fill. Same precedence rules: calls apply in order, the LAST resolvable
/// literal wins, and an unresolvable value can't override an earlier
/// literal.
#[derive(Debug, Default)]
pub(super) struct PyFileConfig {
    pub(super) materialized: Option<String>,
    pub(super) unique_key: Option<serde_json::Value>,
    pub(super) meta: serde_json::Map<String, serde_json::Value>,
    pub(super) alias: Option<String>,
    pub(super) access: Option<String>,
    pub(super) group: Option<String>,
    pub(super) contract: Option<RawContract>,
    pub(super) incremental_strategy: Option<String>,
    /// The LAST declared `enabled=` — `Some(None)` when it was declared
    /// but unreadable (matching `Patterns::enabled`'s "the last
    /// `enabled=` decides" rule, which can't fall back to an earlier
    /// proven value).
    pub(super) enabled: Option<Option<serde_json::Value>>,
}

/// Scan `text` (a `.py` model file) for the calls dbt tracks.
pub(crate) fn scan_python(text: &str) -> PyScan {
    let masked = mask_python(text);
    let mut calls = Vec::new();
    for c in dbt_call_re().captures_iter(&masked) {
        let m = c.get(0).unwrap();
        // `x.dbt.ref(…)` / `x . dbt.ref(…)` flatten to `x.dbt.ref` in
        // dbt's visitor — not a tracked name — so a `.` ending the text
        // before `dbt` disqualifies the call. (`\b` already rejects
        // `dbt` glued to a longer identifier.)
        if masked[..m.start()].trim_end().ends_with('.') {
            continue;
        }
        let kind = match c.get(1).unwrap().as_str() {
            "ref" => PyCallKind::Ref,
            "source" => PyCallKind::Source,
            _ => PyCallKind::Config,
        };
        // The `(` the regex ended on — balance it on the masked text
        // (masked string bodies carry no parens, so a plain depth count
        // closes the call where Python's parser would).
        let args =
            py_call_close(&masked, m.end() - 1).map(|close| parse_py_args(&text[m.end()..close]));
        calls.push(PyCall { kind, args });
    }
    PyScan {
        calls,
        model_defs: def_model_re().find_iter(&masked).count(),
        has_jinja: ["{{", "{%", "{#"].iter().any(|t| text.contains(t)),
    }
}

/// The merged `dbt.config(...)` config of a scanned file — the readers
/// [`scan_python`] feeds. `ctx` names the file for the one warning the
/// pass can raise (an unreadable `dbt.config` call — a dbt parse error —
/// is ignored rather than guessed).
pub(super) fn py_file_config(scan: &PyScan, warnings: &mut Vec<String>, ctx: &str) -> PyFileConfig {
    let mut out = PyFileConfig::default();
    for call in &scan.calls {
        if call.kind != PyCallKind::Config {
            continue;
        }
        let Some(args) = &call.args else {
            warnings.push(format!(
                "{ctx}: a dbt.config(...) call's arguments could not be read; its config is ignored"
            ));
            continue;
        };
        for (key, slot) in [
            ("materialized", &mut out.materialized),
            ("alias", &mut out.alias),
            ("access", &mut out.access),
            ("group", &mut out.group),
            ("incremental_strategy", &mut out.incremental_strategy),
        ] {
            if let Some(Some(serde_json::Value::String(s))) = args.value_of(key) {
                *slot = Some(s);
            }
        }
        if let Some(Some(v)) = args.value_of("unique_key") {
            out.unique_key = Some(v);
        }
        if let Some(Some(v)) = args.value_of("contract") {
            if let Some(c) = contract_value(&v) {
                out.contract = Some(c);
            }
        }
        // `meta` merges per top-level key across calls — the same ordered
        // per-key merge `config_meta` applies to SQL `config(meta={…})`.
        if let Some(Some(serde_json::Value::Object(m))) = args.value_of("meta") {
            out.meta.extend(m);
        }
        if let Some(v) = args.value_of("enabled") {
            out.enabled = Some(v);
        }
    }
    out
}

// ---- the call scan ----------------------------------------------------

/// `dbt.ref`/`dbt.source`/`dbt.config` call heads — whitespace-tolerant
/// the way Python's grammar is (`dbt . ref (` still calls, and dbt's
/// attribute flattening tracks it).
fn dbt_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bdbt\s*\.\s*(ref|source|config)\s*\(").unwrap())
}

/// `def model(` — dbt counts FunctionDef nodes named `model` (nested defs
/// count too, which is what the bare-name search approximates).
fn def_model_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bdef\s+model\s*\(").unwrap())
}

/// The `)` closing the `(` at `open` in masked text — string bodies and
/// comments are masked, so a plain depth count suffices (`)` can only
/// appear as real syntax).
fn py_call_close(masked: &str, open: usize) -> Option<usize> {
    let b = masked.as_bytes();
    let mut depth = 1usize;
    for (i, &c) in b.iter().enumerate().skip(open + 1) {
        match c {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Literal-evaluate one call's argument list — split top-level commas on
/// the masked text (ranges index `args` exactly), split `name=` kwargs,
/// then evaluate each fragment with [`py_lit`].
fn parse_py_args(args: &str) -> PyArgs {
    let masked = mask_python(args);
    let mut out = PyArgs::default();
    let mut last = 0usize;
    let mut depth = 0i32;
    // A `,` at `()[]{}` depth 0 in masked text — string/comment commas
    // are masked away.
    for (i, &c) in masked.as_bytes().iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push_arg(&masked[last..i], &args[last..i]);
                last = i + 1;
            }
            _ => {}
        }
    }
    out.push_arg(&masked[last..], &args[last..]);
    out.literals = py_string_literals(args);
    out
}

/// The position of a `=` at `()[]{}` depth 0 in a masked arg segment —
/// the `name=`/expression separator — or `None` for a positional arg.
/// `==`, `!=`, `<=`, `>=`, and `:=` are comparisons/walrus, not kwargs.
fn kwarg_eq(masked_seg: &str) -> Option<usize> {
    let b = masked_seg.as_bytes();
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let next_is_eq = b.get(i + 1) == Some(&b'=');
                let prev_op = i > 0 && matches!(b[i - 1], b'=' | b'!' | b'<' | b'>' | b':');
                if !next_is_eq && !prev_op {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A bare identifier — the only valid kwarg name.
fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every plain string literal's decoded contents in `text` — `'`/`"`/
/// triple-quoted, `r`/`u`-prefixed (a `b`/`f` prefix isn't a plain
/// literal; an f-string's `{…}` interpolations are walked for inner
/// strings, matching [`mask_python`]'s grammar). These feed the
/// unverifiable-call candidate list — the same job `literal_args_of`
/// does for macro calls.
fn py_string_literals(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    enum Frame {
        Str {
            quote: u8,
            triple: bool,
            raw: bool,
            f: bool,
            collect: bool,
            buf: Vec<u8>,
        },
        Interp {
            depth: usize,
        },
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match stack.last_mut() {
            Some(Frame::Str {
                quote,
                triple,
                raw,
                f,
                collect,
                buf,
            }) => {
                let (q, t, r, is_f) = (*quote, *triple, *raw, *f);
                if !r && b[i] == b'\\' {
                    i += 1;
                    if let Some(c) = text[i..].chars().next() {
                        if *collect {
                            match c {
                                'n' => buf.push(b'\n'),
                                't' => buf.push(b'\t'),
                                'r' => buf.push(b'\r'),
                                other => buf.extend(other.to_string().as_bytes()),
                            }
                        }
                        i += c.len_utf8();
                    }
                } else if is_f && b[i] == b'{' {
                    i += 1;
                    if i < b.len() && b[i] == b'{' {
                        i += 1; // literal `{{`
                    } else {
                        stack.push(Frame::Interp { depth: 1 });
                    }
                } else if is_f && b[i] == b'}' {
                    i += 1;
                    if i < b.len() && b[i] == b'}' {
                        i += 1;
                    }
                } else if t && i + 2 < b.len() && b[i] == q && b[i + 1] == q && b[i + 2] == q {
                    i += 3;
                    let done = stack.pop().expect("frame is on the stack");
                    if let Frame::Str {
                        collect: true, buf, ..
                    } = done
                    {
                        out.push(String::from_utf8_lossy(&buf).into_owned());
                    }
                } else if !t && b[i] == q {
                    i += 1;
                    let done = stack.pop().expect("frame is on the stack");
                    if let Frame::Str {
                        collect: true, buf, ..
                    } = done
                    {
                        out.push(String::from_utf8_lossy(&buf).into_owned());
                    }
                } else {
                    let c = text[i..].chars().next().expect("char boundary");
                    if *collect {
                        buf.extend(c.to_string().as_bytes());
                    }
                    i += c.len_utf8();
                }
            }
            Some(Frame::Interp { depth }) => match b[i] {
                b'#' => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                q @ (b'\'' | b'"') => {
                    let (_pfx, raw, f) = string_prefix(b, i);
                    let triple = b[i + 1..].starts_with(&[q, q]);
                    i += if triple { 3 } else { 1 };
                    stack.push(Frame::Str {
                        quote: q,
                        triple,
                        raw,
                        f,
                        collect: !f,
                        buf: Vec::new(),
                    });
                }
                b'{' => {
                    *depth += 1;
                    i += 1;
                }
                b'}' => {
                    *depth -= 1;
                    i += 1;
                    if *depth == 0 {
                        stack.pop();
                    }
                }
                _ => i += 1,
            },
            None => match b[i] {
                b'#' => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                q @ (b'\'' | b'"') => {
                    let (_pfx, raw, f) = string_prefix(b, i);
                    let triple = b[i + 1..].starts_with(&[q, q]);
                    i += if triple { 3 } else { 1 };
                    stack.push(Frame::Str {
                        quote: q,
                        triple,
                        raw,
                        f,
                        collect: !f,
                        buf: Vec::new(),
                    });
                }
                _ => i += 1,
            },
        }
    }
    out
}

// ---- the Python lexer -------------------------------------------------

/// The `(run_start, raw, f)` of a possible string prefix immediately
/// before the quote at `i` — a run of ≤2 letters drawn from
/// `rRuUbBfF` is a Python string prefix; a longer or foreign run is part
/// of the preceding identifier and the string is plain. The `b`/`u`
/// flags change nothing either caller cares about.
fn string_prefix(b: &[u8], i: usize) -> (usize, bool, bool) {
    let mut start = i;
    while start > 0 && b[start - 1].is_ascii_alphabetic() {
        start -= 1;
    }
    let run = &b[start..i];
    let is_prefix = !run.is_empty()
        && run.len() <= 2
        && run
            .iter()
            .all(|c| matches!(c, b'r' | b'R' | b'u' | b'U' | b'b' | b'B' | b'f' | b'F'));
    if !is_prefix {
        return (i, false, false);
    }
    (
        start,
        run.iter().any(|c| matches!(c, b'r' | b'R')),
        run.iter().any(|c| matches!(c, b'f' | b'F')),
    )
}

/// Mask every byte of `text` that is not live Python code — `#` comments
/// and string bodies — EXCEPT the `{…}` expression inside an f-string,
/// which the Python AST (and so dbt's call scan) still visits.
/// Length-preserving like [`mask_jinja_comments`]: masked bytes become
/// spaces except `\n`, so a range computed on the result indexes `text`.
pub(super) fn mask_python(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    /// A nested lexical frame — the stack's absence means top-level code.
    enum Frame {
        /// Inside a string body.
        Str {
            quote: u8,
            triple: bool,
            raw: bool,
            f: bool,
        },
        /// Inside an f-string `{…}` expression — code again; `depth`
        /// counts `{` so a dict/set literal can't close the field early.
        Interp { depth: usize },
    }
    /// Mask the `#`-comment starting at `i` through end-of-line.
    fn mask_comment(b: &[u8], out: &mut [u8], mut i: usize) -> usize {
        while i < b.len() && b[i] != b'\n' {
            out[i] = b' ';
            i += 1;
        }
        i
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match stack.last() {
            Some(Frame::Str {
                quote,
                triple,
                raw,
                f,
            }) => {
                let (q, t, r, is_f) = (*quote, *triple, *raw, *f);
                if !r && b[i] == b'\\' {
                    out[i] = b' ';
                    i += 1;
                    if i < b.len() {
                        if b[i] != b'\n' {
                            out[i] = b' ';
                        }
                        i += 1;
                    }
                } else if is_f && b[i] == b'{' {
                    out[i] = b' ';
                    i += 1;
                    if i < b.len() && b[i] == b'{' {
                        out[i] = b' ';
                        i += 1; // literal `{{` — stays string body
                    } else {
                        stack.push(Frame::Interp { depth: 1 });
                    }
                } else if is_f && b[i] == b'}' {
                    out[i] = b' ';
                    i += 1;
                    if i < b.len() && b[i] == b'}' {
                        out[i] = b' ';
                        i += 1;
                    }
                } else if t && i + 2 < b.len() && b[i] == q && b[i + 1] == q && b[i + 2] == q {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    out[i + 2] = b' ';
                    i += 3;
                    stack.pop();
                } else if !t && b[i] == q {
                    out[i] = b' ';
                    i += 1;
                    stack.pop();
                } else {
                    if b[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            }
            _ => match b[i] {
                // Top-level code and interpolation expressions share the
                // same arms — `#` comments, string opens — the
                // interpolation frame adds `}`-closes.
                b'#' => i = mask_comment(b, &mut out, i),
                q @ (b'\'' | b'"') => {
                    let (pfx, raw, f) = string_prefix(b, i);
                    let triple = b[i + 1..].starts_with(&[q, q]);
                    let open_end = i + if triple { 3 } else { 1 };
                    for c in &mut out[pfx..open_end] {
                        *c = b' ';
                    }
                    i = open_end;
                    stack.push(Frame::Str {
                        quote: q,
                        triple,
                        raw,
                        f,
                    });
                }
                b'{' => {
                    if let Some(Frame::Interp { depth }) = stack.last_mut() {
                        *depth += 1;
                    }
                    i += 1;
                }
                b'}' => {
                    // Only an interpolation's `}` is masked — a `}` in
                    // ordinary code (a dict/set literal's close) is real
                    // syntax the caller's paren/depth scans need to see.
                    if let Some(Frame::Interp { depth }) = stack.last_mut() {
                        out[i] = b' ';
                        *depth -= 1;
                        if *depth == 0 {
                            stack.pop();
                        }
                    }
                    i += 1;
                }
                _ => i += 1,
            },
        }
    }
    // Every replaced byte is ASCII (space or the kept '\n').
    String::from_utf8(out).expect("python masking emits ASCII spaces only")
}

// ---- the literal evaluator --------------------------------------------

/// Parse `expr` as a complete python literal (`ast.literal_eval`'s accept
/// set): quoted strings incl. triple-quoted and `r`/`u` prefixes (an
/// `f`/`b` prefix is not a plain-string literal), implicit string
/// concatenation, `{'k': v}` dicts, `[…]`/`(…)` sequences, `True` /
/// `False` / `None`, `...`, and numbers. Anything else — a name, a call,
/// an expression — can't eval and returns `None`, the caller's
/// "unverifiable" signal. The whole text must be one value; a trailing
/// `#` comment is legal inside call parens and is skipped.
fn py_lit(expr: &str) -> Option<serde_json::Value> {
    let mut p = PyLit { s: expr, i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return None;
    }
    Some(v)
}

/// Recursive-descent parser for the python literals a `dbt.*` call's
/// arguments hold — [`LitParser`]'s python twin: triple-quoted strings,
/// `r`/`u`/`f`/`b` prefixes, `#` comments between tokens (legal inside
/// call parens), and `(x)`-grouping vs `(x,)`-tuple.
struct PyLit<'a> {
    s: &'a str,
    i: usize,
}

impl PyLit<'_> {
    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }
    /// Whitespace AND `#`-to-EOL comments — legal anywhere inside a
    /// parenthesized expression.
    fn ws(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            if self.peek() == Some('#') {
                while let Some(c) = self.bump() {
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                return;
            }
        }
    }
    fn value(&mut self) -> Option<serde_json::Value> {
        self.ws();
        match self.peek()? {
            '{' => self.dict(),
            '[' => self.seq(']'),
            '(' => self.paren(),
            // `T`/`F`/`N`/`F`-strings reach keyword first (an `f`/`b`-led
            // token that isn't a keyword simply returns `None` there —
            // the same unreadable verdict a literal_eval would give).
            'T' | 'F' | 'N' => self.keyword(),
            '\'' | '"' | 'r' | 'R' | 'u' | 'U' | 'f' | 'b' | 'B' => {
                self.string().map(serde_json::Value::from)
            }
            '.' if self.s[self.i..].starts_with("...") => {
                self.i += 3;
                Some(serde_json::Value::Null)
            }
            _ => self.number(),
        }
    }
    /// `{'k': v, …}` — same shape as [`LitParser::dict`]: non-string keys
    /// stringify, and a bare `{a, b}` (a set — no `dbt.*` arg can use
    /// one) fails like any non-dict.
    fn dict(&mut self) -> Option<serde_json::Value> {
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
    /// `[a, b]` — a JSON array.
    fn seq(&mut self, close: char) -> Option<serde_json::Value> {
        self.bump();
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
    /// `(x)` is GROUPING — the value itself — while `(x,)` is a tuple
    /// (a JSON array): the comma is what makes a tuple.
    fn paren(&mut self) -> Option<serde_json::Value> {
        self.bump();
        self.ws();
        if self.peek() == Some(')') {
            self.bump();
            return Some(serde_json::Value::Array(vec![]));
        }
        let first = self.value()?;
        self.ws();
        match self.bump()? {
            ')' => Some(first),
            ',' => {
                let mut items = vec![first];
                loop {
                    self.ws();
                    if self.peek() == Some(')') {
                        self.bump();
                        return Some(items.into());
                    }
                    items.push(self.value()?);
                    self.ws();
                    match self.bump()? {
                        ',' => continue,
                        ')' => return Some(items.into()),
                        _ => return None,
                    }
                }
            }
            _ => None,
        }
    }
    /// One-or-more adjacent string literals — Python's implicit
    /// concatenation (`"a" 'b'` evals to `"ab"`, and `ast.literal_eval`
    /// accepts it).
    fn string(&mut self) -> Option<String> {
        let mut out = self.string_token()?;
        loop {
            let save = self.i;
            self.ws();
            match self.string_token() {
                Some(s) => out.push_str(&s),
                None => {
                    self.i = save;
                    return Some(out);
                }
            }
        }
    }
    /// One string token at `self.i`: an optional `r`/`u`-family prefix,
    /// the quote kind (single/double, one or three), the body through
    /// the closer. A `b`/`f` prefix or a plain identifier is not a
    /// plain-string literal → `None`.
    fn string_token(&mut self) -> Option<String> {
        let b = self.s.as_bytes();
        let mut j = self.i;
        while j < b.len() && b[j].is_ascii_alphabetic() && j - self.i < 2 {
            j += 1;
        }
        let run = &b[self.i..j];
        let is_prefix = !run.is_empty()
            && run
                .iter()
                .all(|c| matches!(c, b'r' | b'R' | b'u' | b'U' | b'b' | b'B' | b'f' | b'F'))
            && matches!(b.get(j), Some(b'\'') | Some(b'"'));
        let raw = is_prefix && run.iter().any(|c| matches!(c, b'r' | b'R'));
        if is_prefix {
            if run.iter().any(|c| matches!(c, b'f' | b'F' | b'b' | b'B')) {
                return None;
            }
            self.i = j;
        }
        let quote = self.bump()?;
        if quote != '\'' && quote != '"' {
            return None;
        }
        let triple = self.s.as_bytes()[self.i..].starts_with(&[quote as u8, quote as u8]);
        if triple {
            self.i += 2;
        }
        self.string_body(quote, triple, raw)
    }
    /// The body of a just-opened string token through its closing quote —
    /// `raw` = no `\` escapes. Unterminated is `None`.
    fn string_body(&mut self, quote: char, triple: bool, raw: bool) -> Option<String> {
        let mut out = String::new();
        loop {
            match self.bump()? {
                '\\' if !raw => out.push(match self.bump()? {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                }),
                c if !triple && c == quote => return Some(out),
                c if c == quote => {
                    // triple-quoted: two more of the same quote close it.
                    if self.s.as_bytes()[self.i..].starts_with(&[quote as u8, quote as u8]) {
                        self.i += 2;
                        return Some(out);
                    }
                    out.push(c);
                }
                c => out.push(c),
            }
        }
    }
    /// `True` / `False` / `None`, delimited — the ONLY keywords
    /// `ast.literal_eval` knows (lowercase Jinja spellings are names, not
    /// literals).
    fn keyword(&mut self) -> Option<serde_json::Value> {
        for (tok, v) in [
            ("True", serde_json::Value::from(true)),
            ("False", serde_json::Value::from(false)),
            ("None", serde_json::Value::Null),
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
    /// Same accept set as [`LitParser::number`].
    fn number(&mut self) -> Option<serde_json::Value> {
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

#[cfg(test)]
mod py_tests {
    use super::*;

    fn j(v: serde_json::Value) -> serde_json::Value {
        v
    }

    #[test]
    fn lit_parses_the_accepted_shapes() {
        // The `ast.literal_eval` accept set, spelled out one by one.
        assert_eq!(py_lit("'x'"), Some(j("x".into())));
        assert_eq!(py_lit("\"x\""), Some(j("x".into())));
        assert_eq!(py_lit("'''x'''"), Some(j("x".into())));
        assert_eq!(py_lit("\"\"\"x\"\"\""), Some(j("x".into())));
        assert_eq!(py_lit("r'a\\nb'"), Some(j("a\\nb".into())));
        assert_eq!(py_lit("u'x'"), Some(j("x".into())));
        assert_eq!(
            py_lit("'a' \"b\""),
            Some(j("ab".into())),
            "implicit concatenation"
        );
        assert_eq!(py_lit("'a\\n'"), Some(j("a\n".into())));
        assert_eq!(py_lit("'a\\'b'"), Some(j("a'b".into())));
        assert_eq!(py_lit("True"), Some(j(true.into())));
        assert_eq!(py_lit("False"), Some(j(false.into())));
        assert_eq!(py_lit("None"), Some(j(serde_json::Value::Null)));
        assert_eq!(py_lit("1"), Some(j(1.into())));
        assert_eq!(py_lit("-2"), Some(j((-2).into())));
        assert_eq!(py_lit("1.5"), Some(j(1.5.into())));
        assert_eq!(py_lit("['a', 'b']"), Some(j(vec!["a", "b"].into())));
        assert_eq!(
            py_lit("('a', 'b')"),
            Some(j(vec!["a", "b"].into())),
            "a tuple"
        );
        assert_eq!(
            py_lit("('a')"),
            Some(j("a".into())),
            "a parenthesized value, not a tuple"
        );
        assert_eq!(
            py_lit("{'k': 'v', 'n': [1, True]}"),
            Some(j(serde_json::json!({"k": "v", "n": [1, true]})))
        );
        assert_eq!(
            py_lit("'x' # a trailing comment"),
            Some(j("x".into())),
            "a comment inside call parens is legal"
        );
    }

    #[test]
    fn lit_rejects_the_unverifiable_shapes() {
        // Anything `ast.literal_eval` would reject — the caller's
        // "unverifiable" signal, never a fabricated edge.
        for e in [
            "x",
            "name",
            "foo()",
            "dbt.ref('x')",
            "a + b",
            "f'{x}'",
            "b'bytes'",
            "true",
            "none",
            "var('x')",
            "*args",
            "'a' 'b",
            "{'k': v}",
            "{a, b}",
            "",
            "'a', 'b'",
            "['a'",
        ] {
            assert_eq!(py_lit(e), None, "{e:?} must not eval");
        }
    }

    #[test]
    fn mask_hides_comments_and_string_bodies_only() {
        let masked = mask_python("x = 'dbt.ref(1)' # dbt.ref(2)\ndbt.ref(\n 'a', # c\n)\n");
        // The comment and the string body are gone; the call and its
        // parens remain.
        assert_eq!(masked.matches("dbt.ref").count(), 1);
        assert!(!masked.contains("dbt.ref(1)"));
        assert!(!masked.contains("dbt.ref(2)"));
        // Length preserved, newlines preserved.
        assert_eq!(
            masked.len(),
            "x = 'dbt.ref(1)' # dbt.ref(2)\ndbt.ref(\n 'a', # c\n)\n".len()
        );
    }

    #[test]
    fn mask_keeps_fstring_interpolations_live() {
        let masked = mask_python("f\"dep {dbt.ref('x')} done {a: {k: 1}}\"\n");
        assert!(
            masked.contains("dbt.ref"),
            "the interpolation's call stays visible: {masked:?}"
        );
        // The literal text of the f-string is masked.
        assert!(!masked.contains("dep"));
        assert!(!masked.contains("'x'"));
        // A dict inside an interpolation keeps its `}` visible-to-self
        // accounting (the field's own `}` is what got masked).
    }

    #[test]
    fn mask_leaves_plain_braces_visible() {
        // A dict literal in code — the `}` must NOT be masked (it's real
        // syntax the paren/depth scans need).
        let masked = mask_python("dbt.config({'a': 1})\n");
        assert_eq!(masked.matches('{').count(), 1);
        assert_eq!(masked.matches('}').count(), 1);
    }

    #[test]
    fn scan_finds_calls_across_scopes_and_forms() {
        let scan = scan_python(
            r#"
S = "dbt.ref('in_string')"          # string body — not a call
# dbt.ref('commented')              # comment — not a call
def helper(dbt):
    return dbt.ref('helper_parent')
def model(dbt, session):
    s = f"dep {dbt.ref('via_fstring')}"
    x.dbt.ref('attr_chain')          # x.dbt.ref — not a tracked name
    _ = dbt.source(
        "src", "tbl",
    )
    return dbt . ref ( 'spaced' )
"#,
        );
        let refs: Vec<_> = scan
            .calls
            .iter()
            .filter(|c| c.kind == PyCallKind::Ref)
            .collect();
        let names: Vec<Option<String>> = refs
            .iter()
            .map(|c| {
                c.args
                    .as_ref()
                    .and_then(|a| ref_name(a).map(str::to_string))
            })
            .collect();
        assert_eq!(
            names,
            vec![
                Some("helper_parent".to_string()),
                Some("via_fstring".to_string()),
                Some("spaced".to_string())
            ],
            "helper fn, f-string interpolation, and spaced `dbt . ref` — \
             and NOT the string body, the comment, or `x.dbt.ref`"
        );
        assert_eq!(
            scan.calls
                .iter()
                .filter(|c| c.kind == PyCallKind::Source)
                .count(),
            1
        );
        assert_eq!(scan.model_defs, 1);
        assert!(!scan.has_jinja);
    }

    #[test]
    fn nonliteral_args_are_unverifiable_not_fabricated() {
        let scan = scan_python(
            "def model(dbt, session):\n    name = 'x'\n    return [dbt.ref(name), dbt.source('s', t)]\n",
        );
        assert_eq!(scan.calls.len(), 2);
        for c in &scan.calls {
            let args = c.args.as_ref().expect("call args parse");
            assert!(!args.is_literal(), "nonliteral args are not literal");
            match c.kind {
                PyCallKind::Ref => assert_eq!(ref_name(args), None),
                PyCallKind::Source => assert_eq!(source_name(args), None),
                PyCallKind::Config => {}
            }
        }
    }

    #[test]
    fn config_kwargs_and_positional_dict_merge() {
        let mut w = vec![];
        let scan = scan_python(
            "def model(dbt, session):\n    dbt.config(materialized='incremental', unique_key='k')\n    dbt.config({'meta': {'a': 1}, 'alias': 'al'})\n",
        );
        let cfg = py_file_config(&scan, &mut w, "t.py");
        assert_eq!(cfg.materialized.as_deref(), Some("incremental"));
        assert_eq!(
            cfg.unique_key,
            Some(serde_json::Value::String("k".to_string()))
        );
        assert_eq!(cfg.meta["a"], serde_json::Value::from(1));
        assert_eq!(cfg.alias.as_deref(), Some("al"));
        assert!(w.is_empty());
    }

    #[test]
    fn config_enabled_reads_bools_and_warns_on_bad_values() {
        let mut w = vec![];
        let scan = scan_python("def model(dbt, s):\n    dbt.config(enabled=False)\n");
        assert_eq!(
            py_file_config(&scan, &mut w, "t.py").enabled,
            Some(Some(serde_json::Value::Bool(false)))
        );
        let scan = scan_python("def model(dbt, s):\n    dbt.config(enabled='no')\n");
        assert_eq!(
            py_file_config(&scan, &mut w, "t.py").enabled,
            Some(Some(serde_json::Value::String("no".to_string())))
        );
    }

    #[test]
    fn file_shape_facts_are_reported() {
        let scan = scan_python("x = '{{ not python }}'\ndef other():\n    pass\n");
        assert!(scan.has_jinja);
        assert_eq!(scan.model_defs, 0);
    }
}
