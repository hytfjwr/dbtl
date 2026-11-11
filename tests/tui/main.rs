//! TUI tests against the committed fixture (the synthetic
//! jaffle_finance manifest, schema v12): deterministic TTY-free list-assembly
//! assertions plus `TestBackend` renders of `draw`.

use dbtl::model_list::{build_model_list, ModelList, SortMode};
use dbtl::ui::{draw, handle_key, RenderCtx, StatusSegments, ToastLine, UiState};
use dbtl::{load_dag, Dag, NoticeLevel};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::Terminal;

#[path = "../common/mod.rs"]
mod common;
use common::{fixture_dag, FIXTURE_MANIFEST as FIXTURE};

fn fixture_list() -> ModelList {
    build_model_list(&fixture_dag(), SortMode::Layer)
}

mod chrome;
mod cll;
mod diff;
mod home;
mod inspector;
mod issue09;
mod list_assembly;
mod menu;
mod minimap;
mod render;
mod responsive;
mod scroll;
mod toasts;
mod width;

use minimap::*;
use render::*;
use responsive::*;
use scroll::*;
