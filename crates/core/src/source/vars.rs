//! The one `enabled`-expression evaluator — shared by in-file
//! `config(enabled=…)`, the `dbt_project.yml` `+enabled` config-tree keys,
//! and schema.yml's `enabled:` / `config.enabled:` declarations.
//!
//! Real dbt resolves `var()` in TWO scopes, one per render pass (verified
//! on dbt 1.12.6):
//!
//! - **project scope** — `dbt_project.yml` itself renders against a flat
//!   `{**vars.yml vars:, **--vars}` merge (CLI keys over file keys), before
//!   its own `vars:` block becomes a var table. The project file's `vars:`
//!   — and any `<pkg>:` scoped section — are NOT consulted at this pass.
//! - **node scope** — a resource's `config()` calls and schema.yml entries
//!   render inside the node's context: `--vars`' top-level keys first,
//!   then the effective var table's `<proj>:` scoped section, then its
//!   top level. The effective table is `vars.yml`'s `vars:` when declared
//!   (dbt rejects declaring both; the file wins), else `dbt_project.yml`'s
//!   `vars:`. A `<pkg>:` scoped section inside `--vars` does NOT reach
//!   `var()` on 1.12.6 — only its top-level keys are checked.
//!
//! An `enabled` position accepts whatever evaluates to a Python bool:
//! literals, `var()`, `==`/`!=`/ordering comparisons, `in`/`not in`
//! membership, `and`/`or`/`not`, parentheses, and the `| as_bool` filter.
//! YAML scalars render natively — a single `{{ … }}` block yields the
//! expression's value, while any other text (a bare `'True'` string
//! included) keeps its declared type and fails dbt's `enabled` validation.
//! Everything unprovable (`target.name`, other macro calls, type errors)
//! or non-bool is a dbt parse/validation error — degraded here to
//! enabled + a once-per-site warning.

use super::*;

/// An `enabled` verdict at one intake level — the two proven states plus
/// `Unproven`: a `+enabled`/`config(enabled=)`/`config.enabled:` that IS
/// declared but can't be evaluated from source (dbt would surface a
/// parse or validation error). The resource stays enabled — a guess
/// either way could drop a live node — and the node is recorded as
/// `enabled`-unverifiable so lint claims that need its existence or its
/// edges never fire on it. `None` at a level means "not declared here"
/// — the next precedence level decides.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Enabled {
    /// `enabled` proven true — or no declaration anywhere (the default).
    On,
    /// `enabled` proven false — dbt parks the resource under `disabled`.
    Off,
    /// Declared but unprovable — keep the resource and mark it.
    Unproven,
}

impl Enabled {
    /// [`Vars::schema_enabled_state`]'s tri-state → the verdict enum —
    /// `None` (not declared) passes through as "this level doesn't
    /// decide".
    pub(super) fn from_schema(v: Option<Option<bool>>) -> Option<Enabled> {
        match v {
            Some(Some(b)) => Some(if b { Enabled::On } else { Enabled::Off }),
            Some(None) => Some(Enabled::Unproven),
            None => None,
        }
    }
}

/// A `--vars` override map — dbt's highest-precedence var source, parsed
/// once at the CLI boundary and applied to every source-mode load
/// (the current graph, `--diff` baselines, TUI reloads). The raw flag
/// text is kept so `dbt parse` can be handed the same string verbatim.
#[derive(Debug, Clone)]
pub struct CliVars {
    raw: String,
    vars: serde_json::Map<String, serde_json::Value>,
}

impl CliVars {
    /// Parse a `--vars` argument like dbt does: a YAML (or JSON) string
    /// that must decode to a key/value dict — anything else is an input
    /// error, not a warning.
    pub fn parse(text: &str) -> Result<CliVars> {
        let value = yaml_to_json(text, &mut Vec::new())
            .map_err(|e| anyhow::anyhow!("--vars '{text}' is not valid YAML: {e}"))?;
        let serde_json::Value::Object(vars) = value else {
            anyhow::bail!("--vars '{text}' is not a YAML/JSON dictionary")
        };
        Ok(CliVars {
            raw: text.to_string(),
            vars,
        })
    }

    /// The parsed var map — dbt's top-precedence lookup table.
    pub fn vars(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.vars
    }

