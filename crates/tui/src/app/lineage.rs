//! The lineage view: the rooted subgraph, the spatial lineage cursor, the
//! root↔cursor path set, lineage-search matches, and the re-root breadcrumb.
//!
//! Everything here is size-unaware: cursor moves read CharGrid geometry
//! (pane-independent, glyph-mode-identical), never pane size.

use std::collections::HashSet;

use ratatui::crossterm::event::MouseButton;

use crate::action::{Direction, Mode, SearchTarget};
use crate::collapse::{collapse_siblings, CollapseGroup, Collapsed};
use crate::ui::HitTarget;
use crate::{Focus, Subgraph};

use super::App;

impl App {
    /// The lineage subgraph for the current selection under the active
    /// [`LineageView`](super::LineageView) (direction + depth). Empty when
    /// nothing is selected. An owned clone of the memoized
    /// [`subgraph_rc`](App::subgraph_rc) — internal per-frame readers use the
    /// `Rc` handle directly and never pay this clone.
    ///
    /// This is the SEMANTIC lineage (every real node): exports, searches, and
    /// the keep-alive priorities all read it. What the pane *draws* is the
    /// collapsed stage on top — [`collapsed_view`](App::collapsed_view).
    pub fn lineage_subgraph(&self) -> Subgraph {
        (*self.subgraph_rc()).clone()
    }

    /// The lineage cursor's `unique_id`: the stored cursor if it is still a
    /// member of the current DISPLAYED lineage, else the SUBGRAPH's own root
    /// (the cursor's home position — also covers a cursor dropped by a
    /// direction/depth change). `None` only when nothing is selected.
    ///
    /// Membership is checked against the collapsed view (not the semantic
    /// subgraph), so a cursor parked on a `+M more` marker is valid — the
    /// marker is a node there though never a `Dag` node. A cursor on a real
    /// member can never be hidden (the keep-alive priority always shows it).
    ///
    /// The home comes from `sg.selected` rather than the list selection so a
    /// column-focus graph homes the cursor to the TRACED model — the list
    /// selection may sit outside the filtered graph and is never a valid
    /// home there. For a normal rooted subgraph `sg.selected` IS the
    /// selection, so existing behaviour is unchanged.
    pub fn lineage_cursor_uid(&self) -> Option<String> {
        let sg = self.subgraph_rc();
        if let Some(cur) = &self.lineage_cursor {
            if self.collapsed_view().subgraph.contains(cur) {
                return Some(cur.clone());
            }
        }
        if sg.selected.is_empty() {
            self.selected_unique_id()
        } else {
            Some(sg.selected.clone())
        }
    }

    /// Send the lineage cursor home (back to the rooted selection). Called by
    /// the event loop whenever the selection changes, and by `z` (recenter).
    pub fn reset_lineage_cursor(&mut self) {
        self.lineage_cursor = None;
    }

    /// Apply a release-confirmed click on the [`HitTarget`] the event loop
    /// resolved: a list row re-root-selects (via the row's `unique_id`, the
    /// same `reroot_select` contract every selection change follows), a
    /// lineage node runs [`click_lineage_node`](App::click_lineage_node), a
    /// list header / blank interior / lineage blank just takes that pane's
    /// focus (what clicking those cells did before), and the minimap and
    /// reserved targets do literally nothing. Only `MouseButton::Left` acts —
    /// right and middle clicks do nothing yet (Issues 05/06 give them
    /// meaning). The size-aware half (point → target) is `ui::hit_target`.
    pub fn click_target(&mut self, target: &HitTarget, button: MouseButton) {
        if button != MouseButton::Left {
            return;
        }
        match target {
            HitTarget::ListRow(index) => {
                self.ui_state.set_focus(Focus::List);
                let uid = self
                    .active_list()
                    .model_at(*index)
                    .map(|m| m.unique_id.clone());
                if let Some(uid) = uid {
                    // A list click landing on a different model is a re-root,
                    // so it runs the same "leave the column trace" rule as a
                    // lineage click (`reroot_select` owns the contract).
                    self.reroot_select(&uid);
                }
            }
            HitTarget::ListHeader(_) | HitTarget::ListBlank => {
                self.ui_state.set_focus(Focus::List);
            }
            HitTarget::LineageNode(uid) => {
                self.ui_state.set_focus(Focus::RightPane);
                self.click_lineage_node(uid);
            }
            HitTarget::LineageBlank => {
                self.ui_state.set_focus(Focus::RightPane);
            }
            // Inert surfaces do nothing at all — the minimap literally
            // swallows its click (Issue 05 gives it a pan meaning), and the
            // reserved targets wait on Issues 02 (boundary drag), 06 (modal
            // chrome), and 07 (status chips).
            HitTarget::Minimap
            | HitTarget::PaneBorder
            | HitTarget::StatusBar
            | HitTarget::ModalInterior
            | HitTarget::ModalExterior => {}
        }
    }

