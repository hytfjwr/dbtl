//! Issue 10's home screen data product — the Dag-aware snapshot the loop
//! builds once per `(generation, home_stamp, search)` key and hands the
//! renderer as plain data, keeping `RenderCtx` Dag-free.
//!
//! The section order is FIXED (§3: Resume → Attention → Bookmarks → Recent
//! → Actions — the search bar is separate chrome, not a section); empty
//! sections drop except `Actions`, which always carries the runnable set.
//! Every row resolves to a [`HomeItem`] — the flattened `selectable` list is
//! what `App::home_sel`, `j`/`k`, Enter, and the mouse's row hits index.

use std::collections::HashMap;
use std::rc::Rc;

use crate::action::{Action, Mode, SearchTarget, BINDINGS};
use crate::model_list::match_indices;
use crate::App;
use crate::RunStatus;

/// A selectable home row's semantics — what Enter (`HomeOpen`) / a row click
/// (`HomePick`) does with it. Deliberately tiny: everything that names a
/// model opens it as a tab (the §2/T2 dedupe lives in `open_node_as_tab`),
/// everything else is a `BINDINGS` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeItem {
    /// Opens `uid` as a model tab — activating the existing tab when one is
    /// already open (§2/T2, identical to `O` and palette `@`-new-tab).
    Model { uid: String },
    /// Dispatches a `home`-flagged [`BINDINGS`] action. `unavailable` is the
    /// shared [`App::unavailable_reason`](crate::App::unavailable_reason):
    /// `Some` draws the row dim and turns Enter into the reason toast.
    Command {
        action: Action,
        unavailable: Option<String>,
    },
}

/// A row's emphasis — the renderer maps these onto theme tones. `Bad` marks
/// the failure-class attention rows (run errors, lint), `Warn` the softer
/// ones (orphans, layer violations), `Dim` a greyed-out command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeTone {
    Normal,
    Bad,
    Warn,
    Dim,
}

/// One drawn row under a section: display strings only (`label` = the model
/// name / command help, `detail` = the right-hand reason/status/keys).
#[derive(Debug, Clone)]
pub struct HomeRow {
    pub item: HomeItem,
    pub label: String,
    pub detail: String,
    pub tone: HomeTone,
}

/// A fixed-order section: `title` renders the `[Attention]`-style header,
/// `rows` its items (all selectable — headers live on the section, never
/// inside `rows`, so `selectable` is a plain concatenation).
#[derive(Debug, Clone)]
pub struct HomeSection {
    pub title: &'static str,
    pub rows: Vec<HomeRow>,
}

/// The open `/`-search's live state — `Some` while `Mode::Search` carries
/// [`SearchTarget::Home`]. The result cursor (`match_idx` on the mode) is
/// read straight from `ctx.mode` at draw time, so it stays out of this
/// snapshot (it changes without touching the rows).
#[derive(Debug, Clone)]
pub struct HomeSearch {
    /// The live query (user-typed or history-recalled — display verbatim).
    pub query: String,
    /// `Some(i)` while the Up/Down history browse is live (the entry index
    /// into `App::search_history`, for the bar's `<history>` hint).
    pub hist: Option<usize>,
    /// The matched model rows — Enter opens `results[sel]`'s `uid` as a tab.
    /// An empty query lists EVERY model (same contract as palette `@`).
    pub results: Vec<HomeRow>,
}

/// The whole home body minus chrome — precomputed plain data. Rebuilt only
/// when its inputs change: `generation` covers the Dag/diff/run_results/lint
/// set, `home_stamp` covers `recent`/bookmarks/`home_resume` writes, and the
/// search sub-key covers the live query + history browse.
#[derive(Debug, Clone)]
pub struct HomeData {
    /// `metadata.project_name`-style display name (the same readout the tab
    /// bar's project zone shows).
    pub project: String,
    /// The health strip's text cells in fixed order: model count, coverage,
    /// run-results readout, diff readout, lint total. `-` cells for absent
    /// data sources keep the strip's shape stable.
    pub health: Vec<String>,
    /// `(tested, total)` on the shared `coverage_gap` base — feeds the
    /// `cov` gauge bar. `(0, 0)` = no testable resources.
    pub coverage: (usize, usize),
    /// The open `/`-search's bar + results, `None` while `Mode::Home`.
    pub search: Option<HomeSearch>,
    /// The fixed-order sections (see module doc). `selectable` is their rows
    /// concatenated — while `search` is `Some` it instead mirrors
    /// `search.results`' items (the search owns the cursor then).
    pub sections: Vec<HomeSection>,
    /// The flat selectable-item list `home_sel`/`HomePick` index into.
    pub selectable: Vec<HomeItem>,
}

