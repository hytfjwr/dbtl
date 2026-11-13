//! `cargo xtask gen-docs` — the generated reference documents under
//! `docs/reference/` and the JSON Schemas under `schema/`.
//!
//! Every output is derived from a single source of truth — the TUI
//! `BINDINGS` keymap (through `help_lines`), the lint `REGISTRY`, the
//! check-gate registry, the live clap model (rendered by the binary's
//! hidden `cli-reference` subcommand and passed in as `cli_markdown`), and
//! the serde wire structs the config parsers actually deserialize — so the
//! docs can never drift from the behaviour they describe.

use std::path::Path;

use dbtl_core::{
    project_config_schema, severity_setting_schema, CheckGate, GateParamKind, LineageDepth,
    LintConfig, LintRule, ParamKind, REGISTRY,
};
use dbtl_tui::{help_lines, ModeKind};
use serde_json::{json, Map, Value};

/// The generated-file banner — the same string `dbtl cli-reference` puts on
/// the CLI doc. The JSON Schemas carry `GENERATED_COMMENT` as `$comment`
/// (JSON has no comments).
pub const GENERATED_MARKER: &str =
    "<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->";
/// The comment JSON Schema files carry as `$comment`.
const GENERATED_COMMENT: &str =
    "GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs";

/// One generated output: a repo-relative path and the exact bytes to write.
pub struct GeneratedFile {
    pub path: &'static str,
    pub contents: String,
}

/// The full output set, in write order. `cli_markdown` is `dbtl
/// cli-reference`'s stdout — the binary renders that doc from its own clap
/// model (this crate cannot see the bin's types).
pub fn generate(cli_markdown: &str) -> Vec<GeneratedFile> {
    let dbtl_schema = dbtl_yml_schema();
    vec![
        GeneratedFile {
            path: "docs/reference/keybindings.md",
            contents: keybindings_markdown(),
        },
        GeneratedFile {
            path: "docs/reference/lint-rules.md",
            contents: lint_rules_markdown(),
        },
        GeneratedFile {
            path: "docs/reference/cli.md",
            contents: cli_markdown.to_string(),
        },
        GeneratedFile {
            path: "docs/reference/config.md",
            contents: config_markdown(&dbtl_schema),
        },
        GeneratedFile {
            path: "docs/reference/themes.md",
            contents: themes_markdown(),
        },
        GeneratedFile {
            path: "schema/dbtl.schema.json",
            contents: json_document(&dbtl_schema),
        },
        GeneratedFile {
            path: "schema/config.schema.json",
            contents: json_document(&config_yml_schema()),
        },
    ]
}

/// Write the set under `root`, creating parent dirs. Prints one line per
/// file to stdout.
pub fn write(root: &Path, files: &[GeneratedFile]) -> std::io::Result<()> {
    for f in files {
        let path = root.join(f.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &f.contents)?;
        println!("wrote {}", f.path);
    }
    Ok(())
}

/// Compare the set against `root` on disk — writes nothing. `Ok` when
/// every file matches byte-for-byte and no stale marked file remains; `Err`
/// is a printable report (per-file diffs plus the regeneration command).
pub fn check(root: &Path, files: &[GeneratedFile]) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();
    for f in files {
        match std::fs::read_to_string(root.join(f.path)) {
            Ok(disk) if disk == f.contents => {}
            Ok(disk) => problems.push(drift_report(f.path, &disk, &f.contents)),
            Err(_) => problems.push(format!("{}: missing (not yet generated)", f.path)),
        }
    }
    // A file carrying the generated marker that the set no longer produces
    // is stale — flag it so a dropped output can't linger silently.
    for dir in ["docs/reference", "schema"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let rel = format!("{dir}/{}", entry.file_name().to_string_lossy());
            if files.iter().any(|f| f.path == rel) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            if text.contains(GENERATED_MARKER) || text.contains(GENERATED_COMMENT) {
                problems.push(format!("{rel}: stale generated file — no longer produced"));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{}\n\nGenerated reference output is stale — run `cargo xtask gen-docs` and commit the result.",
            problems.join("\n\n")
        ))
    }
}

/// The differing region of one file: the longest common line prefix/suffix
/// is stripped, the middle is printed with `-`/`+` markers (bounded).
fn drift_report(path: &str, committed: &str, generated: &str) -> String {
    let old: Vec<&str> = committed.lines().collect();
    let new: Vec<&str> = generated.lines().collect();
    let pre = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let mut suf = 0;
    while suf < old.len() - pre
        && suf < new.len() - pre
        && old[old.len() - 1 - suf] == new[new.len() - 1 - suf]
    {
        suf += 1;
    }
    const CAP: usize = 40;
    let mut out = format!("{path}: differs from generated output\n");
    let old_mid = &old[pre..old.len() - suf];
    let new_mid = &new[pre..new.len() - suf];
    for (i, line) in old_mid.iter().enumerate() {
        if i == CAP {
            out.push_str(&format!("   - … ({} more lines)\n", old_mid.len() - CAP));
            break;
        }
        out.push_str(&format!("   - {line}\n"));
    }
    for (i, line) in new_mid.iter().enumerate() {
        if i == CAP {
            out.push_str(&format!("   + … ({} more lines)\n", new_mid.len() - CAP));
            break;
        }
        out.push_str(&format!("   + {line}\n"));
    }
    out.push_str(&format!("   (differing region starts at line {})", pre + 1));
    out
}

/// One Markdown doc's opening: marker, title, blank line.
fn doc_header(title: &str) -> String {
    format!("{GENERATED_MARKER}\n\n# {title}\n\n")
}

