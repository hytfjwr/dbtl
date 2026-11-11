//! The home screen data product — the Dag-aware snapshot the loop
//! builds once per `(generation, home_stamp, search)` key and hands the
//! renderer as plain data, keeping `RenderCtx` Dag-free.
//!
//! The section order is FIXED (Resume → Recent → Bookmarks → Health →
//! Last run → Diff → Attention → Actions — the search bar is chrome, not a
//! section, and a fixed `Tip` line closes the plan); empty sections drop
//! except `Actions` (always carries the runnable set) and `Health` (always
//! shows the coverage/counts). Item rows resolve to a [`HomeItem`] — the
//! flattened `selectable` list is what `App::home_sel`, `j`/`k`, Enter, and
//! the mouse's row hits index; the middle sections' `info` lines are
//! content rows, never items.

use std::collections::HashMap;
use std::rc::Rc;

use super::DataSource;
use crate::action::{Action, Mode, SearchTarget, BINDINGS};
use crate::model_list::match_indices;
use crate::App;
use crate::RunStatus;

/// The per-section cap — `Recent`, `Bookmarks`, and `Attention` each show
/// at most this many rows (the overflow reads `+N more`).
const SECTION_CAP: usize = 5;

/// A selectable home row's semantics — what Enter (`HomeOpen`) / a row click
/// (`HomePick`) does with it. Deliberately tiny: everything that names a
/// model opens it as a tab (the activate-or-open dedupe lives in
/// `open_node_as_tab`),
/// everything else is a `BINDINGS` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeItem {
    /// Opens `uid` as a model tab — activating the existing tab when one is
    /// already open (the activate-or-open rule, identical to `O` and
    /// palette `@`-new-tab).
    Model { uid: String },
    /// Dispatches a `home`-flagged [`BINDINGS`] action — the builder already
    /// filtered the unavailable ones out (unrunnable rows never render),
    /// so the row always runs.
    Command { action: Action },
}

/// A row's emphasis — the renderer maps these onto theme tones. `Bad` marks
/// the failure-class attention rows (run errors, lint), `Warn` the softer
/// ones (orphans, layer violations, untested models).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeTone {
    Normal,
    Bad,
    Warn,
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
/// inside `rows`, so `selectable` is a plain concatenation). `tail` is the
/// cap's hidden count — the plan draws it as a non-selectable `+N more`
/// line after the rows. `info` carries the non-selectable content lines
/// (Health / Last run / Diff — content rows, not items).
#[derive(Debug, Clone)]
pub struct HomeSection {
    pub title: &'static str,
    pub info: Vec<HomeInfo>,
    pub rows: Vec<HomeRow>,
    pub tail: Option<usize>,
}

/// One non-selectable info line under a section header — the middle rows
/// (Health / Last run / Diff) carry content, not items. Indexed through
/// [`HomeData::info_flat`] so [`crate::ui::HomeLine`] stays `Copy`.
#[derive(Debug, Clone)]
pub enum HomeInfo {
    /// `coverage [gauge] {pct}% ({tested}/{total})   {tail}` — the gauge is
    /// styled spaces (the no-gauge-glyphs rule), `tail` the
    /// `lint N   layer violations N   orphans N` counts.
    Gauge {
        tested: usize,
        total: usize,
        tail: String,
    },
    /// `{x} {error} error   {warn-mark} {warn} warn   {ok-mark} {ok}
    /// success   @{at}` — marks through Chrome (`run_fail`/`run_warn`/
    /// `run_ok`, so ASCII mode swaps `x`/`!`/`v`); `other`
    /// (skipped/unknown) appends `~ {n} other` only when non-zero.
    Run {
        error: usize,
        warn: usize,
        ok: usize,
        other: usize,
        at: String,
    },
    /// A plain dim line — `+{a} ~{m} -{r}   vs {label}` for the Diff row.
    Text(String),
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
    /// The data-source readout — `manifest {path}` / `source {dir}`.
    pub source: String,
    /// `PackageScope::tag()` — `root` / `+pkgs`.
    pub scope: &'static str,
    /// Every section's [`HomeSection::info`] lines, flattened in plan order
    /// — `HomeLine::Info(i)` indexes here, so the line plan stays `Copy`.
    pub info_flat: Vec<HomeInfo>,
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
    /// The home screen's data product — memoized per `HomeKey`; the loop
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
        // so home's counts can never diverge from it (the numbers are never
        // recomputed separately).
        let stats = self.compute_stats_view();
        // name→uid for the stats-name lists; uid-sorted insertion keeps the
        // winner deterministic when `--include-packages` carries same-named
        // models (`HashMap` iteration order is randomized).
        let mut name_pairs: Vec<(&str, &str)> = self
            .dag
            .nodes()
            .values()
            .filter(|n| n.resource_type == "model")
            .map(|n| (n.unique_id.as_str(), n.name.as_str()))
            .collect();
        name_pairs.sort();
        let name_to_uid: HashMap<&str, &str> = name_pairs
            .into_iter()
            .map(|(uid, name)| (name, uid))
            .collect();