    /// Handle a lineage-pane click on `uid`: a `+M more` / `-M less` marker
    /// TOGGLES its sibling group AND takes the cursor (same focus-the-click
    /// as a source/seed/snapshot — the marker uid is keep-set stable, so it
    /// stays a live position across the toggle); a model RE-ROOTS to it
    /// (recording history, and the cursor goes home with it — also when
    /// clicking the current root); a non-selectable node just MOVES THE
    /// CURSOR there, so Enter can open its structure modal. The domain half
    /// of the mouse path (`main::handle_mouse` does the size-aware hit-test).
    pub fn click_lineage_node(&mut self, uid: &str) {
        if let Some(group) = self.collapse_group(uid) {
            self.lineage_cursor = Some(uid.to_string());
            self.toggle_collapse(&group.key);
            return;
        }
        self.jump_to(uid); // no-op for non-models
        if self.selected_unique_id().as_deref() == Some(uid) {
            // Clicked the (possibly just re-rooted) root: cursor home. The
            // loop's chokepoint only fires when the selected id CHANGES, so
            // clicking the current root must reset here.
            self.lineage_cursor = None;
        } else if self.subgraph_rc().contains(uid) {
            self.lineage_cursor = Some(uid.to_string());
        }
    }

    /// The subgraph the lineage pane DRAWS: the COLLAPSED display view —
    /// the rooted subgraph with oversized sibling groups contracted into
    /// `+M more` markers and `selected` swapped to the cursor, so the existing
    /// emphasis + `selected_rect` machinery highlights and viewport-follows
    /// the cursor with no new render code. Root semantics (pane title,
    /// Mermaid/DOT exports, search matches, hit-test re-root) keep reading
    /// [`lineage_subgraph`](App::lineage_subgraph).
    ///
    /// Production drawing goes through [`styled_lineage_layout`](App::styled_lineage_layout)
    /// (which styles + memoizes on top of this view) — this accessor stays
    /// `pub` as the raw-view seam tests assert on.
    pub fn lineage_display_subgraph(&self) -> Subgraph {
        self.collapsed_view().subgraph.clone()
    }

    /// Apply the sibling-collapse to `base` under the app's current threshold,
    /// expansion set, and keep-alive priorities — the shared body of
    /// [`collapsed_view`](App::collapsed_view) (the memoized pane view) and
    /// [`lineage_ascii`](App::lineage_ascii) (the screen-equivalent yank).
    ///
    /// The keep-alive set comes from [`collapse_keep_set`](App::collapse_keep_set)
    /// — the CURRENT root's keep-alives — so only call this for a `base` rooted
    /// on the selection (or accept that foreign roots leak no cursor/path state,
    /// since out-of-subgraph priorities simply match no member).
    pub(super) fn collapse_view_of(&self, base: &Subgraph) -> Collapsed {
        collapse_siblings(
            base,
            self.collapse_threshold,
            &self.collapse_expanded,
            &self.collapse_keep_set(base),
        )
    }