/// One Markdown table cell: whitespace flattened, `|` escaped.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// `a, b` → `` `a`, `b` ``.
fn ticks(text: &str) -> String {
    text.split(", ")
        .map(|t| format!("`{t}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// keybindings.md — the `?` overlay's rows, grouped by mode.
// ---------------------------------------------------------------------------

fn keybindings_markdown() -> String {
    let mut out = doc_header("Keybindings");
    out.push_str(
        "The TUI's key bindings, grouped by interaction mode — the same rows \
         the in-app `?` overlay shows, generated from the keymap table that \
         also drives dispatch and the command palette.\n\n",
    );

    // Group the help rows by mode, preserving first-appearance order.
    let mut order: Vec<ModeKind> = Vec::new();
    let mut groups: Vec<Vec<(String, String)>> = Vec::new();
    for line in help_lines() {
        let idx = match order.iter().position(|m| *m == line.mode) {
            Some(i) => i,
            None => {
                order.push(line.mode);
                groups.push(Vec::new());
                order.len() - 1
            }
        };
        groups[idx].push((line.keys, line.desc));
    }

    for (mode, lines) in order.iter().zip(&groups) {
        out.push_str(&format!("## {:?}\n\n", mode));
        if let Some(blurb) = mode_blurb(*mode) {
            out.push_str(blurb);
            out.push_str("\n\n");
        }
        out.push_str("| Keys | Action |\n| ---- | ------ |\n");
        for (keys, desc) in lines {
            out.push_str(&format!("| {} | {} |\n", ticks(keys), cell(desc)));
        }
        out.push('\n');
    }

    out.push_str(
        "## Outside the app\n\n\
         Files the TUI writes land in the current working directory: `e` \
         exports the lineage view as `<model>_lineage.txt` \
         (`<project>_overview.txt` from the whole-graph overview), and the `D` \
         modal's `e` exports the PR impact report as `<project>_pr_impact.md`. \
         `o` opens the model's file with `$VISUAL`, falling back to `$EDITOR` \
         when it is unset.\n\n\
         `Cmd`-modified bindings are delivered only by terminals that speak \
         the kitty keyboard protocol — dbtl probes for it at startup; \
         everywhere else their `Ctrl` alternatives cover the same actions.\n",
    );
    out
}

/// A one-line orientation per mode — display-only; a new mode simply has no
/// blurb until one is written.
fn mode_blurb(mode: ModeKind) -> Option<&'static str> {
    Some(match mode {
        ModeKind::Selection => "The main three-pane screen.",
        ModeKind::Search => "The `/` list search.",
        ModeKind::Detail => "The `Enter` node-detail overlay.",
        ModeKind::Sql => "The `s` SQL-preview overlay.",
        ModeKind::Stats => "The `S` project-stats dashboard.",
        ModeKind::Diff => "The `D` baseline-diff summary (needs `--diff`).",
        ModeKind::Columns => "The column-lineage modal.",
        ModeKind::Notifications => "The `Ctrl-n` notification centre.",
        ModeKind::Palette => "The `Ctrl-p` command palette.",
        ModeKind::Yank => "The `y` yank menu.",
        ModeKind::Menu => "The node context menu (`a` / right-click).",
        ModeKind::Help => "The `?` help overlay itself.",
        ModeKind::Home => "The home tab.",
    })
}

// ---------------------------------------------------------------------------
// lint-rules.md — the REGISTRY table as an overview plus per-rule sections.
// ---------------------------------------------------------------------------

fn lint_rules_markdown() -> String {
    let mut out = doc_header("Lint rules");
    out.push_str(&format!(
        "{} rules, evaluated by `dbtl lint`, `dbtl check --lint`, the docs \
         generator, and the TUI's Lint lens — one registry table also backs \
         `--list-rules`, `--explain`, SARIF metadata, and `.dbtl.yml` \
         validation, so all of them describe the same rule set.\n\n",
        REGISTRY.len()
    ));
    out.push_str("| Rule | Category | Default severity | Inputs | Summary |\n");
    out.push_str("| ---- | -------- | ---------------- | ------ | ------- |\n");
    for rule in LintRule::ALL {
        let i = rule.info();
        out.push_str(&format!(
            "| [`{}`](#{}) | {} | {} | {} | {} |\n",
            i.id,
            i.id,
            i.category.name(),
            i.default_severity.name(),
            requires_label(i.requires),
            cell(i.summary)
        ));
    }

    for rule in LintRule::ALL {
        let i = rule.info();
        out.push_str(&format!("\n## `{}`\n\n", i.id));
        out.push_str(&format!(
            "Category: `{}` · Default severity: `{}` · Inputs: {}\n\n",
            i.category.name(),
            i.default_severity.name(),
            requires_label(i.requires)
        ));
        out.push_str(&format!("{}.\n\n", sentence(i.summary)));
        if !i.rationale.is_empty() {
            out.push_str(&format!("**Why:** {}\n\n", cell(i.rationale)));
        }
        if !i.example.is_empty() {
            out.push_str(&format!("**Example:** {}\n\n", cell(i.example)));
        }
        out.push_str(&format!("**Fix:** {}\n\n", cell(i.help)));
        if i.params.is_empty() {
            out.push_str("**Parameters:** none.\n");
        } else {
            out.push_str("**Parameters:**\n\n");
            out.push_str("| Name | Type | Default | Description |\n");
            out.push_str("| ---- | ---- | ------- | ----------- |\n");
            for p in i.params {
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} |\n",
                    p.name,
                    param_type(p.kind),
                    param_default(p.kind),
                    cell(p.description)
                ));
            }
        }
    }
    out
}

/// The registry `requires` as a readable input list.
fn requires_label(requires: dbtl_core::Requires) -> &'static str {
    match requires {
        dbtl_core::Requires::Graph => "graph",
        dbtl_core::Requires::Sql => "graph + parsed SQL",
        dbtl_core::Requires::Catalog => "graph + catalog",
    }
}