    /// The verbatim flag text — `dbt parse --vars` takes it unchanged.
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

/// The var tables real dbt resolves `var()` against — one per render pass
/// (see the module doc): [`Vars::project_scope`] while `dbt_project.yml`
/// renders, [`Vars::node_scope`] while a node's `config()` renders.
pub(super) struct Vars {
    /// `vars.yml`'s `vars:` map — the file half of the project-file
    /// render's var table (flat lookups; its `<pkg>:` sub-maps are just
    /// values there, not a scoping).
    file: serde_json::Map<String, serde_json::Value>,
    /// The node-render table: `file` when non-empty (dbt rejects declaring
    /// both; the file wins), else `dbt_project.yml`'s `vars:`.
    node: serde_json::Map<String, serde_json::Value>,
    /// The `--vars` map — the highest-precedence lookups in BOTH scopes.
    cli: serde_json::Map<String, serde_json::Value>,
    /// The root project name — the `vars: <name>:` scoping key for the
    /// node scope.
    proj: String,
}

impl Vars {
    /// Load `vars.yml` + `dbt_project.yml`'s `vars:` into the two scopes.
    /// Both files declaring non-empty `vars:` is a combination dbt rejects
    /// — surfaced as one warning, `vars.yml` winning (it is also what the
    /// project-file render would have seen). `cli_vars` is the parsed
    /// `--vars` map — empty when the flag wasn't given.
    pub(super) fn load(
        dir: &Path,
        project: &DbtProject,
        cli_vars: Option<&CliVars>,
        warnings: &mut Vec<String>,
    ) -> Vars {
        let file = file_vars(dir, warnings);
        let project_vars = match &project.vars {
            serde_json::Value::Object(m) => m.clone(),
            serde_json::Value::Null => serde_json::Map::new(),
            _ => {
                warnings.push(
                    "dbt_project.yml 'vars:' is not a mapping; its vars are ignored".to_string(),
                );
                serde_json::Map::new()
            }
        };
        if !file.is_empty() && !project_vars.is_empty() {
            warnings.push(
                "vars.yml and dbt_project.yml both declare 'vars:' (dbt rejects this combination); vars.yml wins"
                    .to_string(),
            );
        }
        let cli = cli_vars.map(|v| v.vars().clone()).unwrap_or_default();
        Vars::from_maps(file, project_vars, cli, &project.name)
    }

    /// The maps → scopes split both consumers need (separated from the
    /// filesystem read so tests build scopes straight from values).
    pub(super) fn from_maps(
        file: serde_json::Map<String, serde_json::Value>,
        project: serde_json::Map<String, serde_json::Value>,
        cli: serde_json::Map<String, serde_json::Value>,
        proj: &str,
    ) -> Vars {
        Vars {
            node: if file.is_empty() {
                project
            } else {
                file.clone()
            },
            file,
            cli,
            proj: proj.to_string(),
        }
    }

    /// The boolean a `+enabled` tree value resolves to, when dbt could
    /// produce one: a native bool, or a single `{{ … }}` render the
    /// project scope evaluates to a bool. Everything else (a number, a
    /// plain `'false'` string, another expression, an unresolvable var)
    /// fails dbt's `enabled` validation. The project-file render's
    /// `var()` is STRICT — a missing name raises (verified on 1.12.6),
    /// surfaced here as unresolvable rather than the node scope's `none`.
    pub(super) fn tree_enabled(&self, v: &serde_json::Value) -> Option<bool> {
        self.enabled_scalar(v, |n| self.project_scope(n))
    }

    /// The boolean a schema.yml `enabled:` / `config.enabled:` scalar
    /// resolves to in the NODE render scope — same native-render rule as
    /// [`tree_enabled`](Vars::tree_enabled), different var table. The var
    /// TABLE is the node's, but the missing-name rule is the project
    /// render's strict one (verified on 1.12.6: `var('x')` with no
    /// default raises "Required var not found" here too — only an
    /// in-file `config(enabled=…)` tolerates it, yielding `none`).
    pub(super) fn schema_enabled(&self, v: &serde_json::Value) -> Option<bool> {
        self.enabled_scalar(v, |n| self.node_scope(n))
    }

    /// The shared native-render rule for YAML `enabled` positions: a
    /// native bool passes through; a string must be one `{{ … }}` block
    /// whose expression resolves to a bool; every other shape fails
    /// dbt's validation (→ `None`). Both YAML render passes this feeds
    /// (`dbt_project.yml` and schema.yml) treat a missing `var()` name
    /// as an error — `missing_none` is off.
    fn enabled_scalar(
        &self,
        v: &serde_json::Value,
        lookup: impl Fn(&str) -> Option<serde_json::Value>,
    ) -> Option<bool> {
        match v {
            serde_json::Value::Bool(b) => Some(*b),
            serde_json::Value::String(s) => jinja_expr(s)
                .and_then(|e| eval_enabled_expr(e, &lookup, false))
                .and_then(|v| v.as_bool()),
            _ => None,
        }
    }

    /// The tri-state verdict for a declared schema.yml `enabled:` /
    /// `config.enabled:` scalar: `None` is absent; `Some(Some(b))` is
    /// the resolved bool; `Some(None)` is declared-but-unprovable —
    /// reported once naming `site` (dbt hard-errors on it instead), the
    /// resource stays enabled, and the caller records the node as
    /// `enabled`-unverifiable.
    pub(super) fn schema_enabled_state(
        &self,
        v: Option<&serde_json::Value>,
        site: &str,
        warnings: &mut Vec<String>,
    ) -> Option<Option<bool>> {
        let v = v?;
        match self.schema_enabled(v) {
            Some(b) => Some(Some(b)),
            None => {
                warnings.push(format!(
                    "{site} could not be evaluated as a boolean ({}); the resource stays enabled",
                    serde_json::to_string(v).unwrap_or_else(|_| "?".to_string())
                ));
                Some(None)
            }
        }
    }

    /// `true` when a declared schema.yml `enabled:` / `config.enabled:`
    /// resolves false in the node scope — `None` (absent) is never a
    /// disable. An unresolvable declaration stays enabled with one
    /// warning naming `site` (dbt hard-errors on it instead).
    pub(super) fn schema_disabled(
        &self,
        v: Option<&serde_json::Value>,
        site: &str,
        warnings: &mut Vec<String>,
    ) -> bool {
        matches!(
            self.schema_enabled_state(v, site, warnings),
            Some(Some(false))
        )
    }

