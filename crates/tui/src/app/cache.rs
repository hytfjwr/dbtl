//! Keyed memoization of the per-frame lineage pipeline.
//!
//! The event loop redraws on every input AND on every 250ms idle tick; without
//! caching each frame recomputes the rooted subgraph (a BFS + sort over the
//! `Dag`), the full `Layout` (longest-path columns + a `CharGrid` render), the
//! per-node style map, and the impact closures — all of which are pure
//! functions of a small key. Each cache slot here stores `(key, value)` and is
//! invalidated by KEY COMPARISON only: there is no explicit flush to forget,
//! so a stale value is impossible as long as every input is part of the key.
//!
//! Keys capture every input of the cached computation:
//! - the `Dag` AND the `--diff` baseline diff via [`App::generation`] (bumped
//!   on every [`App::reload`] — the only place the `Dag` is replaced — and by
//!   `App::set_diff_base`, since the Diff lens styles read the diff),
//! - the rooted selection, the [`LineageView`] (direction/depth), the validated
//!   lineage cursor, the active [`LineageLens`], the [`GlyphMode`], and the
//!   whole-graph overview flag (`UiState::overview`).
//!
//! Values are wrapped in `Rc` so a cache hit is a pointer clone, never a deep
//! copy. Interior mutability (`RefCell`) keeps the public accessors `&self`;
//! borrows are taken and released around each slot access, never held across a
//! call back into `App`. Determinism is unaffected: only results of pure,
//! already-deterministic functions are stored.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use crate::collapse::Collapsed;
use crate::{layout_columns, layout_density, Density, GlyphMode, Layout, LineageLens, Subgraph};

use super::{App, LineageView};

/// Identity of a rooted subgraph: `(root, view, column focus, overview, dag
/// generation)`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubgraphKey {
    root: String,
    view: LineageView,
    /// The effective column focus's identity — `(node uid, column)` — so a
    /// focus enter / step / exit builds (and keys) a different subgraph than
    /// the rooted view of the same selection.
    col: Option<(String, String)>,
    overview: bool,
    generation: u64,
}

/// Identity of the collapsed display view: the subgraph identity plus every
/// input the collapse stage reads — the RAW stored cursor and the path
/// start (keep-alive priority), the bookmarks, the expansion set, and the
/// threshold. (The VALIDATED cursor is an output — membership is checked
/// against the produced subgraph, so it can't feed the key.)
#[derive(Debug, Clone, PartialEq, Eq)]
struct CollapsedKey {
    root: String,
    cursor: Option<String>,
    view: LineageView,
    /// Same focus identity as `SubgraphKey.col` — `collapsed_view` reads the
    /// focus-filtered subgraph as its base, so a focus enter/exit must miss.
    col: Option<(String, String)>,
    overview: bool,
    generation: u64,
    expanded: BTreeSet<String>,
    bookmarks: BTreeSet<String>,
    path_anchor: Option<String>,
    threshold: usize,
}

/// Identity of the styled display layout: the subgraph identity plus the
/// validated cursor (which sets the emphasis/`selected_rect`), the active lens
/// (which sets the tints), the glyph mode (which sets the glyph set), and
/// every collapse-stage input the drawn node set depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayoutKey {
    root: String,
    cursor: Option<String>,
    view: LineageView,
    /// Same focus identity as `SubgraphKey.col` — the box column rows are a
    /// layout input, so a focus change must miss the cached geometry too.
    col: Option<(String, String)>,
    lens: LineageLens,
    glyphs: GlyphMode,
    density: Density,
    overview: bool,
    generation: u64,
    expanded: BTreeSet<String>,
    bookmarks: BTreeSet<String>,
    path_anchor: Option<String>,
    threshold: usize,
}

/// Identity of the impact-count triple: `(root, dag generation)`.
type ImpactKey = (String, u64);

/// `(downstream sans exposures, upstream, downstream exposures)` closure sizes.
type ImpactCounts = (usize, usize, usize);

/// Identity of the inspector snapshot: `(inspected uid, generation)` — the
/// generation already covers every input [`inspector_view_for`] reads (the
/// `Dag` + its side maps, the diff, lint findings, run results, and the
/// catalog — all style inputs, all generation-bumped).
type InspectorKey = (String, u64);