/// A param's accepted value shape.
fn param_type(kind: ParamKind) -> String {
    match kind {
        ParamKind::Int { min, .. } => format!("integer ≥ {min}"),
        ParamKind::StringList {
            allowed: Some(set), ..
        } => format!(
            "list of strings (one of {})",
            set.iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        ParamKind::StringList { .. } => "list of strings".to_string(),
        ParamKind::LayerList { builtins, .. } => format!(
            "list of layer names ({} or a `lint.naming_prefixes` layer)",
            builtins
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        ParamKind::RegexList { .. } => "list of regular expressions".to_string(),
        ParamKind::SelectorList { .. } => "list of selectors".to_string(),
        ParamKind::Bool { .. } => "boolean".to_string(),
    }
}

/// A param's registry default as a table cell.
fn param_default(kind: ParamKind) -> String {
    match kind {
        ParamKind::Int { default, .. } => format!("`{default}`"),
        ParamKind::Bool { default } => format!("`{default}`"),
        ParamKind::StringList { default, .. }
        | ParamKind::LayerList { default, .. }
        | ParamKind::RegexList { default }
        | ParamKind::SelectorList { default } => {
            format!("`[{}]`", default.join(", "))
        }
    }
}

/// The registry `summary` as a lead-capitalized sentence (the registry
/// writes lowercase noun phrases; a section body wants a sentence).
fn sentence(summary: &str) -> String {
    let mut s = summary.trim().to_string();
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s
}

// ---------------------------------------------------------------------------
// The .dbtl.yml schema — the derived wire shape with the dynamic sections
// (`lint.rules`, `check.gates`, `docs.lineage_depth`) injected from the
// rule/gate registries, plus the literal bounds the derive cannot express.
// ---------------------------------------------------------------------------

fn dbtl_yml_schema() -> Value {
    let mut schema = project_config_schema().to_value();
    // Inline the `#/$defs/…` references so the file stays draft-07-shaped:
    // a `$ref`-sibling `description` is inert in draft-07, and editors get
    // the text without resolving refs.
    inline_local_refs(&mut schema);

    let mut properties = match schema.pointer("/properties").cloned() {
        Some(Value::Object(p)) => p,
        _ => Map::new(),
    };

    // lint.rules — every rule id, its bool toggle, and its object form
    // (severity + exclude + the declared params), all from REGISTRY.
    let rules_props: Map<String, Value> = REGISTRY
        .iter()
        .map(|info| {
            let mut obj_props = Map::new();
            obj_props.insert("severity".into(), severity_or_null());
            obj_props.insert(
                "exclude".into(),
                json!({
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "dbt selector strings — findings whose node matches the union are dropped",
                }),
            );
            for p in info.params {
                obj_props.insert(p.name.to_string(), param_schema(p));
            }
            (
                info.id.to_string(),
                json!({
                    "description": info.summary,
                    "anyOf": [
                        {"type": "boolean"},
                        {"type": "object", "additionalProperties": false, "properties": obj_props},
                    ],
                }),
            )
        })
        .collect();
    *prop(&mut properties, &["lint", "properties", "rules"]) = json!({
        "type": "object",
        "additionalProperties": false,
        "description": "rule id → `false` (off) | `true` (the registry default) | a settings object (`severity`, `exclude`, and the rule's declared parameters). Unknown rule names are an error.",
        "properties": rules_props,
    });

    // check.gates — bare severity or the object form, ids and params from
    // the gate registry.
    let gates_props: Map<String, Value> = CheckGate::ALL
        .iter()
        .map(|gate| {
            let mut obj_props = Map::new();
            obj_props.insert("severity".into(), severity_or_null());
            for p in gate.params() {
                obj_props.insert(p.name.to_string(), gate_param_schema(p));
            }
            let default = gate
                .default_severity()
                .map_or("off".to_string(), |s| s.name().to_string());
            let diff = if gate.needs_diff() {
                " — runs only under `--diff`"
            } else {
                ""
            };
            (
                gate.id().to_string(),
                json!({
                    "description": format!("{} (default `{default}`{diff})", gate.summary()),
                    "anyOf": [
                        severity_enum(),
                        {"type": "object", "additionalProperties": false, "properties": obj_props},
                    ],
                }),
            )
        })
        .collect();
    *prop(&mut properties, &["check", "properties", "gates"]) = json!({
        "type": "object",
        "additionalProperties": false,
        "description": "gate id → bare severity (`error` | `warn` | `off`) or `{severity, <params>}`. Unknown ids and params are errors.",
        "properties": gates_props,
    });

    // docs.lineage_depth — the `usize`/`'all'` grammar the manual
    // Deserialize impl accepts: `~` (the field is Option<_>), an integer,
    // or a numeric STRING — including `+7`/`07` spellings the parser's
    // `usize::from_str` takes.
    *prop(&mut properties, &["docs", "properties", "lineage_depth"]) = json!({
        "description": "Mermaid ancestry depth per node page: an integer ≥ 1, or `all` for the whole closure (default 2).",
        "anyOf": [
            {"type": "integer", "minimum": 1},
            {"enum": ["all"]},
            {"type": "string", "pattern": "^\\+?0*[1-9][0-9]*$"},
            {"type": "null"},
        ],
    });

    // usize derives `minimum: 0` — the parser requires ≥ 1.
    for key in ["model_fanout_threshold", "source_fanout_threshold"] {
        let node = prop(&mut properties, &["lint", "properties", key]);
        if let Value::Object(m) = node {
            m.insert("minimum".into(), json!(1));
        }
    }

    // `owner_meta_key` must contain a non-space character (the parser
    // rejects empty/whitespace-only values).
    let node = prop(&mut properties, &["owner_meta_key"]);
    if let Value::Object(m) = node {
        m.insert("pattern".into(), json!("\\S"));
    }

    let mut root = Map::new();
    root.insert(
        "$schema".into(),
        json!("http://json-schema.org/draft-07/schema#"),
    );
    root.insert("$comment".into(), json!(GENERATED_COMMENT));
    root.insert("title".into(), json!(".dbtl.yml"));
    root.insert(
        "description".into(),
        json!("Project-local dbtl policy (lint rules, check gates, docs options), discovered beside the data source. Unknown top-level keys are allowed but ignored with a warning."),
    );
    root.insert("type".into(), json!("object"));
    // NO additionalProperties: unknown top-level keys warn rather than fail.
    root.insert("properties".into(), Value::Object(properties));
    Value::Object(root)
}

/// The `error|warn|off` severity schema — derived from the same enum the
/// gate/rule severity fields deserialize (`null` folds to "unset" inside
/// the object forms).
fn severity_enum() -> Value {
    let mut v = severity_setting_schema().to_value();
    if let Value::Object(m) = &mut v {
        m.shift_remove("$schema");
        m.shift_remove("title");
    }
    v
}

fn severity_or_null() -> Value {
    json!({"anyOf": [severity_enum(), {"type": "null"}]})
}

/// A rule parameter's schema — the shape `LintFile`'s flattened params map
/// validates, from the registry declaration.
fn param_schema(p: &dbtl_core::ParamSpec) -> Value {
    match p.kind {
        ParamKind::Int { min, default } => json!({
            "type": "integer",
            "minimum": min,
            "default": default,
            "description": p.description,
        }),
        ParamKind::StringList { default, allowed } => json!({
            "type": "array",
            "items": match allowed {
                Some(set) => json!({"type": "string", "enum": set}),
                None => json!({"type": "string"}),
            },
            "default": default,
            "description": p.description,
        }),
        // No `enum`: the accepted set is builtins PLUS the project's own
        // `lint.naming_prefixes` keys — a fixed schema enum would reject the
        // custom layers the parser accepts.
        ParamKind::LayerList { default, builtins } => json!({
            "type": "array",
            "items": {"type": "string"},
            "default": default,
            "description": format!("{} (built-in layers: {})", p.description, builtins.join(", ")),
        }),
        ParamKind::RegexList { default } => json!({
            "type": "array",
            "items": {"type": "string", "format": "regex"},
            "default": default,
            "description": format!("{} (each item must compile as a regular expression)", p.description),
        }),
        ParamKind::SelectorList { default } => json!({
            "type": "array",
            "items": {"type": "string"},
            "default": default,
            "description": format!("{} (each item must be a valid selector — same grammar as `exclude:`; `state:` is not allowed)", p.description),
        }),
        ParamKind::Bool { default } => json!({
            "type": "boolean",
            "default": default,
            "description": p.description,
        }),
    }
}

/// A `check.gates.<id>` parameter's schema — `min`/`max` bounds from the
/// gate registry (the `usize::MAX` ceiling drops out as meaningless).
fn gate_param_schema(p: &dbtl_core::GateParamSpec) -> Value {
    match p.kind {
        GateParamKind::Int { min, max } => {
            let mut m = Map::new();
            m.insert("type".into(), json!("integer"));
            m.insert("minimum".into(), json!(min));
            if max != usize::MAX {
                m.insert("maximum".into(), json!(max));
            }
            Value::Object(m)
        }
        // `dir: ~` is an explicit no-op in the parser (Value::Null), so the
        // schema accepts null alongside the path string.
        GateParamKind::Path => json!({
            "type": ["string", "null"],
            "description": "a path, resolved against the `.dbtl.yml` directory",
        }),
    }
}

/// Replace every `{"$ref": "#/$defs/X", ...siblings}` node with the def's
/// members merged with its siblings — then drop `$defs`.
fn inline_local_refs(root: &mut Value) {
    let defs = match root.pointer("/$defs").cloned() {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    fn rewrite(v: &mut Value, defs: &Map<String, Value>) {
        match v {
            Value::Object(map) => {
                if let Some(Value::String(r)) = map.get("$ref").cloned() {
                    if let Some(Value::Object(def)) =
                        r.strip_prefix("#/$defs/").and_then(|name| defs.get(name))
                    {
                        let mut merged = def.clone();
                        for (k, val) in map.iter() {
                            if k != "$ref" {
                                merged.insert(k.clone(), val.clone());
                            }
                        }
                        *map = merged;
                    }
                }
                for val in map.values_mut() {
                    rewrite(val, defs);
                }
            }
            Value::Array(items) => {
                for item in items {
                    rewrite(item, defs);
                }
            }
            _ => {}
        }
    }
    rewrite(root, &defs);
    if let Value::Object(m) = root {
        m.shift_remove("$defs");
    }
}

/// The `config.yml` schema — derived from `persist::Config`. Unknown keys
/// are ignored by the parser, so no `additionalProperties` bound is set.
fn config_yml_schema() -> Value {
    let mut derived = schemars::schema_for!(dbtl_tui::persist::Config).to_value();
    let properties = match derived.pointer_mut("/properties") {
        Some(v) => std::mem::take(v),
        None => Value::Null,
    };
    let mut root = Map::new();
    root.insert(
        "$schema".into(),
        json!("http://json-schema.org/draft-07/schema#"),
    );
    root.insert("$comment".into(), json!(GENERATED_COMMENT));
    root.insert("title".into(), json!("dbtl config.yml"));
    root.insert(
        "description".into(),
        json!("User-level defaults at ~/.config/dbtl/config.yml (precedence: config < saved state < explicit CLI flags). Unknown keys are ignored."),
    );
    root.insert("type".into(), json!("object"));
    root.insert("properties".into(), properties);
    Value::Object(root)
}

/// Reach `properties.<path…>` inside a schema object, creating the chain.
fn prop<'a>(properties: &'a mut Map<String, Value>, path: &[&str]) -> &'a mut Value {
    let mut map = properties;
    for &key in &path[..path.len() - 1] {
        map = map
            .entry(key.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("schema path is an object");
    }
    map.entry(path[path.len() - 1].to_string())
        .or_insert_with(|| json!({}))
}

/// `serde_json::to_string_pretty` + a trailing newline (the committed file
/// convention).
fn json_document(schema: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(schema).unwrap())
}

// ---------------------------------------------------------------------------
// config.md — the two config files, keyed tables + the shared sections.
// ---------------------------------------------------------------------------

fn config_markdown(dbtl_schema: &Value) -> String {
    let mut out = doc_header("Configuration reference");
    out.push_str(
        "dbtl reads two YAML config files:\n\n\
         - **`.dbtl.yml`** — project-local policy (lint rules, check gates, \
         docs options), discovered beside the data source.\n\
         - **`~/.config/dbtl/config.yml`** — user-level defaults for the \
         interactive app (precedence: config < saved state < explicit CLI \
         flags).\n\n\
         Both ship a generated JSON Schema under `schema/` — point \
         yaml-language-server at them for completion and validation:\n\n\
         ```yaml\n\
         # .dbtl.yml\n\
         # yaml-language-server: $schema=https://raw.githubusercontent.com/hytfjwr/dbtl/main/schema/dbtl.schema.json\n\n\
         # ~/.config/dbtl/config.yml\n\
         # yaml-language-server: $schema=https://raw.githubusercontent.com/hytfjwr/dbtl/main/schema/config.schema.json\n\
         ```\n\
         (A relative path like `./schema/dbtl.schema.json` works when the \
         repository is checked out.)\n\n",
    );

    let props = dbtl_schema
        .pointer("/properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    // ---- .dbtl.yml --------------------------------------------------
    out.push_str("## `.dbtl.yml`\n\n");
    out.push_str(
        "Looked up beside the data source: the project directory in source \
         mode; in manifest mode the project root first, then `target/` as a \
         fallback. Unknown **top-level** keys are ignored with a warning; \
         unknown keys inside `lint:`/`check:`/`docs:` — or inside a rule or \
         gate entry — are hard errors.\n\n",
    );
    out.push_str("### Top-level keys\n\n");
    out.push_str("| Key | Type | Default | Description |\n");
    out.push_str("| --- | ---- | ------- | ----------- |\n");
    for (key, node) in &props {
        out.push_str(&format!(
            "| `{key}` | {} | {} | {} |\n",
            top_type_label(key, node),
            top_default(key),
            cell(&desc(node))
        ));
    }
    out.push_str("\nAny other top-level key is allowed but ignored (it produces a warning).\n");

    // lint:
    section(&mut out, &props, "lint", "lint", &lint_default, &lint_type);

    // lint.rules.<id>
    out.push_str("\n#### `lint.rules.<rule-id>`\n\n");
    out.push_str(
        "`false` disables the rule, `true` runs it at the registry default, \
         or a settings object sets severity/excludes/parameters:\n\n",
    );
    out.push_str("| Key | Type | Description |\n| --- | ---- | ----------- |\n");
    out.push_str("| `severity` | `error` \\| `warn` \\| `off` | The severity findings report at; `off` disables the rule. |\n");
    out.push_str("| `exclude` | list of strings | dbt selector strings — findings whose node matches the union are dropped. |\n");
    out.push_str(
        "| _rule parameters_ | per-rule | Declared per rule — see the \
         parameters tables in the [lint rules reference](lint-rules.md); an \
         undeclared name is an error. |\n",
    );

    // check:
    section(
        &mut out,
        &props,
        "check",
        "check",
        &check_default,
        &check_type,
    );

    // check.gates.<id>
    out.push_str("\n#### `check.gates.<gate-id>`\n\n");
    out.push_str(
        "A gate accepts a bare severity (`error` \\| `warn` \\| `off`) or an \
         object `{severity, <params>}`. An entry that sets only parameters \
         resolves to `error` when the gate is off by default.\n\n",
    );
    out.push_str("| Gate | Default | Needs `--diff` | Parameters | Summary |\n");
    out.push_str("| ---- | ------- | -------------- | ---------- | ------- |\n");
    for gate in CheckGate::ALL {
        let params = gate
            .params()
            .iter()
            .map(|p| match p.kind {
                GateParamKind::Int { min, max } if max == usize::MAX => {
                    format!("`{}` (integer ≥ {min})", p.name)
                }
                GateParamKind::Int { min, max } => {
                    format!("`{}` (integer {min}–{max})", p.name)
                }
                GateParamKind::Path => format!("`{}` (path)", p.name),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let params = if params.is_empty() {
            "—".to_string()
        } else {
            params
        };
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {} | {} |\n",
            gate.id(),
            gate.default_severity().map_or("off", |s| s.name()),
            if gate.needs_diff() { "yes" } else { "no" },
            params,
            cell(gate.summary())
        ));
    }

    // docs:
    out.push_str("\n### `docs:`\n\n");
    out.push_str("| Key | Type | Default | Description |\n");
    out.push_str("| --- | ---- | ------- | ----------- |\n");
    if let Some(inner) = props
        .get("docs")
        .and_then(|n| n.pointer("/properties"))
        .and_then(Value::as_object)
    {
        for (k, node) in inner {
            out.push_str(&format!(
                "| `{k}` | {} | {} | {} |\n",
                docs_type(k, node),
                docs_default(k),
                cell(&desc(node))
            ));
        }
    }

    // ---- config.yml -------------------------------------------------
    out.push_str("\n## `config.yml` (`~/.config/dbtl/config.yml`)\n\n");
    out.push_str(
        "User-level defaults for the interactive app — every key optional. \
         Unknown keys are ignored.\n\n",
    );
    out.push_str("| Key | Type | Description |\n| --- | ---- | ----------- |\n");
    let cfg = config_yml_schema();
    if let Some(props) = cfg.pointer("/properties").and_then(Value::as_object) {
        for (key, node) in props {
            out.push_str(&format!(
                "| `{key}` | {} | {} |\n",
                config_type(node),
                cell(&desc(node))
            ));
        }
    }
    out
}

/// A `### \`section:\`` keys table.
fn section(
    out: &mut String,
    props: &Map<String, Value>,
    key: &str,
    title: &str,
    default: &dyn Fn(&str) -> String,
    type_of: &dyn Fn(&str) -> String,
) {
    out.push_str(&format!("\n### `{title}:`\n\n"));
    out.push_str("| Key | Type | Default | Description |\n");
    out.push_str("| --- | ---- | ------- | ----------- |\n");
    if let Some(inner) = props
        .get(key)
        .and_then(|n| n.pointer("/properties"))
        .and_then(Value::as_object)
    {
        for (k, node) in inner {
            out.push_str(&format!(
                "| `{k}` | {} | {} | {} |\n",
                type_of(k),
                default(k),
                cell(&desc(node))
            ));
        }
    }
}

/// The readable type of a top-level `.dbtl.yml` key.
fn top_type_label(key: &str, node: &Value) -> String {
    match key {
        "fail_on" => "`error` \\| `warn` \\| `never`".to_string(),
        "lint" | "check" | "docs" => "object".to_string(),
        _ => schema_type(node),
    }
}

/// The readable type of a `lint:` key.
fn lint_type(key: &str) -> String {
    match key {
        "model_fanout_threshold" | "source_fanout_threshold" => "integer ≥ 1".to_string(),
        "naming_prefixes" => "object: layer → list of strings".to_string(),
        "baseline" => "path".to_string(),
        _ => "object".to_string(),
    }
}

/// The readable type of a `check:` key.
fn check_type(key: &str) -> String {
    match key {
        "lint" => "boolean".to_string(),
        _ => "object".to_string(),
    }
}

/// The readable type of a `docs:` key — `lineage_depth` has a custom
/// grammar; every other key's type comes straight off its schema node.
fn docs_type(key: &str, node: &Value) -> String {
    match key {
        "lineage_depth" => "integer ≥ 1 or `all`".to_string(),
        _ => schema_type(node),
    }
}

/// A top-level `.dbtl.yml` key's effective default.
fn top_default(key: &str) -> String {
    match key {
        "fail_on" => "`error`".to_string(),
        "owner_meta_key" => "`owner`".to_string(),
        _ => "—".to_string(),
    }
}

/// A `lint:` key's effective default — registry/param-driven where the
/// value exists.
fn lint_default(key: &str) -> String {
    let cfg = LintConfig::default();
    match key {
        "model_fanout_threshold" => int_param_default(LintRule::ModelFanout),
        "source_fanout_threshold" => int_param_default(LintRule::SourceFanout),
        "naming_prefixes" => cfg
            .naming_prefixes
            .iter()
            .map(|(k, v)| format!("`{k}: [{}]`", v.join(", ")))
            .collect::<Vec<_>>()
            .join(" "),
        "baseline" => format!("`{}`", cfg.baseline.display()),
        _ => "—".to_string(),
    }
}

/// A `check:` key's effective default.
fn check_default(key: &str) -> String {
    match key {
        "lint" => format!("`{}`", dbtl_core::CheckConfig::default().lint),
        _ => "—".to_string(),
    }
}

/// A `docs:` key's effective default.
fn docs_default(key: &str) -> String {
    match key {
        "lineage_depth" => match LineageDepth::DEFAULT {
            LineageDepth::Hops(n) => format!("`{n}`"),
            LineageDepth::All => "`all`".to_string(),
        },
        _ => "`false`".to_string(),
    }
}

/// A rule's declared integer-parameter default (for the compat flat keys).
fn int_param_default(rule: LintRule) -> String {
    rule.info()
        .int_param("threshold")
        .map(|(_, default)| format!("`{default}`"))
        .unwrap_or_else(|| "—".to_string())
}

/// The JSON `type` of a schema node as a readable word.
fn schema_type(node: &Value) -> String {
    let ty = node.pointer("/type");
    let types: Vec<&str> = match ty {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let main =
        types
            .iter()
            .find(|t| **t != "null")
            .copied()
            .unwrap_or(if node.get("enum").is_some() {
                "enum"
            } else {
                "any"
            });
    match main {
        "string" => "string".to_string(),
        "integer" => "integer".to_string(),
        "boolean" => "boolean".to_string(),
        "array" => "array".to_string(),
        "object" => "object".to_string(),
        other => other.to_string(),
    }
}

/// A `config.yml` property's type word.
fn config_type(node: &Value) -> String {
    schema_type(node)
}

/// A schema node's `description` (or empty).
fn desc(node: &Value) -> String {
    node.get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// themes.md — the colour themes: presets + every role a custom file may set.
// ---------------------------------------------------------------------------

fn themes_markdown() -> String {
    use dbtl_tui::ui::theme::{preset_names, ROLE_DOCS};

    let mut out = doc_header("Themes");
    out.push_str(
        "The TUI's colour themes — the built-in presets below plus user files. \
         `--theme <name>` (or `DBTL_THEME`) names a preset or a user file, \
         then falls back to a literal path to a theme YAML file. User files \
         are `*.yml`/`*.yaml` under `$XDG_CONFIG_HOME/dbtl/themes` (else \
         `~/.config/dbtl/themes`); a file's stem is its theme name, and a \
         user file shadows a same-named preset. `dbtl --list-themes` prints \
         every resolvable name.\n\n",
    );

    out.push_str("## Presets\n\n| Name | For |\n| ---- | --- |\n");
    for name in preset_names() {
        out.push_str(&format!("| `{name}` | {} |\n", preset_blurb(name)));
    }

    out.push_str(
        "\n## Custom theme files\n\n\
         A theme file starts from a `base` preset (default `default`) and \
         overrides roles under `colors:`:\n\n\
         ```yaml\n\
         base: dark\n\
         colors:\n\
         \x20 accent: \"#87afff\"\n\
         \x20 class_table: 114\n\
         ```\n\n\
         A colour value is a `\"#rrggbb\"` string or an xterm-256 index \
         (`0`–`255`). Unknown `base` names, unknown role names, and unparsable \
         colours are errors naming the accepted alternatives. `dbtl` warns — \
         but still launches — when a theme's lens tints collide with a class \
         colour, because a tint that shares a class's colour is invisible on \
         those nodes.\n\n\
         ## Roles\n\n\
         Every `colors:` key the parser accepts — the same list the \
         `unknown colour role` error names.\n\n",
    );

    let mut group = "";
    for &(g, name, doc) in ROLE_DOCS {
        if g != group {
            group = g;
            let mut heading = g.to_string();
            if let Some(c) = heading.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            out.push_str(&format!(
                "{}### {heading}\n\n| Role | Paints |\n| ---- | ------ |\n",
                if out.ends_with('\n') && !out.ends_with("\n\n") {
                    "\n"
                } else {
                    ""
                }
            ));
        }
        out.push_str(&format!("| `{name}` | {} |\n", cell(doc)));
    }
    out.push('\n');
    out
}

/// A one-line "what background it suits" per preset — display-only; a new
/// preset falls back to a generic line until one is written.
fn preset_blurb(name: &str) -> &'static str {
    match name {
        "default" => "xterm-256 palette for dark backgrounds; the fallback when no theme is named",
        "dark" => "truecolor, for near-black neutral-cool backgrounds",
        "dusk" => "truecolor, for lighter blue mid-dark backgrounds",
        "light" => "truecolor, for light backgrounds — the only light preset",
        "warm" => "truecolor, for warm umber backgrounds",
        _ => "a built-in preset",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbtl_core::ProjectConfig;
    use std::collections::BTreeSet;

    /// A stand-in for the `dbtl cli-reference` output — the real renderer
    /// stamps the marker itself, so the stub does too.
    const SINK: &str = concat!(
        "<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->\n",
        "\n# cli reference stub\n"
    );

    #[test]
    fn generation_is_deterministic() {
        let a = generate(SINK);
        let b = generate(SINK);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.path, y.path);
            assert_eq!(x.contents, y.contents, "{} changed", x.path);
        }
    }

    #[test]
    fn every_generated_file_carries_the_marker() {
        for f in generate(SINK) {
            if f.path.ends_with(".md") {
                assert!(
                    f.contents.starts_with(GENERATED_MARKER),
                    "{} lacks the marker",
                    f.path
                );
            } else {
                assert!(
                    f.contents.contains(GENERATED_COMMENT),
                    "{} lacks the marker",
                    f.path
                );
            }
        }
    }

    #[test]
    fn keybindings_cover_every_help_line() {
        let doc = keybindings_markdown();
        let rows = doc.lines().filter(|l| l.starts_with("| `")).count();
        assert_eq!(rows, help_lines().len());
        for line in help_lines() {
            assert!(doc.contains(&cell(&line.desc)), "missing {}", line.desc);
            for key in line.keys.split(", ") {
                assert!(doc.contains(&format!("`{key}`")), "missing {key}");
            }
        }
    }

    #[test]
    fn lint_docs_cover_every_rule() {
        let doc = lint_rules_markdown();
        for rule in LintRule::ALL {
            let info = rule.info();
            assert!(doc.contains(&format!("## `{}`", info.id)));
            assert!(doc.contains(info.summary));
            assert!(doc.contains(info.category.name()));
            assert!(
                doc.contains(&format!("**Fix:** {}", info.help)),
                "{}'s fix guidance: {}",
                info.id,
                info.help
            );
            for p in info.params {
                assert!(doc.contains(&format!("`{}`", p.name)));
            }
        }
        // One section + one overview row per rule.
        assert_eq!(doc.matches("## `").count(), LintRule::ALL.len());
    }

    /// The generated schema's accepted key sets — recursive walk vs. the
    /// parser: every nested property set parses, and each level still
    /// rejects a bogus key exactly where the parser does.
    #[test]
    fn dbtl_schema_matches_the_parser() {
        let s = dbtl_yml_schema();
        let get = |path: &[&str]| -> BTreeSet<String> {
            let mut v = &s;
            for &k in path {
                v = &v["properties"][k];
            }
            v["properties"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default()
        };
        // Top level: the five known keys, extra allowed.
        assert_eq!(
            get(&[]),
            ["fail_on", "lint", "check", "docs", "owner_meta_key"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        assert!(s.get("additionalProperties").is_none());
        assert_eq!(
            get(&["lint"]),
            [
                "rules",
                "model_fanout_threshold",
                "source_fanout_threshold",
                "naming_prefixes",
                "baseline"
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        );
        assert_eq!(
            s["properties"]["lint"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            get(&["check"]),
            ["lint", "gates"].iter().map(|s| s.to_string()).collect()
        );
        assert_eq!(
            s["properties"]["check"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            get(&["docs"]),
            ["catalog", "lineage_depth", "run_results"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        assert_eq!(
            s["properties"]["docs"]["additionalProperties"],
            json!(false)
        );
        // rules: exactly the registry ids.
        assert_eq!(
            get(&["lint", "rules"]),
            REGISTRY.iter().map(|i| i.id.to_string()).collect()
        );
        // every rule's object form: severity + exclude + declared params.
        for info in REGISTRY {
            let obj =
                &s["properties"]["lint"]["properties"]["rules"]["properties"][info.id]["anyOf"][1];
            let keys: BTreeSet<String> = obj["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let expected: BTreeSet<String> = ["severity", "exclude"]
                .iter()
                .map(|s| s.to_string())
                .chain(info.params.iter().map(|p| p.name.to_string()))
                .collect();
            assert_eq!(keys, expected, "{}", info.id);
            assert_eq!(obj["additionalProperties"], json!(false));
        }
        // gates: exactly the registry ids.
        assert_eq!(
            get(&["check", "gates"]),
            CheckGate::ALL.iter().map(|g| g.id().to_string()).collect()
        );
        for gate in CheckGate::ALL {
            let obj = &s["properties"]["check"]["properties"]["gates"]["properties"][gate.id()]
                ["anyOf"][1];
            let keys: BTreeSet<String> = obj["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let expected: BTreeSet<String> = ["severity"]
                .iter()
                .map(|s| s.to_string())
                .chain(gate.params().iter().map(|p| p.name.to_string()))
                .collect();
            assert_eq!(keys, expected, "{}", gate.id());
            assert_eq!(obj["additionalProperties"], json!(false));
        }
    }

    /// The kitchen-sink `.dbtl.yml` — everything the schema documents, all
    /// accepted by the real parser.
    #[test]
    fn schema_documented_keys_all_parse() {
        // One valid value per documented key, generated mostly from the
        // schema itself.
        let yaml = r#"
fail_on: warn
owner_meta_key: domain
lint:
  rules:
    model_fanout: { severity: warn, exclude: ["tag:wip"] }
    source_fanout: false
    forbidden_patterns: { patterns: ["\\bUNSAFE\\b"] }
    required_meta_keys: { keys: ["owner"] }
    missing_primary_key_tests: { resource_types: ["model", "snapshot"] }
  model_fanout_threshold: 5
  source_fanout_threshold: 2
  naming_prefixes: { marts: [fct_, dim_], staging: [stg_] }
  baseline: .dbtl-baseline.json
check:
  lint: true
  gates:
    coverage: warn
    changed_hubs: { severity: error, min_downstream: 5 }
    docs_drift: { dir: docs-gen }
docs:
  lineage_depth: 3
"#;
        assert!(ProjectConfig::from_yaml(yaml).is_ok());
        // Lineage depth spellings the schema advertises — including `~`
        // (Option<_>), and the `+7`/`07` strings usize::from_str accepts.
        for v in ["2", "all", "'7'", "~", "'07'", "'+7'"] {
            assert!(
                ProjectConfig::from_yaml(&format!("docs:\n  lineage_depth: {v}\n")).is_ok(),
                "{v}"
            );
        }
        for v in ["0", "'0'", "nope", "-1", "'+0'", "'00'"] {
            assert!(
                ProjectConfig::from_yaml(&format!("docs:\n  lineage_depth: {v}\n")).is_err(),
                "{v}"
            );
        }
        // A null Path param is an explicit no-op in the parser.
        assert!(
            ProjectConfig::from_yaml("check:\n  gates:\n    docs_drift: { dir: ~ }\n").is_ok(),
            "docs_drift.dir: ~ parses"
        );
        // Severity spellings.
        for sev in ["error", "warn", "off", "~"] {
            assert!(
                ProjectConfig::from_yaml(&format!(
                    "lint:\n  rules:\n    model_fanout: {{ severity: {sev} }}\n"
                ))
                .is_ok(),
                "{sev}"
            );
        }
        assert!(
            ProjectConfig::from_yaml("lint:\n  rules:\n    model_fanout: { severity: wrn }\n")
                .is_err()
        );
    }

    /// What the parser rejects, the schema marks too — unknown keys inside
    /// every `deny_unknown_fields` level and unknown rule/gate ids.
    #[test]
    fn parser_rejections_are_schema_rejections() {
        for (yaml, why) in [
            ("lint:\n  rules_typo: true\n", "unknown lint key"),
            ("check:\n  gate: {}\n", "unknown check key"),
            ("docs:\n  depth: 2\n", "unknown docs key"),
            (
                "lint:\n  rules:\n    no_such_rule: false\n",
                "unknown rule id",
            ),
            (
                "lint:\n  rules:\n    model_fanout: { bogus_param: 1 }\n",
                "undeclared rule param",
            ),
            (
                "check:\n  gates:\n    no_such_gate: error\n",
                "unknown gate id",
            ),
            (
                "check:\n  gates:\n    coverage: { bogus: 1 }\n",
                "undeclared gate param",
            ),
            ("docs:\n  lineage_depth: 0\n", "out-of-range depth"),
            ("owner_meta_key: \"  \"\n", "blank owner key"),
        ] {
            assert!(ProjectConfig::from_yaml(yaml).is_err(), "{why}: {yaml}");
        }
        // A non-YAML boolean/integer where the object is expected.
        assert!(ProjectConfig::from_yaml("lint:\n  model_fanout: 5\n").is_err());
        // And the unknown TOP-LEVEL key is a warning, not an error.
        let pc = ProjectConfig::from_yaml("a_future_key: 1\n").unwrap();
        assert_eq!(pc.warnings.len(), 1);
    }

    /// The user `config.yml` schema: every documented key parses, and an
    /// unknown key parses AND stays allowed (the parser ignores it — the
    /// schema must not be stricter).
    #[test]
    fn config_schema_matches_the_parser() {
        let s = config_yml_schema();
        assert!(s.get("additionalProperties").is_none());
        let props = s["properties"].as_object().unwrap();
        for (key, node) in props {
            // A value of the documented type must parse — the parser may
            // reject other types (that's its job), but the schema can't
            // document a key spelling the parser wouldn't take.
            let value = match node {
                n if n.get("type") == Some(&json!("integer")) => "3",
                n if n["type"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|v| v == "integer")) =>
                {
                    "3"
                }
                _ => "\"x\"",
            };
            let yaml = format!("{key}: {value}\n");
            let parsed = dbtl_tui::persist::Config::from_yaml(&yaml);
            assert!(parsed.is_ok(), "{key}: {yaml}");
        }
        assert!(dbtl_tui::persist::Config::from_yaml("bogus: 1\n").is_ok());
    }

    #[test]
    fn check_mode_reports_drift_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = generate(SINK);
        // Missing files.
        let err = check(root, &files).unwrap_err();
        assert!(err.contains("missing"));
        write(root, &files).unwrap();
        check(root, &files).unwrap();
        // A one-character drift is reported and nothing is rewritten.
        let changed = root.join("docs/reference/keybindings.md");
        let mut text = std::fs::read_to_string(&changed).unwrap();
        text.push('x');
        std::fs::write(&changed, &text).unwrap();
        let err = check(root, &files).unwrap_err();
        assert!(err.contains("keybindings.md"), "{err}");
        assert!(err.contains("gen-docs"), "{err}");
        // A stale marked file is flagged too.
        std::fs::write(
            root.join("docs/reference/old.md"),
            format!("{GENERATED_MARKER}\n"),
        )
        .unwrap();
        let err = check(root, &files).unwrap_err();
        assert!(err.contains("old.md"), "{err}");
    }
}
