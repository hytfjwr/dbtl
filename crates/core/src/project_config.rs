//! The project-local `.dbtl.yml` loader — the ONE place the file's
//! discovery rules and top-level shape live. The file is a namespace for
//! project-level settings (`fail_on`, `lint:`, `check:`, `docs:`,
//! `owner_meta_key`). It is deliberately NOT
//! `~/.config/dbtl/config.yml` — lint/gate policy is project-owned (a CI
//! gate must not depend on the developer's machine).
//!
//! Error policy: malformed YAML, unknown keys INSIDE `lint:`/`check:`,
//! unknown rule names, undeclared rule/gate parameters, and malformed or
//! out-of-range values are hard errors — a config a CI gate can't interpret
//! must fail loudly. Unknown TOP-LEVEL keys are warnings
//! ([`ProjectConfig::warnings`]) instead, so the file can grow new
//! sections without breaking older binaries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::gates::{CheckConfig, CheckGate, GateParamKind, GateSetting};
use crate::lint::{
    Category, ExcludeSpec, LintConfig, LintPreset, LintRule, ParamKind, RuleEnabled, Severity,
};

/// The conventional project-local config file name: `.dbtl.yml` next to
/// `dbt_project.yml` (or at the project root above `target/` in manifest
/// mode).
pub const PROJECT_CONFIG_NAME: &str = ".dbtl.yml";

/// `fail_on` — the minimum severity at which findings fail the command:
/// `error` (the default), `warn` (warnings also fail), `never`
/// (report-only). Read here and carried; the CLI decides where it applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FailOn {
    Error,
    Warn,
    Never,
}

impl FailOn {
    /// The stable lowercase tag (same as the serde form).
    pub fn name(self) -> &'static str {
        match self {
            FailOn::Error => "error",
            FailOn::Warn => "warn",
            FailOn::Never => "never",
        }
    }
}

/// Per-node lineage-diagram depth for generated docs: `Hops(n)` draws the
/// neighbourhood within n hops; `All` draws the node's whole closure. This
/// is the shared value `dbtl docs --lineage-depth` and `.dbtl.yml`'s
/// `docs.lineage_depth` both resolve to — parsed once here so every surface
/// agrees on the accepted spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageDepth {
    /// Within `n` hops of the node (n >= 1).
    Hops(usize),
    /// The whole upstream/downstream closure.
    All,
}

impl LineageDepth {
    /// The shipped default — used when neither the flag nor `.dbtl.yml`
    /// names a depth. 2 hops keeps a page's diagram local enough to stay
    /// useful on wide projects.
    pub const DEFAULT: LineageDepth = LineageDepth::Hops(2);

    /// The `Option<usize>` shape [`Dag::subgraph_view`](crate::Dag) takes
    /// (`All` → `None`, unbounded).
    pub fn hops(self) -> Option<usize> {
        match self {
            LineageDepth::Hops(n) => Some(n),
            LineageDepth::All => None,
        }
    }

    /// Parse the flag/config token: `all` or an integer >= 1. Anything else
    /// is a usage/config error at the caller.
    pub fn parse(text: &str) -> Option<Self> {
        if text == "all" {
            return Some(Self::All);
        }
        text.parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .map(Self::Hops)
    }
}

impl<'de> Deserialize<'de> for LineageDepth {
    /// The `docs.lineage_depth` wire value: a YAML integer >= 1 or the word
    /// `all` — the same grammar [`LineageDepth::parse`] accepts on the CLI,
    /// so file and flag reject identically.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, Visitor};
        struct DepthVisitor;
        impl Visitor<'_> for DepthVisitor {
            type Value = LineageDepth;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an integer >= 1 or 'all'")
            }
            fn visit_i64<E: Error>(self, n: i64) -> Result<LineageDepth, E> {
                usize::try_from(n)
                    .ok()
                    .filter(|n| *n >= 1)
                    .map(LineageDepth::Hops)
                    .ok_or_else(|| E::custom("an integer >= 1 or 'all'"))
            }
            fn visit_u64<E: Error>(self, n: u64) -> Result<LineageDepth, E> {
                usize::try_from(n)
                    .ok()
                    .filter(|n| *n >= 1)
                    .map(LineageDepth::Hops)
                    .ok_or_else(|| E::custom("an integer >= 1 or 'all'"))
            }
            fn visit_str<E: Error>(self, s: &str) -> Result<LineageDepth, E> {
                LineageDepth::parse(s).ok_or_else(|| E::custom("an integer >= 1 or 'all'"))
            }
        }
        d.deserialize_any(DepthVisitor)
    }
}

/// The `docs:` section resolved over defaults — all-`None` fields mean the
/// consumer falls back to its shipped default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DocsConfig {
    /// `docs.lineage_depth` — `None` when the file doesn't set it.
    pub lineage_depth: Option<LineageDepth>,
    /// `docs.catalog` — `true` reads the default sibling `catalog.json` into
    /// the generated docs (warehouse types / row counts / sizes). `false`
    /// (or unset) leaves the artifact out entirely — docs never auto-detects.
    pub catalog: bool,
    /// `docs.run_results` — `true` reads the default sibling
    /// `run_results.json` into the generated docs (per-node `last run`
    /// status). `false` (or unset) leaves the artifact out entirely.
    pub run_results: bool,
}

/// A parsed `.dbtl.yml`: the resolved `lint:`/`check:`/`docs:` sections, the
/// top-level `fail_on`/`owner_meta_key`, and any non-fatal notes (unknown
/// top-level keys are WARNED, not errors — forward compatibility with
/// sections added later).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectConfig {
    /// Top-level `fail_on:` value (`None` = the consumer's own default).
    pub fail_on: Option<FailOn>,
    /// The `lint:` section resolved over the shipped defaults.
    pub lint: LintConfig,
    /// `lint.baseline` was written in the file — a missing file at an
    /// explicit path warns, while a missing default stays silent.
    pub baseline_explicit: bool,
    /// The `check:` section resolved over the shipped defaults — the CI
    /// gates `dbtl check` runs and their severities/parameters.
    pub check: CheckConfig,
    /// The `docs:` section resolved over the shipped defaults.
    pub docs: DocsConfig,
    /// Top-level `owner_meta_key:` — the `meta` key the affected-owners
    /// report reads. Wins over `~/.config/dbtl/config.yml`'s
    /// `owner_meta_key`, which wins over the `owner` default.
    pub owner_meta_key: Option<String>,
    /// Non-fatal notes the caller surfaces (CLI: `warning: …` on stderr;
    /// TUI: the startup notice / reload warnings).
    pub warnings: Vec<String>,
}

