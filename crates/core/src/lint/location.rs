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
    "undocumented_exposures",
    "missing_primary_key_tests",
    "public_models_without_contract",
    "contract_columns_without_type",
    "required_meta_keys",
    "warehouse_type_drift",
    "columns_missing_in_warehouse",
    "unused_models",
    "undeclared_warehouse_columns",
    "models_without_unit_tests",
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
/// This searches the whole file: `names[0]` may match a `- name:` entry
/// anywhere (any section, any depth), each deeper name inside the
/// previous match's block. It ignores blank/comment lines, `|`/`>` block
/// scalar text, and the content between the entries it wants — enough
/// for the `- name:` chains a dbt properties file actually nests.
pub fn yaml_name_span(text: &str, names: &[String]) -> Option<(u32, u32, u32)> {
    if names.is_empty() {
        return None;
    }
    let index = YamlIndex::parse(text);
    let i = index.descend(names, 0, usize::MAX)?;
    let e = &index.entries[i];
    Some((e.line as u32 + 1, e.column, e.end_column))
}

/// The 1-based LINE the chain `names` resolves to — `None` on any
/// mismatch. A thin [`yaml_name_span`] wrapper kept for the callers and
/// tests that only want the line.
pub fn yaml_name_line(text: &str, names: &[String]) -> Option<u32> {
    yaml_name_span(text, names).map(|(line, _, _)| line)
}

/// One `- name:` entry from the [`YamlIndex`] pass: the value and its
/// position, the `key:` list it directly sits in (`list_key` —
/// `models`/`sources` for a top-level item, `tables`/`columns` below a
/// name), the entry it nests under (`parent`), and the entries nested
/// directly under it (`children`). `block_end` is the first content line
/// at or below the entry's indent — where its block ends (0-based,
/// exclusive).
struct YamlEntry {
    name: String,
    line: usize,
    column: u32,
    end_column: u32,
    block_end: usize,
    parent: Option<usize>,
    list_key: Option<String>,
    children: Vec<usize>,
}

/// An open block on the parse stack: a `key:` that opened a map or list
/// (`Key` — its `name` becomes the `list_key`/`section` of what nests
/// inside), a `- ` list item (`Item` — `entry` is set only for a
/// `- name:` item), or a `|`/`>` block scalar (`Scalar` — every deeper
/// line is literal text, never a `- name:`).
enum Frame {
    Key { indent: usize, name: String },
    Item { indent: usize, entry: Option<usize> },
    Scalar { indent: usize },
}

impl Frame {
    fn indent(&self) -> usize {
        match *self {
            Frame::Key { indent, .. } | Frame::Item { indent, .. } | Frame::Scalar { indent } => {
                indent
            }
        }
    }
}

/// What follows a `key:` — an empty value (the key opens a container),
/// a `|`/`>` block scalar opener, or an inline value (a leaf, no block).
enum KeyTail {
    Container,
    Scalar,
    Leaf,
}

/// `key: <tail>` where `key` is a bare word (the shape dbt properties
/// keys take: `models`, `tables`, `columns`, `config`, `+meta`, …) —
/// a quoted key answers with its unquoted name (`"models":` declares
/// `models` exactly like `models:`), while flow-prefixed keys simply
/// don't parse and stay plain lines.
fn key_tail(trimmed: &str) -> Option<(String, KeyTail)> {
    let (key, rest) = trimmed.split_once(':')?;
    let key = key.trim_end();
    let key = if key.len() >= 2
        && ((key.starts_with('"') && key.ends_with('"'))
            || (key.starts_with('\'') && key.ends_with('\'')))
    {
        &key[1..key.len() - 1]
    } else {
        key
    };
    if key.is_empty()
        || key.starts_with('-')
        || !key
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'))
    {
        return None;
    }
    let value = rest.trim_start();
    let tail = if value.is_empty() || value.starts_with('#') {
        KeyTail::Container
    } else if value.starts_with('|') || value.starts_with('>') {
        KeyTail::Scalar
    } else if value.starts_with('&') || value.starts_with('!') {
        // An anchor/tag names the node whose content the deeper lines
        // carry (`columns: &c` then `- name: …`) — keep descending.
        KeyTail::Container
    } else {
        KeyTail::Leaf
    };
    Some((key.to_string(), tail))
}

/// Every `- name:` entry in a properties `.yml`, built in ONE pass over
/// its lines: `entries` holds them in file order with position and
/// nesting, `tops` maps each top-level section key (`models`, `sources`,
/// `seeds`, `exposures`, …) to the `- name:` items directly inside it,
/// and `line_of` maps a 0-based line to its entry for the pinned-line
/// column lift. [`fill_finding_lines_with`] builds one per file so a
/// finding's lookup never rescans the text.
struct YamlIndex {
    entries: Vec<YamlEntry>,
    tops: HashMap<String, Vec<usize>>,
    line_of: HashMap<usize, usize>,
}

