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
//! - the `Dag` via [`App::generation`] (bumped on every [`App::reload`], the
//!   only place the `Dag` is replaced),
//! - the rooted selection, the [`LineageView`] (direction/depth), the validated
//!   lineage cursor, the active [`LineageLens`], and the [`GlyphMode`].
//!
//! Values are wrapped in `Rc` so a cache hit is a pointer clone, never a deep
//! copy. Interior mutability (`RefCell`) keeps the public accessors `&self`;
//! borrows are taken and released around each slot access, never held across a
//! call back into `App`. Determinism is unaffected: only results of pure,
//! already-deterministic functions are stored.

use std::cell::RefCell;
use std::rc::Rc;

use crate::{layout_density, Density, GlyphMode, Layout, LineageLens, Subgraph};

use super::{App, LineageView};

/// Identity of a rooted subgraph: `(root, view, dag generation)`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubgraphKey {
    root: String,
    view: LineageView,
    generation: u64,
}

/// Identity of the styled display layout: the subgraph identity plus the
/// validated cursor (which sets the emphasis/`selected_rect`), the active lens
/// (which sets the tints), and the glyph mode (which sets the glyph set).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayoutKey {
    root: String,
    cursor: Option<String>,
    view: LineageView,
    lens: LineageLens,
    glyphs: GlyphMode,
    density: Density,
    generation: u64,
}

/// Identity of the impact-count pair: `(root, dag generation)`.
type ImpactKey = (String, u64);

/// `(downstream, upstream)` transitive closure sizes.
type ImpactCounts = (usize, usize);

/// The per-`App` cache slots. One entry each — the loop only ever needs the
/// CURRENT frame's values, so an LRU would buy nothing.
#[derive(Default)]
pub(super) struct LineageCaches {
    subgraph: RefCell<Option<(SubgraphKey, Rc<Subgraph>)>>,
    layout: RefCell<Option<(LayoutKey, Rc<Layout>)>>,
    impact: RefCell<Option<(ImpactKey, ImpactCounts)>>,
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
            view: self.lineage_view.clone(),
            generation: self.generation,
        };
        if let Some((k, sg)) = self.caches.subgraph.borrow().as_ref() {
            if *k == key {
                return Rc::clone(sg);
            }
        }
        let sg = Rc::new(if key.root.is_empty() {
            Subgraph {
                selected: String::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            }
        } else {
            self.dag.subgraph_view(
                &key.root,
                self.lineage_view.upstream,
                self.lineage_view.downstream,
                self.lineage_view.depth,
            )
        });
        *self.caches.subgraph.borrow_mut() = Some((key, Rc::clone(&sg)));
        sg
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
        let root = self.selected_unique_id()?;
        let key = LayoutKey {
            // The VALIDATED cursor (home == Some(root)), so a stale stored
            // cursor or an explicit home both key identically to "at the root".
            cursor: self.lineage_cursor_uid(),
            root,
            view: self.lineage_view.clone(),
            lens: self.ui_state.lens(),
            glyphs: self.glyph_mode,
            density: self.ui_state.density(),
            generation: self.generation,
        };
        if let Some((k, lay)) = self.caches.layout.borrow().as_ref() {
            if *k == key {
                return Some(Rc::clone(lay));
            }
        }
        let sg = self.lineage_display_subgraph();
        if sg.nodes.is_empty() {
            return None;
        }
        let mut lay = layout_density(&sg, self.glyph_mode, self.ui_state.density());
        lay.apply_edge_styles(&self.lineage_edge_styles(&lay));
        let styles = self.lineage_styles(&lay);
        lay.apply_node_styles(&styles);
        let lay = Rc::new(lay);
        *self.caches.layout.borrow_mut() = Some((key, Rc::clone(&lay)));
        Some(lay)
    }

    /// Memoized [`impact_counts`](App::impact_counts) core: the two transitive
    /// closures are recomputed only when the selection or the `Dag` changes,
    /// not on every status-bar refresh.
    pub(super) fn impact_counts_cached(&self, uid: &str) -> (usize, usize) {
        let key = (uid.to_string(), self.generation);
        if let Some((k, counts)) = self.caches.impact.borrow().as_ref() {
            if *k == key {
                return *counts;
            }
        }
        let counts = self.impact_counts_for(uid);
        *self.caches.impact.borrow_mut() = Some((key, counts));
        counts
    }
}