impl ProjectConfig {
    /// Parse `.dbtl.yml` text: apply the `lint:`/`check:`/`docs:` sections
    /// over the shipped defaults. Errors on malformed YAML, unknown keys
    /// inside a known section, unknown rule names inside `rules:`, unknown
    /// gate names inside `check.gates:`, undeclared rule/gate parameters,
    /// and malformed or out-of-range values — a config a CI gate can't
    /// interpret must fail loudly, never silently gate with defaults.
    /// Unknown TOP-LEVEL keys land in `warnings`.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let file: ProjectFile =
            serde_norway::from_str(text).map_err(|e| format!("invalid .dbtl.yml: {e}"))?;
        let mut lint = LintConfig::default();
        // Per-layer merge BEFORE the rules loop — `unused_models.layers`
        // validates against these keys plus the built-in layer names, so a
        // project-invented layer can be checked like a built-in one. A
        // project overrides only the layers it lists; a layer mapped to
        // `[]` is unchecked (absent == unchecked anyway, but an explicit
        // empty list also SHUTS OFF a default layer).
        if let Some(prefixes) = file.lint.naming_prefixes {
            for (layer, p) in prefixes {
                if layer.trim().is_empty() {
                    return Err("lint.naming_prefixes: layer names must be non-empty".to_string());
                }
                if p.iter().any(|s| s.trim().is_empty()) {
                    return Err(format!(
                        "lint.naming_prefixes.{layer}: prefixes must be non-empty strings"
                    ));
                }
                lint.naming_prefixes.insert(layer, p);
            }
        }
        // `lint.preset` — the base rule set the `categories`/`rules`
        // layers sit above. `preset: ~` is "unset" like every other key's
        // explicit null.
        if let Some(value) = &file.lint.preset {
            if !value.is_null() {
                let name = value
                    .as_str()
                    .ok_or("lint.preset must be one of minimal|standard|strict")?;
                lint.preset = LintPreset::from_name(name).ok_or_else(|| {
                    format!(
                        "unknown lint preset '{name}' (valid presets: {})",
                        LintPreset::ALL
                            .iter()
                            .map(|p| p.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            }
        }
        // `lint.categories` — one `error|warn|off` per category, above the
        // preset and below the per-rule `rules` entries. `<cat>: ~` is
        // "unset" — the same as omitting the entry.
        if let Some(value) = &file.lint.categories {
            if !value.is_null() {
                let map = value
                    .as_mapping()
                    .ok_or("lint.categories must be a table of <category>: error|warn|off")?;
                for (k, v) in map {
                    let key = k
                        .as_str()
                        .ok_or("lint.categories: category names must be strings")?;
                    let category = Category::from_name(key).ok_or_else(|| {
                        format!(
                            "unknown lint category '{key}' (valid categories: {})",
                            Category::ALL
                                .iter()
                                .map(|c| c.name())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;
                    if !v.is_null() {
                        let setting = parse_severity(v, &format!("lint.categories.{key}"))?;
                        lint.categories.insert(category, setting.as_effective());
                    }
                }
            }
        }
        for (name, value) in &file.lint.rules {
            let rule = LintRule::from_name(name).ok_or_else(|| {
                crate::lint::removed_rule_error(name).unwrap_or_else(|| {
                    format!(
                        "unknown lint rule '{name}' (valid rules: {})",
                        valid_rule_names()
                    )
                })
            })?;
            match parse_rule_entry(name, value)? {
                // `true` = enabled — an explicit per-rule entry, so it
                // overrides a category's `off`. The severity stays
                // unpinned: a category `error`/`warn` still applies, else
                // the registry default.
                RuleSpec::Toggle(true) => {
                    lint.rules.insert(rule, RuleEnabled::On(None));
                }
                RuleSpec::Toggle(false) => {
                    lint.rules.insert(rule, RuleEnabled::Off);
                }
                RuleSpec::Settings(settings) => {
                    lint.rules.insert(
                        rule,
                        match settings.severity {
                            // `severity: off` is the object-form disable.
                            Some(sev) => match sev.as_effective() {
                                Some(s) => RuleEnabled::On(Some(s)),
                                None => RuleEnabled::Off,
                            },
                            // No `severity` (or `severity: ~`) — still an
                            // explicit per-rule entry: enabled, unpinned.
                            None => RuleEnabled::On(None),
                        },
                    );
                    if !settings.exclude.is_empty() {
                        // Exclude specs are selectors — validated through the
                        // ONE strict grammar the CLI `--select`/`--exclude`
                        // flags use, so a typo'd spec errors here rather than
                        // silently suppressing nothing at evaluation time.
                        for spec in &settings.exclude {
                            crate::select::validate_selector(&spec.select).map_err(|e| {
                                format!(
                                    "lint.rules.{name}.exclude: invalid selector '{}': {e}",
                                    spec.select
                                )
                            })?;
                            // `state:` needs a diff the lint evaluation
                            // never builds — allowed by the grammar but it
                            // would silently match nothing here.
                            if crate::select::selector_uses_state(&spec.select) {
                                return Err(format!(
                                    "lint.rules.{name}.exclude: 'state:' selectors need a diff; \
                                     rule excludes never have one"
                                ));
                            }
                        }
                        lint.exclude.insert(rule, settings.exclude);
                    }
                    for (param, value) in settings.params {
                        let spec = rule
                            .info()
                            .params
                            .iter()
                            .find(|p| p.name == param)
                            .ok_or_else(|| {
                                format!(
                                    "unknown parameter '{param}' for rule '{name}' (valid params: {})",
                                    valid_params(rule)
                                )
                            })?;
                        match spec.kind {
                            ParamKind::Int { min, .. } => {
                                let v = value
                                    .as_i64()
                                    .and_then(|v| usize::try_from(v).ok())
                                    .filter(|v| *v >= min)
                                    .ok_or_else(|| {
                                        format!(
                                            "lint.rules.{name}.{param} must be an integer >= {min}"
                                        )
                                    })?;
                                lint.params.entry(rule).or_default().insert(param, v);
                            }
                            ParamKind::StringList { allowed, .. } => {
                                let items = value.as_sequence().ok_or_else(|| {
                                    format!("lint.rules.{name}.{param} must be a list of strings")
                                })?;
                                let mut list = Vec::with_capacity(items.len());
                                for item in items {
                                    let s = item.as_str().ok_or_else(|| {
                                        format!(
                                            "lint.rules.{name}.{param} must be a list of strings"
                                        )
                                    })?;
                                    if s.trim().is_empty() {
                                        return Err(format!(
                                            "lint.rules.{name}.{param}: entries must be non-empty"
                                        ));
                                    }
                                    if let Some(allowed) = allowed {
                                        if !allowed.contains(&s) {
                                            return Err(format!(
                                                "lint.rules.{name}.{param}: '{s}' is not a valid value (valid: {})",
                                                allowed.join(", ")
                                            ));
                                        }
                                    }
                                    list.push(s.to_string());
                                }
                                lint.list_params
                                    .entry(rule)
                                    .or_default()
                                    .insert(param, list);
                            }
                            ParamKind::LayerList { builtins, .. } => {
                                let items = value.as_sequence().ok_or_else(|| {
                                    format!(
                                        "lint.rules.{name}.{param} must be a list of layer names"
                                    )
                                })?;
                                let mut list = Vec::with_capacity(items.len());
                                for item in items {
                                    let s = item.as_str().ok_or_else(|| {
                                        format!(
                                            "lint.rules.{name}.{param} must be a list of layer names"
                                        )
                                    })?;
                                    if s.trim().is_empty() {
                                        return Err(format!(
                                            "lint.rules.{name}.{param}: entries must be non-empty"
                                        ));
                                    }
                                    // Built-in layer names plus whatever
                                    // layers the merged `lint.naming_prefixes`
                                    // map declares (merged above, before this
                                    // loop, so a custom layer can be checked
                                    // like a built-in one).
                                    if !builtins.contains(&s)
                                        && !lint.naming_prefixes.contains_key(s)
                                    {
                                        return Err(format!(
                                            "lint.rules.{name}.{param}: '{s}' is not a valid layer \
                                             (valid: {}, or a layer named in lint.naming_prefixes)",
                                            builtins.join(", ")
                                        ));
                                    }
                                    list.push(s.to_string());
                                }
                                lint.list_params
                                    .entry(rule)
                                    .or_default()
                                    .insert(param, list);
                            }
                            ParamKind::SelectorList { .. } => {
                                // Same grammar + same strictness as the
                                // rule's `exclude:` specs — a typo'd
                                // selector must error here rather than
                                // silently suppressing nothing.
                                let items = value.as_sequence().ok_or_else(|| {
                                    format!("lint.rules.{name}.{param} must be a list of selectors")
                                })?;
                                let mut list = Vec::with_capacity(items.len());
                                for item in items {
                                    let s = item.as_str().ok_or_else(|| {
                                        format!(
                                            "lint.rules.{name}.{param} must be a list of selectors"
                                        )
                                    })?;
                                    if s.trim().is_empty() {
                                        return Err(format!(
                                            "lint.rules.{name}.{param}: entries must be non-empty"
                                        ));
                                    }
                                    crate::select::validate_selector(s).map_err(|e| {
                                        format!(
                                            "lint.rules.{name}.{param}: invalid selector '{s}': {e}"
                                        )
                                    })?;
                                    // `state:` needs a diff the lint
                                    // evaluation never builds — allowed by
                                    // the grammar but it would silently
                                    // match nothing here.
                                    if crate::select::selector_uses_state(s) {
                                        return Err(format!(
                                            "lint.rules.{name}.{param}: 'state:' selectors need a \
                                             diff; rule parameters never have one"
                                        ));
                                    }
                                    list.push(s.to_string());
                                }
                                lint.list_params
                                    .entry(rule)
                                    .or_default()
                                    .insert(param, list);
                            }
                            ParamKind::RegexList { .. } => {
                                let items = value.as_sequence().ok_or_else(|| {
                                    format!(
                                        "lint.rules.{name}.{param} must be a list of regex patterns"
                                    )
                                })?;
                                let mut list = Vec::with_capacity(items.len());
                                for item in items {
                                    let s = item.as_str().ok_or_else(|| {
                                        format!(
                                            "lint.rules.{name}.{param} must be a list of regex patterns"
                                        )
                                    })?;
                                    if s.trim().is_empty() {
                                        return Err(format!(
                                            "lint.rules.{name}.{param}: entries must be non-empty"
                                        ));
                                    }
                                    if let Err(e) = regex::Regex::new(s) {
                                        return Err(format!(
                                            "lint.rules.{name}.{param}: '{s}' is not a valid regex: {e}"
                                        ));
                                    }
                                    list.push(s.to_string());
                                }
                                lint.list_params
                                    .entry(rule)
                                    .or_default()
                                    .insert(param, list);
                            }
                            ParamKind::Bool { .. } => {
                                let v = value.as_bool().ok_or_else(|| {
                                    format!("lint.rules.{name}.{param} must be true or false")
                                })?;
                                lint.bool_params.entry(rule).or_default().insert(param, v);
                            }
                        }
                    }
                }
            }
        }
        if let Some(t) = file.lint.model_fanout_threshold {
            set_threshold(
                &mut lint,
                LintRule::ModelFanout,
                "model_fanout_threshold",
                t,
            )?;
        }
        if let Some(t) = file.lint.source_fanout_threshold {
            set_threshold(
                &mut lint,
                LintRule::SourceFanout,
                "source_fanout_threshold",
                t,
            )?;
        }
        let baseline_explicit = file.lint.baseline.is_some();
        if let Some(baseline) = file.lint.baseline {
            lint.baseline = baseline;
        }
        // `check:` — gate severities/parameters resolved over the registry
        // defaults (the `check` gate table is the same one `run_rules`
        // reads, so an id can't parse that doesn't exist).
        let mut check = CheckConfig {
            lint: file.check.lint,
            ..CheckConfig::default()
        };
        for (name, value) in &file.check.gates {
            let (gate, setting) = parse_gate_entry(name, value)?;
            check.gates.insert(gate, setting);
        }
        let docs = DocsConfig {
            lineage_depth: file.docs.lineage_depth,
            catalog: file.docs.catalog,
            run_results: file.docs.run_results,
        };
        let owner_meta_key = match file.owner_meta_key {
            Some(k) if k.trim().is_empty() => {
                return Err("owner_meta_key must be a non-empty string".to_string());
            }
            other => other,
        };
        let warnings = file
            .other
            .keys()
            .map(|k| format!("ignoring unknown top-level key '{k}' in .dbtl.yml"))
            .collect();
        Ok(ProjectConfig {
            fail_on: file.fail_on,
            lint,
            baseline_explicit,
            check,
            docs,
            owner_meta_key,
            warnings,
        })
    }

    /// Load `.dbtl.yml` from `path`: missing file → the shipped defaults (a
    /// project without config still lints), malformed → `InvalidData` with
    /// the parse reason. A relative `lint.baseline` or
    /// `check.gates.docs_drift.dir` resolves against the file's directory
    /// here. Whether a malformed config is fatal or warn+fallback is the
    /// caller's policy — the CI surfaces (`check`, `docs`) treat it as an
    /// error, the TUI warns and falls back.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_yaml(&text)
                .map(|mut pc| {
                    if let Some(dir) = path.parent() {
                        if pc.lint.baseline.is_relative() {
                            pc.lint.baseline = dir.join(&pc.lint.baseline);
                        }
                        // `check.gates.docs_drift.dir` resolves against the
                        // file's directory too — the same rule
                        // `lint.baseline` follows.
                        if let Some(setting) = pc.check.gates.get_mut(&CheckGate::DocsDrift) {
                            if let Some(d) = &mut setting.dir {
                                if d.is_relative() {
                                    *d = dir.join(&d);
                                }
                            }
                        }
                    }
                    pc
                })
                .map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{}: {e}", path.display()),
                    )
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

/// The `.dbtl.yml` candidates for a data source, in priority order:
///
/// - **Source mode** (`project_dir`): the project dir itself — it IS the
///   project root.
/// - **Manifest mode**: when the manifest sits at the standard
///   `<root>/target/manifest.json` layout (same heuristic
///   [`dag`](crate::dag) module uses for the staleness project-root), the
///   PROJECT ROOT wins first, then `target/` itself as an escape hatch. A
///   manifest anywhere else (`--manifest /tmp/x/manifest.json`) gets ONLY its
///   own directory — climbing to an unrelated parent's `.dbtl.yml` would lint
///   a copied artifact against a foreign config.
///
/// The first EXISTING file wins ([`project_config_path`]); when none exists
/// the first candidate is still the conventional place to write one.
pub fn project_config_candidates(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Vec<PathBuf> {
    if let Some(dir) = manifest_path.and_then(Path::parent) {
        let mut out = Vec::new();
        // Standard layout: <root>/target/manifest.json → root first.
        if dir.file_name().is_some_and(|n| n == "target") {
            if let Some(root) = dir.parent() {
                out.push(root.join(PROJECT_CONFIG_NAME));
            }
        }
        out.push(dir.join(PROJECT_CONFIG_NAME));
        return out;
    }
    project_dir.map_or_else(Vec::new, |d| vec![d.join(PROJECT_CONFIG_NAME)])
}

/// The `.dbtl.yml` actually in effect for a data source: the first candidate
/// that exists on disk ([`project_config_candidates`] order), else `None` —
/// the caller then uses [`ProjectConfig::default`].
pub fn project_config_path(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Option<PathBuf> {
    project_config_candidates(manifest_path, project_dir)
        .into_iter()
        .find(|p| p.is_file())
}

/// The `.dbtl.yml` top-level shape. `fail_on`, `lint`, `check`, `docs`, and
/// `owner_meta_key` are consumed; anything else collects into `other` and
/// becomes a warning (never a hard error — the file can grow project-level
/// sections without breaking older binaries).
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default)]
struct ProjectFile {
    /// `error` | `warn` | `never` — the minimum severity at which findings
    /// fail a run. `--fail-on` overrides it.
    fail_on: Option<FailOn>,
    /// `lint:` — the rule-set preset, per-category severities, per-rule
    /// enables, severities, parameters, excludes, naming prefixes, and the
    /// baseline path.
    lint: LintFile,
    /// `check:` — gate severities/parameters and whether the lint gates
    /// fold into `dbtl check`.
    check: CheckFile,
    /// `docs:` — generated-docs options.
    docs: DocsFile,
    /// The `meta:` key the affected-owners report reads (default `owner`;
    /// wins over `config.yml`'s `owner_meta_key`).
    owner_meta_key: Option<String>,
    // Unknown top-level keys collect here and warn — forward compat for
    // sections a newer dbtl adds (never a hard error).
    #[cfg_attr(feature = "schema", schemars(skip))]
    #[serde(flatten)]
    other: BTreeMap<String, serde_norway::Value>,
}

/// The `docs:` section wire shape. `deny_unknown_fields` — the same typo
/// policy as `lint:`: a silently-ignored `lineage_dpeth` would generate a
/// different doc set than the author intended.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
struct DocsFile {
    /// `docs.lineage_depth` — `None` when unset; an invalid value fails the
    /// whole parse (a malformed config is fatal, never silently defaulted).
    #[cfg_attr(feature = "schema", schemars(skip))]
    lineage_depth: Option<LineageDepth>,
    /// `docs.catalog` — opt in to the sibling `catalog.json` (same path rule
    /// as the CLI's auto-detect, but only when the file asks for it).
    catalog: bool,
    /// `docs.run_results` — opt in to the sibling `run_results.json`.
    run_results: bool,
}

/// The `check:` section wire shape. `deny_unknown_fields` — the same typo
/// policy as `lint:`/`docs:`: a silently-ignored `gate:` (vs `gates:`)
/// would leave CI running the default set while looking configured.
/// `gates` values stay raw [`serde_norway::Value`]s so each entry can
/// produce a gate-named error (the untagged bare/object forms would
/// collapse every failure into a generic enum mismatch).
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
struct CheckFile {
    /// `check.lint` — include the lint gates in `check` runs without
    /// `--lint`.
    lint: bool,
    /// `check.gates.<id>` → bare severity or `{severity, <params>}` —
    /// validated against the gate registry in [`parse_gate_entry`].
    #[cfg_attr(feature = "schema", schemars(skip))]
    gates: BTreeMap<String, serde_norway::Value>,
}

/// The `lint:` section wire shape. `deny_unknown_fields` makes a typo'd key
/// a hard error — a silently-ignored `model_fanout_threshld` would leave a
/// CI gate running the wrong rule set.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
struct LintFile {
    /// `minimal` | `standard` | `strict` — the base rule set, below
    /// `categories` and `rules`. A raw [`serde_norway::Value`] (the same
    /// per-entry error reporting as `rules`): `preset: ~` is "unset" and
    /// an unknown name errors listing the valid set.
    #[cfg_attr(feature = "schema", schemars(skip))]
    preset: Option<serde_norway::Value>,
    /// category name → `error` | `warn` | `off` — one setting for a whole
    /// category, above `preset` and below `rules`. Raw values so each
    /// entry errors naming the category.
    #[cfg_attr(feature = "schema", schemars(skip))]
    categories: Option<serde_norway::Value>,
    /// rule name → `bool` | settings object. Values stay raw
    /// [`serde_norway::Value`]s so each entry can produce a rule-named
    /// error — validated in [`parse_rule_entry`] (an untagged enum would
    /// collapse every malformed entry into one generic variant error).
    /// Unknown rule names are validated separately so the error can list
    /// the valid set.
    #[cfg_attr(feature = "schema", schemars(skip))]
    rules: BTreeMap<String, serde_norway::Value>,
    /// Compat flat keys — the same value `rules.<id>.threshold` sets; both
    /// spellings together are an error naming both keys.
    model_fanout_threshold: Option<usize>,
    /// Compat flat key — the same value `rules.source_fanout.threshold`
    /// sets; both spellings together are an error naming both keys.
    source_fanout_threshold: Option<usize>,
    /// layer dir → allowed prefixes; merges per-layer over the defaults.
    naming_prefixes: Option<BTreeMap<String, Vec<String>>>,
    /// The frozen-violations file — resolved against this file's directory
    /// in `ProjectConfig::load`.
    baseline: Option<PathBuf>,
}

/// The JSON Schema for `.dbtl.yml`, derived from the same wire structs the
/// parser deserializes — field names, `deny_unknown_fields`, and doc-comment
/// descriptions can't drift from the parser. Members the derive can't
/// express (`lint.rules`, `check.gates`, `docs.lineage_depth`, and the
/// `flatten`ed top-level catch-all) are skipped here; the doc generator
/// patches in the registry-driven shapes.
#[cfg(feature = "schema")]
pub fn project_config_schema() -> schemars::Schema {
    schemars::schema_for!(ProjectFile)
}

/// The `error` | `warn` | `off` enum as a schema — the rule-settings and
/// gate severities embed it (`severity: ~` folds to "unset", so the
/// generator wraps it in `anyOf [enum, null]` where null is accepted).
#[cfg(feature = "schema")]
pub fn severity_setting_schema() -> schemars::Schema {
    schemars::schema_for!(SeveritySetting)
}

/// The `minimal` | `standard` | `strict` enum as a schema — `lint.preset`
/// embeds it; the generator wraps it in `anyOf [enum, null]` where null
/// is accepted like every other unset key.
#[cfg(feature = "schema")]
pub fn lint_preset_schema() -> schemars::Schema {
    schemars::schema_for!(LintPreset)
}

/// `rules.<id>` accepts the compat `bool` (`false` = off, `true` = the
/// registry default severity) or the object form
/// (`{severity: error|warn|off, exclude: [...], <declared params>}`).
enum RuleSpec {
    Toggle(bool),
    Settings(RuleSettings),
}

/// The object form of `rules.<id>` — built by [`parse_rule_entry`] from the
/// raw [`serde_norway::Value`] so each malformed entry produces a
/// rule-named error (an untagged derive would collapse every failure into
/// the same "did not match any variant" message).
#[derive(Debug, Default)]
struct RuleSettings {
    severity: Option<SeveritySetting>,
    /// dbt-selector specs — findings whose `node_uid` matches the union
    /// are suppressed. Wildcard names (`stg_*`), `path:` and `tag:` atoms
    /// work; an entry may be the bare selector string or the
    /// `{select: <selector>, reason: <why>}` object.
    exclude: Vec<ExcludeSpec>,
    /// Rule-declared params (`threshold`, …) — validated against the
    /// registry's [`ParamSpec`](crate::lint::ParamSpec) at parse so an
    /// undeclared key errors naming the rule and its accepted params.
    params: BTreeMap<String, serde_norway::Value>,
}

/// One `lint.rules.<id>` entry: `root_models: false` (bool toggle) or
/// `{severity: error|warn|off, exclude: [selectors], <declared params>}`.
/// Anything else — a bare string, a list, a number — is a config error that
/// names the key and the accepted shapes. `severity`/`exclude` are peeled
/// off here; every other key lands in `params` for the registry check —
/// unknown param names error in the caller, where the rule's declared set
/// is known.
fn parse_rule_entry(name: &str, value: &serde_norway::Value) -> Result<RuleSpec, String> {
    match value {
        serde_norway::Value::Bool(on) => Ok(RuleSpec::Toggle(*on)),
        serde_norway::Value::Mapping(map) => {
            let mut settings = RuleSettings::default();
            for (k, v) in map {
                let key = k
                    .as_str()
                    .ok_or_else(|| format!("lint.rules.{name}: parameter names must be strings"))?;
                match key {
                    "severity" => {
                        // `severity: ~` is "unset" — the same as omitting
                        // the key (the gate parser follows the same rule).
                        settings.severity = match v {
                            serde_norway::Value::Null => None,
                            _ => Some(parse_severity(v, &format!("lint.rules.{name}.severity"))?),
                        };
                    }
                    "exclude" => {
                        settings.exclude = parse_exclude(name, v)?;
                    }
                    _ => {
                        settings.params.insert(key.to_string(), v.clone());
                    }
                }
            }
            Ok(RuleSpec::Settings(settings))
        }
        _ => Err(format!(
            "lint.rules.{name} must be true|false or a table of settings \
             (severity: error|warn|off, exclude: [selectors], declared params)"
        )),
    }
}

/// One `exclude:` item — the bare selector string or the
/// `{select: <selector>, reason: <why>}` object. `reason` is optional and
/// `~`-able; any other key shape is a config error that names the rule.
fn parse_exclude(name: &str, value: &serde_norway::Value) -> Result<Vec<ExcludeSpec>, String> {
    let items = value
        .as_sequence()
        .ok_or_else(|| format!("lint.rules.{name}.exclude must be a list of selector strings"))?;
    let mut specs = Vec::with_capacity(items.len());
    for item in items {
        match item {
            serde_norway::Value::String(s) => specs.push(ExcludeSpec::new(s)),
            serde_norway::Value::Mapping(map) => {
                let mut select = None;
                let mut reason = None;
                for (k, v) in map {
                    match k.as_str() {
                        Some("select") => {
                            let s = v.as_str().ok_or_else(|| {
                                format!(
                                    "lint.rules.{name}.exclude: 'select' must be a selector string"
                                )
                            })?;
                            select = Some(s.to_string());
                        }
                        Some("reason") => {
                            reason = match v {
                                serde_norway::Value::Null => None,
                                _ => Some(
                                    v.as_str()
                                        .ok_or_else(|| {
                                            format!(
                                        "lint.rules.{name}.exclude: 'reason' must be a string"
                                    )
                                        })?
                                        .to_string(),
                                )
                                // An empty reason reads as none — same as
                                // `meta.dbtl` reason normalization.
                                .filter(|s| !s.is_empty()),
                            };
                        }
                        _ => {
                            return Err(format!(
                                "lint.rules.{name}.exclude entries are selector strings or \
                                 {{select: <selector>, reason: <why>}} objects"
                            ));
                        }
                    }
                }
                let select = select.ok_or_else(|| {
                    format!("lint.rules.{name}.exclude entries must name a 'select' selector")
                })?;
                specs.push(ExcludeSpec { select, reason });
            }
            _ => {
                return Err(format!(
                    "lint.rules.{name}.exclude must be a list of selector strings"
                ));
            }
        }
    }
    Ok(specs)
}

/// The `severity:` wire value — `off` folds into "no severity" (disabled).
#[derive(Debug, Clone, Copy, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
enum SeveritySetting {
    Error,
    Warn,
    Off,
}

impl SeveritySetting {
    fn as_effective(self) -> Option<Severity> {
        match self {
            SeveritySetting::Error => Some(Severity::Error),
            SeveritySetting::Warn => Some(Severity::Warn),
            SeveritySetting::Off => None,
        }
    }
}

/// Deserialize a `SeveritySetting` out of a raw value — the bare gate form
/// (`coverage: warn`) and the `severity:` key inside the object form share
/// it so both reject `coverage: wrn` identically.
fn parse_severity(value: &serde_norway::Value, what: &str) -> Result<SeveritySetting, String> {
    SeveritySetting::deserialize(value.clone())
        .map_err(|_| format!("{what} must be one of error|warn|off"))
}

/// The declared parameter names for `gate`, comma-joined for the
/// undeclared-param error (`(none)` when the gate takes no params).
fn valid_gate_params(gate: CheckGate) -> String {
    let names: Vec<_> = gate.params().iter().map(|p| p.name).collect();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

/// One `check.gates.<id>` entry: `coverage: warn` (bare severity) or
/// `changed_hubs: {severity: error, min_downstream: 3}` (object form). The
/// gate id validates against the [`CheckGate`] registry, every non-`severity`
/// object key against the gate's declared params — unknown ids, unknown
/// params, and out-of-range values are all config errors, never silently
/// defaulted.
fn parse_gate_entry(
    name: &str,
    value: &serde_norway::Value,
) -> Result<(CheckGate, GateSetting), String> {
    let gate = CheckGate::from_name(name).ok_or_else(|| {
        format!(
            "unknown check gate '{name}' (valid gates: {})",
            CheckGate::valid_ids()
        )
    })?;
    let (severity, params): (Option<SeveritySetting>, Vec<(&str, &serde_norway::Value)>) =
        match value {
            // `coverage: warn` — the bare severity spelling.
            serde_norway::Value::String(_) => (
                Some(parse_severity(value, &format!("check.gates.{name}"))?),
                Vec::new(),
            ),
            serde_norway::Value::Mapping(map) => {
                let mut severity = None;
                let mut params = Vec::new();
                for (k, v) in map {
                    let key = k.as_str().ok_or_else(|| {
                        format!("check.gates.{name}: parameter names must be strings")
                    })?;
                    if key == "severity" {
                        // `severity:`/`severity: ~` — an explicit null is
                        // "unset", the same as the rules object form.
                        severity = match v {
                            serde_norway::Value::Null => None,
                            _ => Some(parse_severity(v, &format!("check.gates.{name}.severity"))?),
                        };
                    } else {
                        params.push((key, v));
                    }
                }
                (severity, params)
            }
            _ => {
                return Err(format!(
                    "check.gates.{name} must be a severity (error|warn|off) or a table"
                ));
            }
        };
    let mut setting = GateSetting {
        severity: severity
            .map(SeveritySetting::as_effective)
            .unwrap_or_else(|| {
                // No `severity:` — a params-only entry is the author asking
                // for the gate to run (the same "a param on an off gate
                // lifts it to error" rule the CLI flags apply), so a
                // default-off gate resolves to error rather than silently
                // configuring nothing. An entry that sets nothing at all
                // resolves the registry default.
                if params.is_empty() {
                    gate.default_severity()
                } else {
                    gate.default_severity().or(Some(Severity::Error))
                }
            }),
        ..GateSetting::default()
    };
    for (param, value) in params {
        let spec = gate
            .params()
            .iter()
            .find(|p| p.name == param)
            .ok_or_else(|| {
                format!(
                    "unknown parameter '{param}' for gate '{name}' (valid params: {})",
                    valid_gate_params(gate)
                )
            })?;
        match spec.kind {
            GateParamKind::Int { min, max } => {
                let range = if max == usize::MAX {
                    format!("an integer >= {min}")
                } else {
                    format!("an integer in [{min}, {max}]")
                };
                let n = value
                    .as_i64()
                    .and_then(|v| usize::try_from(v).ok())
                    .filter(|v| *v >= min && *v <= max)
                    .ok_or_else(|| format!("check.gates.{name}.{param} must be {range}"))?;
                match spec.name {
                    "min" => setting.min = Some(n),
                    "max" => setting.max = Some(n),
                    "min_downstream" => setting.min_downstream = Some(n),
                    // The Int params the registry declares today — a new one
                    // lands here when it is added.
                    other => unreachable!("undeclared Int param slot '{other}'"),
                }
            }
            GateParamKind::Path => match value {
                serde_norway::Value::Null => {}
                serde_norway::Value::String(s) => setting.dir = Some(PathBuf::from(s)),
                _ => {
                    return Err(format!(
                        "check.gates.{name}.{param} must be a directory path"
                    ));
                }
            },
        }
    }
    Ok((gate, setting))
}

/// The compat flat `<rule>_threshold` keys write into the same parameter
/// slot as `rules.<id>.threshold` — both spellings together are an error
/// naming both keys (a silent last-wins would surprise CI).
fn set_threshold(
    lint: &mut LintConfig,
    rule: LintRule,
    flat_key: &str,
    value: usize,
) -> Result<(), String> {
    if value == 0 {
        return Err(format!("{flat_key} must be >= 1"));
    }
    let params = lint.params.entry(rule).or_default();
    if params.contains_key("threshold") {
        return Err(format!(
            "{flat_key} and lint.rules.{}.threshold set the same option; use only one",
            rule.name()
        ));
    }
    params.insert("threshold".to_string(), value);
    Ok(())
}

/// `ALL`'s names, comma-joined for the unknown-rule error (the same list the
/// `--rules` parser prints).
fn valid_rule_names() -> String {
    LintRule::ALL
        .iter()
        .map(|r| r.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The declared parameter names for `rule`, comma-joined for the
/// undeclared-param error (`(none)` when the rule takes no params).
fn valid_params(rule: LintRule) -> String {
    let names: Vec<_> = rule.info().params.iter().map(|p| p.name).collect();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<ProjectConfig, String> {
        ProjectConfig::from_yaml(yaml)
    }

    #[test]
    fn rules_accept_the_compat_bool_and_the_object_form() {
        let pc = parse(
            r#"lint:
  rules:
    undocumented_models: false
    root_models: true
    model_fanout:
      severity: warn
      threshold: 3
      exclude: ["stg_legacy_*", "path:models/legacy", "tag:wip"]
    source_fanout:
      severity: off
"#,
        )
        .unwrap();
        assert_eq!(pc.lint.severity(LintRule::UndocumentedModels), None);
        assert_eq!(
            pc.lint.severity(LintRule::RootModels),
            Some(Severity::Warn),
            "`true` = the registry default severity"
        );
        assert_eq!(
            pc.lint.severity(LintRule::ModelFanout),
            Some(Severity::Warn)
        );
        assert_eq!(
            pc.lint.int_param(LintRule::ModelFanout, "threshold"),
            Some(3)
        );
        assert_eq!(
            pc.lint.excludes(LintRule::ModelFanout),
            &[
                ExcludeSpec::new("stg_legacy_*"),
                ExcludeSpec::new("path:models/legacy"),
                ExcludeSpec::new("tag:wip"),
            ]
        );
        assert_eq!(pc.lint.severity(LintRule::SourceFanout), None);
    }

    #[test]
    fn an_empty_rule_object_runs_at_the_default() {
        let pc = parse("lint:\n  rules:\n    model_fanout: {}\n").unwrap();
        assert_eq!(
            pc.lint.severity(LintRule::ModelFanout),
            Some(Severity::Warn)
        );
        assert_eq!(
            pc.lint.int_param(LintRule::ModelFanout, "threshold"),
            Some(crate::PR_HUB_MIN_DOWNSTREAM)
        );
    }

    #[test]
    fn an_empty_file_and_a_lintless_file_use_defaults() {
        let empty = parse("").unwrap();
        assert_eq!(empty.lint, LintConfig::default());
        assert!(empty.warnings.is_empty());
        let bare = parse("fail_on: warn\n").unwrap();
        assert_eq!(bare.fail_on, Some(FailOn::Warn));
        assert_eq!(bare.lint, LintConfig::default());
    }

    #[test]
    fn fail_on_reads_three_values_and_rejects_a_bad_one() {
        for (text, want) in [
            ("error", FailOn::Error),
            ("warn", FailOn::Warn),
            ("never", FailOn::Never),
        ] {
            assert_eq!(
                parse(&format!("fail_on: {text}\n")).unwrap().fail_on,
                Some(want)
            );
        }
        assert!(parse("fail_on: sometimes\n").is_err());
    }

    #[test]
    fn unknown_keys_error_inside_lint_but_warn_at_the_top() {
        // A typo'd lint key is a hard error (deny_unknown_fields).
        let err = parse("lint:\n  nope: 1\n").unwrap_err();
        assert!(err.contains("invalid .dbtl.yml"), "{err}");
        // An unknown TOP-LEVEL key is a warning — forward compat with
        // sections added later.
        let pc = parse("pipeline:\n  strict: true\ntui: {}\nlint: {}\n").unwrap();
        assert_eq!(
            pc.warnings,
            vec![
                "ignoring unknown top-level key 'pipeline' in .dbtl.yml".to_string(),
                "ignoring unknown top-level key 'tui' in .dbtl.yml".to_string()
            ],
            "warnings are sorted by key (BTreeMap order)"
        );
    }

    #[test]
    fn docs_lineage_depth_accepts_ints_and_all_and_rejects_the_rest() {
        let pc = parse("docs:\n  lineage_depth: 3\n").unwrap();
        assert_eq!(pc.docs.lineage_depth, Some(LineageDepth::Hops(3)));
        let pc = parse("docs:\n  lineage_depth: all\n").unwrap();
        assert_eq!(pc.docs.lineage_depth, Some(LineageDepth::All));
        let pc = parse("docs: {}\n").unwrap();
        assert_eq!(pc.docs.lineage_depth, None);
        // The same invalid set the flag rejects — plus a typo'd key inside
        // the section (deny_unknown_fields).
        for bad in [
            "docs:\n  lineage_depth: 0\n",
            "docs:\n  lineage_depth: -1\n",
            "docs:\n  lineage_depth: 1.5\n",
            "docs:\n  lineage_depth: sometimes\n",
            "docs:\n  lineage_dpeth: 2\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must fail to parse");
        }
    }

    #[test]
    fn docs_artifact_opt_ins_default_false_parse_bools_and_reject_the_rest() {
        // Unset means off — docs never auto-detects the sibling artifacts.
        let pc = parse("docs: {}\n").unwrap();
        assert!(!pc.docs.catalog && !pc.docs.run_results);
        let pc = parse("").unwrap();
        assert!(!pc.docs.catalog && !pc.docs.run_results);
        // Explicit true/false both parse.
        let pc = parse("docs:\n  catalog: true\n  run_results: true\n").unwrap();
        assert!(pc.docs.catalog && pc.docs.run_results);
        let pc = parse("docs:\n  catalog: false\n  run_results: true\n").unwrap();
        assert!(!pc.docs.catalog && pc.docs.run_results);
        // Non-bool values and unknown keys fail like the section's other typos.
        for bad in [
            "docs:\n  catalog: [x]\n",
            "docs:\n  catalog: 1\n",
            "docs:\n  run_results: \"yes\"\n",
            "docs:\n  catalogs: true\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must fail to parse");
        }
    }

    #[test]
    fn unknown_rule_names_and_params_are_errors() {
        let err = parse("lint:\n  rules:\n    typo_rule: false\n").unwrap_err();
        assert!(
            err.contains("unknown lint rule 'typo_rule'") && err.contains("root_models"),
            "{err} names the rule and the valid set"
        );
        let err = parse("lint:\n  rules:\n    root_models: {severity: quiet}\n").unwrap_err();
        assert!(
            err.contains("lint.rules.root_models.severity must be one of error|warn|off"),
            "{err}"
        );
        // An undeclared parameter names the rule and its accepted params.
        let err = parse("lint:\n  rules:\n    model_fanout: {threshld: 3}\n").unwrap_err();
        assert!(
            err.contains("unknown parameter 'threshld' for rule 'model_fanout'")
                && err.contains("threshold"),
            "{err}"
        );
        // A param on a rule that declares none.
        let err = parse("lint:\n  rules:\n    root_models: {threshold: 3}\n").unwrap_err();
        assert!(
            err.contains("unknown parameter 'threshold' for rule 'root_models'"),
            "{err}"
        );
        // Wrong types and below-min values error too.
        assert!(parse("lint:\n  rules:\n    model_fanout: {threshold: \"hi\"}\n").is_err());
        assert!(parse("lint:\n  rules:\n    model_fanout: {threshold: 0}\n").is_err());
        assert!(parse("lint:\n  model_fanout_threshold: 0\n").is_err());
    }

    #[test]
    fn the_compat_flat_threshold_and_the_object_form_conflict() {
        // The flat key alone still works.
        let pc = parse("lint:\n  model_fanout_threshold: 4\n").unwrap();
        assert_eq!(
            pc.lint.int_param(LintRule::ModelFanout, "threshold"),
            Some(4)
        );
        // …but BOTH spellings together are an error naming both keys.
        let err = parse(
            "lint:\n  model_fanout_threshold: 4\n  rules:\n    model_fanout: {threshold: 5}\n",
        )
        .unwrap_err();
        assert!(
            err.contains("model_fanout_threshold") && err.contains("rules.model_fanout.threshold"),
            "{err} names both keys"
        );
        // A nested object that does NOT set threshold coexists with the flat
        // key (it isn't "setting the same option").
        let pc = parse(
            "lint:\n  model_fanout_threshold: 4\n  rules:\n    model_fanout: {severity: warn}\n",
        )
        .unwrap();
        assert_eq!(
            pc.lint.int_param(LintRule::ModelFanout, "threshold"),
            Some(4)
        );
    }

    #[test]
    fn string_list_params_parse_and_validate_against_the_allowed_set() {
        let pc = parse(
            "lint:\n  rules:\n    required_meta_keys: {keys: [owner, domain]}\n    missing_primary_key_tests: {resource_types: [model, seed]}\n",
        )
        .unwrap();
        assert_eq!(
            pc.lint.str_list_param(LintRule::RequiredMetaKeys, "keys"),
            Some(vec!["owner".to_string(), "domain".to_string()])
        );
        assert_eq!(
            pc.lint
                .str_list_param(LintRule::MissingPrimaryKeyTests, "resource_types"),
            Some(vec!["model".to_string(), "seed".to_string()])
        );
        // An item outside the allowed set errors naming the valid values.
        let err = parse(
            "lint:\n  rules:\n    missing_primary_key_tests: {resource_types: [model, bogus]}\n",
        )
        .unwrap_err();
        assert!(
            err.contains("'bogus'") && err.contains("model") && err.contains("seed"),
            "{err} names the bad item and the valid set"
        );
        // A scalar or non-string items error as the wrong shape.
        assert!(parse("lint:\n  rules:\n    required_meta_keys: {keys: owner}\n").is_err());
        assert!(parse("lint:\n  rules:\n    required_meta_keys: {keys: [1]}\n").is_err());
        // An empty list is legal — it makes the rule a no-op.
        let pc = parse("lint:\n  rules:\n    required_meta_keys: {keys: []}\n").unwrap();
        assert_eq!(
            pc.lint.str_list_param(LintRule::RequiredMetaKeys, "keys"),
            Some(vec![])
        );
    }

    #[test]
    fn exclude_must_be_a_string_list() {
        assert!(parse("lint:\n  rules:\n    root_models: {exclude: \"x\"}\n").is_err());
        assert!(parse("lint:\n  rules:\n    root_models: {exclude: [1]}\n").is_err());
    }

    #[test]
    fn rule_entry_errors_name_the_key_and_the_accepted_forms() {
        // A scalar where the bool/object forms belong — the error names the
        // key path and both accepted shapes, not a serde variant dump.
        let err = parse("lint:\n  rules:\n    root_models: warn\n").unwrap_err();
        assert!(
            err.contains("lint.rules.root_models must be true|false or a table of settings"),
            "{err}"
        );
        // A number and a list hit the same shape error.
        for bad in [
            "lint:\n  rules:\n    root_models: 3\n",
            "lint:\n  rules:\n    root_models: [warn]\n",
        ] {
            let err = parse(bad).unwrap_err();
            assert!(err.contains("lint.rules.root_models"), "{bad} → {err}");
        }
        // A scalar `exclude` names the key and the list shape it wants.
        let err =
            parse("lint:\n  rules:\n    root_models:\n      exclude: \"stg_*\"\n").unwrap_err();
        assert!(
            err.contains("lint.rules.root_models.exclude must be a list of selector strings"),
            "{err}"
        );
        // A wrong-case severity names the accepted values.
        let err = parse("lint:\n  rules:\n    root_models:\n      severity: Warn\n").unwrap_err();
        assert!(
            err.contains("lint.rules.root_models.severity must be one of error|warn|off"),
            "{err}"
        );
        // A duplicated key inside the entry names the entry and the key —
        // the YAML parser rejects it before the shape check runs.
        let err = parse(
            "lint:\n  rules:\n    root_models:\n      severity: warn\n      severity: error\n",
        )
        .unwrap_err();
        assert!(
            err.contains("lint.rules.root_models") && err.contains("severity"),
            "{err}"
        );
        // `severity: ~` is "unset" — same as omitting the key.
        let pc = parse("lint:\n  rules:\n    root_models:\n      severity: ~\n").unwrap();
        assert_eq!(pc.lint.severity(LintRule::RootModels), Some(Severity::Warn));
    }

    #[test]
    fn blank_strings_in_config_lists_are_errors() {
        // An empty/whitespace string is never a meaningful entry — a blank
        // prefix would match EVERYTHING (starts_with("") is true), a blank
        // meta key checks nothing, a blank selector matches nothing. All
        // are config errors naming the key.
        for (yaml, what) in [
            (
                "lint:\n  naming_prefixes:\n    marts: [\"\"]\n",
                "lint.naming_prefixes.marts: prefixes must be non-empty strings",
            ),
            (
                "lint:\n  naming_prefixes:\n    marts: [\"  \"]\n",
                "lint.naming_prefixes.marts: prefixes must be non-empty strings",
            ),
            (
                "lint:\n  rules:\n    required_meta_keys: {keys: [\"\"]}\n",
                "lint.rules.required_meta_keys.keys: entries must be non-empty",
            ),
            (
                "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [\"\"]}\n",
                "lint.rules.rejoining_upstream.ignore_shared_parents: entries must be non-empty",
            ),
        ] {
            let err = parse(yaml).unwrap_err();
            assert!(err.contains(what), "{yaml} → {err}");
        }
        // A blank layer NAME is an error too — a `"": [p_]` key would
        // register a layer nothing can name.
        let err = parse("lint:\n  naming_prefixes:\n    \"\": [x_]\n").unwrap_err();
        assert!(err.contains("lint.naming_prefixes"), "{err}");
        // A blank `exclude` selector is already an error via the grammar
        // (`empty selector`) — the message names the key path.
        let err = parse("lint:\n  rules:\n    root_models: {exclude: [\"  \"]}\n").unwrap_err();
        assert!(err.contains("lint.rules.root_models.exclude"), "{err}");
        // Real values are untouched.
        parse("lint:\n  naming_prefixes:\n    marts: [fct_, dim_]\n").unwrap();
    }

    #[test]
    fn unused_models_layers_accepts_naming_prefixes_layers() {
        // The built-in layers plus whatever layers `lint.naming_prefixes`
        // declares — a project-invented layer (snapshots, util) can be
        // checked like a built-in one.
        let pc = parse(
            "lint:\n  naming_prefixes:\n    snapshots: [snap_]\n  rules:\n    unused_models:\n      layers: [snapshots, marts]\n",
        )
        .unwrap();
        assert_eq!(
            pc.lint.str_list_param(LintRule::UnusedModels, "layers"),
            Some(vec!["snapshots".to_string(), "marts".to_string()])
        );
        // The built-ins alone still work.
        parse("lint:\n  rules:\n    unused_models:\n      layers: [staging]\n").unwrap();
        // A layer that is neither built-in nor prefix-configured errors,
        // naming both sources of valid values.
        let err =
            parse("lint:\n  rules:\n    unused_models:\n      layers: [bogus]\n").unwrap_err();
        assert!(
            err.contains("lint.rules.unused_models.layers")
                && err.contains("staging")
                && err.contains("naming_prefixes"),
            "{err}"
        );
    }

    #[test]
    fn exclude_rejects_state_selectors() {
        // `state:` parses the strict grammar fine but needs a diff the lint
        // evaluation never builds — rejecting it at parse time keeps it from
        // silently suppressing nothing.
        for spec in ["state:modified", "state:new", "2+state:modified"] {
            let err = parse(&format!(
                "lint:\n  rules:\n    root_models: {{exclude: [\"{spec}\"]}}\n"
            ))
            .unwrap_err();
            assert!(
                err.contains("state:"),
                "exclude [{spec}] must name state: in its error — {err}"
            );
        }
        // Non-state selectors still pass validation.
        assert!(
            parse("lint:\n  rules:\n    root_models: {exclude: [\"stg_*\", \"tag:wip\"]}\n")
                .is_ok()
        );
    }

    #[test]
    fn exclude_object_entries_parse_and_validate() {
        // `{select, reason}` items mix with bare strings; `reason: ~` and a
        // missing `reason` both read as no reason.
        let pc = parse(
            "lint:\n  rules:\n    root_models:\n      exclude:\n        - stg_a\n        - {select: \"stg_*\", reason: staging roots}\n        - {select: \"tag:wip\"}\n        - {select: \"path:legacy\", reason: ~}\n",
        )
        .unwrap();
        assert_eq!(
            pc.lint.excludes(LintRule::RootModels),
            &[
                ExcludeSpec::new("stg_a"),
                ExcludeSpec {
                    select: "stg_*".into(),
                    reason: Some("staging roots".into()),
                },
                ExcludeSpec::new("tag:wip"),
                ExcludeSpec::new("path:legacy"),
            ]
        );
        // Selector validation applies to the `select` field the same way —
        // invalid grammar and `state:` both error naming the key.
        let err = parse(
            "lint:\n  rules:\n    root_models:\n      exclude:\n        - {select: \"foo:bar\", reason: x}\n",
        )
        .unwrap_err();
        assert!(
            err.contains("lint.rules.root_models.exclude") && err.contains("invalid selector"),
            "{err}"
        );
        let err = parse(
            "lint:\n  rules:\n    root_models:\n      exclude:\n        - {select: \"state:modified\", reason: x}\n",
        )
        .unwrap_err();
        assert!(err.contains("state:"), "{err}");
    }

    #[test]
    fn exclude_object_entries_reject_bad_shapes() {
        // Every malformed object entry errors naming the key and the
        // accepted shape — a missing `select`, an unknown key, a non-string
        // `select` or `reason`, a non-scalar list item.
        for (yaml, what) in [
            (
                "lint:\n  rules:\n    root_models:\n      exclude:\n        - {reason: x}\n",
                "must name a 'select' selector",
            ),
            (
                "lint:\n  rules:\n    root_models:\n      exclude:\n        - {select: stg, extra: x}\n",
                "selector strings or {select: <selector>, reason: <why>} objects",
            ),
            (
                "lint:\n  rules:\n    root_models:\n      exclude:\n        - {select: 7}\n",
                "'select' must be a selector string",
            ),
            (
                "lint:\n  rules:\n    root_models:\n      exclude:\n        - {select: stg, reason: 7}\n",
                "'reason' must be a string",
            ),
            (
                "lint:\n  rules:\n    root_models:\n      exclude:\n        - 7\n",
                "must be a list of selector strings",
            ),
        ] {
            let err = parse(yaml).unwrap_err();
            assert!(
                err.contains("lint.rules.root_models.exclude") && err.contains(what),
                "{yaml} → {err}"
            );
        }
    }

    #[test]
    fn selector_list_params_validate_like_exclude() {
        // `ignore_shared_parents` is a `ParamKind::SelectorList` — the same
        // grammar and error surface as `exclude:`.
        let pc = parse(
            "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [\"dim_*\", \"tag:shared\"]}\n",
        )
        .unwrap();
        assert_eq!(
            pc.lint
                .str_list_param(LintRule::RejoiningUpstream, "ignore_shared_parents"),
            Some(vec!["dim_*".to_string(), "tag:shared".to_string()])
        );
        // Wrong shapes and bad atoms error at load, not mid-evaluation.
        for yaml in [
            "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: dim_*}\n",
            "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [1]}\n",
            "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [\"bogus:spec:nop\"]}\n",
        ] {
            assert!(parse(yaml).is_err(), "{yaml} must not parse");
        }
        // `state:` needs a diff the lint evaluation never builds.
        let err = parse(
            "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [\"state:modified\"]}\n",
        )
        .unwrap_err();
        assert!(err.contains("state:"), "{err}");
        // A param declared on a rule that doesn't own it still errors —
        // the registry, not the YAML, decides which keys are legal.
        assert!(
            parse("lint:\n  rules:\n    root_models: {ignore_shared_parents: [\"dim_*\"]}\n")
                .is_err()
        );
    }

    #[test]
    fn lint_preset_parses_the_three_names_and_standard_is_default() {
        // `standard` is the shipped default — absent (and `~`) IS standard.
        for (text, want) in [
            ("", LintPreset::Standard),
            ("lint: {}\n", LintPreset::Standard),
            ("lint:\n  preset: ~\n", LintPreset::Standard),
            ("lint:\n  preset: minimal\n", LintPreset::Minimal),
            ("lint:\n  preset: standard\n", LintPreset::Standard),
            ("lint:\n  preset: strict\n", LintPreset::Strict),
        ] {
            assert_eq!(parse(text).unwrap().lint.preset, want, "{text:?}");
        }
        // An unknown name errors listing the valid set — a typo'd preset
        // must never silently run a different rule set.
        let err = parse("lint:\n  preset: stric\n").unwrap_err();
        assert!(
            err.contains("unknown lint preset 'stric'")
                && err.contains("minimal")
                && err.contains("strict"),
            "{err} names the value and the valid set"
        );
        // Non-string and wrong-case values are shape errors.
        for bad in [
            "lint:\n  preset: 3\n",
            "lint:\n  preset: [minimal]\n",
            "lint:\n  preset: {x: 1}\n",
            "lint:\n  preset: Strict\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must fail to parse");
        }
    }

    #[test]
    fn minimal_runs_only_the_error_default_rules() {
        // `minimal` narrows the run set to the rules whose registry default
        // severity is `error` — the broken-guarantee checks. Every other
        // rule reports off (never a skip entry).
        let pc = parse("lint:\n  preset: minimal\n").unwrap();
        for rule in LintRule::ALL {
            let want = if rule.info().default_severity == Severity::Error {
                Some(Severity::Error)
            } else {
                None
            };
            assert_eq!(pc.lint.severity(rule), want, "{}", rule.name());
        }
    }

    #[test]
    fn the_resolution_layers_stack_preset_categories_rules() {
        // weakest → strongest: preset < categories < rules.<id>.
        let pc = parse(
            "lint:\n  preset: minimal\n  categories:\n    sql: error\n    documentation: off\n  rules:\n    select_star: false\n    duplicate_output_columns: {severity: warn}\n    undocumented_models: true\n    model_fanout: {threshold: 2}\n",
        )
        .unwrap();
        let l = &pc.lint;
        // per-rule `false` beats a category `error`.
        assert_eq!(l.severity(LintRule::SelectStar), None);
        // A pinned per-rule severity beats the category's repin AND the
        // preset — under `minimal` this rule would run at error, under the
        // category at error, configured at warn.
        assert_eq!(
            l.severity(LintRule::DuplicateOutputColumns),
            Some(Severity::Warn)
        );
        // `true` beats `documentation: off` (and the preset): enabled,
        // severity unpinned — the category's `off` supplies no severity, so
        // the registry default applies.
        assert_eq!(
            l.severity(LintRule::UndocumentedModels),
            Some(Severity::Warn)
        );
        // A bare settings object is still an explicit enable: under
        // `minimal` model_fanout would be off — here it runs at the
        // registry default (the modeling category sets no severity).
        assert_eq!(l.severity(LintRule::ModelFanout), Some(Severity::Warn));
        // `sql: error` enables every sql rule at error — above the
        // preset's `minimal` (which leaves warn-default rules off).
        assert_eq!(l.severity(LintRule::CartesianJoin), Some(Severity::Error));
        // `documentation: off` disables the category's remaining rules.
        assert_eq!(l.severity(LintRule::UndocumentedSources), None);
        // The preset still fills the bottom: testing's error-default rule
        // runs, warn-default rules outside any configured layer stay off.
        assert_eq!(
            l.severity(LintRule::MissingPrimaryKeyTests),
            Some(Severity::Error)
        );
        assert_eq!(l.severity(LintRule::NamingConventions), None);
    }

    #[test]
    fn lint_categories_validate_names_and_the_severity_grammar() {
        // Unknown category names error listing the valid set.
        let err = parse("lint:\n  categories:\n    sqll: warn\n").unwrap_err();
        assert!(
            err.contains("unknown lint category 'sqll'") && err.contains("sql"),
            "{err} names the key and the valid set"
        );
        // A non-severity value errors naming the key.
        let err = parse("lint:\n  categories:\n    sql: quiet\n").unwrap_err();
        assert!(
            err.contains("lint.categories.sql must be one of error|warn|off"),
            "{err}"
        );
        // The section must be a table — a scalar or a list is a shape error.
        for bad in [
            "lint:\n  categories: warn\n",
            "lint:\n  categories: [sql]\n",
            "lint:\n  categories: 3\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must fail to parse");
        }
        // `<cat>: ~` is "unset" — the same as omitting the entry.
        let pc = parse("lint:\n  categories:\n    sql: ~\n").unwrap();
        assert!(pc.lint.categories.is_empty());
        // Every real category name parses (also proving `deny_unknown_fields`
        // never confuses `categories` keys with rule names).
        for c in Category::ALL {
            parse(&format!("lint:\n  categories:\n    {}: warn\n", c.name())).unwrap();
        }
    }

    #[test]
    fn malformed_lint_section_is_an_error() {
        assert!(parse("lint: [not, a, map]\n").is_err());
        assert!(parse("lint:\n  rules: {root_models: bogus}\n").is_err());
    }

    #[test]
    fn baseline_path_resolves_against_the_config_dir_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join(PROJECT_CONFIG_NAME);
        // Default: `<dir>/.dbtl-baseline.json`.
        std::fs::write(&cfg_path, "lint: {}\n").unwrap();
        let pc = ProjectConfig::load(&cfg_path).unwrap();
        assert_eq!(
            pc.lint.baseline,
            dir.path().join(crate::lint::DEFAULT_BASELINE_NAME)
        );
        // A relative override joins the config dir; an absolute one stands.
        std::fs::write(&cfg_path, "lint:\n  baseline: baselines/frozen.json\n").unwrap();
        let pc = ProjectConfig::load(&cfg_path).unwrap();
        assert_eq!(
            pc.lint.baseline,
            dir.path().join("baselines").join("frozen.json")
        );
        // `from_yaml` alone keeps the raw relative path.
        let pc = parse("lint:\n  baseline: baselines/frozen.json\n").unwrap();
        assert_eq!(
            pc.lint.baseline,
            PathBuf::from("baselines").join("frozen.json")
        );
    }

    #[test]
    fn load_missing_file_defaults_and_malformed_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(PROJECT_CONFIG_NAME);
        assert_eq!(
            ProjectConfig::load(&missing).unwrap(),
            ProjectConfig::default()
        );
        std::fs::write(&missing, "lint: [nope]\n").unwrap();
        let err = ProjectConfig::load(&missing).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("invalid .dbtl.yml"), "{err}");
    }

    #[test]
    fn naming_prefixes_merge_per_layer() {
        let pc = parse("lint:\n  naming_prefixes:\n    staging: [stg_, base_]\n").unwrap();
        assert_eq!(
            pc.lint.naming_prefixes["staging"],
            vec!["stg_".to_string(), "base_".to_string()]
        );
        assert_eq!(pc.lint.naming_prefixes["intermediate"], vec!["int_"]);
        // An explicit empty list shuts the layer's convention off.
        let pc = parse("lint:\n  naming_prefixes:\n    marts: []\n").unwrap();
        assert!(pc.lint.naming_prefixes["marts"].is_empty());
    }

    // ---- check: ----

    #[test]
    fn check_gates_accept_the_bare_severity_and_the_object_form() {
        let pc = parse(
            r#"check:
  lint: true
  gates:
    coverage: {severity: error, min: 80}
    changed_hubs: warn
    affected_exposures: off
    breaking_columns: error
    blast_radius: {max: 25}
"#,
        )
        .unwrap();
        assert!(pc.check.lint);
        let cov = &pc.check.gates[&CheckGate::Coverage];
        assert_eq!(cov.severity, Some(Severity::Error));
        assert_eq!(cov.min, Some(80));
        // A bare severity lands as the setting; a gate set `off` resolves
        // to no severity.
        assert_eq!(
            pc.check.severity(CheckGate::ChangedHubs),
            Some(Severity::Warn)
        );
        assert_eq!(pc.check.severity(CheckGate::AffectedExposures), None);
        assert_eq!(
            pc.check.severity(CheckGate::BreakingColumns),
            Some(Severity::Error)
        );
        assert!(pc.check.explicit(CheckGate::BreakingColumns));
        // Params without `severity` enable a default-off gate at error —
        // the author configured the gate, so it runs (the same lift the
        // CLI param flags apply).
        let br = &pc.check.gates[&CheckGate::BlastRadius];
        assert_eq!(br.max, Some(25));
        assert_eq!(br.severity, Some(Severity::Error));
        assert_eq!(pc.check.max(CheckGate::BlastRadius), 25);
        // Unnamed gates resolve to the registry defaults — not listed in
        // `gates`, never "explicit".
        assert!(!pc.check.explicit(CheckGate::DocCoverage));
        assert_eq!(
            pc.check.severity(CheckGate::DocCoverage),
            CheckGate::DocCoverage.default_severity()
        );
    }

    #[test]
    fn check_gates_reject_unknown_ids_params_and_out_of_range_values() {
        let err = parse("check:\n  gates:\n    not_a_gate: warn\n").unwrap_err();
        assert!(
            err.contains("unknown check gate 'not_a_gate'") && err.contains("coverage"),
            "{err} names the id and the valid set"
        );
        let err = parse("check:\n  gates:\n    coverage: {mxn: 80}\n").unwrap_err();
        assert!(
            err.contains("unknown parameter 'mxn' for gate 'coverage'") && err.contains("min"),
            "{err}"
        );
        // A param on a gate that declares none.
        let err = parse("check:\n  gates:\n    affected_exposures: {min: 1}\n").unwrap_err();
        assert!(
            err.contains("unknown parameter 'min' for gate 'affected_exposures'"),
            "{err}"
        );
        // Out-of-range and wrong-shape values.
        for bad in [
            "check:\n  gates:\n    coverage: {min: 101}\n",
            "check:\n  gates:\n    coverage: {min: -1}\n",
            "check:\n  gates:\n    coverage: {min: \"eighty\"}\n",
            "check:\n  gates:\n    changed_hubs: {min_downstream: 0}\n",
            "check:\n  gates:\n    coverage: wrn\n",
            "check:\n  gates:\n    coverage: {severity: quiet}\n",
            "check:\n  gates:\n    coverage: [a, list]\n",
            "check:\n  gates:\n    docs_drift: {dir: [a, list]}\n",
            // An unknown key inside `check:` — the same typo policy `lint:`
            // applies.
            "check:\n  strict: true\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must fail to parse");
        }
    }

    #[test]
    fn check_lint_defaults_off_and_off_severity_disables_the_gate() {
        // `check:` absent → lint off, no gate entries.
        let pc = parse("fail_on: warn\n").unwrap();
        assert!(!pc.check.lint);
        assert!(pc.check.gates.is_empty());
        // `coverage: off` turns a default-on gate off — the entry is
        // explicit (it was named) but resolves no severity.
        let pc = parse("check:\n  gates:\n    coverage: off\n").unwrap();
        assert!(pc.check.explicit(CheckGate::Coverage));
        assert_eq!(pc.check.severity(CheckGate::Coverage), None);
        // `severity: off` inside the object form is the same.
        let pc = parse("check:\n  gates:\n    coverage: {severity: off, min: 50}\n").unwrap();
        assert_eq!(pc.check.severity(CheckGate::Coverage), None);
        assert_eq!(pc.check.min(CheckGate::Coverage), 50);
    }

    #[test]
    fn docs_drift_dir_resolves_against_the_config_dir_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join(PROJECT_CONFIG_NAME);
        std::fs::write(
            &cfg_path,
            "check:\n  gates:\n    docs_drift: {dir: generated-docs}\n",
        )
        .unwrap();
        let pc = ProjectConfig::load(&cfg_path).unwrap();
        assert_eq!(
            pc.check.docs_dir().unwrap(),
            dir.path().join("generated-docs").as_path()
        );
        // `from_yaml` alone keeps the raw relative path.
        let pc = parse("check:\n  gates:\n    docs_drift: {dir: generated-docs}\n").unwrap();
        assert_eq!(pc.check.docs_dir().unwrap(), Path::new("generated-docs"));
    }

    #[test]
    fn owner_meta_key_parses_and_rejects_blank() {
        assert_eq!(
            parse("owner_meta_key: team\n").unwrap().owner_meta_key,
            Some("team".to_string())
        );
        assert_eq!(parse("").unwrap().owner_meta_key, None);
        assert!(parse("owner_meta_key: \"\"\n").is_err());
        assert!(parse("owner_meta_key: [a, list]\n").is_err());
    }
}