        let mut sections = Vec::new();
        let mut selectable = Vec::new();
        let push_section = |sections: &mut Vec<HomeSection>,
                            selectable: &mut Vec<HomeItem>,
                            title: &'static str,
                            rows: Vec<HomeRow>,
                            tail: Option<usize>| {
            if rows.is_empty() {
                return;
            }
            selectable.extend(rows.iter().map(|r| r.item.clone()));
            sections.push(HomeSection {
                title,
                info: Vec::new(),
                rows,
                tail,
            });
        };
        // The info rows (Health / Last run / Diff) are sections whose body
        // is content, not items — they carry `info` instead of `rows`.
        let push_info = |sections: &mut Vec<HomeSection>,
                         info_flat: &mut Vec<HomeInfo>,
                         title: &'static str,
                         info: Vec<HomeInfo>| {
            if info.is_empty() {
                return;
            }
            info_flat.extend(info.iter().cloned());
            sections.push(HomeSection {
                title,
                info,
                rows: Vec::new(),
                tail: None,
            });
        };
        let mut info_flat = Vec::new();

        // ---- The worklist (the section sits between Diff and Actions) ----
        let (attention, att_tail) = self.attention_rows(&stats, &name_to_uid);

        // Resume — the last model tab's root.
        if let Some(row) = self
            .home_resume
            .as_deref()
            .and_then(|uid| self.model_row(uid, "previous tab", HomeTone::Normal))
        {
            selectable.push(row.item.clone());
            sections.push(HomeSection {
                title: "Resume",
                info: Vec::new(),
                rows: vec![row],
                tail: None,
            });
        }

        // Recent — the MRU list verbatim (newest first), capped at 5
        // of its 10 entries (the MRU drawer/palette still sees them all).
        let recent: Vec<HomeRow> = self
            .recent
            .iter()
            .take(SECTION_CAP)
            .filter_map(|uid| self.model_row(uid, "", HomeTone::Normal))
            .collect();
        push_section(&mut sections, &mut selectable, "Recent", recent, None);

        // Bookmarks — `BTreeSet` iteration is already uid-sorted (the
        // deterministic-ordering rule); capped at 5, the overflow reads
        // `+N more`.
        let bookmarks: Vec<HomeRow> = self
            .bookmarks
            .iter()
            .filter_map(|uid| self.model_row(uid, "", HomeTone::Normal))
            .collect();
        let (bookmarks, extra) = cap_rows(bookmarks);
        push_section(
            &mut sections,
            &mut selectable,
            "Bookmarks",
            bookmarks,
            extra,
        );

        // Health — the middle info row, ALWAYS shown: the coverage gauge +
        // lint / layer-violation / orphan counts straight from `stats` (the
        // `S` dashboard's own numbers, never recomputed).
        let coverage = self.coverage_summary();
        let lint_total: usize = stats
            .lint_findings
            .iter()
            .map(|(_, names)| names.len())
            .sum();
        push_info(
            &mut sections,
            &mut info_flat,
            "Health",
            vec![HomeInfo::Gauge {
                tested: coverage.0,
                total: coverage.1,
                tail: format!(
                    "lint {lint_total}   layer violations {}   orphans {}",
                    stats.layer_violations.len(),
                    stats.orphan_models.len()
                ),
            }],
        );

