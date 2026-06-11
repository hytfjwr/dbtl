//! The lineage view: the rooted subgraph, the spatial lineage cursor, the
//! root↔cursor path set, lineage-search matches, and the re-root breadcrumb.
//!
//! Everything here is size-unaware: cursor moves read CharGrid geometry
//! (pane-independent, glyph-mode-identical), never pane size.

use crate::action::{Direction, Mode, SearchTarget};
use crate::{Focus, Subgraph};

use super::App;

impl App {
    /// The lineage subgraph for the current selection under the active
    /// [`LineageView`](super::LineageView) (direction + depth). Empty when
    /// nothing is selected. An owned clone of the memoized
    /// [`subgraph_rc`](App::subgraph_rc) — internal per-frame readers use the
    /// `Rc` handle directly and never pay this clone.
    pub fn lineage_subgraph(&self) -> Subgraph {
        (*self.subgraph_rc()).clone()
    }

    /// The lineage cursor's `unique_id`: the stored cursor if it is still a
    /// member of the current subgraph, else the rooted selection (the cursor's
    /// home position — also covers a cursor dropped by a direction/depth
    /// change). `None` only when nothing is selected.
    pub fn lineage_cursor_uid(&self) -> Option<String> {
        let sg = self.subgraph_rc();
        if let Some(cur) = &self.lineage_cursor {
            if sg.contains(cur) {
                return Some(cur.clone());
            }
        }
        self.selected_unique_id()
    }

    /// Send the lineage cursor home (back to the rooted selection). Called by
    /// the event loop whenever the selection changes, and by `z` (recenter).
    pub fn reset_lineage_cursor(&mut self) {
        self.lineage_cursor = None;
    }

    /// Handle a lineage-pane click on `uid`: a model RE-ROOTS to it (recording
    /// history, and the cursor goes home with it — also when clicking the
    /// current root); a non-selectable node (source/seed/snapshot) just MOVES
    /// THE CURSOR there, so Enter can open its structure modal. The domain half
    /// of the mouse path (`main::handle_mouse` does the size-aware hit-test).
    pub fn click_lineage_node(&mut self, uid: &str) {
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

    /// The subgraph the lineage pane DRAWS: the rooted subgraph with `selected`
    /// swapped to the cursor, so the existing emphasis + `selected_rect`
    /// machinery highlights and viewport-follows the cursor with no new render
    /// code. Root semantics (pane title, exports, search matches, hit-test
    /// re-root) keep reading [`lineage_subgraph`](App::lineage_subgraph).
    pub fn lineage_display_subgraph(&self) -> Subgraph {
        let mut sg = self.lineage_subgraph();
        if let Some(cur) = &self.lineage_cursor {
            if sg.contains(cur) {
                sg.selected = cur.clone();
            }
        }
        sg
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
    /// lineage cursor, over the CURRENT [`lineage_subgraph`](App::lineage_subgraph)
    /// treated as UNDIRECTED (the cursor can be up- OR downstream of the root).
    /// Empty when the cursor is home (`== selection`) or nothing is selected — so
    /// a home cursor highlights nothing.
    ///
    /// Deterministic: `lineage_subgraph().edges` are already sorted, the
    /// undirected adjacency is built in that order, and the BFS records the
    /// first-found predecessor — so two calls produce the identical set. The set
    /// is only ever read via `.contains(uid)` (in [`lineage_styles`]), never
    /// iterated into output, so no `HashSet` order can leak.
    ///
    /// [`lineage_styles`]: App::lineage_styles
    pub fn lineage_path_set(&self) -> std::collections::HashSet<String> {
        use std::collections::{HashMap, HashSet, VecDeque};
        let (Some(root), Some(cursor)) = (self.selected_unique_id(), self.lineage_cursor_uid())
        else {
            return HashSet::new();
        };
        if root == cursor {
            return HashSet::new();
        }
        // Root semantics (NOT the display copy): same node/edge set, but keyed off
        // the rooted selection independent of which node the display "selects".
        let sg = self.subgraph_rc();
        // Undirected adjacency, built in the (already-sorted) edge order.
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &sg.edges {
            adj.entry(&e.parent).or_default().push(&e.child);
            adj.entry(&e.child).or_default().push(&e.parent);
        }
        // BFS root -> cursor, recording the first-found predecessor (deterministic).
        let mut prev: HashMap<&str, &str> = HashMap::new();
        let mut seen: HashSet<&str> = HashSet::from([root.as_str()]);
        let mut q: VecDeque<&str> = VecDeque::from([root.as_str()]);
        let mut found = false;
        while let Some(cur) = q.pop_front() {
            if cur == cursor.as_str() {
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
        let mut path = HashSet::new();
        if found {
            // Walk predecessors back from the cursor to the root, collecting both.
            let mut node = cursor.as_str();
            loop {
                path.insert(node.to_string());
                if node == root.as_str() {
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
