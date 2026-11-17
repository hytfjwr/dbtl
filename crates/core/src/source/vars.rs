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
            serde_json::Value::String(s) => {
                // The YAML render passes carry no bound-name table and
                // no macro table — dbt leaves macro names undefined in
                // both (a macro call there is a "… is undefined" render
                // error, degraded here to unprovable).
                let scope = EvalScope {
                    lookup: &lookup,
                    missing_none: false,
                    bound: None,
                    macros: None,
                    proj: &self.proj,
                    locals: None,
                    depth: MAX_MACRO_DEPTH,
                    render: None,
                };
                jinja_expr(s)
                    .and_then(|e| eval_enabled_expr(e, scope))
                    .and_then(|v| v.as_bool())
            }
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
    /// not found"), so `missing_none` is on. `bound` names what the
    /// template itself binds (its `{% set %}`/`{% for %}`/`{% macro %}`
    /// names) — those resolve to a value at render but can't be read
    /// from source, so they — and dbt's own parse-context names — make
    /// the expression unprovable rather than `Undefined`. `macros` is the
    /// project's macro table — the in-file context is the one place dbt
    /// lets a project macro call resolve.
    pub(super) fn node_enabled(
        &self,
        expr: &str,
        bound: &BoundNames,
        macros: &Macros,
    ) -> Option<bool> {
        let lookup = |n: &str| self.node_scope(n);
        eval_enabled_expr(expr, self.node_scope_ctx(bound, macros, &lookup))
            .and_then(|v| v.as_bool())
    }

    /// The truthiness an `{% if %}`/`{% elif %}` condition resolves to in
    /// the same node scope — the same evaluator [`node_enabled`] uses,
    /// read for truthiness rather than bool validation (a `var()`-
    /// produced string is enough to take an arm). `None` is unprovable —
    /// the arm can't be selected and the caller keeps the node and
    /// marks it instead of guessing.
    pub(super) fn node_condition(
        &self,
        expr: &str,
        bound: &BoundNames,
        macros: &Macros,
    ) -> Option<bool> {
        let lookup = |n: &str| self.node_scope(n);
        eval_enabled_expr(expr, self.node_scope_ctx(bound, macros, &lookup))
            .and_then(|v| truthy(&v))
    }

    /// The node-render [`EvalScope`] — the tolerant `var()` table, the
    /// file's bound names, the project's macro table.
    fn node_scope_ctx<'a>(
        &'a self,
        bound: &'a BoundNames,
        macros: &'a Macros,
        lookup: &'a dyn Fn(&str) -> Option<serde_json::Value>,
    ) -> EvalScope<'a> {
        EvalScope {
            lookup,
            missing_none: true,
            bound: Some(bound),
            macros: Some(macros),
            proj: &self.proj,
            locals: None,
            depth: MAX_MACRO_DEPTH,
            render: None,
        }
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

/// A value the Jinja-expression evaluator produced — a resolved
/// Python/Jinja value, or Jinja's `Undefined` (a bare name the context
/// never carries). The two are alike almost everywhere — `Undefined`
/// is falsy, `==` only to itself, and fails the leaf's bool validation
/// just like a non-bool — EXCEPT under the `is defined`/`is undefined`/
/// `is none` tests: Python `None` IS defined while a missing name is
/// not. The `enabled` passes only ever produce `V`/`Undefined`; the
/// remaining variants exist only under the render context
/// [`eval_render`] runs — a macro body's own value domain, where JSON
/// can't model what an expression yields (a tuple is not a list,
/// `range` is lazy, a `ref()` is a relation object, `loop` is Jinja's
/// LoopContext).
#[derive(Clone, Debug)]
pub(crate) enum Val {
    /// A JSON-modelled value — the `enabled` passes' everything, and
    /// the render's strings/ints/bools/none.
    V(serde_json::Value),
    /// A float literal — `Some(spelling)` only when the literal's own
    /// spelling already IS Python's `repr` (`1.5`, `0.25`, `2.0`); the
    /// `{{ }}` emitter may print a float only through that provable
    /// spelling.
    Float(f64, Option<String>),
    /// `[a, b]` — a render-context list.
    List(Vec<Val>),
    /// `(a, b)`/`()` — a render-context tuple, a kind apart from
    /// `List` (`(1, 2) == [1, 2]` is Python-False but unprovable here).
    Tuple(Vec<Val>),
    /// `{k: v}` — a render-context dict, insertion-ordered.
    Dict(Vec<(String, Val)>),
    /// `range(start, stop, step)` — `len` precomputed (a range past
    /// [`RANGE_CAP`] is never minted).
    Range { start: i64, step: i64, len: usize },
    /// A `ref()`/`source()` relation object — emits its relation name
    /// or passes through a call-arg binding; binding it in a `{% set %}`
    /// RHS, comparing it, or conditioning on it is unprovable.
    Relation(Relation),
    /// `loop` inside a `{% for %}` body — `index0`/`length` of the
    /// post-filter sequence.
    Loop(LoopInfo),
    /// Jinja's `Undefined` — the `enabled` passes' missing-name
    /// marker; the render context produces none (a missing name fails
    /// the eval).
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

    /// What `{% for x in v %}` / `{% set a, b = v %}` / `v | join`
    /// iterate — `None` when `v` isn't iterable (a number, `none`, a
    /// relation, `loop`). A dict iterates its keys, a string its
    /// characters — Python's own rules.
    pub(crate) fn iterate(&self) -> Option<Seq> {
        match self {
            Val::V(serde_json::Value::String(s)) => Some(Seq::Chars(s.chars().collect())),
            Val::V(serde_json::Value::Array(a)) => {
                Some(Seq::Items(a.iter().map(|v| Val::V(v.clone())).collect()))
            }
            Val::V(serde_json::Value::Object(m)) => Some(Seq::Items(
                m.keys().map(|k| Val::V(k.clone().into())).collect(),
            )),
            Val::List(items) | Val::Tuple(items) => Some(Seq::Items(items.clone())),
            Val::Dict(d) => Some(Seq::Items(
                d.iter().map(|(k, _)| Val::V(k.clone().into())).collect(),
            )),
            Val::Range { start, step, len } => Some(Seq::Range {
                start: *start,
                step: *step,
                len: *len,
            }),
            _ => None,
        }
    }
}

/// The relation a `ref()`/`source()` binds — a name the render emits
/// or passes through, never a value it compares.
#[derive(Clone, Debug)]
pub(crate) enum Relation {
    /// `ref('name')`.
    Ref(String),
    /// `source('src', 'tbl')`.
    Source(String, String),
}

/// `loop` inside one `{% for %}` iteration — the post-filter index
/// (`index0`, 0-based) and sequence `length`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoopInfo {
    /// The iteration's 0-based position in the post-filter sequence.
    pub(crate) index0: usize,
    /// The post-filter sequence's length.
    pub(crate) len: usize,
}