    /// The keep-alive set for the CURRENT root's collapse view — the nodes a
    /// collapse can never hide: the subgraph's subject (`base.selected`, the
    /// root on the pane), the raw lineage cursor, the anchor↔cursor path
    /// nodes (over `base`'s edges), and the bookmarks that are members. A
    /// marker uid in `lineage_cursor` simply fails the `base.contains` filter
    /// — markers aren't keep targets.
    fn collapse_keep_set(&self, base: &Subgraph) -> HashSet<String> {
        let mut priority: HashSet<String> = HashSet::new();
        priority.insert(base.selected.clone());
        if let Some(anchor) = &self.path_anchor {
            priority.insert(anchor.clone());
        }
        if let Some(cur) = self.lineage_cursor.as_deref().filter(|c| base.contains(c)) {
            priority.insert(cur.to_string());
            // The path highlight is a rooted-view concept (a whole-graph BFS
            // per frame would be wasted work on the overview, like the styles
            // path already treats it).
            if !self.ui_state.overview() {
                let start = self
                    .path_anchor
                    .clone()
                    .unwrap_or_else(|| base.selected.clone());
                if start != cur {
                    priority.extend(self.path_between(base, &start, cur));
                }
            }
        }
        priority.extend(self.bookmarks.iter().filter(|b| base.contains(b)).cloned());
        priority
    }

    /// The collapse applied to a base rooted on a DIFFERENT subject (the
    /// `y`-over-modal `pending_node` path): keep-set = the subject alone —
    /// the cursor, path anchor, and bookmark keep-alives belong to the
    /// CURRENT root's view and must not leak into another root's keep-set
    /// (a leaked cursor would draw itself visible in the export the screen
    /// hides). Expansion state still applies: group keys are global parent
    /// `unique_id`s, meaningful under any root that draws the group.
    pub(super) fn collapse_view_bare(&self, base: &Subgraph) -> Collapsed {
        collapse_siblings(
            base,
            self.collapse_threshold,
            &self.collapse_expanded,
            &HashSet::from([base.selected.clone()]),
        )
    }

    /// The collapse group a `+M more` / `-M less` marker `uid` stands for, if
    /// `uid` is a marker the pane currently draws. Real-node uids (and
    /// unknowns) return `None`.
    pub(super) fn collapse_group(&self, uid: &str) -> Option<CollapseGroup> {
        self.collapsed_view().groups.get(uid).cloned()
    }

    /// Toggle the expansion of the collapse group `key` (a hub parent's
    /// `unique_id`) under the current root: collapsed → expanded shows every
    /// member behind a `-M less` marker; expanded → collapsed puts them back.
    pub(super) fn toggle_collapse(&mut self, key: &str) {
        if !self.collapse_expanded.remove(key) {
            self.collapse_expanded.insert(key.to_string());
        }
    }

    /// Expand the collapse group hiding `uid` behind a `+M more` marker — the
    /// lineage-search auto-reveal. No-op when `uid` is already drawn (it is
    /// then a visible member or a marker itself) or unknown to the view.
    pub(super) fn reveal_in_lineage(&mut self, uid: &str) {
        let cv = self.collapsed_view();
        if cv.subgraph.contains(uid) {
            return;
        }
        let key = cv
            .groups
            .values()
            .find(|g| g.members.iter().any(|m| m == uid))
            .map(|g| g.key.clone());
        if let Some(key) = key {
            self.collapse_expanded.insert(key);
        }
    }

    /// Reveal the currently-cycled lineage-search match when it hides behind a
    /// `+M more` marker — so the match the loop anchors on is actually drawn.
    /// No-op outside an active lineage search.
    pub(super) fn reveal_current_lineage_match(&mut self) {
        if let Some(uid) = self.current_lineage_match() {
            self.reveal_in_lineage(&uid);
        }
    }