impl YamlIndex {
    /// Single indentation walk over `text`: `- name:` entries record
    /// their `key:` list and `- name:` parent, `key:`/`Scalar` frames
    /// only bound blocks — same depth rules the line scan used (a
    /// content line at or below an indent ends that block), plus one
    /// correction: `|`/`>` scalar bodies no longer read as `- name:`.
    fn parse(text: &str) -> Self {
        let mut index = YamlIndex {
            entries: Vec::new(),
            tops: HashMap::new(),
            line_of: HashMap::new(),
        };
        let lines: Vec<&str> = text.lines().collect();
        let mut stack: Vec<Frame> = Vec::new();
        fn close(stack: &mut Vec<Frame>, entries: &mut [YamlEntry], line: usize) {
            if let Some(Frame::Item { entry: Some(e), .. }) = stack.pop() {
                entries[e].block_end = line;
            }
        }
        for (i, &line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = line.len() - trimmed.len();
            // Inside a `|`/`>` scalar body the deeper lines are text.
            if matches!(stack.last(), Some(Frame::Scalar { indent: s }) if indent > *s) {
                continue;
            }
            let item = trimmed
                .strip_prefix('-')
                .filter(|r| r.is_empty() || r.starts_with(|c: char| c.is_whitespace()));
            if let Some(rest) = item {
                // A sibling item at this indent ends the open items and
                // any deeper frames — but a `key:` AT this indent is the
                // container these items belong to (yaml allows
                // `columns:` then `- name:` at the same indent).
                while let Some(top) = stack.last() {
                    let pop = match top {
                        Frame::Key { indent: k, .. } => *k > indent,
                        _ => top.indent() >= indent,
                    };
                    if !pop {
                        break;
                    }
                    close(&mut stack, &mut index.entries, i);
                }
                // `- name: <v>` is a declaration entry — unless the
                // "name" opens a `|`/`>` scalar, which names nothing.
                let entry = name_value_range(trimmed)
                    .filter(|r| !trimmed[r.clone()].starts_with(['|', '>']))
                    .map(|range| {
                        let cols = name_value_columns(line);
                        index.push_entry(&stack, &trimmed[range], i, cols)
                    });
                stack.push(Frame::Item { indent, entry });
                // `- <key>: |` / `- |` open a scalar deeper lines feed.
                let rest = rest.trim_start();
                let scalar = rest.starts_with(['|', '>'])
                    || matches!(key_tail(rest), Some((_, KeyTail::Scalar)));
                if scalar {
                    stack.push(Frame::Scalar {
                        indent: line.len() - rest.len(),
                    });
                }
                continue;
            }
            // Any other content line ends every block at or below it.
            while matches!(stack.last(), Some(f) if f.indent() >= indent) {
                close(&mut stack, &mut index.entries, i);
            }
            match key_tail(trimmed) {
                Some((name, KeyTail::Container)) => stack.push(Frame::Key { indent, name }),
                Some((_, KeyTail::Scalar)) => stack.push(Frame::Scalar { indent }),
                _ => {}
            }
        }
        // Items never closed end at EOF.
        for frame in &stack {
            if let Frame::Item { entry: Some(e), .. } = frame {
                index.entries[*e].block_end = lines.len();
            }
        }
        index.line_of = index
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.line, i))
            .collect();
        for (i, e) in index.entries.iter().enumerate() {
            // A top-level `- name:` item's own `key:` IS the section —
            // anything nested has a `parent` and answers a deeper step.
            if e.parent.is_none() {
                if let Some(key) = &e.list_key {
                    index.tops.entry(key.clone()).or_default().push(i);
                }
            }
        }
        index
    }

    /// Record one `- name:` entry at `line` (0-based): `parent` is the
    /// nearest enclosing `- name:` item on the stack, `list_key` the
    /// nearest enclosing `key:` (`sources`/`tables`/`columns`/…).
    /// Returns the entry's index.
    fn push_entry(
        &mut self,
        stack: &[Frame],
        name: &str,
        line: usize,
        cols: Option<(u32, u32)>,
    ) -> usize {
        let parent = stack.iter().rev().find_map(|f| match f {
            Frame::Item { entry: Some(e), .. } => Some(*e),
            _ => None,
        });
        let list_key = stack.iter().rev().find_map(|f| match f {
            Frame::Key { name, .. } => Some(name.clone()),
            _ => None,
        });
        let (column, end_column) = cols.unwrap_or((0, 0));
        let i = self.entries.len();
        self.entries.push(YamlEntry {
            name: name.to_string(),
            line,
            column,
            end_column,
            block_end: usize::MAX,
            parent,
            list_key,
            children: Vec::new(),
        });
        if let Some(p) = parent {
            self.entries[p].children.push(i);
        }
        i
    }

    /// The whole-file chain walk [`yaml_name_span`] runs: `- name:
    /// names[0]` candidates in file order (`entries[from..]` while still
    /// inside `bound`), each deeper name inside the match's block —
    /// falling through to the next same-name entry when a match's
    /// subtree can't complete the chain.
    fn descend(&self, names: &[String], from: usize, bound: usize) -> Option<usize> {
        let mut i = from;
        while i < self.entries.len() && self.entries[i].line < bound {
            if self.entries[i].name == names[0] {
                if names.len() == 1 {
                    return Some(i);
                }
                if let Some(found) = self.descend(&names[1..], i + 1, self.entries[i].block_end) {
                    return Some(found);
                }
            }
            i += 1;
        }
        None
    }

    /// A `- name:` chain scoped to ONE top-level section: `steps[0]`
    /// resolves only among that section's direct items (`tops`), each
    /// deeper step among the entry's `children` sitting in the step's
    /// `list_key`. A model `orders` therefore never matches a source
    /// table of the same name — and a chain that doesn't resolve leaves
    /// the position unset rather than guessing.
    fn lookup(&self, lookup: &YamlLookup) -> Option<(u32, u32, u32)> {
        let (_, first) = lookup.steps.first()?;
        for &i in self.tops.get(&lookup.section)? {
            if self.entries[i].name != *first {
                continue;
            }
            if let Some(last) = self.resolve(i, &lookup.steps[1..]) {
                let e = &self.entries[last];
                return Some((e.line as u32 + 1, e.column, e.end_column));
            }
        }
        None
    }

    /// Descend `steps` under `entries[at]` — each step among its direct
    /// `children` in `list_key`, backtracking to the next same-name
    /// sibling when a subtree can't complete the chain.
    fn resolve(&self, at: usize, steps: &[(String, String)]) -> Option<usize> {
        let Some((list_key, name)) = steps.first() else {
            return Some(at);
        };
        for &c in &self.entries[at].children {
            let e = &self.entries[c];
            if e.list_key.as_deref() == Some(list_key.as_str()) && e.name == *name {
                if let Some(found) = self.resolve(c, &steps[1..]) {
                    return Some(found);
                }
            }
        }
        None
    }
}

