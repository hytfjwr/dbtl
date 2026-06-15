//! dbtl core library — the crate root is a thin facade: every module's public
//! surface is re-exported here so callers (the binary, the integration tests)
//! keep flat `dbtl::X` paths regardless of where a type lives.
//!
//! The data flow: a `manifest.json` ([`manifest`] + [`load_dag`]) or a dbt
//! project tree ([`source`], which synthesizes a [`RawManifest`]) becomes a
//! pruned [`Dag`] ([`dag`]), which the [`app`] owns and the [`ui`] renders as
//! a [`layout`]-built ASCII lineage diagram.

pub mod action;
pub mod app;
pub mod dag;
pub mod diff;
pub mod effect;
pub mod layout;
pub mod manifest;
pub mod model_list;
pub mod source;
pub mod ui;

pub use action::{
    dispatch, help_lines, Action, DiffView, Direction, HelpLine, Mode, ModeKind, SearchTarget,
    SqlView, StatsView,
};
pub use app::{apply_action, layer_violation_edges, App, AppStats, ListFilter, Outcome};
pub use dag::{
    coverage_gap, is_exposure, load_dag, ColumnInfo, Dag, Edge, ExposureInfo, NodeDetail, NodeInfo,
    Subgraph, TestInfo,
};
pub use diff::{compute_diff, DagDiff, DiffStatus};
pub use effect::Effect;
pub use layout::{
    anchor_offset, blit, clamp_offset, layout, layout_density, layout_mode, CellAttr, CharGrid,
    Density, GlyphMode, Layout, LensTint, MaterializationClass, NodeRect,
};
pub use manifest::{
    load_manifest, RawConfig, RawDependsOn, RawExposure, RawExposureOwner, RawManifest, RawNode,
    RawSource, RawTestMetadata,
};
pub use model_list::{
    build_filtered_model_list, build_model_list, match_indices, name_matches_query, DisplayRow,
    ModelGroup, ModelList, SortMode, LAYER_ORDER,
};
pub use source::{load_dag_from_source, manifest_from_source};
pub use ui::{
    draw, handle_key, reduce_selection, Focus, KeyOutcome, LineageLens, RenderCtx, UiState,
};