    /// Move the lineage cursor one node SPATIALLY over the current layout:
    /// Up/Down step through the cursor's own column stack, Left/Right jump to
    /// the row-nearest node of the adjacent column (every column `0..=max` is
    /// populated — longest-path layering guarantees a parent in `c-1` for any
    /// node in `c`). No-op at a graph edge or on an empty subgraph.
    ///
    /// Size-unaware by contract: this reads CharGrid geometry (which is
    /// glyph-mode-identical and pane-independent), never pane size. The
    /// deterministic tie-break (row distance, then y, then the column sort) is
    /// what keeps cursor paths reproducible across runs.
    pub fn move_lineage_cursor(&mut self, dir: Direction) {
        // Geometry from the cached display layout: rects/columns are identical
        // across glyph modes AND across the cursor swap (only emphasis moves),
        // so the cache primed for the last draw is reused as-is.
        let Some(lay) = self.styled_lineage_layout() else {
            return; // empty subgraph / nothing selected
        };
        let Some(cur) = self.lineage_cursor_uid() else {
            return;
        };
        let (Some(&cur_rect), Some(&cur_col)) = (lay.rects.get(&cur), lay.columns.get(&cur)) else {
            return;
        };
        let target_col = match dir {
            Direction::Up | Direction::Down => cur_col,
            Direction::Left => match cur_col.checked_sub(1) {
                Some(c) => c,
                None => return, // already in the leftmost (most-upstream) column
            },
            Direction::Right => cur_col + 1,
        };
        // The candidate column, y-stacked (rects is a HashMap; sorting restores
        // determinism — y is unique within a column, uid breaks hypothetical ties).
        let mut col: Vec<_> = lay
            .rects
            .iter()
            .filter(|(uid, _)| lay.columns.get(uid.as_str()) == Some(&target_col))
            .collect();
        col.sort_by_key(|(uid, r)| (r.y, (*uid).clone()));

        let next = match dir {
            Direction::Down | Direction::Up => {
                let Some(i) = col.iter().position(|(uid, _)| uid.as_str() == cur) else {
                    return;
                };
                let j = match dir {
                    Direction::Down => i + 1,
                    _ => match i.checked_sub(1) {
                        Some(j) => j,
                        None => return, // already at the top of the column
                    },
                };
                col.get(j).map(|(uid, _)| (*uid).clone())
            }
            Direction::Left | Direction::Right => {
                // Nearest by middle-row distance; equidistant prefers the upper.
                let cur_mid = cur_rect.y + cur_rect.height / 2;
                col.iter()
                    .min_by_key(|(_, r)| (cur_mid.abs_diff(r.y + r.height / 2), r.y))
                    .map(|(uid, _)| (*uid).clone())
            }
        };
        if let Some(uid) = next {
            self.lineage_cursor = Some(uid);
        }
    }

    /// Send the lineage cursor to the leftmost (column 0) or rightmost column,
    /// picking the row-nearest node — the same middle-row metric as the h/l
    /// single-column moves, so H is "h all the way" and L is "l all the way".
    pub fn move_lineage_cursor_extreme(&mut self, rightmost: bool) {
        // Same cached-geometry read as `move_lineage_cursor`.
        let Some(lay) = self.styled_lineage_layout() else {
            return; // empty subgraph / nothing selected
        };
        let Some(cur) = self.lineage_cursor_uid() else {
            return;
        };
        let Some(&cur_rect) = lay.rects.get(&cur) else {
            return;
        };
        let target_col = if rightmost {
            lay.columns.values().max().copied().unwrap_or(0)
        } else {
            0
        };
        let cur_mid = cur_rect.y + cur_rect.height / 2;
        // Nearest by middle-row distance; equidistant prefers the upper (the
        // same deterministic tie-break as move_lineage_cursor's Left/Right).
        let next = lay
            .rects
            .iter()
            .filter(|(uid, _)| lay.columns.get(uid.as_str()) == Some(&target_col))
            .min_by_key(|(uid, r)| (cur_mid.abs_diff(r.y + r.height / 2), r.y, (*uid).clone()))
            .map(|(uid, _)| uid.clone());
        if let Some(uid) = next {
            self.lineage_cursor = Some(uid);
        }
    }

