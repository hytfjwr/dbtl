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

use crate::app::{App, LineageView, TabState};
use crate::{build_model_list_grouped, Action, Density, LineageLens, SortMode};

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
    /// The `unique_id` selected when the app exited; `None` = unset. Kept
    /// writing even after `tabs`/`active_tab` arrived — an older dbtl still
    /// reads it (the backward-compat field).
    pub last_selected: Option<String>,
    /// The open tab strip (the `tabs` key): ordered roots + per-tab view filters.
    /// Empty on files written before tabs existed — `apply_prefs` then falls
    /// back to `last_selected`, so an old state file restores unchanged.
    pub tabs: Vec<PersistedTab>,
    /// The active tab's index into `tabs`; `None` = unset (or — the home
    /// tab). Clamped into range on apply.
    pub active_tab: Option<usize>,
    /// Recently-rooted models, newest-first, max 10 — the home screen reads
    /// it; restored per project like the strip itself.
    pub recent: Vec<String>,
    /// The list pane's folded group keys (the `folded_groups` key): the `-` /
    /// header-click set. A set field like `bookmarks`, not an Option —
    /// absent = nothing folded, same as an empty set. Keys that no longer
    /// name a live group are dropped on apply (same rule as `reload`).
    pub folded_groups: BTreeSet<String>,
    /// The list pane's saved width in columns (columns, not
    /// ratios). `None` = unset.
    pub list_width: Option<u16>,
    /// The inspector pane's saved width in columns (saved ahead of the
    /// pane so `=` and an early drag already round-trip). `None` = unset.
    pub inspector_width: Option<u16>,
    /// The inspector visibility pref (`auto`/`on`/`off` — see
    /// [`InspectorPolicy::name`](crate::ui::InspectorPolicy::name)). `None` =
    /// unset. The resolved on/off is never persisted — it is the loop's
    /// width-aware answer, not a user choice.
    pub inspector: Option<String>,
    /// Whether the list pane was visible (the `Ctrl-b` toggle). `None` = unset.
    pub list_visible: Option<bool>,
    /// Whether the lineage minimap was on (the `M` toggle). `None` = unset.
    pub minimap: Option<bool>,
    /// The active theme's name (the `--theme`/`Ctrl-t` selection). `None` =
    /// unset. Applied by the binary's theme loader (saved < `--theme`, >
    /// `config.yml` in precedence), not by [`App::apply_prefs`]: the theme LIST is
    /// loaded there too, so an absent name degrades to the default.
    pub theme: Option<String>,
    /// Confirmed list-search queries, most-recent-first (max 20 — what the
    /// `/`-mode Up/Down recall browses). `None` = absent (apply keeps the
    /// current list — same "None keeps" contract every other field follows,
    /// so a sparse layer like `Config::as_prefs` can't wipe the recall lists).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_history: Option<Vec<String>>,
    /// Recently-run palette commands as `Action` names, most-recent-first
    /// (max 5). Names gone from `BINDINGS` are dropped on apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palette_recent: Option<Vec<String>>,
}

