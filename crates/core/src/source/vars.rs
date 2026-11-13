//! The one `var()`-aware evaluator for `enabled` positions — shared by the
//! `+enabled` config-tree keys and in-file `config(enabled=...)`.
//!
//! Real dbt resolves `var()` in TWO scopes, one per render pass (verified
//! on dbt 1.12.7):
//!
//! - **project scope** — `dbt_project.yml` itself is rendered before its
//!   `vars:` block becomes the runtime var table, so a `+enabled:
//!   "{{ var('x', d) }}"` sees only `vars.yml`'s top-level `vars:` (plus
//!   `--vars`, which dbtl does not pass). The project file's own `vars:` —
//!   and any `vars: <pkg>:` scoping — are NOT consulted at this pass.
//! - **node scope** — a resource's `config()` calls render inside the
//!   parsed node's context: `vars.yml`'s `vars:` when declared (dbt
//!   forbids declaring both; the file wins), else `dbt_project.yml`'s
//!   `vars:` — entries nested under the project name shadowing the
//!   top-level keys.
//!
//! The shapes dbt accepts in an `enabled` position: a native bool, or a
//! `var()` / `var(...) | as_bool` call yielding one. `as_bool` literal_evals
//! the rendered value and demands a Python bool — `True`/`False` and real
//! bools pass, `'true'`/`'yes'`/`0` fail. Everything else (a plain
//! `'false'` string, a number, a var missing with no default, another
//! expression) is a dbt parse/validation error — degraded here to
//! enabled + a once-per-site warning.

use super::*;

/// The var tables real dbt resolves `var()` against — one per render pass
/// (see the module doc): [`Vars::project_scope`] while `dbt_project.yml`
/// renders, [`Vars::node_scope`] while a node's `config()` renders.
pub(super) struct Vars {
    /// `vars.yml`'s `vars:` map — the ONLY table the project-file render
    /// consults (flat lookups; its `<pkg>:` sub-maps are just values
    /// there, not a scoping).
    file: serde_json::Map<String, serde_json::Value>,
    /// The node-render table: `file` when non-empty (dbt rejects declaring
    /// both; the file wins), else `dbt_project.yml`'s `vars:`.
    node: serde_json::Map<String, serde_json::Value>,
    /// The root project name — the `vars: <name>:` scoping key for the
    /// node scope.
    proj: String,
}

impl Vars {
    /// Load `vars.yml` + `dbt_project.yml`'s `vars:` into the two scopes.
    /// Both files declaring non-empty `vars:` is a combination dbt rejects
    /// — surfaced as one warning, `vars.yml` winning (it is also what the
    /// project-file render would have seen).
    pub(super) fn load(dir: &Path, project: &DbtProject, warnings: &mut Vec<String>) -> Vars {
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
        Vars::from_maps(file, project_vars, &project.name)
    }

    /// The maps → scopes split both consumers need (separated from the
    /// filesystem read so tests build scopes straight from values).
    pub(super) fn from_maps(
        file: serde_json::Map<String, serde_json::Value>,
        project: serde_json::Map<String, serde_json::Value>,
        proj: &str,
    ) -> Vars {
        Vars {
            node: if file.is_empty() {
                project
            } else {
                file.clone()
            },
            file,
            proj: proj.to_string(),
        }
    }

    /// The boolean a `+enabled` tree value resolves to, when dbt could
    /// produce one: a native bool, or a `{{ var(...) }}`/`{{- ... -}}`
    /// string the project scope resolves. Everything else (a number, a
    /// plain `'false'` string, another expression, an unresolvable var)
    /// fails dbt's `enabled` validation.
    pub(super) fn tree_enabled(&self, v: &serde_json::Value) -> Option<bool> {
        match v {
            serde_json::Value::Bool(b) => Some(*b),
            serde_json::Value::String(s) => {
                jinja_expr(s).and_then(|e| eval_var_enabled(e, |n| self.project_scope(n)))
            }
            _ => None,
        }
    }