    /// A short label for the lineage pane title describing the active view:
    /// direction (`↑↓` / `↑` / `↓` / `·`, or `<>` / `<` / `>` / `-` in ASCII
    /// glyph mode — left=upstream like the diagram) and depth (`≤N` / `<=N`
    /// when limited).
    pub fn lineage_view_label(&self) -> String {
        let v = &self.lineage_view;
        let (dir, leq) = match self.glyph_mode {
            crate::GlyphMode::Unicode => (
                match (v.upstream, v.downstream) {
                    (true, true) => "↑↓",
                    (true, false) => "↑",
                    (false, true) => "↓",
                    (false, false) => "·",
                },
                "≤",
            ),
            crate::GlyphMode::Ascii => (
                match (v.upstream, v.downstream) {
                    (true, true) => "<>",
                    (true, false) => "<",
                    (false, true) => ">",
                    (false, false) => "-",
                },
                "<=",
            ),
        };
        match v.depth {
            Some(d) => format!("{dir} {leq}{d}"),
            None => dir.to_string(),
        }
    }

    /// The `unique_id` a focus-routed overlay (`Enter`/`s`) acts on: the lineage
    /// cursor when the lineage pane is focused (so the action targets whatever the
    /// cursor is on), else the list selection. The single source of that policy —
    /// `DetailOpen` and `SqlOpen` both call it, so a focus-routing change touches
    /// one place.
    pub fn focus_target(&self) -> Option<String> {
        if self.ui_state.focus() == Focus::RightPane {
            self.lineage_cursor_uid()
        } else {
            self.selected_unique_id()
        }
    }

