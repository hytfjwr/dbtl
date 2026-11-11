//! The project-local `.dbtl.yml` loader — the ONE place the file's
//! discovery rules and top-level shape live. The file is a namespace for
//! project-level settings (`fail_on`, `lint:`, `docs:` today; `check:` is
//! added by its own feature). It is deliberately NOT
//! `~/.config/dbtl/config.yml` — lint/gate policy is project-owned (a CI
//! gate must not depend on the developer's machine).
//!
//! Error policy: malformed YAML, unknown keys INSIDE `lint:`, unknown rule
//! names and undeclared rule parameters are hard errors — a config a CI
//! gate can't interpret must fail loudly. Unknown TOP-LEVEL keys are
//! warnings ([`ProjectConfig::warnings`]) instead, so the file can grow new
//! sections without breaking older binaries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::lint::{LintConfig, LintRule, ParamKind, Severity};

/// The conventional project-local config file name: `.dbtl.yml` next to
/// `dbt_project.yml` (or at the project root above `target/` in manifest
/// mode).
pub const PROJECT_CONFIG_NAME: &str = ".dbtl.yml";

/// `fail_on` — the minimum severity at which findings fail the command:
/// `error` (the default), `warn` (warnings also fail), `never`
/// (report-only). Read here and carried; the CLI decides where it applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
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
}

/// A parsed `.dbtl.yml`: the resolved `lint:`/`docs:` sections, the
/// top-level `fail_on`, and any non-fatal notes (unknown top-level keys are
/// WARNED, not errors — forward compatibility with sections added later).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectConfig {
    /// Top-level `fail_on:` value (`None` = the consumer's own default).
    pub fail_on: Option<FailOn>,
    /// The `lint:` section resolved over the shipped defaults.
    pub lint: LintConfig,
    /// `lint.baseline` was written in the file — a missing file at an
    /// explicit path warns, while a missing default stays silent.
    pub baseline_explicit: bool,
    /// The `docs:` section resolved over the shipped defaults.
    pub docs: DocsConfig,
    /// Non-fatal notes the caller surfaces (CLI: `warning: …` on stderr;
    /// TUI: the startup notice / reload warnings).
    pub warnings: Vec<String>,
}