/// An iterable's view for `{% for %}`/`{% set a, b = %}` — `Range`
/// stays lazy (a 100k range allocates nothing).
pub(crate) enum Seq {
    /// Materialized elements (list/tuple/dict keys).
    Items(Vec<Val>),
    /// `range` elements computed on demand.
    Range { start: i64, step: i64, len: usize },
    /// A string's characters.
    Chars(Vec<char>),
}

impl Seq {
    /// Element count — `{% for %}` bounds and `loop.length` share it.
    pub(crate) fn len(&self) -> usize {
        match self {
            Seq::Items(v) => v.len(),
            Seq::Range { len, .. } => *len,
            Seq::Chars(c) => c.len(),
        }
    }

    /// The `i`th element as a value.
    pub(crate) fn item(&self, i: usize) -> Val {
        match self {
            Seq::Items(v) => v[i].clone(),
            Seq::Range { start, step, .. } => Val::V((start + i as i64 * step).into()),
            Seq::Chars(c) => Val::V(c[i].to_string().into()),
        }
    }
}

/// A parsed call's argument list — positional values, then the
/// `(name, value)` keyword args in order.
pub(crate) type ArgVals = (Vec<Val>, Vec<(String, Val)>);

/// The largest `range(…)` a macro body may mint — `| length`, `in`,
/// and `{% for %}` all compute off it (Python's own range is lazy and
/// unbounded, but a `range(200000)` shape is past anything a render
/// needs to prove — the loop/output caps would cut it anyway).
const RANGE_CAP: usize = 100_000;

/// The render context an `enabled`/`{% if %}` expression evaluates
/// under — one per render pass. `lookup`/`missing_none` carry the pass's
/// `var()` table and missing-name rule; `bound` names what the template
/// binds (in-file passes only); `macros` is the project's macro table —
/// `Some` only for the in-file `config()`/`{% if %}` passes (the one
/// place dbt lets a project macro call resolve — the YAML renders leave
/// the names undefined); `locals` is a macro body's own frame — its
/// params and `{% set %}` names — `Some` while a body evaluates, even
/// when empty; `depth` is the remaining project-macro call budget.
#[derive(Clone)]
struct EvalScope<'b> {
    lookup: &'b dyn Fn(&str) -> Option<serde_json::Value>,
    /// The render pass's missing-`var()` rule — see [`eval_enabled_expr`].
    missing_none: bool,
    /// The node render's bound-name table — `Some` for an in-file
    /// `config()` or `{% if %}`/`{% elif %}` condition (see
    /// [`Vars::node_enabled`]), `None` for the YAML render passes, where
    /// every bare name stays `Undefined`.
    bound: Option<&'b BoundNames>,
    /// The project's macro table — `Some` for the in-file passes only.
    macros: Option<&'b Macros>,
    /// The root project name — `<proj>.macro(…)` resolves into `macros`.
    proj: &'b str,
    /// A macro body's local frame — `Some` while one evaluates.
    locals: Option<&'b HashMap<String, Val>>,
    /// Remaining project-macro calls the evaluator will follow —
    /// [`MAX_MACRO_DEPTH`]-bounded like the edge pass's.
    depth: usize,
    /// `Some` marks the scope a macro-body render — the `enabled`
    /// fields it replaces (`lookup`/`missing_none`/`bound`/`macros`/
    /// `locals`/`depth`) are dead there.
    render: Option<RenderCx<'b>>,
}

/// The render-context half of [`EvalScope`] — the value domain a macro
/// body's own Jinja evaluates under. Same grammar as the `enabled`
/// subset; the differences are what a name resolves to (the frame
/// only — params, `{% set %}` locals, loop variables, `loop`), which
/// calls exist (`range`/`ref`/`source` only — macro names resolve in
/// the caller, everything else is unprovable), which literals are
/// exact (strings honor `\\ \' \" \n \t \r` escapes only; ints are
/// i64 with no leading zeros; floats carry a canonicality flag for the
/// `{{ }}` emitter), and what postfix allows (`.attr` on `loop` only;
/// `|` on `join`/`length` only).
#[derive(Clone, Copy)]
struct RenderCx<'b> {
    /// The scope stack — innermost last — of param/`{% set %}`/loop
    /// bindings. A bare name reads no further than this.
    frame: &'b [HashMap<String, Val>],
    /// Model-level `{{ m(…) }}` argument position — narrows the
    /// grammar to plain literals plus `ref`/`source` calls: no
    /// operators, tests, filters, `range`, or names. Anything else
    /// leaves input unconsumed and fails the eval's end check.
    literals_only: bool,
}

/// Evaluate a Jinja `enabled` expression to the Python value it resolves
/// to: literals, `var()` calls, `==`/`!=`/`<`/`<=`/`>`/`>=` chains, `in` /
/// `not in` membership, `and`/`or`/`not` (Python truthiness — `and`/`or`
/// return operand values, non-empty strings and nonzero numbers are
/// truthy), parentheses, the `| as_bool` filter, and — when the scope
/// carries the macro table — calls to value-shaped project macros.
/// `missing_none` picks the render pass's missing-`var()` rule: ON for
/// an in-file `config()` (a missing name yields `none` — dbt's
/// require_vars=False), OFF for the `dbt_project.yml` and schema.yml
/// renders (a missing name raises — unprovable here). `None` means the
/// expression is unprovable from source — a syntax error, an unsupported
/// call/filter (`target.name`, `env_var()`, a package macro or a body
/// outside the value shape), or a Python type error — which dbt would
/// surface as a parse error; callers degrade it to enabled + warning.
fn eval_enabled_expr(expr: &str, scope: EvalScope) -> Option<Val> {
    let mut e = EnabledExpr {
        p: LitParser { s: expr, i: 0 },
        scope,
        used_var: false,
    };
    let v = e.parse_or()?;
    e.p.ws();
    (e.p.i == expr.len()).then_some(v)
}