    /// The boolean an in-file `config(enabled=<expr>)` resolves to in the
    /// node scope — the expression's native value when it is a bool.
    /// Anything else fails dbt's `enabled` validation. This render's
    /// `var()` is the tolerant one — a missing name yields `none`
    /// (verified on 1.12.6: `config(enabled=var('x'))` on a missing `x`
    /// records `enabled: None`, a validation error, not "Required var
    /// not found"), so `missing_none` is on.
    pub(super) fn node_enabled(&self, expr: &str) -> Option<bool> {
        eval_enabled_expr(expr, &|n| self.node_scope(n), true).and_then(|v| v.as_bool())
    }

    /// `var('name')` as `dbt_project.yml`'s own render sees it — the
    /// `{**file, **cli}` flat merge (CLI keys win).
    fn project_scope(&self, name: &str) -> Option<serde_json::Value> {
        self.cli.get(name).or_else(|| self.file.get(name)).cloned()
    }

    /// `var('name')` as a node's `config()` sees it — `--vars`' top-level
    /// keys first, then `vars: <proj>:` scoped entries, then the top
    /// level of the effective table.
    fn node_scope(&self, name: &str) -> Option<serde_json::Value> {
        self.cli
            .get(name)
            .or_else(|| {
                self.node
                    .get(&self.proj)
                    .and_then(|v| v.as_object())
                    .and_then(|m| m.get(name))
            })
            .or_else(|| self.node.get(name))
            .cloned()
    }
}

/// `vars.yml`'s `vars:` map when the file exists and parses — dbt merges
/// it into the project-file render context (and the node var table) ahead
/// of `vars:`, so both `var()` scopes consult it. An unparsable file
/// contributes nothing but the warning.
fn file_vars(dir: &Path, warnings: &mut Vec<String>) -> serde_json::Map<String, serde_json::Value> {
    let path = dir.join("vars.yml");
    if !path.is_file() {
        return serde_json::Map::new();
    }
    match yaml_to_json(&read(&path), warnings) {
        Ok(v) => match &v["vars"] {
            serde_json::Value::Object(m) => m.clone(),
            serde_json::Value::Null => serde_json::Map::new(),
            _ => {
                warnings
                    .push("vars.yml 'vars:' is not a mapping; its vars are ignored".to_string());
                serde_json::Map::new()
            }
        },
        Err(e) => {
            warnings.push(format!(
                "vars.yml is not parseable ({e}); its vars are ignored"
            ));
            serde_json::Map::new()
        }
    }
}

/// A value the `enabled` evaluator produced — a resolved Python/Jinja
/// value, or Jinja's `Undefined` (a bare name the context never carries).
/// The two are alike almost everywhere — `Undefined` is falsy, `==` only
/// to itself, and fails the leaf's bool validation just like a non-bool —
/// EXCEPT under the `is defined`/`is undefined`/`is none` tests: Python
/// `None` IS defined while a missing name is not.
#[derive(Clone, Debug)]
enum Val {
    V(serde_json::Value),
    Undefined,
}

impl Val {
    /// The leaf `enabled` extraction: a real bool, else `None` — a
    /// non-bool value AND an `Undefined` both fail dbt's validation
    /// (the caller degrades either to enabled + warning).
    fn as_bool(&self) -> Option<bool> {
        match self {
            Val::V(serde_json::Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }
}

/// Evaluate a Jinja `enabled` expression to the Python value it resolves
/// to: literals, `var()` calls, `==`/`!=`/`<`/`<=`/`>`/`>=` chains, `in` /
/// `not in` membership, `and`/`or`/`not` (Python truthiness — `and`/`or`
/// return operand values, non-empty strings and nonzero numbers are
/// truthy), parentheses, and the `| as_bool` filter. `missing_none` picks
/// the render pass's missing-`var()` rule: ON for an in-file `config()`
/// (a missing name yields `none` — dbt's require_vars=False), OFF for
/// the `dbt_project.yml` and schema.yml renders (a missing name raises —
/// unprovable here). `None` means the expression is unprovable from
/// source — a syntax error, an unsupported call/filter (`target.name`,
/// `env_var()`, a project macro), or a Python type error — which dbt
/// would surface as a parse error; callers degrade it to enabled +
/// warning.
fn eval_enabled_expr(
    expr: &str,
    lookup: &dyn Fn(&str) -> Option<serde_json::Value>,
    missing_none: bool,
) -> Option<Val> {
    let mut e = EnabledExpr {
        p: LitParser { s: expr, i: 0 },
        lookup,
        missing_none,
        used_var: false,
    };
    let v = e.parse_or()?;
    e.p.ws();
    (e.p.i == expr.len()).then_some(v)
}

/// Recursive-descent parser/evaluator for the `enabled`-position Jinja
/// subset — precedence low→high: `or` < `and` < `not` < comparisons <
/// postfix `| filter`/`is` tests < primary (literals, `var()`, parens).
/// Every method returns `None` on an unprovable shape (see
/// [`eval_enabled_expr`]) — parse errors and Python type errors share
/// that channel.
struct EnabledExpr<'a, 'b> {
    p: LitParser<'a>,
    lookup: &'b dyn Fn(&str) -> Option<serde_json::Value>,
    /// The render pass's missing-`var()` rule — see [`eval_enabled_expr`].
    missing_none: bool,
    /// `var()` was consulted in the current operand — an `is sameas`
    /// argument is provable only when it is a pure literal (a var-drawn
    /// operand could share the operand object's identity, which value
    /// comparison can't see).
    used_var: bool,
}

impl<'a> EnabledExpr<'a, '_> {
    /// The unconsumed input (for keyword lookahead).
    fn rest(&self) -> &'a str {
        let s = self.p.s;
        &s[self.p.i..]
    }