    /// The first node in the current lineage subgraph whose name matches `query`
    /// (subsequence). The subgraph is `dag.subgraph(selected)`, whose `nodes` are
    /// sorted by `unique_id`, so the match is deterministic. Used by the lineage
    /// node-jump search (size-unaware: just the match; the loop does the anchor).
    pub fn lineage_matches(&self, query: &str) -> Vec<String> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        self.subgraph_rc()
            .nodes
            .iter()
            .filter(|n| crate::name_matches_query(&n.name, query))
            .map(|n| n.unique_id.clone())
            .collect()
    }

    /// The lineage node currently targeted by an active lineage-search: the
    /// `match_idx`-th match (cycled by Up/Down). `None` unless a lineage search
    /// with a non-empty query is open and has at least one match — so an empty
    /// query never jolts the viewport off the rooted node.
    pub fn current_lineage_match(&self) -> Option<String> {
        if let Mode::Search(s) = &self.mode {
            if s.target == SearchTarget::Lineage {
                let matches = self.lineage_matches(&s.query);
                if matches.is_empty() {
                    return None;
                }
                let idx = s.match_idx % matches.len();
                return matches.get(idx).cloned();
            }
        }
        None
    }

    /// The set of node `unique_id`s on the path from the rooted selection to the
    /// lineage cursor, over the current DRAWN (collapsed) lineage view treated
    /// as UNDIRECTED (the cursor can be up- OR downstream of the root — and can
    /// be a `+M more` marker, which only exists in that view). Empty when the
    /// cursor is home (`== selection`) or nothing is selected — so a home
    /// cursor highlights nothing.
    ///
    /// Deterministic: the collapsed view's edges are already sorted, the
    /// undirected adjacency is built in that order, and the BFS records the
    /// first-found predecessor — so two calls produce the identical set. The set
    /// is only ever read via `.contains(uid)` (in [`lineage_styles`]), never
    /// iterated into output, so no `HashSet` order can leak.
    ///
    /// [`lineage_styles`]: App::lineage_styles
    pub fn lineage_path_set(&self) -> std::collections::HashSet<String> {
        self.lineage_path_nodes().into_iter().collect()
    }

    /// The ORDERED root→cursor path (cursor-side first — the walk-back order;
    /// only set membership and consecutive PAIRS are consumed, so the direction
    /// of iteration is irrelevant as long as it is deterministic). Empty when
    /// the cursor is home or nothing is selected. The shared core of
    /// [`lineage_path_set`](App::lineage_path_set) (node membership) and
    /// [`lineage_path_edges`](App::lineage_path_edges) (the connector runs).
    fn lineage_path_nodes(&self) -> Vec<String> {
        // The root↔cursor path is a rooted-view concept; the overview has no
        // meaningful root-to-cursor story (and a whole-graph BFS per frame would
        // be wasted work), so no path — and therefore no dimming — while it is on.
        if self.ui_state.overview() {
            return Vec::new();
        }
        let (Some(root), Some(cursor)) = (self.selected_unique_id(), self.lineage_cursor_uid())
        else {
            return Vec::new();
        };
        // A `p`-anchored path start overrides the root: the highlight then
        // answers "how does ANCHOR reach the cursor" (two arbitrary nodes —
        // dbt's `A+,+B`), not "how does the root reach the cursor".
        let start = self.path_anchor.clone().unwrap_or(root);
        if start == cursor {
            return Vec::new();
        }
        // The path runs over the DRAWN (collapsed) graph, so a cursor parked
        // on a `+M more` marker lights a real path to it (and the dim applies
        // to exactly what is on screen).
        let sg = self.collapsed_view();
        self.path_between(&sg.subgraph, &start, &cursor)
    }

    /// BFS `start -> cursor` over `sg` treated as UNDIRECTED (the cursor can be
    /// up- OR downstream of the start), returning the ordered cursor-side-first
    /// path — empty when `start` can't reach `cursor`.
    ///
    /// Deterministic: `sg.edges` are already sorted, the undirected adjacency
    /// is built in that order, and the BFS records the first-found
    /// predecessor — so two calls produce the identical set.
    ///
    /// Shared by [`lineage_path_nodes`](App::lineage_path_nodes) (the drawn
    /// path over the collapsed view) and [`collapse_view_of`](App::collapse_view_of)
    /// (the keep-alive path over the UNcollapsed base — the collapse stage
    /// can't read its own output).
    pub(super) fn path_between(&self, sg: &Subgraph, start: &str, cursor: &str) -> Vec<String> {
        use std::collections::{HashMap, VecDeque};
        // Undirected adjacency, built in the (already-sorted) edge order.
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &sg.edges {
            adj.entry(&e.parent).or_default().push(&e.child);
            adj.entry(&e.child).or_default().push(&e.parent);
        }
        // BFS start -> cursor, recording the first-found predecessor (deterministic).
        let mut prev: HashMap<&str, &str> = HashMap::new();
        let mut seen: HashSet<&str> = HashSet::from([start]);
        let mut q: VecDeque<&str> = VecDeque::from([start]);
        let mut found = false;
        while let Some(cur) = q.pop_front() {
            if cur == cursor {
                found = true;
                break;
            }
            if let Some(ns) = adj.get(cur) {
                for &nb in ns {
                    if seen.insert(nb) {
                        prev.insert(nb, cur);
                        q.push_back(nb);
                    }
                }
            }
        }
        let mut path = Vec::new();
        if found {
            // Walk predecessors back from the cursor to the root, collecting both.
            let mut node = cursor;
            loop {
                path.push(node.to_string());
                if node == start {
                    break;
                }
                match prev.get(node) {
                    Some(&p) => node = p,
                    None => break,
                }
            }
        }
        path
    }

    /// The root↔cursor path as CANONICAL `(parent, child)` edge keys — each
    /// consecutive path pair oriented the way it appears in the subgraph's edge
    /// list (the path is undirected, the edges are not). Keys match
    /// [`Layout::edge_cells`](crate::Layout::edge_cells), so the styles layer can
    /// stamp exactly the path's connector runs. Empty when the cursor is home.
    pub fn lineage_path_edges(&self) -> Vec<(String, String)> {
        let path = self.lineage_path_nodes();
        if path.len() < 2 {
            return Vec::new();
        }
        // Canonical orientation is checked against the DRAWN edges — the same
        // edge set `Layout::edge_cells` is keyed by (markers included).
        let sg = self.collapsed_view();
        let edge_set: std::collections::HashSet<(&str, &str)> = sg
            .subgraph
            .edges
            .iter()
            .map(|e| (e.parent.as_str(), e.child.as_str()))
            .collect();
        path.windows(2)
            .filter_map(|w| {
                let (a, b) = (w[0].as_str(), w[1].as_str());
                if edge_set.contains(&(a, b)) {
                    Some((w[0].clone(), w[1].clone()))
                } else if edge_set.contains(&(b, a)) {
                    Some((w[1].clone(), w[0].clone()))
                } else {
                    None // disconnected pair: BFS over sg edges makes this unreachable
                }
            })
            .collect()
    }

    /// The lineage re-root breadcrumb for the pane title: the last up-to-3
    /// back-history node NAMES followed by the current root name, joined with
    /// `" > "` (ASCII). `None` when the back-history is empty (so a fresh title is
    /// unchanged). Uids that no longer resolve to a node are skipped.
    ///
    /// If the joined trail exceeds `max_width`, whole entries are dropped from the
    /// LEFT (oldest first) until it fits, then a `".."` prefix is added (ASCII —
    /// never the `…` ellipsis, which is ambiguous-width). Size-unaware:
    /// `max_width` is a parameter, chosen at the width-aware render seam.
    /// Deterministic: it reads the ordered `back` stack and the Dag.
    ///
    /// This producer is *lenient*: when even `".. > root"` overflows it returns it
    /// anyway (the title block clips it). The render seam re-truncates STRICTLY via
    /// [`fit_breadcrumb`] so the trailing `[{v}]{lens}` title suffixes always
    /// survive — so the loop should hand the FULL trail (`usize::MAX`) here and let
    /// the draw seam be the single width authority.
    pub fn breadcrumb(&self, max_width: usize) -> Option<String> {
        // The breadcrumb narrates the re-root trail — meaningless while the pane
        // shows the whole graph, so the title falls back to the overview body.
        if self.ui_state.overview() {
            return None;
        }
        if self.back.is_empty() {
            return None;
        }
        let root = self.selected_unique_id()?;
        // The last up-to-3 history uids (oldest→newest), then the current root;
        // uids that no longer resolve are skipped (so a vanished node drops out).
        let mut entries: Vec<String> = self
            .back
            .iter()
            .rev()
            .take(3)
            .rev()
            .filter_map(|uid| self.dag.get(uid).map(|n| n.name.clone()))
            .collect();
        entries.push(self.dag.get(&root).map_or(root, |n| n.name.clone()));

        let join = |parts: &[String]| parts.join(" > ");
        let full = join(&entries);
        // Lenient: the strict fit can drop to empty, but a producer never returns
        // an empty trail — fall back to ".. > root" (the title block clips it).
        Some(
            fit_breadcrumb(&full, max_width)
                .unwrap_or_else(|| format!(".. > {}", entries[entries.len() - 1])),
        )
    }
}