        // Last run — per-status counts over this Dag's covered nodes + the
        // artifact's timestamp (the row drops when no run_results loaded).
        if let Some(rr) = self.run_results() {
            let (mut error, mut warn, mut ok, mut other) = (0, 0, 0, 0);
            for uid in self.dag.nodes().keys() {
                match rr.status(uid) {
                    Some(crate::RunStatus::Error | crate::RunStatus::Fail) => error += 1,
                    Some(crate::RunStatus::Warn) => warn += 1,
                    Some(crate::RunStatus::Success) => ok += 1,
                    Some(_) => other += 1,
                    None => {}
                }
            }
            push_info(
                &mut sections,
                &mut info_flat,
                "Last run",
                vec![HomeInfo::Run {
                    error,
                    warn,
                    ok,
                    other,
                    at: rr.generated_at.clone().unwrap_or_else(|| "-".to_string()),
                }],
            );
        }

        // Diff — `+added ~modified -removed  vs {baseline}` (the row drops
        // when no baseline loaded).
        if let Some(d) = self.diff() {
            push_info(
                &mut sections,
                &mut info_flat,
                "Diff",
                vec![HomeInfo::Text(format!(
                    "+{} ~{} -{}   vs {}",
                    d.added.len(),
                    d.modified.len(),
                    d.removed.len(),
                    self.diff_label()
                ))],
            );
        }

        // Attention — the worklist (computed above); capped at 5, the
        // overflow reads `+N more` (`S` shows the rest).
        push_section(
            &mut sections,
            &mut selectable,
            "Attention",
            attention,
            att_tail,
        );

        // Actions — the `home`-flagged BINDINGS rows minus the unavailable
        // ones (unrunnable rows never render — a baseline-less `D` is dropped,
        // never greyed, so a drawn row always runs).
        let actions: Vec<HomeRow> = BINDINGS
            .iter()
            .filter(|b| b.home)
            .filter(|b| self.unavailable_reason(b.action).is_none())
            .map(|b| {
                // The label prefers the Home-mode binding's own help when
                // the action has one (`/` reads "search models" here, not
                // the Selection row's "search (list filter / lineage jump)").
                let help = BINDINGS
                    .iter()
                    .find(|h| h.mode == crate::ModeKind::Home && h.action == b.action)
                    .map(|h| h.help)
                    .unwrap_or(b.help);
                HomeRow {
                    item: HomeItem::Command { action: b.action },
                    label: help.to_string(),
                    detail: b.key_label(),
                    tone: HomeTone::Normal,
                }
            })
            .collect();
        selectable.extend(actions.iter().map(|r| r.item.clone()));
        sections.push(HomeSection {
            title: "Actions",
            info: Vec::new(),
            rows: actions,
            tail: None,
        });