    /// Consume a bare word (`or`/`and`/`not`/`in`) when it is not the
    /// head of a longer identifier (`orange` is not `or`).
    fn eat_kw(&mut self, w: &str) -> bool {
        if !self.rest().starts_with(w) {
            return false;
        }
        let end = self.p.i + w.len();
        if self.p.s[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            return false;
        }
        self.p.i = end;
        true
    }

    /// Read an identifier (`[A-Za-z_][A-Za-z0-9_]*`), advancing past it.
    fn ident(&mut self) -> Option<&'a str> {
        let s = self.p.s;
        let start = self.p.i;
        let c = self.p.peek()?;
        if !(c.is_alphabetic() || c == '_') {
            return None;
        }
        while self
            .p
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            self.p.bump();
        }
        Some(&s[start..self.p.i])
    }

    /// `a or b or …` — Python `or`: the first truthy operand's value,
    /// else the last one. The operand value matters (`0 or false` is
    /// `false`), so a short-circuit only excuses the rhs being
    /// UNPROVABLE, never its parse.
    fn parse_or(&mut self) -> Option<Val> {
        let mut l = self.parse_and()?;
        loop {
            self.p.ws();
            if !self.eat_kw("or") {
                return Some(l);
            }
            // A short-circuit only excuses the rhs being UNPROVABLE when
            // trailing input remains — a tail that consumed to EOF failed
            // to parse (e.g. `false and (`), which is a syntax error in
            // dbt, not a value. Any `None` rhs makes the whole expression
            // unprovable → warn + enabled, never a silent disable.
            match self.parse_and() {
                Some(r) if !truthy(&l) => l = r,
                Some(_) => {}
                None => return None,
            }
        }
    }

    /// `a and b and …` — Python `and`: the first falsy operand's value,
    /// else the last one.
    fn parse_and(&mut self) -> Option<Val> {
        let mut l = self.parse_not()?;
        loop {
            self.p.ws();
            if !self.eat_kw("and") {
                return Some(l);
            }
            // Same rule as `parse_or` — see the note there.
            match self.parse_not() {
                Some(r) if truthy(&l) => l = r,
                Some(_) => {}
                None => return None,
            }
        }
    }

    /// `not x` — a real bool result (`not` binds tighter than `and`/`or`,
    /// looser than comparisons, so `not a == b` is `not (a == b)`).
    fn parse_not(&mut self) -> Option<Val> {
        self.p.ws();
        if self.eat_kw("not") {
            let v = self.parse_not()?;
            Some(Val::V((!truthy(&v)).into()))
        } else {
            self.parse_cmp()
        }
    }

    /// Chained comparisons — `a == b != c` is Python's
    /// `(a == b) and (b == c)`: pairwise links `and`ed, the middle
    /// operand evaluated once (it is already a value here). A link that
    /// is `False` short-circuits the remaining comparisons like CPython
    /// does — a later link's type error can then never fire.
    fn parse_cmp(&mut self) -> Option<Val> {
        let mut prev = self.parse_postfix()?;
        let mut chained = false;
        let mut ok = true;
        loop {
            let Some(op) = self.peek_cmp() else {
                return if chained {
                    Some(Val::V(ok.into()))
                } else {
                    Some(prev)
                };
            };
            let cur = self.parse_postfix()?;
            if ok {
                ok = self.compare(op, &prev, &cur)?;
            }
            prev = cur;
            chained = true;
        }
    }

    /// `a <op> b` → the comparison's bool; `None` on a Python TypeError
    /// (mixed ordering types, `in` on a non-container) — unresolvable.
    fn compare(&self, op: Cmp, a: &Val, b: &Val) -> Option<bool> {
        match op {
            Cmp::Eq => Some(py_eq(a, b)),
            Cmp::Ne => Some(!py_eq(a, b)),
            Cmp::Lt => py_cmp(a, b).map(|o| o == std::cmp::Ordering::Less),
            Cmp::Le => py_cmp(a, b).map(|o| o != std::cmp::Ordering::Greater),
            Cmp::Gt => py_cmp(a, b).map(|o| o == std::cmp::Ordering::Greater),
            Cmp::Ge => py_cmp(a, b).map(|o| o != std::cmp::Ordering::Less),
            Cmp::In => contains(b, a),
            Cmp::NotIn => contains(b, a).map(|b| !b),
        }
    }

    /// A comparison operator at the cursor, if any — two-char ops and
    /// `not in` before their prefixes.
    fn peek_cmp(&mut self) -> Option<Cmp> {
        self.p.ws();
        for (tok, op) in [
            ("==", Cmp::Eq),
            ("!=", Cmp::Ne),
            ("<=", Cmp::Le),
            (">=", Cmp::Ge),
            ("<", Cmp::Lt),
            (">", Cmp::Gt),
        ] {
            if self.rest().starts_with(tok) {
                self.p.i += tok.len();
                return Some(op);
            }
        }
        // `not` only means a comparison when `in` follows — rewind if it
        // doesn't, so a trailing `not` counts as unparsable input rather
        // than vanishing into a result.
        let save = self.p.i;
        if self.eat_kw("not") {
            self.p.ws();
            if self.eat_kw("in") {
                return Some(Cmp::NotIn);
            }
            self.p.i = save;
            return None;
        }
        self.eat_kw("in").then_some(Cmp::In)
    }

    /// A primary plus its postfix chain — `| filter` (only `as_bool`
    /// resolves), `.attr`, `[sub]`, `(args)`, `is` tests. Anything but
    /// `as_bool` and the supported tests is unprovable (`target.name`,
    /// `env_var(…)`, `x[0]`, a macro call).
    fn parse_postfix(&mut self) -> Option<Val> {
        let mut v = self.parse_primary()?;
        loop {
            self.p.ws();
            match self.p.peek() {
                Some('|') => {
                    self.p.bump();
                    self.p.ws();
                    match self.ident()? {
                        "as_bool" => {
                            self.p.ws();
                            if self.rest().starts_with("()") {
                                self.p.i += 2;
                            }
                            v = as_bool(&v)?;
                        }
                        _ => return None,
                    }
                }
                Some('.') | Some('[') | Some('(') => return None,
                // Jinja `is` tests — the same operand-postfix slot `|`
                // filters occupy. `Val::Undefined` stands in for Jinja's
                // `Undefined` (`var()` produces `none` instead — the
                // render paths never surface a real `Undefined` there),
                // so `is defined`/`is none`/`is undefined` read off the
                // variant split dbt applies; other arg-taking tests
                // (`equalto`, `divisibleby`, …) stay unprovable.
                _ if self.eat_kw("is") => {
                    self.p.ws();
                    let neg = self.eat_kw("not");
                    self.p.ws();
                    let t = self.ident()?;
                    self.p.ws();
                    let r = match t {
                        "sameas" => py_is(&v, &self.test_operand()?)?,
                        _ if self.p.peek() == Some('(') => return None,
                        "defined" => !matches!(v, Val::Undefined),
                        "undefined" => matches!(v, Val::Undefined),
                        "none" => matches!(v, Val::V(serde_json::Value::Null)),
                        "string" => matches!(v, Val::V(serde_json::Value::String(_))),
                        // `isinstance(True, Number)` — a bool IS a number.
                        "number" => matches!(
                            v,
                            Val::V(serde_json::Value::Number(_))
                                | Val::V(serde_json::Value::Bool(_))
                        ),
                        "boolean" => matches!(v, Val::V(serde_json::Value::Bool(_))),
                        "mapping" => matches!(v, Val::V(serde_json::Value::Object(_))),
                        // Jinja's `sequence`/`iterable` tests duck-type:
                        // lists, dicts AND strings pass both, and so does
                        // `Undefined` (it defines `__len__`/`__getitem__`/
                        // `__iter__` — verified on Jinja 3.1: `missing is
                        // sequence` renders True).
                        "sequence" | "iterable" => matches!(
                            v,
                            Val::Undefined
                                | Val::V(serde_json::Value::Array(_))
                                | Val::V(serde_json::Value::String(_))
                                | Val::V(serde_json::Value::Object(_))
                        ),
                        "true" => matches!(v, Val::V(serde_json::Value::Bool(true))),
                        "false" => matches!(v, Val::V(serde_json::Value::Bool(false))),
                        // `x % 2` — a Python TypeError on a non-number
                        // (Undefined included) is unprovable, not False.
                        "even" => num_val(&v)? % 2.0 == 0.0,
                        "odd" => num_val(&v)? % 2.0 != 0.0,
                        _ => return None,
                    };
                    v = Val::V((r != neg).into());
                }
                _ => return Some(v),
            }
        }
    }

    /// The operand of an `is` test that takes one — `sameas(<expr>)`
    /// parenthesized or `sameas <primary>` bare, Jinja's two argument
    /// spellings. Only a pure literal proves identity: a `var()`-drawn
    /// operand could BE the left operand's object (`var('n') is sameas
    /// var('n')` holds in Python whatever the value), which value
    /// comparison can't see — so a `var()` inside makes it unprovable.
    fn test_operand(&mut self) -> Option<Val> {
        let used_var = self.used_var;
        self.used_var = false;
        let v = if self.p.peek() == Some('(') {
            self.p.bump();
            self.p.ws();
            let a = self.parse_or();
            self.p.ws();
            if a.is_none() || self.p.bump() != Some(')') {
                self.used_var |= used_var;
                return None;
            }
            a
        } else {
            // The bare operand form is Jinja's restricted `primary` —
            // literals/containers/names, NOT a unary `+`/`-` expression:
            // `x is sameas -3` is a syntax error in Jinja (verified),
            // which dbt surfaces as a parse error — unprovable.
            if matches!(self.p.peek(), Some('-') | Some('+')) {
                return None;
            }
            self.parse_primary()
        };
        let literal = !self.used_var;
        self.used_var |= used_var;
        v.filter(|_| literal)
    }

    /// A literal, a `var(…)` call, a parenthesized expression, or a unary
    /// `-`/`+` over a literal.
    fn parse_primary(&mut self) -> Option<Val> {
        self.p.ws();
        match self.p.peek()? {
            '(' => {
                self.p.bump();
                self.p.ws();
                // `()` — the empty tuple.
                if self.p.peek() == Some(')') {
                    self.p.bump();
                    return Some(Val::V(serde_json::Value::Array(vec![])));
                }
                let first = self.parse_or()?;
                self.p.ws();
                match self.p.bump()? {
                    // `(a, b)` / `(a,)` — a tuple maps to a JSON array.
                    ',' => {
                        let mut items = vec![first];
                        loop {
                            self.p.ws();
                            if self.p.peek() == Some(')') {
                                self.p.bump();
                                return val_seq(items);
                            }
                            items.push(self.parse_or()?);
                            self.p.ws();
                            match self.p.bump()? {
                                ',' => continue,
                                ')' => return val_seq(items),
                                _ => return None,
                            }
                        }
                    }
                    ')' => Some(first),
                    _ => None,
                }
            }
            '[' => self.seq(),
            '{' => self.map_literal(),
            '\'' | '"' => self.p.string().map(|s| Val::V(s.into())),
            // Unary `-`/`+` — Python TypeErrors on a non-number operand.
            '-' | '+' => {
                let neg = self.p.bump() == Some('-');
                match self.parse_primary()? {
                    Val::V(serde_json::Value::Number(n)) if !neg => Some(Val::V(n.into())),
                    Val::V(serde_json::Value::Number(n)) => {
                        if let Some(i) = n.as_i64() {
                            Some(Val::V(i.wrapping_neg().into()))
                        } else {
                            n.as_f64()
                                .and_then(|f| serde_json::Number::from_f64(-f))
                                .map(|n| Val::V(n.into()))
                        }
                    }
                    _ => None,
                }
            }
            '0'..='9' | '.' => self.p.number().map(Val::V),
            _ => self.parse_name_or_call(),
        }
    }

    /// `[a, b, …]` — elements are full expressions (`[var('x'), 'y']` is
    /// legal Jinja); the `[` is consumed already.
    fn seq(&mut self) -> Option<Val> {
        self.p.bump();
        let mut items = Vec::new();
        loop {
            self.p.ws();
            if self.p.peek() == Some(']') {
                self.p.bump();
                return val_seq(items);
            }
            items.push(self.parse_or()?);
            self.p.ws();
            match self.p.bump()? {
                ',' => continue,
                ']' => return val_seq(items),
                _ => return None,
            }
        }
    }

    /// `{k: v, …}` — keys evaluate like values but must be strings for
    /// the JSON map (a `var()`-produced string key is fine).
    fn map_literal(&mut self) -> Option<Val> {
        self.p.bump();
        let mut obj = serde_json::Map::new();
        loop {
            self.p.ws();
            if self.p.peek() == Some('}') {
                self.p.bump();
                return Some(Val::V(obj.into()));
            }
            let key = match self.parse_or()? {
                Val::V(serde_json::Value::String(s)) => s,
                _ => return None,
            };
            self.p.ws();
            if self.p.bump() != Some(':') {
                return None;
            }
            let Val::V(v) = self.parse_or()? else {
                // A `{k: Undefined}` member is a dbt-side error, not a
                // value the map carries — unprovable.
                return None;
            };
            obj.insert(key, v);
            self.p.ws();
            match self.p.bump()? {
                ',' => continue,
                '}' => return Some(Val::V(obj.into())),
                _ => return None,
            }
        }
    }

    /// A keyword literal, the `var(…)` call, or a bare name — Jinja
    /// resolves an unknown name to `Undefined` ([`Val::Undefined`] stands
    /// in: falsy, `==` only to itself, fails `is defined` and the leaf's
    /// bool validation).
    fn parse_name_or_call(&mut self) -> Option<Val> {
        let name = self.ident()?;
        match name {
            "true" | "True" => Some(Val::V(true.into())),
            "false" | "False" => Some(Val::V(false.into())),
            "none" | "None" | "null" | "Null" => Some(Val::V(serde_json::Value::Null)),
            _ => {
                self.p.ws();
                if self.p.peek() != Some('(') {
                    return Some(Val::Undefined);
                }
                if name != "var" {
                    return None; // a macro/env_var/target call — unprovable
                }
                self.p.bump();
                let mut args = Vec::new();
                loop {
                    self.p.ws();
                    if self.p.peek() == Some(')') {
                        self.p.bump();
                        break;
                    }
                    args.push(self.parse_or()?);
                    self.p.ws();
                    match self.p.bump()? {
                        ',' => continue,
                        ')' => break,
                        _ => return None,
                    }
                }
                match args.as_slice() {
                    // `var('name')` — under the tolerant render a missing
                    // name yields `none` (the enabled position then fails
                    // validation); under the strict passes it raises —
                    // unprovable here.
                    [Val::V(serde_json::Value::String(name))] => {
                        self.used_var = true;
                        match (self.lookup)(name) {
                            Some(v) => Some(Val::V(v)),
                            None if self.missing_none => Some(Val::V(serde_json::Value::Null)),
                            None => None,
                        }
                    }
                    [Val::V(serde_json::Value::String(name)), default] => {
                        self.used_var = true;
                        Some(
                            (self.lookup)(name)
                                .map(Val::V)
                                .unwrap_or_else(|| default.clone()),
                        )
                    }
                    _ => None, // var(), var(5), var('a','b','c'), a non-string name
                }
            }
        }
    }
}