/// Fit a `" > "`-joined breadcrumb `trail` into `max_width` DISPLAY CELLS by
/// dropping whole entries from the LEFT (oldest first) and prefixing `".."`
/// (ASCII — never the `…` ellipsis, which is ambiguous-width). The current root
/// (the last entry) is always kept.
///
/// STRICT: returns `None` when even `".. > root"` cannot fit — the render seam
/// uses that to drop the crumb to empty rather than let it overrun and evict the
/// trailing title suffixes (the bug being fixed). The size-unaware producer
/// [`App::breadcrumb`] handles `None` with its own lenient fallback.
///
/// Measures with [`UnicodeWidthStr`] so a CJK node name (which `chars().count()`
/// undercounts) is budgeted honestly. Pure + deterministic; shared by the
/// producer and the draw seam so the left-drop logic lives in ONE place.
pub(crate) fn fit_breadcrumb(trail: &str, max_width: usize) -> Option<String> {
    use unicode_width::UnicodeWidthStr;
    if trail.width() <= max_width {
        return Some(trail.to_string());
    }
    // `entries[last]` is the current root; never dropped.
    let entries: Vec<&str> = trail.split(" > ").collect();
    for start in 1..entries.len() {
        let candidate = format!(".. > {}", entries[start..].join(" > "));
        if candidate.width() <= max_width {
            return Some(candidate);
        }
    }
    None
}
