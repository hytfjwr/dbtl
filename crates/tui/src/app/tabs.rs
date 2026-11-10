//! Tabs (Issue 03): several open roots at once, each keeping the §2
//! "タブごと" state set — its own root, lineage view, cursor, re-root history,
//! sibling-collapse expansions, path anchor, column trace, and lineage scroll.
//!
//! `App` owns the ordered tab list plus the active index; every read/write of
//! the ACTIVE tab's state funnels through [`App::tab`]/[`App::tab_mut`] — the
//! one accessor site the spec requires, which is what keeps the old
//! direct-field call sites a mechanical rewrite.
//!
//! Everything here is size-unaware: the tab BAR's pixel layout is pure
//! geometry (`ui::geom::tab_bar_layout`), and the scroll-position swap reads
//! `UiState` counters, never pane size.

use std::collections::BTreeSet;

use ratatui::layout::Rect;

use crate::action::{Mode, PaletteState};

use super::column::ColumnFocus;
use super::{App, LineageView};

/// Everything one open root keeps between tab switches — the §2 per-tab set.
/// The tab's IDENTITY is its root `unique_id`; the rest is the view state that
/// root's lineage accumulates, restored verbatim on reactivation.
#[derive(Default)]
pub struct TabState {
    /// The rooted `unique_id` — the tab's identity (dedupe, label,
    /// `last_selected` compat). `None` only for the transient empty tab (a
    /// project whose list has no models at all).
    pub root: Option<String>,
    /// The lineage direction/depth filter (`u`/`d`/`[`/`]`/`0`).
    pub lineage_view: LineageView,
    /// The lineage cursor (`None` = home at the root). Validated against the
    /// live subgraph on every read, so a stale id can never escape.
    pub lineage_cursor: Option<String>,
    /// The re-root history stacks (`b`/`f`) — per tab, like a browser's.
    pub back: Vec<String>,
    /// Forward half of the history stack.
    pub forward: Vec<String>,
    /// The `+M more` sibling groups expanded under this root.
    pub collapse_expanded: BTreeSet<String>,
    /// The `p` path anchor (root↔cursor runs anchor↔cursor while set).
    pub path_anchor: Option<String>,
    /// The traced-column state (the `col` trace filters this tab's lineage).
    pub(crate) column_focus: Option<ColumnFocus>,
    /// The lineage pane's scroll offsets `(x, y)` in CharGrid space, stashed
    /// when the tab was switched away from. `None` = never anchored (a fresh
    /// or restored tab — the loop's selection-change path anchors it on the
    /// root, exactly like a re-root).
    pub scroll: Option<(usize, usize)>,
}

impl TabState {
    /// A fresh tab rooted on `uid`: default view, cursor home, empty history.
    /// `scroll: None` marks "not yet anchored" so the first draw centres the
    /// root rather than trusting a stale viewport.
    fn rooted(uid: &str) -> Self {
        TabState {
            root: Some(uid.to_string()),
            ..TabState::default()
        }
    }
}

/// A tab label's model name is middle-truncated past this many chars (§2 —
/// the same "keep the tail, elide the middle" rule §10 uses for list names).
const TAB_NAME_MAX: usize = 24;

impl App {
    /// The ordered tab list. Always non-empty — `close_tab` refuses to drop
    /// the last one, and `reload` re-seeds it.
    pub(crate) fn tabs(&self) -> &[TabState] {
        &self.tabs
    }

    /// The number of open tabs (the `1`..`9` key bound + the bar's cell count).
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// The active tab's index (0-based — the `1`-key's target minus 1).
    pub fn active_tab(&self) -> usize {
        self.active
    }

    /// The active tab's per-root state — THE read accessor every per-tab
    /// field flows through (`self.tab().lineage_view`, etc.).
    pub fn tab(&self) -> &TabState {
        &self.tabs[self.active]
    }