/// Comparison operators the `enabled` subset recognizes.
#[derive(Clone, Copy)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
}

/// Python truthiness: `None`/`false`/`0`/`''`/empty containers are falsy;
/// `'false'` and `'0'` are TRUTHY (non-empty strings). Jinja's
/// `Undefined` is falsy too.
fn truthy(v: &Val) -> bool {
    match v {
        Val::Undefined => false,
        Val::V(serde_json::Value::Null) => false,
        Val::V(serde_json::Value::Bool(b)) => *b,
        Val::V(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Val::V(serde_json::Value::String(s)) => !s.is_empty(),
        Val::V(serde_json::Value::Array(a)) => !a.is_empty(),
        Val::V(serde_json::Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `==`: bool compares as `0`/`1` against numbers, ints compare
/// numerically across int/float, everything else is strict — a string is
/// never a number and a number is never `True`'s string form. Jinja's
/// `Undefined` equals only another `Undefined` (each lookup mints a new
/// one, but `Undefined() == Undefined()` holds — verified).
fn py_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Undefined, Val::Undefined) => true,
        (Val::Undefined, _) | (_, Val::Undefined) => false,
        (Val::V(a), Val::V(b)) => py_eq_j(a, b),
    }
}

/// `py_eq` on two `serde_json` values — containers can't hold a
/// `Undefined` ([`val_seq`] refuses them), so every member is a plain
/// `Val::V` and the recursion stays on raw values.
fn py_eq_j(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value::*;
    match (a, b) {
        (Null, Null) => true,
        (Bool(x), Bool(y)) => x == y,
        (Bool(x), Number(y)) => num_eq(&serde_json::Number::from(*x as u8), y),
        (Number(x), Bool(y)) => num_eq(x, &serde_json::Number::from(*y as u8)),
        (Number(x), Number(y)) => num_eq(x, y),
        (String(x), String(y)) => x == y,
        (Array(x), Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(u, v)| py_eq_j(u, v)),
        (Object(x), Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| py_eq_j(v, w)))
        }
        _ => false,
    }
}