/// The cache slot's identity — everything a rebuild depends on:
/// `(generation, home_stamp, search(query, hist))`.
pub(crate) type HomeKey = (u64, u64, Option<(String, Option<usize>)>);

impl App {
    /// The home screen's data product — memoized per [`HomeKey`]; the loop
    /// clones the `Rc` into `RenderCtx.home` while the home tab is active.
    /// Size-unaware by construction: every section emits its FULL row list
    /// and the renderer caps at the visible budget (`+N more`), same rule as
    /// the stats dashboard.
    pub fn home_data(&self) -> Rc<HomeData> {
        let key = self.home_key();
        if let Some((k, d)) = self.caches.home.borrow().as_ref() {
            if *k == key {
                return Rc::clone(d);
            }
        }
        let d = Rc::new(self.build_home_data());
        *self.caches.home.borrow_mut() = Some((key, Rc::clone(&d)));
        d
    }

    fn home_key(&self) -> HomeKey {
        let search = match &self.mode {
            Mode::Search(s) if s.target == SearchTarget::Home => Some((s.query.clone(), s.hist)),
            _ => None,
        };
        (self.generation, self.home_stamp, search)
    }

    /// `j`/`k` under `Mode::Home` — move `home_sel` over the flattened
    /// selectable rows, clamped at both ends (no wrap — same rule the list
    /// cursor follows).
    pub fn move_home_sel(&mut self, dir: crate::Direction) {
        let len = self.home_data().selectable.len();
        if len == 0 {
            return;
        }
        self.home_sel = match dir {
            crate::Direction::Down => (self.home_sel + 1).min(len - 1),
            crate::Direction::Up => self.home_sel.saturating_sub(1),
            _ => self.home_sel,
        };
    }

    /// The count a home-search's Up/Down (the `match_idx` move) clamps to —
    /// the result rows the query currently produces. Reading through
    /// `home_data` keeps the reducer and the renderer on one definition.
    pub fn home_search_len(&self) -> usize {
        self.home_data()
            .search
            .as_ref()
            .map(|s| s.results.len())
            .unwrap_or(0)
    }

    /// The `unique_id` a home-search Enter opens — the `match_idx`'d result
    /// row, when it carries a model item.
    pub fn home_search_pick(&self) -> Option<String> {
        let (query_sel, results) = match &self.mode {
            Mode::Search(s) if s.target == SearchTarget::Home => {
                (s.match_idx, &self.home_data().search)
            }
            _ => (0, &None),
        };
        let rows = &results.as_ref()?.results;
        let row = rows.get(query_sel.min(rows.len().saturating_sub(1)))?;
        match &row.item {
            HomeItem::Model { uid } => Some(uid.clone()),
            _ => None,
        }
    }

    // ---- the builder (one pass, fixed section order) ----