    /// The active tab's per-root state, mutable — THE write accessor.
    pub(crate) fn tab_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active]
    }

    /// Drain the one-shot "a tab switch happened this frame" flag. The event
    /// loop's uid-tracked selection-change chokepoint consumes it: a switch is
    /// an id-changing selection (so it lands on the one path every selection
    /// change takes), but the cursor/collapse reset is SKIPPED — the incoming
    /// tab's own cursor and expansions restore instead (§2's rule).
    pub fn take_tab_switch(&mut self) -> bool {
        std::mem::take(&mut self.tab_switched)
    }

    /// Whether the active tab carries a stashed lineage viewport — the loop's
    /// anchor chain restores it verbatim on reactivation; a fresh tab falls
    /// through to the usual re-root anchor.
    pub fn tab_scroll_saved(&self) -> bool {
        self.tab().scroll.is_some()
    }

    /// The stored first-visible index for the tab strip — the intent the
    /// wheel/`‹`/`›` write and [`tab_bar_layout`](crate::ui::tab_bar_layout)
    /// resolves. Synced to the RESOLVED scroll by [`set_tab_scroll`] after
    /// every draw, so a hit-test always sees what the frame drew.
    pub fn tab_scroll(&self) -> usize {
        self.tab_scroll
    }

    /// Store the resolved first-visible index (the loop's post-draw sync).
    /// Clamped to the strip's last position — a shrink can't strand it.
    pub fn set_tab_scroll(&mut self, scroll: usize) {
        self.tab_scroll = scroll.min(self.tabs.len().saturating_sub(1));
    }

    /// Wheel/`‹`/`›`: move the strip's first-visible index by `by` tabs
    /// (negative = left). The stored intent clamps to `0..count`; the
    /// active-visibility clamp is `tab_bar_layout`'s job — a manual scroll
    /// may look around the strip (the active tab re-centres on the next
    /// switch/structure change).
    pub fn scroll_tab_bar(&mut self, by: i32) {
        let hi = self.tabs.len().saturating_sub(1) as i32;
        self.tab_scroll = (self.tab_scroll as i32 + by).clamp(0, hi) as usize;
    }

    /// The tab-strip geometry for `strip`, resolved from the app's own
    /// labels/scroll/active/project — the single derivation the render ctx
    /// and the hit-test share (the Dag-aware half — names live on `dag` —
    /// stays inside `App`, keeping `RenderCtx` Dag-free).
    /// `ensure_active` applies §2's "the active tab is always scrolled into
    /// view" — pass `false` only when replaying the stored scroll for a
    /// manual-scroll frame or a hit-test.
    pub fn tab_bar_layout_for(&self, strip: Rect, ensure_active: bool) -> crate::ui::TabBarLayout {
        crate::ui::tab_bar_layout(
            strip,
            &self.tab_labels(),
            self.active,
            self.tab_scroll,
            self.stats.project.as_str(),
            ensure_active,
        )
    }

    /// The tab-bar labels (`{n} {name}` — the number only for positions 1-9,
    /// the name middle-truncated at [`TAB_NAME_MAX`]). Dag-aware (names live
    /// on `dag`), so the render layer stays Dag-free.
    pub fn tab_labels(&self) -> Vec<String> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let name = t
                    .root
                    .as_deref()
                    .map(|uid| mid_truncate(&self.node_name_or_uid(uid), TAB_NAME_MAX))
                    .unwrap_or_else(|| "-".to_string());
                if i < 9 {
                    format!("{} {}", i + 1, name)
                } else {
                    name
                }
            })
            .collect()
    }

    /// Write the ACTIVE tab's root — the single write site the spec asks for.
    /// The cursor/collapse resets that a root change implies live at the
    /// event loop's selection-change chokepoint (it diffs `root` frames), so
    /// this accessor only moves the identity.
    pub(super) fn set_root(&mut self, root: Option<String>) {
        self.tab_mut().root = root;
    }

    /// Record `uid` at the front of the `recent` MRU (Issue 10's home reads
    /// it — max 10, newest first, deduped). Called wherever a model becomes a
    /// root: `select_by_unique_id` lands and tab activations.
    pub(super) fn record_recent(&mut self, uid: &str) {
        self.recent.retain(|u| u != uid);
        self.recent.insert(0, uid.to_string());
        self.recent.truncate(10);
    }

    /// `O` / middle-click / `+` / `--select`: open `uid` as a tab — under
    /// `jump_to`'s openability rule (the model must resolve in the ACTIVE
    /// list, the same gate a lineage Enter applies), a non-model /
    /// filtered-out node toasts `can't open a <kind> as a tab` instead and
    /// nothing moves.
    pub fn open_node_as_tab(&mut self, uid: &str) -> bool {
        if self.active_list().models.iter().any(|n| n.unique_id == uid) {
            self.open_in_new_tab(uid);
            true
        } else {
            let kind = self
                .dag
                .get(uid)
                .map(|n| n.resource_type.as_str())
                .unwrap_or("node");
            self.set_notice(format!("can't open a {kind} as a tab"));
            false
        }
    }

    /// Insert a fresh tab for `uid` right of the active tab and activate it —
    /// T2 dedupe first: already-open `uid` just activates that tab.
    fn open_in_new_tab(&mut self, uid: &str) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.root.as_deref() == Some(uid))
        {
            self.activate_tab(i);
            return;
        }
        self.tabs.insert(self.active + 1, TabState::rooted(uid));
        self.activate_tab(self.active + 1);
    }

    /// Activate tab `idx`: stash the OUTGOING tab's live viewport, swap in the
    /// incoming tab's state, and flag the switch for the loop's chokepoint.
    /// `idx >= len` is a no-op (an out-of-range `1`-`9` key or stale target).
    pub(crate) fn activate_tab(&mut self, idx: usize) {
        if idx == self.active || idx >= self.tabs.len() {
            return;
        }
        self.stash_active_scroll();
        self.active = idx;
        self.enter_active_tab();
    }

    /// Close tab `idx` (× / middle-click / `X`): the RIGHT neighbour takes
    /// over, else the LEFT (§2). The last tab refuses to close — toast only.
    /// Closing a non-active tab just shifts the active index left when needed;
    /// the live tab's state is untouched.
    pub(crate) fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 {
            self.set_notice("can't close the last tab");
            return;
        }
        if idx >= self.tabs.len() {
            return;
        }
        let was_active = idx == self.active;
        self.tabs.remove(idx);
        if self.active > idx {
            // The live tab slid left — follow it (no switch, no restore).
            self.active -= 1;
        }
        if was_active {
            // idx is already the position the right neighbour slid into; when
            // the closed tab was the last, idx == len → the left neighbour.
            self.active = idx.min(self.tabs.len() - 1);
            self.enter_active_tab();
        }
    }

    /// `{` / `}`: previous / next tab, wrapping at both ends (§2).
    pub(crate) fn cycle_tab(&mut self, next: bool) {
        let n = self.tabs.len();
        if n <= 1 {
            return;
        }
        let idx = if next {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
        self.activate_tab(idx);
    }

    /// The state swap a switch performs: restore the incoming tab's saved
    /// lineage viewport (a `None` scroll falls through to the re-root anchor),
    /// close any open search (§2 — a switch mid-search drops it), re-anchor
    /// the list cursor on the incoming root when it resolves (a filtered-out
    /// root leaves the cursor detached at the top, like `--select` on a
    /// filtered list), and record the root in `recent`.
    fn enter_active_tab(&mut self) {
        if let Some((x, y)) = self.tab().scroll {
            self.ui_state.set_lineage_scroll(x, y);
        }
        if matches!(self.mode, Mode::Search(_)) {
            self.close_search(None);
        }
        if let Some(root) = self.tab().root.clone() {
            if let Some(i) = self
                .active_list()
                .models
                .iter()
                .position(|n| n.unique_id == root)
            {
                self.ui_state.set_selected(i);
            } else {
                // Root survives but is filtered out of the active list: the
                // §2 detach — the lineage still shows the root; the list
                // selection simply points at the top.
                self.ui_state.set_selected(0);
            }
            self.record_recent(&root);
        }
        self.tab_switched = true;
    }

    /// Stash the ACTIVE tab's live lineage scroll into its `TabState` — the
    /// one swap-out the per-tab viewport needs (the UiState fields stay live
    /// only for the active tab).
    fn stash_active_scroll(&mut self) {
        let scroll = (
            self.ui_state.lineage_scroll_x(),
            self.ui_state.lineage_scroll_y(),
        );
        self.tabs[self.active].scroll = Some(scroll);
    }

    /// Re-home the whole app onto tab `i` after a restore or reload replaced
    /// the tab list under it: clamp the index, then `enter_active_tab`.
    pub(crate) fn enter_tab_forced(&mut self) {
        self.active = self.active.min(self.tabs.len() - 1);
        self.enter_active_tab();
    }

    /// `--select`'s tab form: activate the tab rooted on `name`'s model —
    /// opening one when it isn't already open (§2's activate-or-open), gated
    /// by the same active-list rule as `open_node_as_tab`.
    pub fn open_tab_by_name(&mut self, name: &str) -> bool {
        let uid = self
            .active_list()
            .models
            .iter()
            .find(|n| n.name == name)
            .map(|n| n.unique_id.clone());
        uid.is_some_and(|uid| self.open_node_as_tab(&uid))
    }

    /// The `+` cell's click: open the command palette pre-seeded with `@` —
    /// the model-search mode (§7) whose Enter lands the model in a NEW tab
    /// (`PaletteState::new_tab`). A plain Ctrl-p palette typed with `@`
    /// searches models too but keeps re-root semantics.
    pub(crate) fn open_tab_palette(&mut self) {
        self.mode = Mode::Palette(PaletteState {
            query: "@".to_string(),
            selected: 0,
            new_tab: true,
        });
    }

    /// Swap the whole tab list in — the persistence restore (validated
    /// upstream: every entry's root already resolved against the live `dag`).
    /// `active` clamps to the new strip; then `enter_tab_forced` re-homes
    /// the app like any other switch.
    pub(crate) fn restore_tabs(&mut self, tabs: Vec<TabState>, active: usize) {
        if tabs.is_empty() {
            return;
        }
        self.tabs = tabs;
        self.active = active.min(self.tabs.len() - 1);
        self.tab_scroll = 0;
        self.enter_tab_forced();
    }
}

/// Middle-elide `name` to at most `max` display chars, keeping head and tail
/// (`stg_payment__…_departments` — §10's rule: dbt names share prefixes, so
/// the tail is what distinguishes them). The ellipsis is `..` — never the
/// `…` char, which is ambiguous-width on CJK terminals (the same rule the
/// breadcrumb follows).
fn mid_truncate(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max {
        return name.to_string();
    }
    if max <= 4 {
        // Degenerate budget: `..` + a couple of tail chars.
        let tail: String = chars[chars.len() - max.saturating_sub(2).max(1)..]
            .iter()
            .collect();
        return format!("..{tail}");
    }
    // Keep head + `..` + tail summing to `max`, biasing to the tail.
    let tail_len = (max - 2) / 2 + (max - 2) % 2;
    let head_len = max - 2 - tail_len;
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();
    format!("{head}..{tail}")
}