/// Numeric equality across serde_json's i64/u64/f64 split — exact for
/// integers (u64-vs-i64 included), `as_f64` only when a float is present.
fn num_eq(x: &serde_json::Number, y: &serde_json::Number) -> bool {
    if let (Some(a), Some(b)) = (x.as_i64(), y.as_i64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (x.as_u64(), y.as_u64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (x.as_i64(), y.as_u64()) {
        return u64::try_from(a).is_ok_and(|a| a == b);
    }
    if let (Some(a), Some(b)) = (x.as_u64(), y.as_i64()) {
        return u64::try_from(b).is_ok_and(|b| a == b);
    }
    x.as_f64() == y.as_f64()
}

/// Python ordering (`<`/`<=`/`>`/`>=`): numbers against numbers (a bool
/// counts as 0/1), strings lexicographically, lists element-wise — every
/// other pairing is a TypeError dbt would raise (`None` here). An
/// `Undefined` operand TypeErrors too — Jinja's `missing < x` raises.
fn py_cmp(a: &Val, b: &Val) -> Option<std::cmp::Ordering> {
    let (Val::V(a), Val::V(b)) = (a, b) else {
        return None;
    };
    py_cmp_j(a, b)
}

/// `py_cmp` on raw values — containers can't hold a `Undefined`, so the
/// elementwise recursion stays on `serde_json` (see [`py_eq_j`]).
fn py_cmp_j(a: &serde_json::Value, b: &serde_json::Value) -> Option<std::cmp::Ordering> {
    use serde_json::Value::*;
    match (a, b) {
        (Bool(x), Bool(y)) => Some(x.cmp(y)),
        (Bool(x), Number(y)) | (Number(y), Bool(x)) => {
            num_cmp(&serde_json::Number::from(*x as u8), y)
        }
        (Number(x), Number(y)) => num_cmp(x, y),
        (String(x), String(y)) => Some(x.cmp(y)),
        (Array(x), Array(y)) => {
            for (u, v) in x.iter().zip(y.iter()) {
                match py_eq_j(u, v) {
                    true => continue,
                    false => return py_cmp_j(u, v),
                }
            }
            Some(x.len().cmp(&y.len()))
        }
        _ => None,
    }
}

/// Ordering on serde_json's split number types — `as_f64` is total here
/// (the u64/i64 edge both fit a float well enough for a comparison).
fn num_cmp(x: &serde_json::Number, y: &serde_json::Number) -> Option<std::cmp::Ordering> {
    if let (Some(a), Some(b)) = (x.as_i64(), y.as_i64()) {
        return Some(a.cmp(&b));
    }
    if let (Some(a), Some(b)) = (x.as_u64(), y.as_u64()) {
        return Some(a.cmp(&b));
    }
    if let (Some(a), Some(b)) = (x.as_i64(), y.as_u64()) {
        return match u64::try_from(a) {
            Ok(a) => Some(a.cmp(&b)),
            Err(_) => Some(std::cmp::Ordering::Less), // negative i64 < any u64
        };
    }
    if let (Some(a), Some(b)) = (x.as_u64(), y.as_i64()) {
        return match u64::try_from(b) {
            Ok(b) => Some(a.cmp(&b)),
            Err(_) => Some(std::cmp::Ordering::Greater),
        };
    }
    x.as_f64().and_then(|a| y.as_f64().map(|b| a.total_cmp(&b)))
}

/// `x in container` — list membership by `py_eq`, a string substring
/// (the needle must be a string), or a dict key lookup. Every other
/// shape is a Python TypeError — unresolvable (`None`); an `Undefined`
/// operand raises too.
fn contains(container: &Val, x: &Val) -> Option<bool> {
    let (Val::V(container), Val::V(x)) = (container, x) else {
        return None;
    };
    match (container, x) {
        (serde_json::Value::Array(items), _) => Some(items.iter().any(|i| py_eq_j(i, x))),
        (serde_json::Value::String(s), serde_json::Value::String(sub)) => Some(s.contains(sub)),
        (serde_json::Value::Object(m), serde_json::Value::String(k)) => Some(m.contains_key(k)),
        _ => None,
    }
}

/// A sequence/tuple's `Val` elements → the JSON array — `None` when one
/// is `Undefined`: a container can't carry it in this representation,
/// and the enabled position never needs one (dbt would hold it — but an
/// unprovable degrade is the safe direction).
fn val_seq(items: Vec<Val>) -> Option<Val> {
    items
        .into_iter()
        .map(|v| match v {
            Val::V(v) => Some(v),
            Val::Undefined => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|v| Val::V(v.into()))
}

/// `| as_bool` — dbt's `BoolMarker` semantics: `literal_eval` the value
/// and demand a Python bool, so only real bools and the exact strings
/// `'True'`/`'False'` pass; `'true'`/`'yes'`/`0` all raise (→ `None`),
/// as does an `Undefined`.
fn as_bool(v: &Val) -> Option<Val> {
    match v {
        Val::V(serde_json::Value::Bool(_)) => Some(v.clone()),
        Val::V(serde_json::Value::String(s)) => match s.trim() {
            "True" => Some(Val::V(true.into())),
            "False" => Some(Val::V(false.into())),
            _ => None,
        },
        _ => None,
    }
}

/// `x % 2` for the `even`/`odd` tests — a real number (a bool counts as
/// `0`/`1`, like Python's `True % 2`); any other shape is a Python
/// TypeError — unprovable (`None`), never `false`.
fn num_val(v: &Val) -> Option<f64> {
    match v {
        Val::V(serde_json::Value::Number(n)) => n.as_f64(),
        Val::V(serde_json::Value::Bool(b)) => Some(*b as u8 as f64),
        _ => None,
    }
}

/// `x is sameas y` — Python `is`: identity, not equality. `True`/`False`/
/// `None` are singletons (value equality); ints inside CPython's
/// small-int cache (-5..=256) are singletons too — verified on dbt
/// 1.12.6: `var('n') is sameas 1` renders True, `… 300` renders False.
/// Equal strings stay unprovable: identity there is interning- and
/// provenance-dependent (`var('s') is sameas 'yes'` renders False in
/// dbt, the same literal twice renders True). Everything else — floats,
/// out-of-range ints, containers, `Undefined`, cross-type pairs — is
/// `False`: freshly boxed values are never the same object as a literal.
fn py_is(a: &Val, b: &Val) -> Option<bool> {
    use serde_json::Value as J;
    match (a, b) {
        (Val::V(J::Bool(x)), Val::V(J::Bool(y))) => Some(x == y),
        (Val::V(J::Null), Val::V(J::Null)) => Some(true),
        (Val::V(J::Number(x)), Val::V(J::Number(y))) => Some(small_int_is(x, y)),
        (Val::V(J::String(x)), Val::V(J::String(y))) if x == y => None,
        _ => Some(false),
    }
}

/// `x is y` for numbers — `True` iff both are cached small ints
/// (-5..=256) of one value; a float or an out-of-range int is a distinct
/// object from any literal/var-loaded twin (verified: `300 is sameas
/// 300` and `1.5 is sameas 1.5` both render False under dbt 1.12.6).
fn small_int_is(x: &serde_json::Number, y: &serde_json::Number) -> bool {
    match (x.as_i64(), y.as_i64()) {
        (Some(a), Some(b)) => (-5..=256).contains(&a) && (-5..=256).contains(&b) && a == b,
        _ => false,
    }
}

/// The expression inside a string that is exactly one `{{ ... }}` block,
/// else `None`. Whitespace OUTSIDE the delimiters renders literally —
/// dbt then sees " False " and fails validation — unless a `{{-`/`-}}`
/// dash on that side strips it, so outer whitespace is tolerated only
/// with the matching whitespace-control dash (mixed text renders to a
/// non-bool string anyway, which fails dbt's validation).
fn jinja_expr(s: &str) -> Option<&str> {
    let lead_ws = s.len() - s.trim_start().len();
    let trail_ws = s.len() - s.trim_end().len();
    let inner = s.trim().strip_prefix("{{")?.strip_suffix("}}")?;
    let (inner, lctl) = match inner.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (inner, false),
    };
    let (inner, rctl) = match inner.strip_suffix('-') {
        Some(rest) => (rest, true),
        None => (inner, false),
    };
    if (lead_ws > 0 && !lctl) || (trail_ws > 0 && !rctl) {
        return None;
    }
    let inner = inner.trim();
    (!inner.is_empty()).then_some(inner)
}