    fn build_home_data(&self) -> HomeData {
        let search = match &self.mode {
            Mode::Search(s) if s.target == SearchTarget::Home => Some(HomeSearch {
                query: s.query.clone(),
                hist: s.hist,
                results: self.home_search_rows(&s.query),
            }),
            _ => None,
        };

        // The stats dashboard's view — the SAME source the `S` modal reads,
        // so home's counts can never diverge from it.
        let stats = self.compute_stats_view();
        let name_to_uid: HashMap<&str, &str> = self
            .dag
            .nodes()
            .values()
            .filter(|n| n.resource_type == "model")
            .map(|n| (n.name.as_str(), n.unique_id.as_str()))
            .collect();

        let mut sections = Vec::new();
        let mut selectable = Vec::new();

        // ---- Attention is computed first: the health strip reads the same
        // counts (failures/violations/lint) instead of recomputing them. ----
        let attention = self.attention_rows(&stats, &name_to_uid);
        let (run_fails, lint_total) = attention.iter().fold((0usize, 0usize), |(f, l), r| {
            if r.detail.starts_with("run ") {
                (f + 1, l)
            } else if r.detail.starts_with("lint") {
                (f, l + 1)
            } else {
                (f, l)
            }
        });

        // Resume — the last model tab's root (§3 "前回のアクティブタブ").
        if let Some(row) = self
            .home_resume
            .as_deref()
            .and_then(|uid| self.model_row(uid, "previous tab", HomeTone::Normal))
        {
            selectable.push(row.item.clone());
            sections.push(HomeSection {
                title: "Resume",
                rows: vec![row],
            });
        }

        if !attention.is_empty() {
            selectable.extend(attention.iter().map(|r| r.item.clone()));
            sections.push(HomeSection {
                title: "Attention",
                rows: attention,
            });
        }

        // Bookmarks — `BTreeSet` iteration is already uid-sorted; names show
        // sorted-by-uid order (deterministic, the spec's only requirement).
        let bookmarks: Vec<HomeRow> = self
            .bookmarks
            .iter()
            .filter_map(|uid| self.model_row(uid, "", HomeTone::Normal))
            .collect();
        if !bookmarks.is_empty() {
            selectable.extend(bookmarks.iter().map(|r| r.item.clone()));
            sections.push(HomeSection {
                title: "Bookmarks",
                rows: bookmarks,
            });
        }

        // Recent — the Issue 03 MRU verbatim (newest first, already ≤10).
        let recent: Vec<HomeRow> = self
            .recent
            .iter()
            .filter_map(|uid| self.model_row(uid, "", HomeTone::Normal))
            .collect();
        if !recent.is_empty() {
            selectable.extend(recent.iter().map(|r| r.item.clone()));
            sections.push(HomeSection {
                title: "Recent",
                rows: recent,
            });
        }

        // Actions — the `home`-flagged BINDINGS rows, availability through
        // the shared `unavailable_reason` so a greyed row can never disagree
        // with what the key would do.
        let actions: Vec<HomeRow> = BINDINGS
            .iter()
            .filter(|b| b.home)
            .map(|b| {
                let reason = self.unavailable_reason(b.action);
                let (detail, tone) = match &reason {
                    Some(r) => (r.clone(), HomeTone::Dim),
                    None => (b.key_label(), HomeTone::Normal),
                };
                HomeRow {
                    item: HomeItem::Command {
                        action: b.action,
                        unavailable: reason,
                    },
                    label: b.help.to_string(),
                    detail,
                    tone,
                }
            })
            .collect();
        selectable.extend(actions.iter().map(|r| r.item.clone()));
        sections.push(HomeSection {
            title: "Actions",
            rows: actions,
        });

        // ---- the health strip: fixed-order cells, `-` for absent sources ----
        let models = self
            .dag
            .nodes()
            .values()
            .filter(|n| n.resource_type == "model")
            .count();
        let mut health = vec![format!("{models} models")];
        let coverage = self.coverage_summary();
        if coverage.1 > 0 {
            health.push(format!(
                "cov {}/{} {}%",
                coverage.0,
                coverage.1,
                coverage.0 * 100 / coverage.1
            ));
        } else {
            health.push("cov -".to_string());
        }
        health.push(match self.run_results() {
            Some(rr) => {
                let covered = rr.covered_in(&self.dag);
                let at = rr.generated_at.as_deref().unwrap_or("-");
                format!("run {covered} covered, {run_fails} failed, {at}")
            }
            None => "run -".to_string(),
        });
        health.push(match self.diff() {
            Some(d) => format!(
                "diff +{} -{} ~{} vs {}",
                d.added.len(),
                d.removed.len(),
                d.modified.len(),
                self.diff_label()
            ),
            None => "diff -".to_string(),
        });
        health.push(format!("lint {lint_total}"));

        HomeData {
            project: self.dag.project_name().unwrap_or("?").to_string(),
            health,
            coverage,
            search,
            sections,
            selectable,
        }
        .with_search_selectables()
    }

    /// A model row: name in `label`, the caller's reason in `detail`.
    /// `None` when `uid` doesn't resolve to a model (a stale `recent`/
    /// bookmark/resume entry — pruned sets can still carry dead ids briefly).
    fn model_row(&self, uid: &str, detail: &str, tone: HomeTone) -> Option<HomeRow> {
        let n = self.dag.get(uid)?;
        if n.resource_type != "model" {
            return None;
        }
        Some(HomeRow {
            item: HomeItem::Model {
                uid: uid.to_string(),
            },
            label: n.name.clone(),
            detail: detail.to_string(),
            tone,
        })
    }

