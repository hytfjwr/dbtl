//! Per-project UI state persistence and the optional `config.yml` defaults.
//!
//! `PersistedState` is the serializable subset of `App` restored across runs:
//! bookmarks, the lineage lens, the box density, the model-list sort, and the
//! last selected model. It is saved as JSON under the XDG state dir (one file
//! per project, keyed by the project root path — the path resolution lives in
//! the `dbtl` binary, this module is pure data + serde).
//!
//! `Config` is the user-authored `~/.config/dbtl/config.yml`: default values
//! for the same prefs plus the colour theme. Precedence is
//! `config < saved state < explicit CLI flags` — each field is an `Option`, so
//! applying state over config only overrides what the state actually recorded.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::{build_model_list_grouped, Density, LineageLens, SortMode};

/// The `App` state that survives a restart (saved per project).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    /// Bookmarked `unique_id`s (the `Space`/`'` set).
    pub bookmarks: BTreeSet<String>,
    /// Lineage lens name (see [`lens_name`]); `None` = unset.
    pub lens: Option<String>,
    /// Lineage density name (see [`density_name`]); `None` = unset.
    pub density: Option<String>,
    /// Model-list sort name (see [`sort_name`]); `None` = unset.
    pub sort: Option<String>,
    /// The `unique_id` selected when the app exited; `None` = unset.
    pub last_selected: Option<String>,
}

/// User defaults from `~/.config/dbtl/config.yml` — every field optional, so a
/// missing file and an empty file behave identically (all `None`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default colour theme: a preset name or a theme under
    /// `~/.config/dbtl/themes/<name>.yml`. Lower precedence than
    /// `--theme`/`DBTL_THEME`.
    pub theme: Option<String>,
    /// Default lineage lens name.
    pub lens: Option<String>,
    /// Default lineage density name.
    pub density: Option<String>,
    /// Default model-list sort name.
    pub sort: Option<String>,
    /// Group the model list by this `meta` key's VALUE instead of the layer
    /// dirs (`group_by_meta: owner` → `owner=alice` groups, `(no owner)`
    /// last; unset = the default layer grouping). The `.` sort cycle still
    /// orders within each group.
    pub group_by_meta: Option<String>,
    /// The `meta` key the PR-impact pack collects as "affected owners"
    /// (default `owner`) — read by the `D` modal, its `e` export, and
    /// `dbtl impact`/`check`.
    pub owner_meta_key: Option<String>,
}

impl Config {
    /// Parse a `config.yml` document. A missing/empty file yields the
    /// all-`None` default; malformed YAML and unknown prefs surface as errors
    /// (the caller turns them into a startup warning, never a panic).
    pub fn from_yaml(text: &str) -> Result<Self, serde_norway::Error> {
        serde_norway::from_str(text)
    }

    /// Load `path`, returning the all-`None` default when it does not exist.
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_yaml(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// The config as a prefs layer for [`App::apply_prefs`]: only the pref
    /// fields map over — bookmarks/last_selected are state-file-only.
    pub fn as_prefs(&self) -> PersistedState {
        PersistedState {
            lens: self.lens.clone(),
            density: self.density.clone(),
            sort: self.sort.clone(),
            ..PersistedState::default()
        }
    }
}

/// Stable CLI/config names for [`LineageLens`] (the saved/config spelling —
/// kebab-case, matching the other string flags).
pub fn lens_name(lens: LineageLens) -> &'static str {
    match lens {
        LineageLens::Off => "off",
        LineageLens::Coverage => "coverage",
        LineageLens::DegreeHeat => "degree-heat",
        LineageLens::Layer => "layer",
        LineageLens::LayerViolation => "layer-violation",
        LineageLens::Diff => "diff",
        LineageLens::RunStatus => "run-status",
        LineageLens::RunTime => "run-time",
    }
}

/// Parse a [`lens_name`] string. `None` on unknown names — callers warn+skip.
pub fn lens_from_name(name: &str) -> Option<LineageLens> {
    Some(match name {
        "off" => LineageLens::Off,
        "coverage" => LineageLens::Coverage,
        "degree-heat" => LineageLens::DegreeHeat,
        "layer" => LineageLens::Layer,
        "layer-violation" => LineageLens::LayerViolation,
        "diff" => LineageLens::Diff,
        "run-status" => LineageLens::RunStatus,
        "run-time" => LineageLens::RunTime,
        _ => return None,
    })
}

/// Stable names for [`Density`].
pub fn density_name(density: Density) -> &'static str {
    match density {
        Density::Comfortable => "comfortable",
        Density::Compact => "compact",
    }
}

/// Parse a [`density_name`] string.
pub fn density_from_name(name: &str) -> Option<Density> {
    Some(match name {
        "comfortable" => Density::Comfortable,
        "compact" => Density::Compact,
        _ => return None,
    })
}

/// Stable names for [`SortMode`].
pub fn sort_name(sort: SortMode) -> &'static str {
    match sort {
        SortMode::Layer => "layer",
        SortMode::Downstream => "downstream",
        SortMode::Tests => "tests",
    }
}

/// Parse a [`sort_name`] string.
pub fn sort_from_name(name: &str) -> Option<SortMode> {
    Some(match name {
        "layer" => SortMode::Layer,
        "downstream" => SortMode::Downstream,
        "tests" => SortMode::Tests,
        _ => return None,
    })
}

impl App {
    /// Snapshot the persistable prefs for saving on exit.
    pub fn prefs_snapshot(&self) -> PersistedState {
        PersistedState {
            bookmarks: self.bookmarks.clone(),
            lens: Some(lens_name(self.ui_state.lens()).to_string()),
            density: Some(density_name(self.ui_state.density()).to_string()),
            sort: Some(sort_name(self.sort).to_string()),
            last_selected: self.selected_unique_id(),
        }
    }

