//! Finding `location` refinement that needs the project tree: the dag
//! knows a node's file (`original_file_path`), but the line a finding
//! belongs on lives in the source text — the `- name:` entries inside a
//! properties `.yml`. The rules that report "declared here" attributes
//! (descriptions, tests, freshness, contract columns) point at
//! `patch_path`, so after [`run_rules`] fills `location.file` the CLI
//! asks this module to fill `location.line`/`column`/`end_line`/
//! `end_column` best-effort (missing files simply stay line-less —
//! `fill_finding_lines` returns nothing on error).
//!
//! The OTHER direction — byte offsets inside a text (the raw SQL a rule
//! reads via [`Dag::raw_code`]) to line/column positions — is
//! [`byte_span_position`], shared by every rule that attaches its own
//! position so no two callers ever count a column differently.
//!
//! [`run_rules`]: crate::lint::run_rules

use std::collections::HashMap;
use std::fs;
use std::ops::Range;
use std::path::Path;

use crate::Dag;

use super::{Finding, LintRule};

/// The rules whose findings concern YAML-declared attributes (descriptions,
/// columns, `data_type`, `access`, `contract`, `meta`, tests). For these,
/// [`run_rules`] prefers the node's `patch_path` — the properties file the
/// attribute was (or should have been) declared in — over
/// `original_file_path` (the `.sql`) when it fills `location.file`, and
/// [`fill_finding_lines`] then resolves the `- name:` entry inside it.
/// Source rules need no entry: a source's `original_file_path` IS the
/// declaring yml.
///
/// [`run_rules`]: crate::lint::run_rules
pub const YAML_ATTRIBUTE_RULE_IDS: &[&str] = &[
    "undocumented_models",
    "undocumented_columns",
    "missing_primary_key_tests",
    "public_models_without_contract",
    "contract_columns_without_type",
    "required_meta_keys",
    "warehouse_type_drift",
    "columns_missing_in_warehouse",
];

/// The 1-based `(line, column)` span of `text[start..end]` — `start`'s
/// position and `end`'s as an EXCLUSIVE end (the position immediately
/// after the last character: a range ending at a line's end reports the
/// next column on the same line, not the next line's column 1). Lines and
/// columns count `char`s — a tab is one column — and `\r\n` is one
/// newline. `None` when `end < start`, `end` runs past the text, or
/// either offset cuts a `char` in half: a position the lookup can't pin
/// exactly is no position.
pub fn byte_span_position(text: &str, start: usize, end: usize) -> Option<(u32, u32, u32, u32)> {
    if end < start
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    let (mut line, mut column) = (1u32, 1u32);
    let (mut at, mut to) = (None, None);
    for (i, c) in text.char_indices() {
        if i == start {
            at = Some((line, column));
        }
        if i == end {
            to = Some((line, column));
        }
        if at.is_some() && to.is_some() {
            break;
        }
        match c {
            '\n' => {
                line += 1;
                column = 1;
            }
            _ => column += 1,
        }
    }
    // An offset AT the very end resolves after the last char's advance.
    if start == text.len() {
        at = Some((line, column));
    }
    if end == text.len() {
        to = Some((line, column));
    }
    Some((at?.0, at?.1, to?.0, to?.1))
}

fn is_yml(file: &str) -> bool {
    file.ends_with(".yml") || file.ends_with(".yaml")
}

/// The byte range of the `- name:` VALUE inside `trimmed` (a left-trimmed
/// line) — the range of a quoted name is INSIDE its quotes. `None` when
/// the line isn't a `- name:` entry or names nothing. The name token is
/// whitespace-delimited — dbt identifiers carry no spaces.
fn name_value_range(trimmed: &str) -> Option<Range<usize>> {
    let rest = trimmed.strip_prefix('-')?.trim_start();
    let rest = rest.strip_prefix("name")?.trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let token_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
    if token_len == 0 {
        return None;
    }
    let token = &rest[..token_len];
    let quoted = token.len() >= 2
        && (token.starts_with('"') && token.ends_with('"')
            || token.starts_with('\'') && token.ends_with('\''));
    let (inner_start, inner_len) = if quoted {
        (1, token.len() - 2)
    } else {
        (0, token_len)
    };
    if inner_len == 0 {
        return None;
    }
    // `rest` is a suffix of `trimmed`; its offset inside it is the delta.
    let base = trimmed.len() - rest.len();
    Some(base + inner_start..base + inner_start + inner_len)
}