/// The per-`App` cache slots. One entry each — the loop only ever needs the
/// CURRENT frame's values, so an LRU would buy nothing.
#[derive(Default)]
pub(super) struct LineageCaches {
    subgraph: RefCell<Option<(SubgraphKey, Rc<Subgraph>)>>,
    collapsed: RefCell<Option<(CollapsedKey, Rc<Collapsed>)>>,
    layout: RefCell<Option<(LayoutKey, Rc<Layout>)>>,
    impact: RefCell<Option<(ImpactKey, ImpactCounts)>>,
    inspector: RefCell<Option<(InspectorKey, Rc<crate::InspectorView>)>>,
}

impl App {
    /// The rooted lineage subgraph as a shared handle: a cache hit is an `Rc`
    /// clone. The memoized core of [`lineage_subgraph`](App::lineage_subgraph)
    /// (which clones out of it to keep its owned-value signature) and of every
    /// internal per-frame reader (`lineage_cursor_uid`, `lineage_matches`,
    /// the styled-layout builder).
    pub(super) fn subgraph_rc(&self) -> Rc<Subgraph> {
        let root = self.selected_unique_id().unwrap_or_default();
        let key = SubgraphKey {
            root,
            view: self.tab().lineage_view.clone(),
            col: self.column_focus_key(),
            overview: self.ui_state.overview(),
            generation: self.generation,
        };
        if let Some((k, sg)) = self.caches.subgraph.borrow().as_ref() {
            if *k == key {
                return Rc::clone(sg);
            }
        }
        let sg = Rc::new(if key.overview {
            // The whole graph, with the current selection as the emphasised
            // "you are here" node (an empty root just emphasises nothing).
            let mut sg = self.dag.full_subgraph();
            sg.selected = key.root.clone();
            sg
        } else if let Some(focus) = self.active_column_focus() {
            // Column focus replaces the rooted view wholesale: the filtered
            // related-models graph with the traced model as its home (the
            // `col` key already names this exact focus).
            self.column_subgraph(focus)
        } else if key.root.is_empty() {
            Subgraph {
                selected: String::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            }
        } else {
            self.dag.subgraph_view(
                &key.root,
                key.view.upstream,
                key.view.downstream,
                key.view.depth,
            )
        });
        *self.caches.subgraph.borrow_mut() = Some((key, Rc::clone(&sg)));
        sg
    }

    /// The memoized COLLAPSED display view — the stage between
    /// [`subgraph_rc`](App::subgraph_rc) and `layout` where sibling groups
    /// over the threshold contract into `+M more` markers (Issue 13). The
    /// returned `subgraph` also carries `selected` = the lineage cursor when
    /// it resolves to a drawn node OR a marker (so the existing emphasis +
    /// `selected_rect` machinery highlights it, like the old cursor swap did).
    ///
    /// Membership here is what makes a marker a VALID cursor position and a
    /// hit-test/Enter target: `groups` indexes every marker uid the pane
    /// currently draws.
    pub(super) fn collapsed_view(&self) -> Rc<Collapsed> {
        let base = self.subgraph_rc();
        let key = CollapsedKey {
            root: self.selected_unique_id().unwrap_or_default(),
            cursor: self.tab().lineage_cursor.clone(),
            view: self.tab().lineage_view.clone(),
            col: self.column_focus_key(),
            overview: self.ui_state.overview(),
            generation: self.generation,
            expanded: self.tab().collapse_expanded.clone(),
            bookmarks: self.bookmarks.clone(),
            path_anchor: self.tab().path_anchor.clone(),
            threshold: self.collapse_threshold,
        };
        if let Some((k, c)) = self.caches.collapsed.borrow().as_ref() {
            if *k == key {
                return Rc::clone(c);
            }
        }
        let mut c = self.collapse_view_of(&base);
        // The display `selected` = the validated cursor (a drawn node OR a
        // marker), else the root — the cursor-swap the emphasis machinery has
        // always highlighted, now applied after the collapse so a marker under
        // the cursor emphasises too.
        c.subgraph.selected = self
            .tab()
            .lineage_cursor
            .as_ref()
            .filter(|u| c.subgraph.contains(u))
            .cloned()
            .unwrap_or_else(|| base.selected.clone());
        let c = Rc::new(c);
        *self.caches.collapsed.borrow_mut() = Some((key, Rc::clone(&c)));
        c
    }