        HomeData {
            project: self.dag.project_name().unwrap_or("?").to_string(),
            source: match &self.source {
                DataSource::Manifest(p) => format!("manifest {}", p.display()),
                DataSource::Project(d) => format!("source {}", d.display()),
            },
            scope: self.scope().tag(),
            info_flat,
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

    /// The worklist — returns `(rows, tail)`: `rows` are the capped
    /// selectable entries and `tail` the overflow count the plan draws as
    /// `+N more`.
    ///
    /// Class order: last-run error/fail (name order) → models with no
    /// tests sorted by downstream count desc → then the softer classes
    /// (orphans → layer violations → lint findings). A model appearing in
    /// two classes keeps its FIRST (worst) row.
    fn attention_rows(
        &self,
        stats: &crate::StatsView,
        name_to_uid: &HashMap<&str, &str>,
    ) -> (Vec<HomeRow>, Option<usize>) {
        let mut rows: Vec<(u8, HomeRow)> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        let model_row = |uid: &str, label: &str, detail: String, tone: HomeTone| HomeRow {
            item: HomeItem::Model {
                uid: uid.to_string(),
            },
            label: label.to_string(),
            detail,
            tone,
        };
        // 0 — failed last run (run_results.json present); name-sorted.
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
                    model_row(uid, name, format!("run {status}"), HomeTone::Bad),
                ));
            }
        }
        // 1 — models with no tests, most-depended-on first (the second
        // class): `coverage_gap` is the same predicate the Coverage lens
        // and the stats gauge read. Ties break on the name.
        let mut untested: Vec<(&str, &str, usize)> = self
            .dag
            .nodes()
            .values()
            .filter(|n| {
                n.resource_type == "model"
                    && crate::coverage_gap(n)
                    && !seen.contains(&n.unique_id.as_str())
            })
            .map(|n| {
                (
                    n.name.as_str(),
                    n.unique_id.as_str(),
                    self.dag.downstream(&n.unique_id).len(),
                )
            })
            .collect();
        untested.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)).then(a.1.cmp(b.1)));
        for (name, uid, down) in untested {
            seen.push(uid);
            rows.push((
                1,
                model_row(
                    uid,
                    name,
                    format!("untested ({down} downstream)"),
                    HomeTone::Warn,
                ),
            ));
        }
        // 2 — orphans (the stats dashboard's exact list — names).
        for name in &stats.orphan_models {
            if let Some(uid) = name_to_uid.get(name.as_str()) {
                if seen.contains(uid) {
                    continue;
                }
                seen.push(uid);
                rows.push((
                    2,
                    model_row(
                        uid,
                        name,
                        "orphan - no upstream or downstream".to_string(),
                        HomeTone::Warn,
                    ),
                ));
            }
        }
        // 3 — layer violations: the CHILD is the mislayered model (the tab
        // Enter opens); the detail names the violating edge.
        for (parent, child) in &stats.layer_violations {
            if let Some(uid) = name_to_uid.get(child.as_str()) {
                if seen.contains(uid) {
                    continue;
                }
                seen.push(uid);
                rows.push((
                    3,
                    model_row(
                        uid,
                        child,
                        format!("layer violation: {parent} -> {child}"),
                        HomeTone::Warn,
                    ),
                ));
            }
        }
        // 4 — lint findings (rule name + node, stats order = LintRule::ALL).
        for (rule, names) in &stats.lint_findings {
            for name in names {
                if let Some(uid) = name_to_uid.get(name.as_str()) {
                    if seen.contains(uid) {
                        continue;
                    }
                    seen.push(uid);
                    rows.push((
                        4,
                        model_row(uid, name, format!("lint: {rule}"), HomeTone::Bad),
                    ));
                }
            }
        }
        // Stable class sort keeps each class's own order (name for most,
        // downstream-desc for untested).
        rows.sort_by_key(|a| a.0);
        let mut out: Vec<HomeRow> = rows.into_iter().map(|(_, r)| r).collect();
        let extra = out.len().saturating_sub(SECTION_CAP);
        out.truncate(SECTION_CAP);
        (out, (extra > 0).then_some(extra))
    }

    /// The `/`-search's model rows over the UNFILTERED `model_list` (home
    /// search reaches models a `T`/`*` filter or a fold hid), empty query =
    /// all models (palette `@`'s contract). The syntax IS the list `/`'s
    /// (the same syntax as the list `/`): `!spec` routes through
    /// `select::select`'s dbt selectors, `?needle` through the full-text
    /// match (descriptions, columns, raw SQL), anything else through fuzzy
    /// name matching.
    fn home_search_rows(&self, query: &str) -> Vec<HomeRow> {
        let q = query.trim();
        let base = &self.model_list;
        let matched: Vec<&crate::NodeInfo> = if let Some(spec) = q.strip_prefix('!') {
            if spec.trim().is_empty() {
                // A bare `!` is the selector PREFIX, not a query — keep the
                // full result set while the user is still typing (same rule
                // `refilter` applies to the list).
                base.models.iter().collect()
            } else {
                let set = crate::select::select(&self.dag, self.diff(), spec);
                base.models
                    .iter()
                    .filter(|m| set.contains(&m.unique_id))
                    .collect()
            }
        } else if let Some(needle) = q.strip_prefix('?') {
            let needle = needle.to_lowercase();
            base.models
                .iter()
                .filter(|m| self.full_text_match(&m.unique_id, &needle))
                .collect()
        } else {
            base.models
                .iter()
                .filter(|m| q.is_empty() || !match_indices(&m.name, q).is_empty())
                .collect()
        };
        matched
            .into_iter()
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

/// The per-section cap: keep the first [`SECTION_CAP`] rows and report the
/// hidden remainder (the `+N more` tail) as `Some(n)` — `None` when nothing
/// clipped, so the plan emits no tail line.
fn cap_rows(mut rows: Vec<HomeRow>) -> (Vec<HomeRow>, Option<usize>) {
    let extra = rows.len().saturating_sub(SECTION_CAP);
    rows.truncate(SECTION_CAP);
    (rows, (extra > 0).then_some(extra))
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