/// The 1-based `(column, end_column)` of the `- name:` value on `line` —
/// `end_column` exclusive, columns counted in `char`s. `None` when `line`
/// isn't a `- name:` entry.
fn name_value_columns(line: &str) -> Option<(u32, u32)> {
    let trimmed = line.trim_start();
    let range = name_value_range(trimmed)?;
    // The value's byte offset in the full line: the whitespace
    // `trim_start` dropped, plus the range's own start.
    let start = line.len() - trimmed.len() + range.start;
    let width = range.end - range.start;
    let column = line[..start].chars().count() as u32 + 1;
    Some((
        column,
        column + line[start..start + width].chars().count() as u32,
    ))
}

/// The 1-based `(line, column, end_column)` of the name VALUE the chain
/// `names` resolves to — `end_column` exclusive; quoted names point
/// INSIDE the quotes. `- name:` values sit on a single line, so the
/// span's `end_line` is `line` itself (the caller repeats it into
/// [`Location::end_line`](super::Location)).
///
/// `names` is the chain to descend: for a source finding
/// `[source, table]` — the `- name:` under `sources:` then the table's
/// under `tables:`; for a model finding `[model]`; for a column finding
/// `[model, column]` — the model's entry, then the column's under
/// `columns:`. `None` on any mismatch — no guesses.
///
/// This is an indentation walk over yml LINES, not a parser: it ignores
/// blank/comment lines and nested content between the entries it wants —
/// enough for the `- name:` chains a dbt properties file actually nests.
pub fn yaml_name_span(text: &str, names: &[String]) -> Option<(u32, u32, u32)> {
    if names.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let i = descend(&lines, names, 0, None)?;
    let (column, end_column) = name_value_columns(lines[i])?;
    Some((i as u32 + 1, column, end_column))
}

/// The 1-based LINE the chain `names` resolves to — `None` on any
/// mismatch. A thin [`yaml_name_span`] wrapper kept for the callers and
/// tests that only want the line.
pub fn yaml_name_line(text: &str, names: &[String]) -> Option<u32> {
    yaml_name_span(text, names).map(|(line, _, _)| line)
}

/// Search `lines[from..]` for `- name: names[0]` entries indented deeper
/// than `parent_indent`, then recurse for the rest of the chain inside the
/// match's block. Returns the LAST name's line index (0-based).
/// `parent_indent` bounds the search: a content line at or below it means
/// the enclosing block ended and candidates stop. `None` (the file top
/// level) scans to EOF.
fn descend(
    lines: &[&str],
    names: &[String],
    from: usize,
    parent_indent: Option<usize>,
) -> Option<usize> {
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
        if name_value_range(trimmed).is_none_or(|r| trimmed[r] != names[0]) {
            continue;
        }
        if names.len() == 1 {
            return Some(i - 1);
        }
        if let Some(found) = descend(lines, &names[1..], i, Some(indent)) {
            return Some(found);
        }
        // The chain didn't resolve inside this match's block — fall through
        // to the next same-name entry at this level.
    }
    None
}