    /// The lineage pane's fully-styled display [`Layout`] (cursor-emphasised,
    /// lens-tinted), memoized — the single producer the event loop draws from,
    /// and the geometry source for the cursor moves. `None` when nothing is
    /// selected / the subgraph is empty.
    ///
    /// Building it runs the exact pipeline the loop used to run inline every
    /// frame: display subgraph → [`layout_mode`] → [`App::lineage_styles`] →
    /// [`Layout::apply_node_styles`]. A cache hit skips all of it.
    pub fn styled_lineage_layout(&self) -> Option<Rc<Layout>> {
        let overview = self.ui_state.overview();
        let root = match self.selected_unique_id() {
            Some(r) => r,
            None if overview => String::new(),
            None => return None,
        };
        let density = self.effective_density();
        let key = LayoutKey {
            // The VALIDATED cursor (home == Some(root)), so a stale stored
            // cursor or an explicit home both key identically to "at the root".
            cursor: self.lineage_cursor_uid(),
            root,
            view: self.tab().lineage_view.clone(),
            col: self.column_focus_key(),
            lens: self.ui_state.lens(),
            glyphs: self.glyph_mode,
            density,
            overview,
            generation: self.generation,
            expanded: self.tab().collapse_expanded.clone(),
            bookmarks: self.bookmarks.clone(),
            path_anchor: self.tab().path_anchor.clone(),
            threshold: self.collapse_threshold,
        };
        if let Some((k, lay)) = self.caches.layout.borrow().as_ref() {
            if *k == key {
                return Some(Rc::clone(lay));
            }
        }
        let collapsed = self.collapsed_view();
        if collapsed.subgraph.nodes.is_empty() {
            return None;
        }
        // Column focus feeds each box's related-column rows (`ColLine`s, marks
        // resolved to the glyph mode at focus-enter); an all-empty map and a
        // Compact density both fall back inside `layout_columns` to the classic
        // `layout_density` geometry, byte-identical.
        let mut lay = match self.active_column_focus() {
            Some(focus) => {
                layout_columns(&collapsed.subgraph, self.glyph_mode, density, &focus.lines)
            }
            None => layout_density(&collapsed.subgraph, self.glyph_mode, density),
        };
        lay.apply_edge_styles(&self.lineage_edge_styles(&lay));
        let styles = self.lineage_styles(&lay);
        lay.apply_node_styles(&styles);
        let lay = Rc::new(lay);
        *self.caches.layout.borrow_mut() = Some((key, Rc::clone(&lay)));
        Some(lay)
    }

    /// Memoized [`impact_breakdown_for`](App::impact_breakdown_for) core: the
    /// transitive closures are recomputed only when the selection or the `Dag`
    /// changes, not on every status-bar refresh.
    pub(super) fn impact_breakdown_cached(&self, uid: &str) -> (usize, usize, usize) {
        let key = (uid.to_string(), self.generation);
        if let Some((k, counts)) = self.caches.impact.borrow().as_ref() {
            if *k == key {
                return *counts;
            }
        }
        let counts = self.impact_breakdown_for(uid);
        *self.caches.impact.borrow_mut() = Some((key, counts));
        counts
    }

    /// The inspector pane's snapshot for the current target (the lineage
    /// cursor or the rooted selection — [`inspector_target`]), memoized by
    /// `(uid, generation)` so every idle-tick redraw is an `Rc` clone, not a
    /// `detail_view_for` rebuild + lint/diff/run scans. `None` when the Dag
    /// has no selectable node at all. The event loop hands `as_deref()` to
    /// `RenderCtx` — the render layer stays `Dag`-free.
    ///
    /// [`inspector_target`]: App::inspector_target
    pub fn inspector_view(&self) -> Option<Rc<crate::InspectorView>> {
        // `inspector_target` already falls back to a real node — the
        // `dag.get` filter here keeps the contract explicit at the cache
        // seam (a `+M more` marker uid can never key a snapshot).
        let uid = self
            .inspector_target()
            .filter(|u| self.dag.get(u).is_some())?;
        let key = (uid.clone(), self.generation);
        if let Some((k, v)) = self.caches.inspector.borrow().as_ref() {
            if *k == key {
                return Some(Rc::clone(v));
            }
        }
        let v = Rc::new(super::reducer::inspector_view_for(self, uid));
        *self.caches.inspector.borrow_mut() = Some((key, Rc::clone(&v)));
        Some(v)
    }
}