/// Evaluate `expr` inside a project macro body's render — the
/// [`RenderCx`] context: literal values, the frame's bound names
/// (params, `{% set %}` locals, loop variables, `loop`),
/// `ref`/`source`/`range` calls, the shared operator grammar, `| join`
/// / `| length` filters, and `is` tests. `None` on anything dbt would
/// fail or source can't prove — a name outside the frame, a
/// context/builtin call (`var`, `config`, `target`, `execute`,
/// `adapter`, `exceptions`, `caller`, and macro calls, which the caller
/// resolves), an attribute read that isn't `loop.<field>`, a subscript,
/// an unknown filter, a type error, or trailing input. `literals_only`
/// narrows the grammar to plain literals + `ref`/`source` calls — the
/// shape a model-level `{{ m(…) }}` argument accepts.
pub(crate) fn eval_render(
    expr: &str,
    frame: &[HashMap<String, Val>],
    literals_only: bool,
) -> Option<Val> {
    let no_vars = |_: &str| None;
    let mut e = EnabledExpr {
        p: LitParser { s: expr, i: 0 },
        scope: EvalScope {
            lookup: &no_vars,
            missing_none: false,
            bound: None,
            macros: None,
            proj: "",
            locals: None,
            depth: 0,
            render: Some(RenderCx {
                frame,
                literals_only,
            }),
        },
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
    /// The render context — see [`EvalScope`].
    scope: EvalScope<'b>,
    /// `var()` was consulted in the current operand — an `is sameas`
    /// argument is provable only when it is a pure literal (a var-drawn
    /// operand could share the operand object's identity, which value
    /// comparison can't see).
    used_var: bool,
}

/// The names dbt's model parse context always binds (the macro callables
/// `ref`/`source`/`config`, the `target`/`this`/`model` objects, the
/// `env_var`/`log`/`modules` helpers, run metadata like `run_started_at`
/// and `invocation_id`, the `is_incremental`/`sql_now`-style builtins and
/// `caller`, the value `{% call %}` blocks expose) — defined but
/// unreadable from source, so an `enabled` expression or `{% if %}`
/// condition that names one is unprovable rather than `Undefined`.
/// `execute` is deliberately NOT here — it is the one context name whose
/// parse-time value dbt fixes (falsy), so it evaluates instead.
const DBT_PARSE_NAMES: &[&str] = &[
    "adapter",
    "api",
    "as_bool",
    "as_native",
    "as_number",
    "as_text",
    "builtins",
    "caller",
    "config",
    "dbt_version",
    "deployment_env",
    "docs",
    "env_var",
    "exceptions",
    "flags",
    "fromjson",
    "fromyaml",
    "graph",
    "invocation_id",
    "is_incremental",
    "load_agate_table",
    "load_result",
    "local_md5",
    "log",
    "model",
    "modules",
    "project_name",
    "ref",
    "return",
    "run_query",
    "run_started_at",
    "schema",
    "selected_resources",
    "sql",
    "sql_now",
    "statement",
    "store_raw_result",
    "store_result",
    "submit_python_job",
    "target",
    "this",
    "thread_id",
    "tojson",
    "toyaml",
    "var",
];

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

    /// The render context this expression evaluates under, if any.
    fn render(&self) -> Option<RenderCx<'_>> {
        self.scope.render
    }

    /// Model-level `{{ m(…) }}` argument position — the literal-only
    /// sub-grammar (see [`RenderCx::literals_only`]): every operator
    /// level is left unconsumed so a `+`/`<`/`is`/`|`/`or`/`and`/`not`
    /// trailing a literal fails the eval's end-of-input check.
    fn literals_only(&self) -> bool {
        self.scope.render.is_some_and(|r| r.literals_only)
    }

    /// A frame lookup — the innermost scope wins; a miss is unprovable.
    fn frame_get(&self, name: &str) -> Option<Val> {
        self.scope
            .render?
            .frame
            .iter()
            .rev()
            .find_map(|f| f.get(name))
            .cloned()
    }

    /// `a or b or …` — Python `or`: the first truthy operand's value,
    /// else the last one. The operand value matters (`0 or false` is
    /// `false`), so a short-circuit only excuses the rhs being
    /// UNPROVABLE, never its parse.
    fn parse_or(&mut self) -> Option<Val> {
        if self.literals_only() {
            return self.parse_and();
        }
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
            let r = self.parse_and()?;
            if !truthy(&l)? {
                l = r;
            }
        }
    }

    /// `a and b and …` — Python `and`: the first falsy operand's value,
    /// else the last one.
    fn parse_and(&mut self) -> Option<Val> {
        if self.literals_only() {
            return self.parse_not();
        }
        let mut l = self.parse_not()?;
        loop {
            self.p.ws();
            if !self.eat_kw("and") {
                return Some(l);
            }
            // Same rule as `parse_or` — see the note there.
            let r = self.parse_not()?;
            if truthy(&l)? {
                l = r;
            }
        }
    }

    /// `not x` — a real bool result (`not` binds tighter than `and`/`or`,
    /// looser than comparisons, so `not a == b` is `not (a == b)`).
    fn parse_not(&mut self) -> Option<Val> {
        if self.literals_only() {
            return self.parse_cmp();
        }
        self.p.ws();
        if self.eat_kw("not") {
            let v = self.parse_not()?;
            Some(Val::V((!truthy(&v)?).into()))
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
        if self.literals_only() {
            return self.parse_postfix();
        }
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
            Cmp::Eq => py_eq(a, b),
            Cmp::Ne => py_eq(a, b).map(|b| !b),
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

    /// A primary plus its postfix chain — `| filter`, `.attr`,
    /// `[sub]`, `(args)`, `is` tests. The `enabled` pass resolves only
    /// `as_bool` + the supported tests (`target.name`, `env_var(…)`,
    /// `x[0]`, a macro call are unprovable); the render context
    /// additionally resolves `join`/`length` and `loop.<field>` — its
    /// only attribute reads.
    fn parse_postfix(&mut self) -> Option<Val> {
        let mut v = self.parse_primary()?;
        if self.literals_only() {
            return Some(v);
        }
        loop {
            self.p.ws();
            match self.p.peek() {
                Some('|') => {
                    self.p.bump();
                    self.p.ws();
                    match self.ident()? {
                        "as_bool" if self.render().is_none() => {
                            self.p.ws();
                            if self.rest().starts_with("()") {
                                self.p.i += 2;
                            }
                            v = as_bool(&v)?;
                        }
                        // Render-context filters — `join` over
                        // verbatim-renderable elements, `length` on a
                        // sequence/mapping/string.
                        "join" if self.render().is_some() => {
                            self.p.ws();
                            let sep = if self.p.peek() == Some('(') {
                                self.p.bump();
                                let arg = self.parse_or()?;
                                self.p.ws();
                                if self.p.bump() != Some(')') {
                                    return None;
                                }
                                let Val::V(serde_json::Value::String(s)) = arg else {
                                    return None;
                                };
                                s
                            } else {
                                String::new()
                            };
                            v = render_join(&v, &sep)?;
                        }
                        "length" if self.render().is_some() => {
                            v = render_len(&v)?;
                        }
                        _ => return None,
                    }
                }
                Some('.') if self.render().is_some() => {
                    self.p.bump();
                    v = loop_attr(&v, self.ident()?)?;
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
                        _ => is_test(t, &v)?,
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
                    return Some(if self.render().is_some() {
                        Val::Tuple(vec![])
                    } else {
                        Val::V(serde_json::Value::Array(vec![]))
                    });
                }
                let first = self.parse_or()?;
                self.p.ws();
                match self.p.bump()? {
                    // `(a, b)` / `(a,)` — a tuple: its own kind under
                    // the render context, a JSON array elsewhere.
                    ',' => {
                        let mut items = vec![first];
                        loop {
                            self.p.ws();
                            if self.p.peek() == Some(')') {
                                self.p.bump();
                                return self.seq_end(items, true);
                            }
                            items.push(self.parse_or()?);
                            self.p.ws();
                            match self.p.bump()? {
                                ',' => continue,
                                ')' => return self.seq_end(items, true),
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
            '\'' | '"' if self.render().is_some() => self.render_string().map(|s| Val::V(s.into())),
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
                    Val::Float(f, canon) => {
                        let f = if neg { -f } else { f };
                        Some(Val::Float(f, canon.map(|_| py_float_repr(f))))
                    }
                    _ => None,
                }
            }
            '0'..='9' | '.' if self.render().is_some() => self.render_number(),
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
                return self.seq_end(items, false);
            }
            items.push(self.parse_or()?);
            self.p.ws();
            match self.p.bump()? {
                ',' => continue,
                ']' => return self.seq_end(items, false),
                _ => return None,
            }
        }
    }

    /// The `[…]`/`(…)` element list → the mode's sequence value — a
    /// `List`/`Tuple` under the render context, a JSON `Array`
    /// otherwise.
    fn seq_end(&self, items: Vec<Val>, tuple: bool) -> Option<Val> {
        if self.render().is_some() {
            if items.iter().any(|v| matches!(v, Val::Undefined)) {
                return None;
            }
            return Some(if tuple {
                Val::Tuple(items)
            } else {
                Val::List(items)
            });
        }
        val_seq(items)
    }

    /// `{k: v, …}` — keys evaluate like values but must be strings (a
    /// `var()`-produced string key is fine under the enabled pass).
    fn map_literal(&mut self) -> Option<Val> {
        self.p.bump();
        let mut obj = serde_json::Map::new();
        let mut pairs: Vec<(String, Val)> = Vec::new();
        loop {
            self.p.ws();
            if self.p.peek() == Some('}') {
                self.p.bump();
                return Some(self.map_end(obj, pairs));
            }
            let key = match self.parse_or()? {
                Val::V(serde_json::Value::String(s)) => s,
                _ => return None,
            };
            self.p.ws();
            if self.p.bump() != Some(':') {
                return None;
            }
            let v = self.parse_or()?;
            if self.render().is_some() {
                // A `{k: Undefined}` member is a dbt-side error, not a
                // value the map carries — unprovable.
                if matches!(v, Val::Undefined) {
                    return None;
                }
                pairs.push((key, v));
            } else {
                let Val::V(v) = v else {
                    return None;
                };
                obj.insert(key, v);
            }
            self.p.ws();
            match self.p.bump()? {
                ',' => continue,
                '}' => return Some(self.map_end(obj, pairs)),
                _ => return None,
            }
        }
    }

    /// The `{k: v}` members → the mode's map value — a `Dict` under
    /// the render context, a JSON `Object` otherwise.
    fn map_end(
        &self,
        obj: serde_json::Map<String, serde_json::Value>,
        pairs: Vec<(String, Val)>,
    ) -> Val {
        if self.render().is_some() {
            Val::Dict(pairs)
        } else {
            Val::V(obj.into())
        }
    }

    /// A keyword literal, a `var(…)` call, a `name(…)`/`ns.name(…)`
    /// project-macro call (when the pass carries the macro table), or a
    /// bare name — Jinja resolves an unknown name to `Undefined`
    /// ([`Val::Undefined`] stands in: falsy, `==` only to itself, fails
    /// `is defined` and the leaf's bool validation).
    fn parse_name_or_call(&mut self) -> Option<Val> {
        let name = self.ident()?;
        match name {
            "true" | "True" => Some(Val::V(true.into())),
            "false" | "False" => Some(Val::V(false.into())),
            "none" | "None" => Some(Val::V(serde_json::Value::Null)),
            // The render context resolves names through the frame
            // only — `null`/`Null` are just NAMES there (Python/Jinja
            // spell `none`), so they stay on the generic path.
            _ if self.render().is_some() => self.render_name(name),
            "null" | "Null" => Some(Val::V(serde_json::Value::Null)),
            _ => {
                // A `ns.rest` chain ending in `(` is ONE callee spelling
                // (`demo.bool_var(…)`); a `.attr` without the call is
                // the unprovable postfix read — rewind so it rejects.
                let head_pos = self.p.i;
                self.p.ws();
                let mut spelling = name.to_string();
                loop {
                    let save = self.p.i;
                    if self.p.peek() == Some('.') {
                        self.p.bump();
                        self.p.ws();
                        if let Some(part) = self.ident() {
                            spelling.push('.');
                            spelling.push_str(part);
                            self.p.ws();
                            continue;
                        }
                    }
                    self.p.i = save;
                    break;
                }
                if self.p.peek() != Some('(') {
                    self.p.i = head_pos;
                    return self.bare_name(name);
                }
                if spelling != "var" {
                    return self.macro_call(&spelling);
                }
                // `var` resolves like any other callee spelling: a bound
                // name — or the project's own `var` macro — shadows the
                // builtin (the table's classify already puts project defs
                // ahead of the builtins — dbt's resolution). Only an
                // unclaimed `var` reads the vars table.
                if self.callee_shadowed(&spelling) {
                    return None;
                }
                if self.scope.macros.is_some_and(|m| {
                    matches!(
                        m.classify_callee(&spelling, self.scope.proj),
                        Callee::Project(_)
                    )
                }) {
                    return self.macro_call(&spelling);
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
                        match (self.scope.lookup)(name) {
                            Some(v) => Some(Val::V(v)),
                            None if self.scope.missing_none => {
                                Some(Val::V(serde_json::Value::Null))
                            }
                            None => None,
                        }
                    }
                    [Val::V(serde_json::Value::String(name)), default] => {
                        self.used_var = true;
                        Some(
                            (self.scope.lookup)(name)
                                .map(Val::V)
                                .unwrap_or_else(|| default.clone()),
                        )
                    }
                    _ => None, // var(), var(5), var('a','b','c'), a non-string name
                }
            }
        }
    }

    /// The callee's head segment names a value the frame or file binds —
    /// a `{% set %}`/`{% for %}`/param/local name — shadowing both the
    /// macro table and the builtins. A value isn't callable, so the call
    /// is unprovable.
    fn callee_shadowed(&self, spelling: &str) -> bool {
        let head = spelling.split('.').next().unwrap_or(spelling);
        self.scope.locals.is_some_and(|f| f.contains_key(head))
            || self.scope.bound.is_some_and(|b| b.contains(head))
    }

    /// A bare name at leaf position — inside a macro body the params
    /// and `{% set %}` names resolve to their bound values first. After
    /// them, a name the template binds (`{% set %}`/`{% for %}`/
    /// `{% macro %}`… — the caller file's names reach a macro through
    /// the render context) or dbt's parse context carries is defined but
    /// unprovable — never an `Undefined` the `is defined` test could
    /// prove missing — and a name defined nowhere stays `Undefined`,
    /// the same as dbt.
    fn bare_name(&self, name: &str) -> Option<Val> {
        if self.scope.bound.is_none() && self.scope.locals.is_none() {
            // A YAML render pass — every bare name stays `Undefined`.
            return Some(Val::Undefined);
        }
        if let Some(v) = self.scope.locals.and_then(|f| f.get(name)) {
            return Some(v.clone());
        }
        if self.scope.bound.is_some_and(|b| b.contains(name)) {
            return None;
        }
        // `execute` is the parse-time falsy dbt fixes (the run-time flag
        // can't be read from source) — when nothing binds it first (a
        // `{% set %}` or macro param CAN shadow the name, like any other).
        if name == "execute" {
            return Some(Val::V(false.into()));
        }
        if DBT_PARSE_NAMES.contains(&name) {
            return None;
        }
        Some(Val::Undefined)
    }

    /// A `name(…)`/`ns.name(…)` call — `(` at the cursor — resolved
    /// through the macro table: a project macro's value-shape body
    /// evaluates to its `return(…)`; everything else (builtins, package
    /// namespaces, `adapter.dispatch`, the dep calls) is unprovable.
    fn macro_call(&mut self, spelling: &str) -> Option<Val> {
        let (args_text, end) = scan_call_args(self.p.s, self.p.i + 1)?;
        self.p.i = end;
        // A name the caller's frame binds shadows the macro table — a
        // `{% set %}`/`{% for %}`/param/local name resolves to a value,
        // and a value isn't callable.
        if self.callee_shadowed(spelling) {
            return None;
        }
        let def = match self
            .scope
            .macros?
            .classify_callee(spelling, self.scope.proj)
        {
            Callee::Project(def) => def,
            _ => return None,
        };
        let shape = def.shape.as_ref()?;
        if self.scope.depth == 0 {
            return None;
        }
        // Split the args like the edge pass's `classify_args` — a
        // `name=` segment is a kwarg, everything else positional — each
        // arg's expression evaluated in THIS frame (the caller's).
        let mut pos: Vec<Val> = Vec::new();
        let mut kw: Vec<(String, Val)> = Vec::new();
        for seg in top_level_segments(args_text) {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            match split_kwarg(seg) {
                Some((name, expr)) => kw.push((
                    name.to_string(),
                    self.eval_in(expr, self.scope.locals, self.scope.depth)?,
                )),
                None => {
                    // A positional after a keyword — Python's own
                    // parser rejects the call before it binds.
                    if !kw.is_empty() {
                        return None;
                    }
                    pos.push(self.eval_in(seg, self.scope.locals, self.scope.depth)?);
                }
            }
        }
        // A call shape dbt itself rejects — a positional past the
        // arity, a keyword naming no parameter or re-naming a filled
        // one (Python's "multiple values" TypeError) — is a parse
        // error, not a binding.
        if !call_shape_valid(&def.params, &pos, &kw) {
            return None;
        }
        // Bind through the shared positional/keyword/default rule —
        // call args bind first, then defaults fill the leftovers
        // evaluated in the callee frame against the params bound so far.
        let slots = bind_slots(&def.params, &pos, &kw);
        let mut frame: HashMap<String, Val> = HashMap::new();
        for (i, slot) in slots.iter().enumerate() {
            let v = match slot {
                Some(SlotSource::Arg(v)) => (**v).clone(),
                Some(SlotSource::Default(d)) => self.eval_body(d, &frame)?,
                // A param the call supplies nothing for — dbt errors
                // on the missing argument.
                None => return None,
            };
            frame.insert(def.params[i].name.clone(), v);
        }
        // The body's straight-line `{% set %}`s extend the frame in
        // declaration order; the final `return` answers the call.
        for (name, expr) in &shape.sets {
            let v = self.eval_body(expr, &frame)?;
            frame.insert(name.clone(), v);
        }
        self.eval_body(&shape.ret, &frame)
    }

    /// Evaluate `expr` in a fresh parser sharing this render context —
    /// `locals`/`depth` carry the callee frame a macro call evaluates
    /// under. A `var()` consulted inside counts toward the caller's
    /// `is sameas` literalness check.
    fn eval_in(
        &mut self,
        expr: &str,
        locals: Option<&HashMap<String, Val>>,
        depth: usize,
    ) -> Option<Val> {
        let mut e = EnabledExpr {
            p: LitParser { s: expr, i: 0 },
            scope: EvalScope {
                locals,
                depth,
                ..self.scope.clone()
            },
            used_var: false,
        };
        let v = e.parse_or()?;
        e.p.ws();
        let done = e.p.i == expr.len();
        self.used_var |= e.used_var;
        done.then_some(v)
    }

    /// [`eval_in`] inside a project macro's body — the body's own
    /// `frame` (params + `{% set %}` names) and one depth unit spent.
    fn eval_body(&mut self, expr: &str, frame: &HashMap<String, Val>) -> Option<Val> {
        self.eval_in(expr, Some(frame), self.scope.depth.saturating_sub(1))
    }

    // -- the render context --------------------------------------------------
    //
    // The methods below only run under [`RenderCx`]: a macro body's own
    // value domain — frame names, `ref`/`source` relations, `range`, and
    // the literal spelling rules the `{{ }}` emitter needs.

    /// A name/call in the render context — frame values only. `ref`/
    /// `source` bind relation objects; `range` binds the lazy sequence
    /// (off under `literals_only` — model-level args take literals +
    /// `ref`/`source` only). Everything else — a name the frame doesn't
    /// bind, a context/builtin call (`var`, `config`, `target`,
    /// `adapter`, `exceptions`, macro calls — the caller resolves
    /// those), or calling a frame value — is unprovable.
    fn render_name(&mut self, name: &str) -> Option<Val> {
        let save = self.p.i;
        self.p.ws();
        if self.p.peek() == Some('(') {
            // A name the frame binds is a VALUE — calling one is
            // Jinja's "not callable" error.
            if self.frame_get(name).is_some() {
                return None;
            }
            return match name {
                "ref" | "source" => self.render_dep(name),
                "range" if !self.literals_only() => self.render_range(),
                _ => None,
            };
        }
        self.p.i = save;
        self.frame_get(name)
    }

    /// `(a, b, k=c)` at the cursor — the `(` is consumed — each arg a
    /// full expression, `name=` segments peeled as kwargs (a `=` that
    /// isn't `==`, the same scan [`split_kwarg`] runs over text).
    fn call_args(&mut self) -> Option<ArgVals> {
        debug_assert_eq!(self.p.peek(), Some('('));
        self.p.bump();
        let mut pos = Vec::new();
        let mut kw: Vec<(String, Val)> = Vec::new();
        loop {
            self.p.ws();
            if self.p.peek() == Some(')') {
                self.p.bump();
                break;
            }
            let save = self.p.i;
            let mut kwarg_name: Option<String> = None;
            if let Some(id) = self.ident() {
                let inner_save = self.p.i;
                self.p.ws();
                if self.p.peek() == Some('=')
                    && self.p.s.as_bytes().get(self.p.i + 1) != Some(&b'=')
                {
                    self.p.bump();
                    kwarg_name = Some(id.to_string());
                } else {
                    self.p.i = inner_save;
                }
            }
            if kwarg_name.is_none() {
                self.p.i = save;
            }
            let v = self.parse_or()?;
            match kwarg_name {
                Some(n) => {
                    if kw.iter().any(|(k, _)| *k == n) {
                        return None;
                    }
                    kw.push((n, v));
                }
                None => {
                    // A positional after a keyword — Python's own
                    // parser rejects the call before it binds.
                    if !kw.is_empty() {
                        return None;
                    }
                    pos.push(v);
                }
            }
            self.p.ws();
            match self.p.bump()? {
                ',' => continue,
                ')' => break,
                _ => return None,
            }
        }
        Some((pos, kw))
    }

    /// `ref(…)`/`source(…)` under the render context — the relation
    /// object the call binds; [`dep_name`] applies the arg-shape rules
    /// (string names, no duplicated slots, pins read but never reach
    /// the text).
    fn render_dep(&mut self, name: &str) -> Option<Val> {
        let (pos, kw) = self.call_args()?;
        dep_name(name, &pos, &kw).map(Val::Relation)
    }

    /// `range(…)` — 1–3 integer args like Python; `len` computes now
    /// so a range past [`RANGE_CAP`] never materializes. Non-int args,
    /// a `0` step, or kwargs are unprovable.
    fn render_range(&mut self) -> Option<Val> {
        let (pos, kw) = self.call_args()?;
        if !kw.is_empty() {
            return None;
        }
        let ints: Option<Vec<i64>> = pos
            .iter()
            .map(|v| match v {
                Val::V(serde_json::Value::Number(n)) => n.as_i64(),
                _ => None,
            })
            .collect();
        let (start, stop, step) = match ints?.as_slice() {
            [s] => (0, *s, 1),
            [a, b] => (*a, *b, 1),
            [a, b, s] => (*a, *b, *s),
            _ => return None,
        };
        let len = range_len(start, stop, step)?;
        if len > RANGE_CAP {
            return None;
        }
        Some(Val::Range { start, step, len })
    }

    /// A `'…'`/`"…"` literal the render context reads — strict escapes
    /// (`\\ \' \" \n \t \r` only). Anything else (`\a`, `\x41`, …) is
    /// a spelling Jinja reads differently than source can prove →
    /// `None`.
    fn render_string(&mut self) -> Option<String> {
        let quote = self.p.bump()?;
        let mut out = String::new();
        loop {
            match self.p.bump()? {
                '\\' => out.push(match self.p.bump()? {
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    _ => return None,
                }),
                c if c == quote => return Some(out),
                c => out.push(c),
            }
        }
    }

    /// A number literal the render context reads — an i64 int
    /// (leading zeros are a Jinja tokenize error; `u64`-sized and
    /// float spellings aren't ints), else a [`Val::Float`] carrying
    /// whether its spelling IS Python's `repr` — the only float the
    /// `{{ }}` emitter may print.
    fn render_number(&mut self) -> Option<Val> {
        let start = self.p.i;
        while self
            .p
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
        {
            self.p.bump();
        }
        let tok = &self.p.s[start..self.p.i];
        if tok.contains(['.', 'e', 'E']) {
            let v: f64 = tok.parse().ok()?;
            if !v.is_finite() {
                return None;
            }
            let canon = (py_float_repr(v) == tok).then(|| tok.to_string());
            Some(Val::Float(v, canon))
        } else {
            if tok.len() > 1 && tok.starts_with('0') {
                // `007` — Jinja's tokenizer rejects a leading zero.
                return None;
            }
            tok.parse::<i64>().ok().map(|i| Val::V(i.into()))
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
/// `Undefined` is falsy too. `None` is a relation operand — the render
/// refuses to judge a `ref()`/`source()` value's truthiness at all.
pub(crate) fn truthy(v: &Val) -> Option<bool> {
    Some(match v {
        Val::Undefined => false,
        Val::V(serde_json::Value::Null) => false,
        Val::V(serde_json::Value::Bool(b)) => *b,
        Val::V(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Val::V(serde_json::Value::String(s)) => !s.is_empty(),
        Val::V(serde_json::Value::Array(a)) => !a.is_empty(),
        Val::V(serde_json::Value::Object(o)) => !o.is_empty(),
        Val::Float(f, _) => *f != 0.0,
        Val::List(x) | Val::Tuple(x) => !x.is_empty(),
        Val::Dict(d) => !d.is_empty(),
        Val::Range { len, .. } => *len > 0,
        Val::Relation(_) => return None,
        Val::Loop(_) => true,
    })
}

/// Python `==`: bool compares as `0`/`1` against numbers, ints compare
/// numerically across int/float, sequences of the SAME kind compare
/// element-wise. Jinja's `Undefined` equals only another `Undefined`
/// (each lookup mints a new one, but `Undefined() == Undefined()` holds
/// — verified). Cross-kind sequence pairs (`(1,2) == [1,2]`,
/// `range(3) == [0,1,2]` — Python answers `False`, but the render
/// refuses to prove the kind distinction) and a relation operand are
/// unprovable — `None`.
fn py_eq(a: &Val, b: &Val) -> Option<bool> {
    use serde_json::Value as J;
    match (a, b) {
        (Val::Relation(_), _) | (_, Val::Relation(_)) => None,
        (Val::Loop(x), Val::Loop(y)) => Some(x == y),
        (Val::Loop(_), _) | (_, Val::Loop(_)) => Some(false),
        (Val::Undefined, Val::Undefined) => Some(true),
        (Val::Undefined, _) | (_, Val::Undefined) => Some(false),
        (Val::V(a), Val::V(b)) => Some(py_eq_j(a, b)),
        (Val::Float(x, _), Val::Float(y, _)) => Some(x == y),
        (Val::Float(x, _), Val::V(n)) | (Val::V(n), Val::Float(x, _)) => match n {
            J::Number(y) => y.as_f64().map(|y| *x == y),
            J::Bool(y) => Some(*x == (*y as u8) as f64),
            _ => Some(false),
        },
        (Val::Float(_, _), _) | (_, Val::Float(_, _)) => Some(false),
        (Val::List(x), Val::List(y)) | (Val::Tuple(x), Val::Tuple(y)) => seq_eq(x, y),
        (Val::Range { .. }, Val::Range { .. }) => range_eq(a, b),
        (
            Val::List(_) | Val::Tuple(_) | Val::Range { .. },
            Val::List(_) | Val::Tuple(_) | Val::Range { .. },
        ) => None,
        (Val::List(_) | Val::Tuple(_) | Val::Range { .. }, _)
        | (_, Val::List(_) | Val::Tuple(_) | Val::Range { .. }) => Some(false),
        (Val::Dict(x), Val::Dict(y)) => dict_eq(x, y),
        (Val::Dict(_), _) | (_, Val::Dict(_)) => Some(false),
    }
}

/// Same-kind sequence `==` — element-wise, `None` propagating.
fn seq_eq(a: &[Val], b: &[Val]) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    for (x, y) in a.iter().zip(b) {
        if !py_eq(x, y)? {
            return Some(false);
        }
    }
    Some(true)
}

/// `range(a) == range(b)` — element-wise over the lazy elements.
fn range_eq(a: &Val, b: &Val) -> Option<bool> {
    let (
        Val::Range {
            start: s1,
            step: t1,
            len: l1,
        },
        Val::Range {
            start: s2,
            step: t2,
            len: l2,
        },
    ) = (a, b)
    else {
        return None;
    };
    if l1 != l2 {
        return Some(false);
    }
    Some((0..*l1).all(|i| *s1 + i as i64 * *t1 == *s2 + i as i64 * *t2))
}

/// `dict == dict` — insertion order aside: every key of one maps equal
/// in the other.
fn dict_eq(a: &[(String, Val)], b: &[(String, Val)]) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    for (k, v) in a {
        let Some(w) = b.iter().find(|(bk, _)| bk == k).map(|(_, bv)| bv) else {
            return Some(false);
        };
        if !py_eq(v, w)? {
            return Some(false);
        }
    }
    Some(true)
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
/// counts as 0/1), strings lexicographically, same-kind sequences
/// element-wise — every other pairing is a TypeError dbt would raise
/// (`None` here): mixed kinds, `range`, dicts, relations, `Undefined`.
fn py_cmp(a: &Val, b: &Val) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Val::V(a), Val::V(b)) => py_cmp_j(a, b),
        (Val::Float(x, _), Val::Float(y, _)) => Some(x.total_cmp(y)),
        (Val::Float(x, _), Val::V(n)) => num_f64_of(n).map(|y| x.total_cmp(&y)),
        (Val::V(n), Val::Float(y, _)) => num_f64_of(n).map(|x| x.total_cmp(y)),
        (Val::List(x), Val::List(y)) | (Val::Tuple(x), Val::Tuple(y)) => seq_cmp(x, y),
        _ => None,
    }
}

/// A serde number/bool as f64 — for ordering against a [`Val::Float`].
fn num_f64_of(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::Bool(b) => Some(*b as u8 as f64),
        _ => None,
    }
}

/// Element-wise ordering of two same-kind sequences — Python's
/// find-first-difference rule.
fn seq_cmp(a: &[Val], b: &[Val]) -> Option<std::cmp::Ordering> {
    for (x, y) in a.iter().zip(b) {
        if py_eq(x, y)? {
            continue;
        }
        return py_cmp(x, y);
    }
    Some(a.len().cmp(&b.len()))
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
/// (the needle must be a string), a range's arithmetic membership, or
/// a dict key lookup. A sequence as the needle (`[1] in ys`,
/// `(1,2) in ys`) and relation/loop/`Undefined` operands are
/// unprovable; every other shape is a Python TypeError —
/// unresolvable (`None`).
fn contains(container: &Val, x: &Val) -> Option<bool> {
    use serde_json::Value as J;
    if matches!(
        x,
        Val::List(_) | Val::Tuple(_) | Val::Range { .. } | Val::Relation(_) | Val::Loop(_)
    ) {
        return None;
    }
    match container {
        Val::V(J::Array(items)) => {
            let Val::V(x) = x else { return None };
            Some(items.iter().any(|i| py_eq_j(i, x)))
        }
        Val::V(J::String(s)) => match x {
            Val::V(J::String(sub)) => Some(s.contains(sub)),
            _ => None,
        },
        Val::V(J::Object(m)) => match x {
            Val::V(J::String(k)) => Some(m.contains_key(k)),
            _ => None,
        },
        Val::V(_) | Val::Float(..) | Val::Undefined => None,
        Val::List(items) | Val::Tuple(items) => {
            for i in items {
                if py_eq(i, x)? {
                    return Some(true);
                }
            }
            Some(false)
        }
        Val::Dict(d) => match x {
            Val::V(J::String(k)) => Some(d.iter().any(|(key, _)| key == k)),
            // An unhashable or unjudgeable needle TypeErrors; every
            // other non-string needle provably misses (str keys).
            Val::Dict(_) | Val::Undefined => None,
            _ => Some(false),
        },
        Val::Range { start, step, len } => range_contains(*start, *step, *len, x),
        Val::Relation(_) | Val::Loop(_) => None,
    }
}

/// `x in range(start, …, step)` — membership by arithmetic: a
/// non-numeric needle can't equal an element (provable `False`), a
/// numeric one is a member iff `(x - start) / step` lands in `[0, len)`.
fn range_contains(start: i64, step: i64, len: usize, x: &Val) -> Option<bool> {
    let v = match x {
        Val::V(serde_json::Value::Number(n)) => n.as_f64()?,
        Val::V(serde_json::Value::Bool(b)) => *b as u8 as f64,
        Val::Float(f, _) => *f,
        Val::V(_) => return Some(false),
        _ => return None,
    };
    if v.fract() != 0.0 || v < i64::MIN as f64 || v > i64::MAX as f64 {
        return Some(false);
    }
    let (d, st) = (v as i64 as i128 - start as i128, step as i128);
    if d % st != 0 {
        return Some(false);
    }
    let k = d / st;
    Some(k >= 0 && k < len as i128)
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
            _ => None,
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
        Val::Float(f, _) => Some(*f),
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
/// A relation/`loop` operand is unprovable — `is` on a `ref()` value
/// (`r is string`) answers with context the render refuses to judge.
fn py_is(a: &Val, b: &Val) -> Option<bool> {
    use serde_json::Value as J;
    match (a, b) {
        (Val::V(J::Bool(x)), Val::V(J::Bool(y))) => Some(x == y),
        (Val::V(J::Null), Val::V(J::Null)) => Some(true),
        (Val::V(J::Number(x)), Val::V(J::Number(y))) => Some(small_int_is(x, y)),
        (Val::V(J::String(x)), Val::V(J::String(y))) if x == y => None,
        (Val::Relation(_), _) | (_, Val::Relation(_)) => None,
        _ => Some(false),
    }
}

/// The bare `x is <test>` set — `None` on an unsupported test or an
/// operand the render refuses to judge (a relation or `loop` object).
fn is_test(t: &str, v: &Val) -> Option<bool> {
    use serde_json::Value as J;
    if matches!(v, Val::Relation(_) | Val::Loop(_)) {
        return None;
    }
    Some(match t {
        "defined" => !matches!(v, Val::Undefined),
        "undefined" => matches!(v, Val::Undefined),
        "none" => matches!(v, Val::V(J::Null)),
        "string" => matches!(v, Val::V(J::String(_))),
        // `isinstance(True, Number)` — a bool IS a number.
        "number" => matches!(
            v,
            Val::V(J::Number(_)) | Val::V(J::Bool(_)) | Val::Float(..)
        ),
        "boolean" => matches!(v, Val::V(J::Bool(_))),
        "mapping" => matches!(v, Val::V(J::Object(_)) | Val::Dict(_)),
        // Jinja's `sequence`/`iterable` tests duck-type: lists, dicts
        // AND strings pass both, and so does `Undefined` (it defines
        // `__len__`/`__getitem__`/`__iter__` — verified on Jinja 3.1:
        // `missing is sequence` renders True). The render's own
        // containers and `range` pass the same way.
        "sequence" | "iterable" => matches!(
            v,
            Val::Undefined
                | Val::V(J::Array(_))
                | Val::V(J::String(_))
                | Val::V(J::Object(_))
                | Val::List(_)
                | Val::Tuple(_)
                | Val::Dict(_)
                | Val::Range { .. }
        ),
        "true" => matches!(v, Val::V(J::Bool(true))),
        "false" => matches!(v, Val::V(J::Bool(false))),
        // `x % 2` — a Python TypeError on a non-number (Undefined
        // included) is unprovable, not False.
        "even" => num_val(v)? % 2.0 == 0.0,
        "odd" => num_val(v)? % 2.0 != 0.0,
        _ => return None,
    })
}

/// `loop.<field>` — the only attribute read the render context proves
/// (`cycle`/`previtem`/`nextitem`/`changed`/`depth*` stay unprovable).
fn loop_attr(v: &Val, attr: &str) -> Option<Val> {
    let Val::Loop(l) = v else {
        return None;
    };
    let int = |n: usize| Val::V((n as i64).into());
    Some(match attr {
        "index" => int(l.index0 + 1),
        "index0" => int(l.index0),
        "revindex" => int(l.len - l.index0),
        "revindex0" => int(l.len - l.index0 - 1),
        "first" => Val::V((l.index0 == 0).into()),
        "last" => Val::V((l.index0 + 1 == l.len).into()),
        "length" => int(l.len),
        _ => return None,
    })
}

/// `v | join(sep)` — every element must render verbatim: a string as
/// itself, an integer decimal. A float/bool/container/relation/`loop`
/// element — or a non-iterable — is unprovable (`None`).
fn render_join(v: &Val, sep: &str) -> Option<Val> {
    fn piece(v: &Val) -> Option<String> {
        match v {
            Val::V(serde_json::Value::String(s)) => Some(s.clone()),
            Val::V(serde_json::Value::Number(n)) => n.as_i64().map(|i| i.to_string()),
            _ => None,
        }
    }
    let items: Vec<String> = match v {
        Val::List(items) | Val::Tuple(items) => items.iter().map(piece).collect::<Option<_>>()?,
        Val::V(serde_json::Value::Array(items)) => items
            .iter()
            .map(|i| piece(&Val::V(i.clone())))
            .collect::<Option<_>>()?,
        Val::Dict(d) => d.iter().map(|(k, _)| k.clone()).collect(),
        Val::V(serde_json::Value::Object(m)) => m.keys().cloned().collect(),
        Val::V(serde_json::Value::String(s)) => s.chars().map(|c| c.to_string()).collect(),
        Val::Range { start, step, len } => (0..*len)
            .map(|i| (start + i as i64 * step).to_string())
            .collect(),
        _ => return None,
    };
    Some(Val::V(items.join(sep).into()))
}

/// `v | length` — Python `len()`: a string's code points, a container's
/// element count, a range's computed length. `None` on `int`/`bool`/
/// `none`/`float`/relation/`loop`.
fn render_len(v: &Val) -> Option<Val> {
    let n = match v {
        Val::V(serde_json::Value::String(s)) => s.chars().count(),
        Val::V(serde_json::Value::Array(a)) => a.len(),
        Val::V(serde_json::Value::Object(o)) => o.len(),
        Val::List(x) | Val::Tuple(x) => x.len(),
        Val::Dict(d) => d.len(),
        Val::Range { len, .. } => *len,
        _ => return None,
    };
    Some(Val::V((n as i64).into()))
}

/// The relation a `ref(…)`/`source(…)` call binds — `None` when an arg
/// is non-string, a slot binds twice, or an unknown keyword shows up
/// (all shapes dbt itself errors on). `ref` reads its LAST positional
/// as the name (or `name=` when no positional binds it); `v`/`version`
/// pins read fine but never reach the text.
fn dep_name(name: &str, pos: &[Val], kw: &[(String, Val)]) -> Option<Relation> {
    let kw_arg = |n: &str| kw.iter().find(|(k, _)| k == n).map(|(_, v)| v);
    let name_of = |v: &Val| match v {
        Val::V(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    match name {
        "ref" => {
            if pos.len() > 2
                || kw
                    .iter()
                    .any(|(k, _)| !matches!(k.as_str(), "name" | "v" | "version"))
                || (!pos.is_empty() && kw_arg("name").is_some())
            {
                return None;
            }
            Some(Relation::Ref(name_of(
                pos.last().or_else(|| kw_arg("name"))?,
            )?))
        }
        "source" => {
            let dup = kw.iter().any(|(k, _)| {
                (k == "source_name" && !pos.is_empty()) || (k == "table_name" && pos.len() > 1)
            });
            if pos.len() > 2
                || kw
                    .iter()
                    .any(|(k, _)| !matches!(k.as_str(), "source_name" | "table_name"))
                || dup
            {
                return None;
            }
            let at = |i: usize, n: &str| pos.get(i).or_else(|| kw_arg(n));
            match (at(0, "source_name"), at(1, "table_name")) {
                (Some(a), Some(b)) => Some(Relation::Source(name_of(a)?, name_of(b)?)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `len(range(start, stop, step))` — `None` on a `0` step (Python's
/// ValueError). i128 math keeps the `i64` edges honest.
fn range_len(start: i64, stop: i64, step: i64) -> Option<usize> {
    if step == 0 {
        return None;
    }
    let (s, e, st) = (start as i128, stop as i128, step as i128);
    let len = if st > 0 {
        if e <= s {
            0
        } else {
            (e - s + st - 1) / st
        }
    } else {
        let st = -st;
        if e >= s {
            0
        } else {
            (s - e + st - 1) / st
        }
    };
    usize::try_from(len).ok()
}

/// Python's `repr(float)` — what `{{ }}` emits for a float: the
/// shortest round-trip decimal for exponents -4..=15, the `d[.ddd]e±NN`
/// form outside them (`1e+16`, `1.5e-05`). Rust's shortest-float
/// formatting agrees on the digits.
pub(crate) fn py_float_repr(v: f64) -> String {
    let sci = format!("{v:e}");
    let Some((mant, exp)) = sci.split_once('e') else {
        return format!("{v}");
    };
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        // Fixed-point form — Python insists a spelled-out float keeps
        // its `.0` tail.
        let s = format!("{v}");
        if s.contains(['.', 'e', 'E']) {
            s
        } else {
            format!("{s}.0")
        }
    } else {
        format!("{mant}e{}{:02}", if exp < 0 { "-" } else { "+" }, exp.abs())
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