    /// Apply a prefs/config layer: every `Some` field is applied, `None` keeps
    /// the current value — so `config → saved state` layering works by calling
    /// this twice. Unknown names are skipped (a typo'd config never breaks
    /// startup); bookmarked ids that no longer exist in the graph are dropped
    /// (same rule as `reload`).
    pub fn apply_prefs(&mut self, prefs: &PersistedState) {
        self.bookmarks.extend(
            prefs
                .bookmarks
                .iter()
                .filter(|uid| self.dag.nodes().contains_key(*uid))
                .cloned(),
        );
        if let Some(lens) = prefs.lens.as_deref().and_then(lens_from_name) {
            self.ui_state.set_lens(lens);
        }
        if let Some(density) = prefs.density.as_deref().and_then(density_from_name) {
            self.ui_state.set_density(density);
        }
        if let Some(sort) = prefs.sort.as_deref().and_then(sort_from_name) {
            if sort != self.sort {
                // Same rebuild-then-reselect dance as the SortCycle arm.
                self.sort = sort;
                let current = self.selected_unique_id();
                self.model_list =
                    build_model_list_grouped(&self.dag, sort, self.group_by_meta.as_deref());
                self.apply_list_filter();
                if let Some(uid) = &current {
                    self.select_by_unique_id(uid);
                }
            }
        }
        if let Some(uid) = &prefs.last_selected {
            self.select_by_unique_id(uid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/manifest.json"
    );

    fn fixture_app() -> App {
        let dag = crate::load_dag(FIXTURE).expect("fixture loads");
        App::new(dag, PathBuf::from(FIXTURE))
    }

    #[test]
    fn name_maps_round_trip() {
        for lens in [
            LineageLens::Off,
            LineageLens::Coverage,
            LineageLens::DegreeHeat,
            LineageLens::Layer,
            LineageLens::LayerViolation,
            LineageLens::Diff,
            LineageLens::RunStatus,
            LineageLens::RunTime,
        ] {
            assert_eq!(lens_from_name(lens_name(lens)), Some(lens));
        }
        for density in [Density::Comfortable, Density::Compact] {
            assert_eq!(density_from_name(density_name(density)), Some(density));
        }
        for sort in [SortMode::Layer, SortMode::Downstream, SortMode::Tests] {
            assert_eq!(sort_from_name(sort_name(sort)), Some(sort));
        }
        assert_eq!(lens_from_name("nope"), None);
        assert_eq!(density_from_name("nope"), None);
        assert_eq!(sort_from_name("nope"), None);
    }

    #[test]
    fn prefs_snapshot_then_apply_restores_state() {
        let mut a = fixture_app();
        a.bookmarks
            .insert("model.jaffle_finance.rpt_delivery_base_metrics".to_string());
        a.ui_state.set_lens(LineageLens::Coverage);
        a.ui_state.set_density(Density::Compact);
        a.sort = SortMode::Tests;
        a.model_list = build_model_list_grouped(&a.dag, a.sort, None);
        a.select_by_unique_id("model.jaffle_finance.rpt_delivery_base_metrics");

        let snap = a.prefs_snapshot();
        let mut b = fixture_app();
        b.apply_prefs(&snap);
        assert!(b
            .bookmarks
            .contains("model.jaffle_finance.rpt_delivery_base_metrics"));
        assert_eq!(b.ui_state.lens(), LineageLens::Coverage);
        assert_eq!(b.ui_state.density(), Density::Compact);
        assert_eq!(b.sort, SortMode::Tests);
        assert_eq!(
            b.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.rpt_delivery_base_metrics")
        );
    }

    #[test]
    fn apply_prefs_drops_bookmarks_and_selection_missing_from_dag() {
        let mut a = fixture_app();
        let prefs = PersistedState {
            bookmarks: [
                "model.jaffle_finance.rpt_delivery_base_metrics",
                "model.gone.nope",
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
            lens: None,
            density: None,
            sort: None,
            last_selected: Some("model.gone.nope".to_string()),
        };
        a.apply_prefs(&prefs);
        assert!(a
            .bookmarks
            .contains("model.jaffle_finance.rpt_delivery_base_metrics"));
        assert!(!a.bookmarks.contains("model.gone.nope"));
        assert_ne!(a.selected_unique_id().as_deref(), Some("model.gone.nope"));
    }

    #[test]
    fn config_from_yaml_parses_partial_and_empty() {
        let c = Config::from_yaml("theme: midnight\nlens: coverage\n").unwrap();
        assert_eq!(c.theme.as_deref(), Some("midnight"));
        assert_eq!(c.lens.as_deref(), Some("coverage"));
        assert_eq!(c.density, None);
        assert_eq!(c.group_by_meta, None);
        assert_eq!(c.owner_meta_key, None);
        // The meta prefs parse as plain strings.
        let m = Config::from_yaml("group_by_meta: owner\nowner_meta_key: team\n").unwrap();
        assert_eq!(m.group_by_meta.as_deref(), Some("owner"));
        assert_eq!(m.owner_meta_key.as_deref(), Some("team"));
        assert_eq!(Config::from_yaml("").unwrap(), Config::default());
        assert!(Config::from_yaml("lens: [unclosed").is_err());
    }

    #[test]
    fn persisted_state_json_round_trips() {
        let p = PersistedState {
            bookmarks: ["model.a.x"].iter().map(ToString::to_string).collect(),
            lens: Some("coverage".to_string()),
            density: None,
            sort: Some("tests".to_string()),
            last_selected: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PersistedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        // Unknown/absent keys deserialize as defaults — forward-compatible.
        let sparse: PersistedState = serde_json::from_str("{}").unwrap();
        assert_eq!(sparse, PersistedState::default());
    }
}