/// One persisted tab: its root and the rooted-view filter. The tab's
/// VIEW state persists (`view`) while its session state — cursor, back /
/// forward history, collapse expansions, scroll — deliberately does not: a
/// restored tab opens homed on its root, which is what "restore the tabs I
/// had" means across a restart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedTab {
    /// The tab's rooted `unique_id`.
    pub root: String,
    /// The tab's lineage direction/depth filter (`u`/`d`/`[`/`]`/`0`).
    pub view: LineageView,
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
    /// The sibling-collapse threshold: a lineage column stacking more than
    /// this many same-parent siblings contracts the tail into one `+M more`
    /// marker (default 8; `0` = never collapse). Config-only — it is a
    /// display policy, not a per-project pref, so the state file ignores it.
    pub lineage_collapse: Option<usize>,
    /// Default inspector visibility pref (`auto`/`on`/`off`) — the
    /// `I` cycle. Same Option-layering as the other pref names.
    pub inspector: Option<String>,
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
            inspector: self.inspector.clone(),
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
        LineageLens::Lint => "lint",
        LineageLens::Diff => "diff",
        LineageLens::RunStatus => "run-status",
        LineageLens::RunTime => "run-time",
        LineageLens::ColumnImpact => "column-impact",
        LineageLens::PrimaryKey => "primary-key",
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
        "lint" => LineageLens::Lint,
        "diff" => LineageLens::Diff,
        "run-status" => LineageLens::RunStatus,
        "run-time" => LineageLens::RunTime,
        "column-impact" => LineageLens::ColumnImpact,
        "primary-key" => LineageLens::PrimaryKey,
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
            // `last_selected` keeps being written — the compat field an older
            // dbtl reads (`tabs`/`active_tab` are the same truth, newer shape).
            // On home the active tab carries no root; the Resume row's uid is
            // the closest equivalent an older dbtl can land on.
            last_selected: self.selected_unique_id().or(self.home_resume.clone()),
            tabs: self
                .tabs()
                .iter()
                .filter_map(|t| {
                    t.root.as_deref().map(|root| PersistedTab {
                        root: root.to_string(),
                        view: t.lineage_view.clone(),
                    })
                })
                .collect(),
            // `active_tab` is a MODEL-tab index (home is never persisted):
            // `None` = home was active; `Some(n)` = the nth rooted tab — the
            // count of persistable tabs below the live index, so any
            // root-less gap can't skew it.
            active_tab: if self.home_active() {
                None
            } else {
                Some(
                    self.tabs()
                        .iter()
                        .take(self.active_tab())
                        .filter(|t| t.root.is_some())
                        .count(),
                )
            },
            recent: self.recent.clone(),
            folded_groups: self.folded_groups.clone(),
            list_width: Some(self.ui_state.list_width()),
            inspector_width: Some(self.ui_state.inspector_width()),
            inspector: Some(self.ui_state.inspector_policy().name().to_string()),
            list_visible: Some(self.ui_state.list_visible()),
            minimap: Some(self.ui_state.minimap_visible()),
            theme: Some(self.theme_name().to_string()),
            search_history: Some(self.search_history.clone()),
            palette_recent: Some(self.palette_recent.clone()),
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
        // The strip: saved tabs restore roots + view filters — a root
        // that vanished (or is no longer a model) drops its whole tab. When
        // every saved tab is dead — or the state file predates tabs — the
        // compat `last_selected` applies like before. `active_tab` clamps to
        // the surviving strip (`restore_tabs` enters the tab it lands on).
        let saved: Vec<TabState> = prefs
            .tabs
            .iter()
            .filter(|t| self.is_rootable(&t.root))
            .map(|t| TabState {
                root: Some(t.root.clone()),
                lineage_view: t.view.clone(),
                ..TabState::default()
            })
            .collect();
        if !saved.is_empty() {
            // `active_tab: None` = the file was written on home — land on
            // home again (the strip gets its fixed home tab re-seeded).
            self.restore_tabs(saved, prefs.active_tab);
            // The compat `last_selected` doubles as "the tab you were on" —
            // seed the Resume row whenever nothing better was recorded, so a
            // later `~` offers it even after a model-tab landing.
            if self.home_resume.is_none() {
                self.home_resume = prefs
                    .last_selected
                    .clone()
                    .filter(|uid| self.dag.get(uid).is_some());
            }
        } else if let Some(uid) = &prefs.last_selected {
            self.select_by_unique_id(uid);
        }
        // The recent-roots MRU (read by home, restored early): prune ids
        // that no longer resolve — same rule as bookmarks/tabs.
        self.recent = prefs
            .recent
            .iter()
            .filter(|uid| self.dag.get(uid).is_some())
            .cloned()
            .collect();
        self.recent.truncate(10);
        // The folded groups — same set-field layering as bookmarks: keys
        // that no longer name a live group drop (a restored fold against a
        // renamed/emptied group would pin a phantom), then
        // `apply_list_filter` materializes the fold view (the single writer
        // — the restore can't build `folded_base` by hand). A restored fold
        // holding the tab's root detaches the cursor the same way a live
        // `-` fold does.
        if !prefs.folded_groups.is_empty() {
            self.folded_groups.extend(
                prefs
                    .folded_groups
                    .iter()
                    .filter(|k| self.model_list.groups.iter().any(|g| &g.layer == *k))
                    .cloned(),
            );
            self.apply_list_filter();
        }
        // Pane/display prefs: same Option-layering as the rest —
        // `None` keeps the current (or config-defaulted) value. Out-of-range
        // widths are NOT rejected here: `pane_layout` clamps them for display,
        // so a stale 200-col write degrades to a capped pane, not an error.
        // `prefs.theme` is deliberately skipped — the binary's `load_themes`
        // applies it (the theme list only exists there; see the field's doc).
        if let Some(w) = prefs.list_width {
            self.ui_state.set_list_width(w);
        }
        if let Some(w) = prefs.inspector_width {
            self.ui_state.set_inspector_width(w);
        }
        if let Some(policy) = prefs
            .inspector
            .as_deref()
            .and_then(crate::ui::InspectorPolicy::from_name)
        {
            self.ui_state.set_inspector_policy(policy);
        }
        if let Some(visible) = prefs.list_visible {
            self.ui_state.set_list_visible(visible);
        }
        if let Some(minimap) = prefs.minimap {
            self.ui_state.set_minimap(minimap);
        }
        // The recall lists restore with the same normalization record-time
        // applies (dedup + cap + no blanks), so a hand-edited state file can't
        // inject shape the live path never produces. Palette names whose
        // action is gone from BINDINGS drop — the Recent section mirrors the
        // live keymap, never a stale command source. `None` = the layer says
        // nothing about the list: keep the current value like every Option
        // field above.
        if let Some(history) = &prefs.search_history {
            let mut seen = std::collections::HashSet::new();
            self.search_history = history
                .iter()
                .filter(|q| !q.trim().is_empty() && seen.insert((*q).clone()))
                .take(crate::app::palette::SEARCH_HISTORY_MAX)
                .cloned()
                .collect();
        }
        if let Some(recent) = &prefs.palette_recent {
            let mut seen = std::collections::HashSet::new();
            self.palette_recent = recent
                .iter()
                .filter(|n| {
                    crate::action::palette_binding_named(n)
                        .is_some_and(|b| !matches!(b.action, Action::Quit))
                        && seen.insert((*n).clone())
                })
                .take(crate::app::palette::PALETTE_RECENT_MAX)
                .cloned()
                .collect();
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
        let mut a = App::new(dag, PathBuf::from(FIXTURE));
        // Same pin as `app::tests::app` — the fixture's hubs exceed the
        // collapse default, and these tests predate the display transform.
        a.set_collapse_threshold(0);
        a
    }

    #[test]
    fn name_maps_round_trip() {
        for lens in [
            LineageLens::Off,
            LineageLens::Coverage,
            LineageLens::DegreeHeat,
            LineageLens::Layer,
            LineageLens::LayerViolation,
            LineageLens::Lint,
            LineageLens::Diff,
            LineageLens::RunStatus,
            LineageLens::RunTime,
            LineageLens::ColumnImpact,
            LineageLens::PrimaryKey,
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
        // The pane/display prefs: set non-defaults so the round-trip
        // proves they apply, not just that the defaults coincide.
        a.ui_state.set_list_width(50);
        a.ui_state.set_inspector_width(60);
        a.ui_state.toggle_minimap();
        a.search_history = vec!["?order".to_string(), "stg_".to_string()];
        a.palette_recent = vec!["ToggleMinimap".to_string()];

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
        assert_eq!(b.ui_state.list_width(), 50);
        assert_eq!(b.ui_state.inspector_width(), 60);
        assert!(b.ui_state.minimap_visible(), "minimap pref restored");
        assert!(b.ui_state.list_visible(), "list stays visible");
        assert_eq!(snap.theme.as_deref(), Some(a.theme_name()));
        // The recall lists ride the same snapshot/apply round-trip.
        assert_eq!(
            b.search_history,
            vec!["?order".to_string(), "stg_".to_string()]
        );
        assert_eq!(b.palette_recent, vec!["ToggleMinimap".to_string()]);
    }

    #[test]
    fn apply_prefs_restores_a_hidden_list_and_minimap() {
        // The Ctrl-b/M result persisted across a restart — the toggles used to
        // be session-only. `apply_prefs` on the snapshot must
        // reproduce the hidden list + minimap-on combination.
        let mut a = fixture_app();
        a.ui_state.toggle_list_pane(); // hide the list
        a.ui_state.toggle_minimap();
        let snap = a.prefs_snapshot();

        let mut b = fixture_app();
        b.apply_prefs(&snap);
        assert!(!b.ui_state.list_visible(), "hidden list restores hidden");
        assert!(b.ui_state.minimap_visible());
        // Restoring a hidden list must pin focus off it (the toggle's rule).
        assert_eq!(b.ui_state.focus(), crate::ui::Focus::RightPane);
    }

    #[test]
    fn apply_prefs_normalizes_the_recall_lists() {
        // The restore path applies the same dedup/cap/no-blank normalization
        // record-time uses, and drops palette names whose action left
        // BINDINGS — a hand-edited state file can't inject stale commands.
        let mut a = fixture_app();
        let prefs = PersistedState {
            bookmarks: Default::default(),
            lens: None,
            density: None,
            sort: None,
            last_selected: None,
            search_history: Some(
                (0..30)
                    .map(|i| format!("q{i:02}"))
                    .chain(["  ".to_string(), "q00".to_string()])
                    .collect(),
            ),
            palette_recent: Some(
                [
                    "ToggleMinimap",
                    "NoSuchAction",  // stale: not an Action in BINDINGS
                    "ToggleMinimap", // dup folds into the first occurrence
                    "Quit",          // record-time skips it → restore drops it too
                    "ThemeCycle",
                    "StatsOpen",
                    "DbtParse",
                    "Reload",
                ]
                .iter()
                .map(ToString::to_string)
                .collect(),
            ),
            ..PersistedState::default()
        };
        a.apply_prefs(&prefs);
        assert_eq!(a.search_history.len(), 20, "search history caps at 20");
        assert_eq!(a.search_history[0], "q00");
        assert!(
            a.search_history.iter().all(|q| !q.trim().is_empty()),
            "blanks dropped"
        );
        assert_eq!(
            a.palette_recent,
            vec![
                "ToggleMinimap".to_string(),
                "ThemeCycle".to_string(),
                "StatsOpen".to_string(),
                "DbtParse".to_string(),
                "Reload".to_string(),
            ],
            "stale name + Quit dropped, dup folded, cap at 5 — order preserved"
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
            last_selected: Some("model.gone.nope".to_string()),
            ..PersistedState::default()
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
    fn prefs_snapshot_then_apply_restores_tabs() {
        // The strip round-trips — roots + per-tab view filters +
        // active index + the recent-roots MRU — while the compat
        // `last_selected` keeps mirroring the ACTIVE tab's root.
        let mut a = fixture_app();
        let m1 = "model.jaffle_finance.rpt_delivery_base_metrics".to_string();
        a.tab_mut().lineage_view.upstream = false; // the seeded tab carries a non-default view
        a.open_node_as_tab(&m1);
        let snap = a.prefs_snapshot();
        assert_eq!(snap.tabs.len(), 2);
        assert_eq!(snap.active_tab, Some(1));
        assert_eq!(
            snap.last_selected.as_deref(),
            Some(m1.as_str()),
            "last_selected mirrors the active tab for older dbtl"
        );
        assert_eq!(snap.recent.first().map(String::as_str), Some(m1.as_str()));

        let mut b = fixture_app();
        b.apply_prefs(&snap);
        assert_eq!(b.tab_count(), 3, "home + the two restored model tabs");
        assert_eq!(b.active_tab(), 2);
        assert_eq!(b.selected_unique_id().as_deref(), Some(m1.as_str()));
        assert!(b.tabs()[0].home, "index 0 is the re-seeded home tab");
        assert!(!b.tabs()[1].lineage_view.upstream, "tab 1's view restored");
        assert!(b.tabs()[2].lineage_view.upstream, "tab 2 kept the default");
        assert_eq!(b.recent.first().map(String::as_str), Some(m1.as_str()));
        // Session state deliberately does NOT restore — the tab re-homes on
        // its root (cursor/history/scroll start fresh).
        assert!(b.tabs()[1].back.is_empty() && b.tabs()[1].lineage_cursor.is_none());
    }

    #[test]
    fn apply_prefs_drops_dead_tab_roots_and_clamps_active() {
        let mut a = fixture_app();
        let prefs = PersistedState {
            tabs: vec![
                PersistedTab {
                    root: "model.gone.nope".to_string(), // not in the Dag
                    ..PersistedTab::default()
                },
                PersistedTab {
                    root: "source.jaffle_finance.dev_lake_jaffle_payment.suppliers".to_string(), // in the Dag but not a model
                    ..PersistedTab::default()
                },
                PersistedTab {
                    root: "model.jaffle_finance.rpt_delivery_base_metrics".to_string(),
                    ..PersistedTab::default()
                },
            ],
            active_tab: Some(9), // out of range even before the drop
            ..PersistedState::default()
        };
        a.apply_prefs(&prefs);
        assert_eq!(
            a.tab_count(),
            2,
            "dead roots drop their whole tab; home stays"
        );
        assert_eq!(a.active_tab(), 1, "active clamps to the survivors");
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.rpt_delivery_base_metrics")
        );
    }

    #[test]
    fn apply_prefs_falls_back_to_last_selected_when_no_tabs_survive() {
        let mut a = fixture_app();
        // A pre-03 state file (empty `tabs`) — or a strip whose roots all
        // died — restores through the compat `last_selected` like before.
        let prefs = PersistedState {
            last_selected: Some("model.jaffle_finance.rpt_delivery_base_metrics".to_string()),
            ..PersistedState::default()
        };
        a.apply_prefs(&prefs);
        assert_eq!(a.tab_count(), 2, "home + the re-rooted seeded tab");
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.rpt_delivery_base_metrics")
        );
        // Every-saved-tab-dead takes the same fallback.
        let prefs = PersistedState {
            tabs: vec![PersistedTab {
                root: "model.gone.nope".to_string(),
                ..PersistedTab::default()
            }],
            last_selected: Some("model.jaffle_finance.rpt_delivery_base_metrics".to_string()),
            ..PersistedState::default()
        };
        let mut b = fixture_app();
        b.apply_prefs(&prefs);
        assert_eq!(b.tab_count(), 2, "home + the re-rooted seeded tab");
        assert_eq!(
            b.selected_unique_id().as_deref(),
            Some("model.jaffle_finance.rpt_delivery_base_metrics")
        );
    }

    #[test]
    fn home_landing_persists_none_active_tab_and_resume_seeds() {
        // A file written on the home tab has `active_tab: None`
        // (home is never serialized — it re-seeds on apply) and lands back
        // on home. `last_selected` mirrors the Resume uid so an older dbtl
        // can still land on a model.
        let mut a = fixture_app();
        a.open_node_as_tab("model.jaffle_finance.rpt_delivery_base_metrics");
        a.enter_home();
        let snap = a.prefs_snapshot();
        assert_eq!(snap.active_tab, None, "home persists as None");
        assert!(
            snap.tabs.iter().all(|t| t.root.starts_with("model.")),
            "the home tab never serializes — every row is a model root: {:?}",
            snap.tabs
        );
        assert_eq!(
            snap.last_selected.as_deref(),
            a.home_resume.as_deref(),
            "last_selected mirrors the Resume uid"
        );
        assert_eq!(
            snap.recent.first().map(String::as_str),
            Some("model.jaffle_finance.rpt_delivery_base_metrics"),
            "the MRU serializes with the strip"
        );

        let mut b = fixture_app();
        b.apply_prefs(&snap);
        assert!(b.home_active(), "a home-written file lands back on home");
        assert_eq!(
            b.home_resume.as_deref(),
            Some("model.jaffle_finance.rpt_delivery_base_metrics"),
            "the Resume row restores from last_selected"
        );
        assert!(matches!(b.mode, crate::action::Mode::Home));
    }

    #[test]
    fn apply_prefs_prunes_recent_to_resolvable_models() {
        let mut a = fixture_app();
        let prefs = PersistedState {
            recent: (0..15)
                .map(|i| {
                    if i == 0 {
                        "model.jaffle_finance.rpt_delivery_base_metrics".to_string()
                    } else {
                        format!("model.gone.m{i}")
                    }
                })
                .collect(),
            ..PersistedState::default()
        };
        a.apply_prefs(&prefs);
        assert_eq!(
            a.recent,
            vec!["model.jaffle_finance.rpt_delivery_base_metrics".to_string()],
            "dead ids prune from the MRU"
        );
    }

    #[test]
    fn persisted_state_json_round_trips() {
        let p = PersistedState {
            bookmarks: ["model.a.x"].iter().map(ToString::to_string).collect(),
            lens: Some("coverage".to_string()),
            density: None,
            sort: Some("tests".to_string()),
            last_selected: None,
            tabs: vec![PersistedTab {
                root: "model.a.x".to_string(),
                view: crate::app::LineageView {
                    upstream: false,
                    ..crate::app::LineageView::default()
                },
            }],
            active_tab: Some(0),
            recent: vec!["model.a.x".to_string()],
            folded_groups: ["staging"].iter().map(ToString::to_string).collect(),
            list_width: Some(50),
            inspector_width: Some(60),
            inspector: Some("on".to_string()),
            list_visible: Some(false),
            minimap: Some(true),
            theme: Some("midnight".to_string()),
            search_history: Some(vec!["?order".to_string()]),
            palette_recent: Some(vec!["ToggleMinimap".to_string()]),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PersistedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        // Unknown/absent keys deserialize as defaults — forward-compatible,
        // and an OLD state file (no pane/display fields) still loads.
        let sparse: PersistedState = serde_json::from_str("{}").unwrap();
        assert_eq!(sparse, PersistedState::default());
        let old: PersistedState =
            serde_json::from_str(r#"{"bookmarks": [], "lens": "coverage", "sort": "layer"}"#)
                .unwrap();
        assert_eq!(old.list_width, None);
        assert_eq!(old.list_visible, None);
        assert_eq!(old.minimap, None);
        assert_eq!(old.theme, None);
    }
}
