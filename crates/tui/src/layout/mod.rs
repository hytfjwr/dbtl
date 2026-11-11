//! Pure, ratatui-independent lineage layout.
//!
//! `layout(subgraph) -> Layout` produces a rectangular [`CharGrid`] of the
//! *entire* lineage graph (never clipped to a viewport) plus structured maps
//! (`columns`, `rects`, `selected_coord`). Invariant checks run against the
//! maps, NOT against CharGrid text, which would false-positive on shared
//! prefixes like `stg_payment__suppliers` vs `…supplier_departments`.
//!
//! Split by stage, all re-exported below so `crate::layout::X` (and the
//! `crate::X` facade re-exports) are unchanged:
//! - [`glyphs`]   — `GlyphMode` / `Density` and the `BoxGlyphs` repertoires.
//! - [`attr`]     — the parallel per-cell style data: `MaterializationClass`,
//!   `LensTint`, `CellAttr`.
//! - [`grid`]     — the value types: `ColLine`, `NodeRect`, `CharGrid`,
//!   `Layout`.
//! - [`boxes`]    — node-box metrics and the box/row stamps.
//! - [`place`]    — the placement pipeline: longest-path columns, the public
//!   `layout*` entry points, `layout_impl`.
//! - [`viewport`] — the scroll-window ops: `clamp_offset`, `anchor_offset`,
//!   `blit`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{ColumnEdgeKind, Subgraph};

mod attr;
mod boxes;
mod glyphs;
mod grid;
mod place;
#[cfg(test)]
mod tests;
mod viewport;

use boxes::*;
use glyphs::*;

pub use attr::{CellAttr, LensTint, MaterializationClass};
pub use glyphs::{Density, GlyphMode};
pub use grid::{CharGrid, ColLine, Layout, NodeRect};
pub(crate) use place::longest_path_columns;
pub use place::{layout, layout_columns, layout_density, layout_mode};
pub use viewport::{anchor_offset, blit, clamp_offset};