/// The `- name:` chain [`yaml_name_span`] descends for `f` — see the
/// function's doc for the per-rule shapes. `None` when the node vanished.
fn name_chain(dag: &Dag, f: &Finding) -> Option<Vec<String>> {
    let n = dag.get(&f.node_uid)?;
    if n.resource_type == "source" {
        // `source.<pkg>.<source>.<table>` — the two declaring names, plus
        // the column for a column-scoped finding (`undocumented_columns`
        // under `resource_types: [source]` points at the bare `- name:`
        // inside the table's `columns:` list, not the table's own).
        let mut parts = f.node_uid.splitn(4, '.');
        parts.next()?;
        parts.next()?;
        let source = parts.next()?;
        let table = parts.next()?;
        let mut chain = vec![source.to_string(), table.to_string()];
        if let Some(column) = offending_column(dag, f) {
            chain.push(column);
        }
        return Some(chain);
    }
    let mut chain = vec![n.name.clone()];
    if let Some(column) = offending_column(dag, f) {
        chain.push(column);
    }
    Some(chain)
}

/// The FIRST column a finding's message lists, for the rules that flag
/// columns — re-derived from the detail the rule predicates on rather than
/// parsed out of the message. `None` for every other rule (their findings
/// land on the model's `- name:` line).
fn offending_column(dag: &Dag, f: &Finding) -> Option<String> {
    let d = dag.detail(&f.node_uid)?;
    match f.rule {
        LintRule::UndocumentedColumns => d
            .columns
            .iter()
            .find(|c| c.description.is_none())
            .map(|c| c.name.clone()),
        LintRule::ContractColumnsWithoutType => d
            .columns
            .iter()
            .find(|c| c.data_type.is_none())
            .map(|c| c.name.clone()),
        _ => None,
    }
}

/// Fill `location.line`/`column`/`end_line`/`end_column` for findings
/// whose file is a properties `.yml`: the `- name:` VALUE's range inside
/// it (the name the fix edits), annotated `declared here` — best-effort:
/// a missing file, a non-yml file, or an unmatched chain leaves the
/// position alone.
///
/// What a rule pinned is respected; only the missing parts fill. A
/// pinned `line` keeps its claim and lifts the columns from THAT line's
/// own `- name:` value — the chain's span belongs to a different line and
/// can't be transplanted onto the pin (and a pinned line that isn't a
/// `- name:` entry stays column-less). An unpinned `line` resolves the
/// whole span via the chain; a pinned `column` then keeps its claim and
/// `end_column` fills only when the start column is the span's own —
/// otherwise it would cap a span it doesn't belong to. A fully-pinned
/// start (line AND column) is left untouched.
///
/// The dag carries no yml line info, so this does the file IO the pure
/// [`run_rules`] must not: `lint`/`check` call it once with the project
/// root after evaluation. Each file is read at most once — the text is
/// shared across the findings that land in it.
///
/// [`run_rules`]: crate::lint::run_rules
pub fn fill_finding_lines(findings: &mut [Finding], dag: &Dag, project_root: &Path) {
    let mut texts: HashMap<String, Option<String>> = HashMap::new();
    for f in findings.iter_mut() {
        let Some(loc) = f.location.as_ref() else {
            continue;
        };
        let file = loc.file.clone();
        if !is_yml(&file) || (loc.line.is_some() && loc.column.is_some()) {
            continue;
        }
        // The chain resolves only an unpinned line — a pinned one takes
        // its columns from its own `- name:` value instead.
        let names = if loc.line.is_none() {
            name_chain(dag, f)
        } else {
            None
        };
        let text = texts
            .entry(file.clone())
            .or_insert_with(|| fs::read_to_string(project_root.join(&file)).ok());
        let Some(text) = text.as_deref() else {
            continue;
        };
        let loc = f.location.as_mut().expect("checked above");
        let span = match loc.line {
            Some(line) => text
                .lines()
                .nth(line as usize - 1)
                .and_then(name_value_columns)
                .map(|(column, end_column)| (line, column, end_column)),
            None => names.and_then(|names| yaml_name_span(text, &names)),
        };
        if let Some((line, column, end_column)) = span {
            loc.line.get_or_insert(line);
            loc.column.get_or_insert(column);
            loc.end_line.get_or_insert(line);
            if loc.end_column.is_none() && loc.column == Some(column) && loc.end_line == Some(line)
            {
                loc.end_column = Some(end_column);
            }
            f.label.get_or_insert_with(|| "declared here".to_string());
        }
    }
}