/// The `- name:` chain a finding resolves inside ONE top-level section:
/// `section` is the properties key the node kind declares under —
/// `models`/`seeds`/`snapshots`/`exposures`/`sources`/`metrics`/… — and
/// `steps` the `(list_key, name)` pairs to descend, each entry required
/// to sit directly inside that list (`sources` → `tables` → `columns`,
/// `models` → `columns`, `versions`-housed `columns` answer as the
/// model's children since `- v:` items are no `- name:`).
struct YamlLookup {
    section: String,
    steps: Vec<(String, String)>,
}

/// The [`YamlLookup`] `f` resolves — see [`YamlIndex::lookup`] for the
/// section and step shapes. `None` when the node vanished.
fn name_chain(dag: &Dag, f: &Finding) -> Option<YamlLookup> {
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
        let mut steps = vec![
            ("sources".to_string(), source.to_string()),
            ("tables".to_string(), table.to_string()),
        ];
        if let Some(column) = offending_column(dag, f) {
            steps.push(("columns".to_string(), column));
        }
        return Some(YamlLookup {
            section: "sources".to_string(),
            steps,
        });
    }
    let section = match n.resource_type.as_str() {
        "saved_query" => "saved_queries".to_string(),
        "analysis" => "analyses".to_string(),
        rt => format!("{rt}s"),
    };
    let mut steps = vec![(section.clone(), n.name.clone())];
    if let Some(column) = offending_column(dag, f) {
        steps.push(("columns".to_string(), column));
    }
    Some(YamlLookup { section, steps })
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
    fill_finding_lines_with(findings, dag, &|file| {
        fs::read_to_string(project_root.join(file)).ok()
    });
}

/// The same fill as [`fill_finding_lines`], with the file read inverted:
/// `read_text` resolves a `location.file` path into the file's text
/// (`None` leaves the position unset, exactly like an unreadable file).
/// Each file is resolved at most once — the same `location.file` shares
/// one text across every finding that lands in it. Callers that hold the
/// sources in memory rather than on disk run this entry point.
pub fn fill_finding_lines_with(
    findings: &mut [Finding],
    dag: &Dag,
    read_text: &dyn Fn(&str) -> Option<String>,
) {
    // The `- name:` index per file — resolved once (the `read_text` call
    // AND the line walk), then shared by every finding that lands in it.
    let mut indexes: HashMap<String, Option<YamlIndex>> = HashMap::new();
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
        let lookup = if loc.line.is_none() {
            name_chain(dag, f)
        } else {
            None
        };
        let index = indexes
            .entry(file.clone())
            .or_insert_with(|| read_text(&file).map(|text| YamlIndex::parse(&text)));
        let Some(index) = index.as_ref() else {
            continue;
        };
        let loc = f.location.as_mut().expect("checked above");
        let span = match loc.line {
            Some(line) => line
                .checked_sub(1)
                .and_then(|l| index.line_of.get(&(l as usize)))
                .map(|&e| (line, index.entries[e].column, index.entries[e].end_column)),
            None => lookup.as_ref().and_then(|lk| index.lookup(lk)),
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