    /// The boolean an in-file `config(enabled=<expr>)` resolves to in the
    /// node scope — a bool literal, or a `var()` / `var(...) | as_bool`
    /// call yielding one. Anything else fails dbt's `enabled` validation.
    pub(super) fn node_enabled(&self, expr: &str) -> Option<bool> {
        match lit_value(expr) {
            Some(serde_json::Value::Bool(b)) => Some(b),
            _ => eval_var_enabled(expr, |n| self.node_scope(n)),
        }
    }

    /// `var('name')` as `dbt_project.yml`'s own render sees it — flat
    /// `vars.yml` lookups only.
    fn project_scope(&self, name: &str) -> Option<serde_json::Value> {
        self.file.get(name).cloned()
    }

    /// `var('name')` as a node's `config()` sees it — `vars: <proj>:`
    /// entries shadowing the top-level keys of the effective table.
    fn node_scope(&self, name: &str) -> Option<serde_json::Value> {
        self.node
            .get(&self.proj)
            .and_then(|v| v.as_object())
            .and_then(|m| m.get(name))
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

/// `var('name'[, default])` — optionally `| as_bool` — resolved through
/// `lookup`: the ONLY dynamic `enabled` shape dbtl evaluates. Returns the
/// boolean dbt would validate, or `None` when the expression isn't that
/// shape or the resolved value isn't a bool dbt accepts (a dbt error the
/// caller degrades to enabled + warning).
fn eval_var_enabled(
    expr: &str,
    lookup: impl Fn(&str) -> Option<serde_json::Value>,
) -> Option<bool> {
    let var = parse_var_expr(expr)?;
    let value = lookup(&var.name).or(var.default)?;
    match (&value, var.as_bool) {
        (serde_json::Value::Bool(b), _) => Some(*b),
        // `| as_bool` — dbt's literal_eval-then-bool: only 'True'/'False'
        // (exact casing) become bools; 'true'/'yes'/0 all fail it.
        (serde_json::Value::String(s), true) => match s.as_str() {
            "True" => Some(true),
            "False" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A parsed `var('name'[, default])` call, `| as_bool` flag included.
struct VarExpr {
    name: String,
    default: Option<serde_json::Value>,
    as_bool: bool,
}

/// `var('name'[, default])` optionally followed by `| as_bool` — and
/// nothing else. Args must be literal: a non-literal default
/// (`var('x', var('y'))`) is legal Jinja dbt would evaluate but outside
/// this evaluator's contract.
fn parse_var_expr(expr: &str) -> Option<VarExpr> {
    let s = expr.trim();
    let after = s.strip_prefix("var")?.trim_start();
    if !after.starts_with('(') {
        return None; // `variant(...)` is not `var(`
    }
    // The balanced arg scan — quoted parens/nesting can't truncate it.
    let (args, end) = scan_call_args(s, s.len() - after.len() + 1)?;
    let as_bool = match s[end..].trim() {
        "" => false,
        t if t.strip_prefix('|').is_some_and(|f| f.trim() == "as_bool") => true,
        _ => return None, // `| other`, trailing garbage
    };
    let mut segs = top_level_segments(args)
        .into_iter()
        .filter(|seg| !seg.trim().is_empty()); // a trailing comma yields an empty seg
    let name = match segs.next().and_then(lit_value) {
        Some(serde_json::Value::String(s)) => s,
        _ => return None, // `var()`, `var(x)`, `var('x','y','z')`-ish
    };
    let default = match segs.next() {
        None => None,
        Some(seg) => Some(lit_value(seg)?),
    };
    if segs.next().is_some() {
        return None;
    }
    Some(VarExpr {
        name,
        default,
        as_bool,
    })
}

/// The expression inside a string that is exactly one `{{ ... }}` block —
/// whitespace-control `{{-`/`-}}` allowed — else `None` (mixed text
/// renders to a non-bool string anyway, which fails dbt's validation).
fn jinja_expr(s: &str) -> Option<&str> {
    let inner = s.trim().strip_prefix("{{")?.strip_suffix("}}")?;
    let inner = inner.strip_prefix('-').unwrap_or(inner);
    let inner = inner.strip_suffix('-').unwrap_or(inner);
    let inner = inner.trim();
    (!inner.is_empty()).then_some(inner)
}