impl ProjectConfig {
    /// Parse `.dbtl.yml` text: apply the `lint:`/`docs:` sections over the
    /// shipped defaults. Errors on malformed YAML, unknown keys inside a
    /// known section, unknown rule names inside `rules:` and undeclared
    /// rule parameters — a config a CI gate can't interpret must fail
    /// loudly, never silently lint with defaults. Unknown TOP-LEVEL keys
    /// land in `warnings`.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let file: ProjectFile =
            serde_norway::from_str(text).map_err(|e| format!("invalid .dbtl.yml: {e}"))?;
        let mut lint = LintConfig::default();
        for (name, spec) in file.lint.rules {
            let rule = LintRule::from_name(&name).ok_or_else(|| {
                format!(
                    "unknown lint rule '{name}' (valid rules: {})",
                    valid_rule_names()
                )
            })?;
            match spec {
                // `true` = enabled at the registry default — the same as an
                // absent entry.
                RuleSpec::Toggle(true) => {}
                RuleSpec::Toggle(false) => {
                    lint.severities.insert(rule, None);
                }
                RuleSpec::Settings(settings) => {
                    if let Some(sev) = settings.severity {
                        lint.severities.insert(rule, sev.as_effective());
                    }
                    if !settings.exclude.is_empty() {
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
                                        format!("{name}.{param} must be an integer >= {min}")
                                    })?;
                                lint.params.entry(rule).or_default().insert(param, v);
                            }
                            ParamKind::StringList { allowed, .. } => {
                                let items = value.as_sequence().ok_or_else(|| {
                                    format!("{name}.{param} must be a list of strings")
                                })?;
                                let mut list = Vec::with_capacity(items.len());
                                for item in items {
                                    let s = item.as_str().ok_or_else(|| {
                                        format!("{name}.{param} must be a list of strings")
                                    })?;
                                    if let Some(allowed) = allowed {
                                        if !allowed.contains(&s) {
                                            return Err(format!(
                                                "{name}.{param}: '{s}' is not a valid value (valid: {})",
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
        // Per-layer merge: a project overrides only the layers it lists; a
        // layer mapped to `[]` is unchecked (absent == unchecked anyway, but
        // an explicit empty list also SHUTS OFF a default layer).
        if let Some(prefixes) = file.lint.naming_prefixes {
            for (layer, p) in prefixes {
                lint.naming_prefixes.insert(layer, p);
            }
        }
        let baseline_explicit = file.lint.baseline.is_some();
        if let Some(baseline) = file.lint.baseline {
            lint.baseline = baseline;
        }
        let docs = DocsConfig {
            lineage_depth: file.docs.lineage_depth,
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
            docs,
            warnings,
        })
    }

    /// Load `.dbtl.yml` from `path`: missing file → the shipped defaults (a
    /// project without config still lints), malformed → `InvalidData` with
    /// the parse reason. A relative `lint.baseline` resolves against the
    /// file's directory here. Whether a malformed config is fatal or
    /// warn+fallback is the caller's policy — the CI surfaces (`check`,
    /// `docs`) treat it as an error, the TUI warns and falls back.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_yaml(&text)
                .map(|mut pc| {
                    if pc.lint.baseline.is_relative() {
                        if let Some(dir) = path.parent() {
                            pc.lint.baseline = dir.join(&pc.lint.baseline);
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
///   [`dag.rs`](crate::dag) uses for the staleness project-root), the
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

/// The `.dbtl.yml` top-level shape. `fail_on` and `lint` are consumed;
/// anything else collects into `other` and becomes a warning (never a hard
/// error — the file can grow project-level sections without breaking older
/// binaries).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProjectFile {
    fail_on: Option<FailOn>,
    lint: LintFile,
    docs: DocsFile,
    #[serde(flatten)]
    other: BTreeMap<String, serde_norway::Value>,
}

/// The `docs:` section wire shape. `deny_unknown_fields` — the same typo
/// policy as `lint:`: a silently-ignored `lineage_dpeth` would generate a
/// different doc set than the author intended.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DocsFile {
    /// `docs.lineage_depth` — `None` when unset; an invalid value fails the
    /// whole parse (a malformed config is fatal, never silently defaulted).
    lineage_depth: Option<LineageDepth>,
}

/// The `lint:` section wire shape. `deny_unknown_fields` makes a typo'd key
/// a hard error — a silently-ignored `model_fanout_threshld` would leave a
/// CI gate running the wrong rule set.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LintFile {
    /// rule name → `bool` | settings object. Unknown rule names are
    /// validated separately so the error can list the valid set.
    rules: BTreeMap<String, RuleSpec>,
    /// Compat flat keys — the same value `rules.<id>.threshold` sets; both
    /// spellings together are an error naming both keys.
    model_fanout_threshold: Option<usize>,
    source_fanout_threshold: Option<usize>,
    /// layer dir → allowed prefixes; merges per-layer over the defaults.
    naming_prefixes: Option<BTreeMap<String, Vec<String>>>,
    /// The frozen-violations file — resolved against this file's directory
    /// in [`ProjectConfig::load`].
    baseline: Option<PathBuf>,
}

/// `rules.<id>` accepts the compat `bool` (`false` = off, `true` = the
/// registry default severity) or the object form
/// (`{severity: error|warn|off, exclude: [...], <declared params>}`).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RuleSpec {
    Toggle(bool),
    Settings(RuleSettings),
}

/// The object form of `rules.<id>`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RuleSettings {
    severity: Option<SeveritySetting>,
    /// dbt-selector strings — findings whose `node_uid` matches the union
    /// are dropped. Wildcard names (`stg_*`), `path:` and `tag:` atoms work.
    exclude: Vec<String>,
    /// Rule-declared params (`threshold`, …) — validated against the
    /// registry's [`ParamSpec`](crate::lint::ParamSpec) at parse so an
    /// undeclared key errors naming the rule and its accepted params.
    #[serde(flatten)]
    params: BTreeMap<String, serde_norway::Value>,
}

/// The `severity:` wire value — `off` folds into "no severity" (disabled).
#[derive(Debug, Clone, Copy, Deserialize)]
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
            "{flat_key} and rules.{}.threshold set the same option; use only one",
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
            Some(Severity::Error),
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
            &["stg_legacy_*", "path:models/legacy", "tag:wip"]
        );
        assert_eq!(pc.lint.severity(LintRule::SourceFanout), None);
    }

    #[test]
    fn an_empty_rule_object_runs_at_the_default() {
        let pc = parse("lint:\n  rules:\n    model_fanout: {}\n").unwrap();
        assert_eq!(
            pc.lint.severity(LintRule::ModelFanout),
            Some(Severity::Error)
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
        let pc = parse("check:\n  strict: true\ntui: {}\nlint: {}\n").unwrap();
        assert_eq!(
            pc.warnings,
            vec![
                "ignoring unknown top-level key 'check' in .dbtl.yml".to_string(),
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
    fn unknown_rule_names_and_params_are_errors() {
        let err = parse("lint:\n  rules:\n    typo_rule: false\n").unwrap_err();
        assert!(
            err.contains("unknown lint rule 'typo_rule'") && err.contains("root_models"),
            "{err} names the rule and the valid set"
        );
        let err = parse("lint:\n  rules:\n    root_models: {severity: quiet}\n").unwrap_err();
        assert!(err.contains("invalid .dbtl.yml"), "{err}");
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
}
