//! Finding `location` refinement — the two pieces `run_rules` cannot do
//! itself because they need the filesystem:
//!
//! - [`YAML_ATTRIBUTE_RULE_IDS`] — the rule ids whose findings concern
//!   attributes declared in a properties `.yml`. `run_rules` consults it to
//!   pick `location.file` (`patch_path` before `original_file_path`); it
//!   lives here because the line-filler uses the same notion of "this
//!   finding belongs to a yml".
//! - [`yaml_name_line`] / [`fill_finding_lines`] — best-effort 1-based line
//!   numbers inside a properties yml, filled by a separate pass the `check`
//!   and `lint` CLIs call. Kept out of `run_rules` so the shared pass
//!   stays pure (the TUI reads no files).

use std::collections::HashMap;
use std::path::Path;

use super::{Finding, LintRule};
use crate::Dag;

/// The rules whose findings concern YAML-declared attributes (descriptions,
/// columns, `data_type`, `access`, `contract`, `meta`, tests). For these,
/// `location.file` prefers the node's `patch_path` — the properties file the
/// attribute was (or should have been) declared in — over
/// `original_file_path` (the `.sql`). Every other rule (graph shape, SQL,
/// naming, sources) keeps `original_file_path`. Ids not yet registered name
/// catalog rules that land in the same bucket when they arrive.
pub const YAML_ATTRIBUTE_RULE_IDS: &[&str] = &[
    "undocumented_models",
    "undocumented_columns",
    "undocumented_public_models",
    "missing_primary_key_tests",
    "public_models_without_contract",
    "contract_columns_without_type",
    "required_meta_keys",
    "warehouse_type_drift",
    "columns_missing_in_warehouse",
];

/// Best-effort 1-based line of the `- name: <name>` declaration chain
/// `names` inside `text`, a properties yml. Each entry must appear inside
/// the previous entry's block: the search only accepts lines indented
/// deeper than the last match and gives up at the first content line back
/// out to the parent's indent (blank lines and comments never end a block).
/// A first-level name that matches but can't complete the chain doesn't
/// settle it — the next same-name match is tried (a `raw` *model* sitting
/// in the same file as a `raw` *source*).
///
/// `- name:` keys whose value is quoted (`'orders'`, `"orders"`) match the
/// unquoted form. Returns `None` when the chain can't be resolved.
///
/// `names` is `[source, table]` for a source finding, `[model]` for a
/// model finding, `[model, column]` for a column finding.
pub fn yaml_name_line(text: &str, names: &[String]) -> Option<u32> {
    if names.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    descend(&lines, names, 0, None)
}

/// Search `lines[from..]` for `- name: names[0]` entries indented deeper
/// than `parent_indent`, then recurse for the rest of the chain inside the
/// match's block. Returns the LAST name's line. `parent_indent` bounds the
/// search: a content line at or below it means the enclosing block ended
/// and candidates stop. `None` (the file top level) scans to EOF.
fn descend(
    lines: &[&str],
    names: &[String],
    from: usize,
    parent_indent: Option<usize>,
) -> Option<u32> {
    let mut i = from;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        i += 1;
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if parent_indent.is_some_and(|p| indent <= p) {
            return None;
        }
        let Some(value) = list_entry_name(trimmed) else {
            continue;
        };
        if value != names[0] {
            continue;
        }
        if names.len() == 1 {
            return Some(i as u32);
        }
        if let Some(found) = descend(lines, &names[1..], i, Some(indent)) {
            return Some(found);
        }
        // The chain didn't resolve inside this match's block — fall through
        // to the next same-name entry at this level.
    }
    None
}

/// The bare value of a `- name: <value>` line — `Some(name)` when `trimmed`
/// is exactly that shape, `None` otherwise. The value runs to the first
/// whitespace (dbt resource names never contain spaces) and is unquoted.
fn list_entry_name(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix('-')?.trim_start();
    let rest = rest.strip_prefix("name")?.trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let token = rest.split_whitespace().next()?;
    let unquoted = match (token.as_bytes().first(), token.as_bytes().last()) {
        (Some(b'"'), Some(b'"')) | (Some(b'\''), Some(b'\'')) if token.len() >= 2 => {
            &token[1..token.len() - 1]
        }
        _ => token,
    };
    Some(unquoted.to_string())
}

/// Fill `location.line` for findings whose file is a properties yml under
/// `project_root` — the pass `check`/`lint` call after `run_rules` (which
/// stays file-pure). Each yml is read at most once; a missing or
/// unreadable file just leaves the line `None`, never fails.
///
/// The name chain is `[source, table]` for source findings, `[model,
/// column]` for the rules whose message lists offending columns (the FIRST
/// one — re-derived from the rule's own predicate so it can't drift from
/// the message), `[model]` for everything else.
pub fn fill_finding_lines(findings: &mut [Finding], dag: &Dag, project_root: &Path) {
    let mut texts: HashMap<String, Option<String>> = HashMap::new();
    for f in findings.iter_mut() {
        let Some(file) = f.location.as_ref().map(|l| l.file.clone()) else {
            continue;
        };
        if !is_yml(&file) {
            continue;
        }
        let Some(names) = name_chain(dag, f) else {
            continue;
        };
        let text = texts
            .entry(file.clone())
            .or_insert_with(|| std::fs::read_to_string(project_root.join(&file)).ok());
        if let Some(loc) = &mut f.location {
            loc.line = text.as_deref().and_then(|t| yaml_name_line(t, &names));
        }
    }
}

/// `.yml` / `.yaml` — the file a line lookup can walk.
fn is_yml(file: &str) -> bool {
    file.ends_with(".yml") || file.ends_with(".yaml")
}

/// The `- name:` chain [`yaml_name_line`] descends for `f` — see the
/// function's doc for the per-rule shapes. `None` when the node vanished.
fn name_chain(dag: &Dag, f: &Finding) -> Option<Vec<String>> {
    let n = dag.get(&f.node_uid)?;
    if n.resource_type == "source" {
        // `source.<pkg>.<source>.<table>` — the two declaring names.
        let mut parts = f.node_uid.splitn(4, '.');
        parts.next()?;
        parts.next()?;
        let source = parts.next()?;
        let table = parts.next()?;
        return Some(vec![source.to_string(), table.to_string()]);
    }
    let mut chain = vec![n.name.clone()];
    if let Some(column) = offending_column(dag, f) {
        chain.push(column);
    }
    Some(chain)
}

/// The FIRST column a finding's message lists, for the rules that flag
/// columns — re-derived from the detail the rule predicates on rather than
/// parsed out of the message. `None` for every other rule and for
/// `undocumented_public_models` when the MODEL's own description is the
/// missing attribute (the finding lands on the model's `- name:` line).
fn offending_column(dag: &Dag, f: &Finding) -> Option<String> {
    let d = dag.detail(&f.node_uid)?;
    match f.rule {
        LintRule::UndocumentedColumns => d
            .columns
            .iter()
            .find(|c| c.description.is_none())
            .map(|c| c.name.clone()),
        LintRule::UndocumentedPublicModels => {
            if d.description.is_none() {
                None
            } else {
                d.columns
                    .iter()
                    .find(|c| c.description.is_none())
                    .map(|c| c.name.clone())
            }
        }
        LintRule::ContractColumnsWithoutType => d
            .columns
            .iter()
            .find(|c| c.data_type.is_none())
            .map(|c| c.name.clone()),
        _ => None,
    }
}