    /// The §3 worklist, deterministic order: run failures → orphans → layer
    /// violations → lint findings; name-sorted inside each class. A model
    /// appearing in two classes keeps its FIRST (worst) row.
    fn attention_rows(
        &self,
        stats: &crate::StatsView,
        name_to_uid: &HashMap<&str, &str>,
    ) -> Vec<HomeRow> {
        let mut rows: Vec<(u8, HomeRow)> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        // 0 — failed last run (run_results.json present).
        if let Some(rr) = self.run_results() {
            let mut fails: Vec<(&str, &str, &RunStatus)> = self
                .dag
                .nodes()
                .values()
                .filter(|n| n.resource_type == "model")
                .filter_map(|n| {
                    rr.status(&n.unique_id)
                        .filter(|s| s.is_failure())
                        .map(|s| (n.name.as_str(), n.unique_id.as_str(), s))
                })
                .collect();
            fails.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));
            for (name, uid, status) in fails {
                seen.push(uid);
                let status = match status {
                    RunStatus::Error => "error",
                    RunStatus::Fail => "fail",
                    _ => "failed",
                };
                rows.push((
                    0,
                    HomeRow {
                        item: HomeItem::Model {
                            uid: uid.to_string(),
                        },
                        label: name.to_string(),
                        detail: format!("run {status}"),
                        tone: HomeTone::Bad,
                    },
                ));
            }
        }
        // 1 — orphans (the stats dashboard's exact list — names).
        for name in &stats.orphan_models {
            if let Some(uid) = name_to_uid.get(name.as_str()) {
                if seen.contains(uid) {
                    continue;
                }
                seen.push(uid);
                rows.push((
                    1,
                    HomeRow {
                        item: HomeItem::Model {
                            uid: (*uid).to_string(),
                        },
                        label: name.clone(),
                        detail: "orphan - no upstream or downstream".to_string(),
                        tone: HomeTone::Warn,
                    },
                ));
            }
        }
        // 2 — layer violations: the CHILD is the mislayered model (the tab
        // Enter opens); the detail names the violating edge.
        for (parent, child) in &stats.layer_violations {
            if let Some(uid) = name_to_uid.get(child.as_str()) {
                if seen.contains(uid) {
                    continue;
                }
                seen.push(uid);
                rows.push((
                    2,
                    HomeRow {
                        item: HomeItem::Model {
                            uid: (*uid).to_string(),
                        },
                        label: child.clone(),
                        detail: format!("layer violation: {parent} -> {child}"),
                        tone: HomeTone::Warn,
                    },
                ));
            }
        }
        // 3 — lint findings (rule name + node, stats order = LintRule::ALL).
        for (rule, names) in &stats.lint_findings {
            for name in names {
                if let Some(uid) = name_to_uid.get(name.as_str()) {
                    if seen.contains(uid) {
                        continue;
                    }
                    seen.push(uid);
                    rows.push((
                        3,
                        HomeRow {
                            item: HomeItem::Model {
                                uid: (*uid).to_string(),
                            },
                            label: name.clone(),
                            detail: format!("lint: {rule}"),
                            tone: HomeTone::Bad,
                        },
                    ));
                }
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.label.cmp(&b.1.label)));
        rows.into_iter().map(|(_, r)| r).collect()
    }

    /// The `/`-search's model rows — fuzzy name match over the UNFILTERED
    /// model list (home search reaches models a list filter hid), empty
    /// query = all models (palette `@`'s contract).
    fn home_search_rows(&self, query: &str) -> Vec<HomeRow> {
        let q = query.trim();
        self.base_list()
            .models
            .iter()
            .filter(|m| q.is_empty() || !match_indices(&m.name, q).is_empty())
            .map(|m| HomeRow {
                item: HomeItem::Model {
                    uid: m.unique_id.clone(),
                },
                label: m.name.clone(),
                detail: String::new(),
                tone: HomeTone::Normal,
            })
            .collect()
    }
}

impl HomeData {
    /// Fix the `selectable` mirror once `search` is decided: an open search
    /// owns the cursor, so the list becomes its result rows' items.
    fn with_search_selectables(mut self) -> Self {
        if let Some(s) = &self.search {
            self.selectable = s.results.iter().map(|r| r.item.clone()).collect();
        }
        self
    }
}
