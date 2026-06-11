//! Pure UI state and the two-level reducer's bottom layer.
//!
//! - The selection space is the flat list of selectable *models* (`0..len`);
//!   group headers are drawn but never selectable.
//! - Scrolling lives in *display-row space* (`ModelList::rows`), so the unit
//!   that must stay visible is the selected model's *display row*. The renderer
//!   (`render::draw_list`) and the scroll-follow logic (`ensure_visible`) measure
//!   against the exact same row sequence.
//! - [`reduce_selection`] / [`handle_key`] are PURE and SIZE-UNAWARE: they never
//!   read pane height. Scroll-follow (`ensure_visible` / `anchor_lineage` /
//!   `ensure_lineage_visible` / `clamp_lineage`) is applied by the event loop
//!   with the terminal size.

use ratatui::crossterm::event::KeyEvent;

/// The lineage colour lens, cycled by `t` (replaces the old coverage on/off bool).
/// Each non-`Off` lens recolours the lineage node boxes by a different metric; the
/// `Coverage` arm is exactly the old behaviour, so ONE `t` press from the default
/// reproduces the muscle-memory coverage view (and keeps the status-bar cov%).
///
/// Cycle order (`cycle_lens`): Off → Coverage → DegreeHeat → Layer → LayerViolation
/// → Off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineageLens {
    /// No lens — node boxes show their materialization-class colour.
    #[default]
    Off,
    /// Test-coverage: testable nodes with zero tests are warning-coloured (the
    /// old `coverage_lens` behaviour; also drives the status-bar cov% segment).
    Coverage,
    /// Degree heat: nodes coloured by transitive-downstream "blast radius".
    DegreeHeat,
    /// Layer: nodes coloured by their dbt logical layer.
    Layer,
    /// Layer violations: nodes incident to a backward (e.g. marts→staging) edge.
    LayerViolation,
}

impl LineageLens {
    /// The next lens in the `t`-cycle (wraps `LayerViolation → Off`).
    pub fn next(self) -> LineageLens {
        match self {
            LineageLens::Off => LineageLens::Coverage,
            LineageLens::Coverage => LineageLens::DegreeHeat,
            LineageLens::DegreeHeat => LineageLens::Layer,
            LineageLens::Layer => LineageLens::LayerViolation,
            LineageLens::LayerViolation => LineageLens::Off,
        }
    }
}

/// Which pane currently has focus: the left model list or the right lineage pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The left model list pane.
    List,
    /// The right lineage pane.
    RightPane,
}

impl Focus {
    /// Toggle between the two panes.
    fn toggled(self) -> Self {
        match self {
            Focus::List => Focus::RightPane,
            Focus::RightPane => Focus::List,
        }
    }
}

/// What the event loop should do after a key was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Keep running.
    Continue,
    /// The user asked to quit (`q` / `Ctrl-c`).
    Quit,
}

/// Pure UI state: selection index (model space), focus, and scroll offset
/// (first visible *display row*, display-row space), plus the lineage pane's
/// 2-axis scroll (CharGrid space, sliced by `blit`). The lineage scroll is
/// driven by the mouse wheel (pan intent, saturating) and by the event loop's
/// geometry-aware follow ([`anchor_lineage`](UiState::anchor_lineage) /
/// [`ensure_lineage_visible`](UiState::ensure_lineage_visible) /
/// [`clamp_lineage`](UiState::clamp_lineage)) — movement KEYS move the
/// App-level lineage cursor instead, never this viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    /// Selected model index in `0..model_count` (the flat selectable list).
    selected: usize,
    /// Index of the first visible *display row* (display-row space, i.e. into
    /// `ModelList::rows`). NOTE: row space, not model space — headers force it.
    offset: usize,
    /// Currently focused pane.
    focus: Focus,
    /// Whether the left model-list pane is shown. While hidden the lineage pane
    /// takes the full width and focus is pinned to [`Focus::RightPane`] (the
    /// toggle and [`toggle_focus`](UiState::toggle_focus) both enforce it), so
    /// no key can ever drive an invisible list.
    list_visible: bool,
    /// Number of selectable models; the selection space is `0..model_count`.
    model_count: usize,
    /// Horizontal scroll offset of the lineage pane (CharGrid x).
    lineage_scroll_x: usize,
    /// Vertical scroll offset of the lineage pane (CharGrid y).
    lineage_scroll_y: usize,
    /// The active lineage colour lens (a pure view-pref like `list_visible`),
    /// cycled by `Action::CycleLens` directly in `apply_action` — never through
    /// `reduce_selection`. `Off` is the default; `Coverage` reproduces the old
    /// coverage-lens behaviour (untested testable nodes warning-coloured in both
    /// panes + the status-bar cov% segment).
    lens: LineageLens,
    /// Whether the lineage minimap inset is shown (default OFF). A pure view-pref
    /// like `lens`: when on AND the diagram overflows the pane, the
    /// renderer stamps a small occupancy-map inset in the top-right of the lineage
    /// interior. Toggled by `Action::ToggleMinimap` directly in `apply_action` —
    /// never through `reduce_selection`. Defaulting OFF keeps every existing
    /// lineage render test (whose goldens never toggle it) untouched.
    minimap_visible: bool,
    /// The lineage box density (a pure view-pref like `lens`): Comfortable
    /// 3-row boxes (the default — every existing render/golden is untouched)
    /// or Compact 1-row `|name|` nodes for big-graph overviews. Toggled by
    /// `Action::ToggleDensity` directly in `apply_action`.
    density: crate::Density,
}

impl UiState {
    /// New state for a list of `model_count` selectable models. Initial
    /// selection is index 0, focus on the list, offset at the top.
    pub fn new(model_count: usize) -> Self {
        UiState {
            selected: 0,
            offset: 0,
            focus: Focus::List,
            list_visible: true,
            model_count,
            lineage_scroll_x: 0,
            lineage_scroll_y: 0,
            lens: LineageLens::default(),
            minimap_visible: false,
            density: crate::Density::default(),
        }
    }

    /// The selected model index (in `0..model_count`).
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The first visible display-row index (scroll offset, display-row space).
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// The currently focused pane.
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Whether the left model-list pane is shown.
    pub fn list_visible(&self) -> bool {
        self.list_visible
    }

    /// Show / hide the left model-list pane. Hiding pins focus to the lineage
    /// pane (the only one left on screen); showing hands it back to the list
    /// (the reason you bring it back is to use it).
    pub fn toggle_list_pane(&mut self) {
        self.list_visible = !self.list_visible;
        self.focus = if self.list_visible {
            Focus::List
        } else {
            Focus::RightPane
        };
    }

    /// The active lineage colour lens.
    pub fn lens(&self) -> LineageLens {
        self.lens
    }

    /// Advance the lineage lens one step in the `t`-cycle. A pure view-pref
    /// mutation; the renderer reads [`lens`](UiState::lens) next frame.
    pub fn cycle_lens(&mut self) {
        self.lens = self.lens.next();
    }

    /// Whether the lineage minimap inset is shown.
    pub fn minimap_visible(&self) -> bool {
        self.minimap_visible
    }

    /// Flip the lineage minimap. A pure view-pref toggle; the renderer reads
    /// [`minimap_visible`](UiState::minimap_visible) next frame.
    pub fn toggle_minimap(&mut self) {
        self.minimap_visible = !self.minimap_visible;
    }

    /// The lineage box density.
    pub fn density(&self) -> crate::Density {
        self.density
    }

    /// Flip the lineage density (Comfortable <-> Compact). A pure view-pref
    /// toggle like the minimap; the cached layout rebuilds next frame (density
    /// is part of its key) and the loop force-anchors the reshaped diagram.
    pub fn toggle_density(&mut self) {
        self.density = match self.density {
            crate::Density::Comfortable => crate::Density::Compact,
            crate::Density::Compact => crate::Density::Comfortable,
        };
    }

    /// Toggle the focused pane — a no-op while the list pane is hidden (the
    /// lineage is the only focusable pane then).
    fn toggle_focus(&mut self) {
        if self.list_visible {
            self.focus = self.focus.toggled();
        }
    }

    /// The number of selectable models.
    pub fn model_count(&self) -> usize {
        self.model_count
    }

    /// The lineage pane's horizontal scroll offset (CharGrid x).
    pub fn lineage_scroll_x(&self) -> usize {
        self.lineage_scroll_x
    }

    /// The lineage pane's vertical scroll offset (CharGrid y).
    pub fn lineage_scroll_y(&self) -> usize {
        self.lineage_scroll_y
    }

    /// Set the selected model index, clamped to the current model count.
    ///
    /// Used by the `App` when selection is driven *externally* — mouse re-root,
    /// search confirm, or reload restore — rather than by relative `move_*`. The
    /// relative movers stay private so `handle_key`'s contract is unchanged.
    pub fn set_selected(&mut self, index: usize) {
        self.selected = index.min(self.max_index());
    }

    /// Update the number of selectable models (after a filter or reload changes
    /// the active list) and re-clamp the selection into range. Pairs with
    /// [`set_selected`] so the App keeps selection coherent across list changes
    /// without resetting focus/scroll.
    ///
    /// [`set_selected`]: UiState::set_selected
    pub fn set_model_count(&mut self, count: usize) {
        self.model_count = count;
        self.selected = self.selected.min(self.max_index());
    }

    /// Set the focused pane (used by a mouse click on a pane).
    pub fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
    }

    /// Move the selection by a wheel notch (focus-independent), clamped. Used by
    /// a mouse wheel over the list pane.
    pub fn wheel_list(&mut self, down: bool) {
        if self.model_count == 0 {
            return;
        }
        self.selected = if down {
            (self.selected + WHEEL_STEP).min(self.max_index())
        } else {
            self.selected.saturating_sub(WHEEL_STEP)
        };
    }

    /// Scroll the lineage pane vertically by a wheel notch (focus-independent;
    /// the loop's `clamp_lineage` bounds it next frame). Used by a wheel over the
    /// lineage pane.
    pub fn wheel_lineage(&mut self, down: bool) {
        self.lineage_scroll_y = if down {
            self.lineage_scroll_y.saturating_add(WHEEL_STEP)
        } else {
            self.lineage_scroll_y.saturating_sub(WHEEL_STEP)
        };
    }

    /// Reset the lineage scroll to the initial anchor centering the selected
    /// node's label in a `view_w x view_h` viewport over `grid`. Called by the
    /// event loop whenever the selection changes, so the new node is centered
    /// (using the rect's WIDTH so the whole label fits, not just its start cell).
    pub fn anchor_lineage(
        &mut self,
        selected_rect: Option<crate::NodeRect>,
        grid: &crate::CharGrid,
        view_w: usize,
        view_h: usize,
    ) {
        let (x, y) = crate::anchor_offset(selected_rect, grid, view_w, view_h);
        self.lineage_scroll_x = x;
        self.lineage_scroll_y = y;
    }

    /// Clamp the lineage scroll to the valid range for the given grid and
    /// viewport sizes (the size-aware upper bound the pure wheel-scroll intent
    /// can't know). Mirrors the list's `ensure_visible` end-clamp, in 2D.
    /// Saturating.
    pub fn clamp_lineage(&mut self, grid_w: usize, grid_h: usize, view_w: usize, view_h: usize) {
        self.lineage_scroll_x = crate::clamp_offset(grid_w, view_w, self.lineage_scroll_x);
        self.lineage_scroll_y = crate::clamp_offset(grid_h, view_h, self.lineage_scroll_y);
    }

    /// Scroll the lineage viewport MINIMALLY so `rect` (the cursor node's box)
    /// is fully visible — the 2D analogue of the list's [`ensure_visible`]:
    /// no movement at all when the rect already shows, edge-aligned otherwise.
    /// Deliberately NOT a re-center: centering on every cursor step would
    /// reintroduce the "viewport pans on every keypress" feel the cursor
    /// replaced. When the rect is larger than the viewport the start edge wins
    /// (names read left-to-right). Called by the event loop when the lineage
    /// cursor moves.
    ///
    /// [`ensure_visible`]: UiState::ensure_visible
    pub fn ensure_lineage_visible(
        &mut self,
        rect: crate::NodeRect,
        grid_w: usize,
        grid_h: usize,
        view_w: usize,
        view_h: usize,
    ) {
        // Scroll forward just enough to reveal the far edge…
        if rect.x + rect.width > self.lineage_scroll_x + view_w {
            self.lineage_scroll_x = (rect.x + rect.width).saturating_sub(view_w);
        }
        if rect.y + rect.height > self.lineage_scroll_y + view_h {
            self.lineage_scroll_y = (rect.y + rect.height).saturating_sub(view_h);
        }
        // …then let the near edge win (covers rects wider/taller than the view).
        if rect.x < self.lineage_scroll_x {
            self.lineage_scroll_x = rect.x;
        }
        if rect.y < self.lineage_scroll_y {
            self.lineage_scroll_y = rect.y;
        }
        self.clamp_lineage(grid_w, grid_h, view_w, view_h);
    }

    /// The largest valid selection index, or 0 when the list is empty.
    fn max_index(&self) -> usize {
        self.model_count.saturating_sub(1)
    }

    /// Move the selection down by one, clamped at the last model. No-op when the
    /// list pane is not focused or the list is empty. Never panics.
    fn move_down(&mut self) {
        if self.focus != Focus::List || self.model_count == 0 {
            return;
        }
        if self.selected < self.max_index() {
            self.selected += 1;
        }
    }

    /// Move the selection up by one, clamped at the first model. No-op when the
    /// list pane is not focused or the list is empty. Never panics.
    fn move_up(&mut self) {
        if self.focus != Focus::List || self.model_count == 0 {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the selection a fixed page (10) down / up, clamped at the list
    /// edges. Fixed-step (not viewport-sized) so the reducer stays size-unaware.
    /// No-op when the list pane is not focused or the list is empty.
    fn page_move(&mut self, down: bool) {
        if self.focus != Focus::List || self.model_count == 0 {
            return;
        }
        const PAGE: usize = 10;
        self.selected = if down {
            (self.selected + PAGE).min(self.max_index())
        } else {
            self.selected.saturating_sub(PAGE)
        };
    }

    /// Jump to the first model. No-op unless the list pane is focused.
    fn jump_top(&mut self) {
        if self.focus != Focus::List {
            return;
        }
        self.selected = 0;
    }

    /// Jump to the last model. No-op unless the list pane is focused or empty.
    fn jump_bottom(&mut self) {
        if self.focus != Focus::List || self.model_count == 0 {
            return;
        }
        self.selected = self.max_index();
    }

    /// Adjust the scroll offset so the selected model's *display row* stays
    /// visible within a pane that can show `height` display rows.
    ///
    /// Load-bearing scroll-follow, measured in **display-row space** (the same
    /// space `draw_list` renders), so headers are accounted for and the selected
    /// model can never be clipped below the pane border. Pure; called by the
    /// event loop after `handle_key` with:
    /// - `height`: number of display rows the list pane can show,
    /// - `top_row`: topmost row to reveal when scrolling UP (a first-in-group
    ///   model's preceding header, so the header scrolls in with it; row 0 for
    ///   model 0, preserving "index 0 ⇒ offset 0"),
    /// - `model_row`: the selected model's own row (must end up visible),
    /// - `total_rows`: total display rows.
    ///
    /// Two anchors: scrolling DOWN anchors on `model_row`; scrolling UP anchors
    /// on `top_row`. The final `model_row` re-check resolves `height == 1` (model
    /// wins). Then clamp so we never scroll past the end. `height == 0` is treated
    /// as 1; all arithmetic saturates, so degenerate inputs never panic.
    pub fn ensure_visible(
        &mut self,
        height: usize,
        top_row: usize,
        model_row: usize,
        total_rows: usize,
    ) {
        let height = height.max(1);
        // 1. Scroll down enough to show the selected model's own row.
        if model_row + 1 > self.offset + height {
            self.offset = model_row + 1 - height;
        }
        // 2. Scroll up to reveal the top anchor (header + model for first-in-group).
        if top_row < self.offset {
            self.offset = top_row;
        }
        // 3. height == 1 can't show both header and model: the model must win.
        if model_row + 1 > self.offset + height {
            self.offset = model_row + 1 - height;
        }
        // 4. Never scroll past the end (no blank trailing window).
        let max_offset = total_rows.saturating_sub(height);
        if self.offset > max_offset {
            self.offset = max_offset;
        }
    }
}

/// How many rows/cells one mouse-wheel notch moves.
const WHEEL_STEP: usize = 3;

/// Apply the **UiState-only** subset of an [`Action`] (the legacy keys).
///
/// The pure, size-unaware sub-reducer at the bottom of the two-level reducer:
/// `app::apply_action` forwards these arms here (for the LIST-focused side of
/// the movement keys — the lineage-focused side moves the App-level lineage
/// cursor, which needs the `Dag` and so never reaches this layer), and the
/// frozen [`handle_key`] facade drives it. The `move_*`/`jump_*` methods no-op
/// unless the list pane is focused. Domain actions (search/detail/help/yank/…)
/// are no-ops here; they are handled at the `App` level where the `Dag` is in
/// reach.
///
/// [`Action`]: crate::action::Action
pub fn reduce_selection(state: &mut UiState, action: crate::action::Action) {
    use crate::action::Action;
    match action {
        Action::MoveDown => state.move_down(),
        Action::MoveUp => state.move_up(),
        Action::PageDown => state.page_move(true),
        Action::PageUp => state.page_move(false),
        Action::JumpTop => state.jump_top(),
        Action::JumpBottom => state.jump_bottom(),
        Action::ToggleFocus => state.toggle_focus(),
        Action::ToggleListPane => state.toggle_list_pane(),
        _ => {}
    }
}

/// Apply a key event to the UI state and report whether to keep running.
///
/// **Frozen facade.** Kept for the unit/integration tests that drive [`UiState`]
/// directly. Dispatches the key in `Mode::Selection` through the keymap (the
/// single source of truth) and applies the UiState subset via [`reduce_selection`].
/// The real event loop calls `app::apply_action` instead.
pub fn handle_key(state: &mut UiState, key: KeyEvent) -> KeyOutcome {
    match crate::action::dispatch(&crate::action::Mode::Selection, key) {
        Some(crate::action::Action::Quit) => KeyOutcome::Quit,
        Some(action) => {
            reduce_selection(state, action);
            KeyOutcome::Continue
        }
        None => KeyOutcome::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn state(n: usize) -> UiState {
        UiState::new(n)
    }

    #[test]
    fn j_and_down_are_equivalent_increment() {
        let mut s = state(45);
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Char('j'))),
            KeyOutcome::Continue
        );
        assert_eq!(s.selected(), 1);
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected(), 2);
    }

    #[test]
    fn k_and_up_are_equivalent_decrement() {
        let mut s = state(45);
        s.selected = 5;
        handle_key(&mut s, press(KeyCode::Char('k')));
        assert_eq!(s.selected(), 4);
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.selected(), 3);
    }

    #[test]
    fn clamp_at_top_does_not_panic_or_underflow() {
        let mut s = state(45);
        assert_eq!(s.selected(), 0);
        handle_key(&mut s, press(KeyCode::Char('k')));
        assert_eq!(s.selected(), 0, "k at top stays at 0");
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.selected(), 0, "up at top stays at 0");
    }

    #[test]
    fn clamp_at_bottom_does_not_panic_or_overflow() {
        let mut s = state(45);
        s.selected = 44;
        handle_key(&mut s, press(KeyCode::Char('j')));
        assert_eq!(s.selected(), 44, "j at bottom stays at 44");
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected(), 44, "down at bottom stays at 44");
    }

    #[test]
    fn g_and_capital_g_jump_to_ends() {
        let mut s = state(45);
        s.selected = 20;
        handle_key(&mut s, press(KeyCode::Char('G')));
        assert_eq!(s.selected(), 44);
        handle_key(&mut s, press(KeyCode::Char('g')));
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn tab_toggles_focus() {
        let mut s = state(45);
        assert_eq!(s.focus(), Focus::List);
        handle_key(&mut s, press(KeyCode::Tab));
        assert_eq!(s.focus(), Focus::RightPane);
        handle_key(&mut s, press(KeyCode::Tab));
        assert_eq!(s.focus(), Focus::List);
    }

    #[test]
    fn toggle_list_pane_pins_focus_and_round_trips() {
        let mut s = state(45);
        assert!(s.list_visible(), "the list starts visible");

        // Hide: the lineage is the only pane left, so focus is forced there…
        s.toggle_list_pane();
        assert!(!s.list_visible());
        assert_eq!(s.focus(), Focus::RightPane, "hiding pins focus to lineage");
        // …and Tab cannot focus the invisible list.
        handle_key(&mut s, press(KeyCode::Tab));
        assert_eq!(s.focus(), Focus::RightPane, "Tab is a no-op while hidden");
        // List-side movement keys are no-ops too (move_* guard on List focus).
        handle_key(&mut s, press(KeyCode::Char('G')));
        assert_eq!(s.selected(), 0, "no list nav while the list is hidden");

        // Show: focus hands back to the list.
        s.toggle_list_pane();
        assert!(s.list_visible());
        assert_eq!(s.focus(), Focus::List, "showing focuses the list again");
    }

    #[test]
    fn right_pane_focus_makes_jk_noop() {
        let mut s = state(45);
        s.selected = 10;
        handle_key(&mut s, press(KeyCode::Tab)); // focus -> right
        assert_eq!(s.focus(), Focus::RightPane);
        handle_key(&mut s, press(KeyCode::Char('j')));
        handle_key(&mut s, press(KeyCode::Char('k')));
        handle_key(&mut s, press(KeyCode::Char('g')));
        handle_key(&mut s, press(KeyCode::Char('G')));
        assert_eq!(s.selected(), 10, "no list nav when right pane focused");
    }

    // ---- lineage pane 2-axis scroll (wheel pan + loop follow only) ----

    #[test]
    fn movement_keys_never_pan_the_lineage_viewport() {
        // The keys moved the viewport in the old design; now they move the
        // App-level lineage cursor (which this pure layer never sees), so the
        // scroll offsets must stay untouched under EITHER focus.
        let mut s = state(45);
        s.selected = 5;
        for _ in 0..2 {
            for k in [
                KeyCode::Char('h'),
                KeyCode::Char('l'),
                KeyCode::Char('j'),
                KeyCode::Char('k'),
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Down,
                KeyCode::Up,
            ] {
                handle_key(&mut s, press(k));
            }
            assert_eq!(s.lineage_scroll_x(), 0, "keys never pan horizontally");
            assert_eq!(s.lineage_scroll_y(), 0, "keys never pan vertically");
            handle_key(&mut s, press(KeyCode::Tab)); // 2nd round: right pane
        }
    }

    #[test]
    fn wheel_scrolls_list_and_lineage_focus_independently() {
        let mut s = state(45);
        s.wheel_list(true); // down one notch
        assert_eq!(
            s.selected(),
            WHEEL_STEP,
            "wheel down moves the selection a notch"
        );
        s.wheel_list(false);
        assert_eq!(s.selected(), 0, "wheel up returns, clamped at 0");
        // Lineage wheel nudges scroll_y regardless of focus (List focus here).
        s.wheel_lineage(true);
        assert_eq!(s.lineage_scroll_y(), WHEEL_STEP);
        s.wheel_lineage(false);
        assert_eq!(s.lineage_scroll_y(), 0);
        // Empty list: wheel is a safe no-op.
        let mut empty = state(0);
        empty.wheel_list(true);
        assert_eq!(empty.selected(), 0);
    }

    #[test]
    fn clamp_lineage_bounds_both_axes() {
        // Wheel pan records saturating intent; the loop's clamp bounds it.
        let mut s = state(45);
        s.focus = Focus::RightPane;
        for _ in 0..100 {
            s.wheel_lineage(true); // down intent (y only; x is set directly)
        }
        s.lineage_scroll_x = 1000;
        s.clamp_lineage(20, 10, 8, 4);
        assert_eq!(s.lineage_scroll_x(), 20 - 8, "x clamped to grid-view");
        assert_eq!(s.lineage_scroll_y(), 10 - 4, "y clamped to grid-view");
        s.clamp_lineage(5, 3, 8, 4);
        assert_eq!(s.lineage_scroll_x(), 0);
        assert_eq!(s.lineage_scroll_y(), 0);
    }

    // ---- ensure_lineage_visible: minimal 2D scroll-follow for the cursor ----

    fn rect(x: usize, y: usize, w: usize, h: usize) -> crate::NodeRect {
        crate::NodeRect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn ensure_lineage_visible_is_a_noop_when_rect_already_shows() {
        // The anti-"viewport pans on every keypress" property: a visible rect
        // must not move the window at all (NOT a re-center).
        let mut s = state(45);
        s.lineage_scroll_x = 10;
        s.lineage_scroll_y = 4;
        s.ensure_lineage_visible(rect(12, 5, 6, 3), 100, 50, 20, 10);
        assert_eq!((s.lineage_scroll_x(), s.lineage_scroll_y()), (10, 4));
    }

    #[test]
    fn ensure_lineage_visible_scrolls_minimally_toward_each_edge() {
        let mut s = state(45);
        // Right/bottom overflow: align the rect's far edge with the view edge.
        s.ensure_lineage_visible(rect(30, 20, 10, 3), 100, 50, 20, 10);
        assert_eq!(s.lineage_scroll_x(), 30 + 10 - 20, "right edge aligned");
        assert_eq!(s.lineage_scroll_y(), 20 + 3 - 10, "bottom edge aligned");
        // Left/top overflow: align the rect's near edge.
        s.ensure_lineage_visible(rect(5, 2, 4, 3), 100, 50, 20, 10);
        assert_eq!(s.lineage_scroll_x(), 5, "left edge aligned");
        assert_eq!(s.lineage_scroll_y(), 2, "top edge aligned");
    }

    #[test]
    fn ensure_lineage_visible_prefers_start_edge_for_oversized_rects_and_clamps() {
        let mut s = state(45);
        // A rect wider than the viewport: the start (left) edge wins.
        s.ensure_lineage_visible(rect(40, 0, 30, 3), 100, 50, 20, 10);
        assert_eq!(s.lineage_scroll_x(), 40, "start edge wins for wide rects");
        // A rect at the grid's far corner: result stays within clamp bounds.
        s.ensure_lineage_visible(rect(95, 48, 5, 2), 100, 50, 20, 10);
        assert_eq!(s.lineage_scroll_x(), 100 - 20, "x clamped to grid-view");
        assert_eq!(s.lineage_scroll_y(), 50 - 10, "y clamped to grid-view");
        // Degenerate zero-size viewport: never panics, offsets clamp to 0.
        s.ensure_lineage_visible(rect(0, 0, 1, 1), 1, 1, 0, 0);
        assert_eq!(s.lineage_scroll_x(), 0);
        assert_eq!(s.lineage_scroll_y(), 0);
    }

    #[test]
    fn anchor_lineage_centers_and_clamps() {
        use crate::layout::{layout, NodeRect};
        use crate::{Edge, NodeInfo, Subgraph};
        let n = |id: &str| NodeInfo {
            unique_id: id.into(),
            name: id.into(),
            resource_type: "model".into(),
            path: Some(format!("staging/{id}.sql")),
            ..Default::default()
        };
        let mut nodes = vec![n("a"), n("b"), n("c")];
        nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
        let sg = Subgraph {
            selected: "c".into(),
            nodes,
            edges: vec![
                Edge {
                    parent: "a".into(),
                    child: "b".into(),
                },
                Edge {
                    parent: "b".into(),
                    child: "c".into(),
                },
            ],
        };
        let lay = layout(&sg);
        let mut s = state(1);
        let (vw, vh) = (5usize, 1usize);
        s.anchor_lineage(lay.selected_rect, &lay.grid, vw, vh);
        let (sx, _sy) = lay.selected_coord.unwrap();
        let ox = s.lineage_scroll_x();
        assert!(
            sx >= ox && sx < ox + vw,
            "selected x {sx} in [{ox},{})",
            ox + vw
        );
        let _r = NodeRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
    }

    #[test]
    fn lens_cycle_is_off_coverage_heat_layer_violation_wrap() {
        // Default Off, and one cycle_lens() from default == the old coverage lens
        // (muscle memory). Full cycle wraps back to Off.
        let mut s = state(45);
        assert_eq!(s.lens(), LineageLens::Off, "default lens is Off");
        s.cycle_lens();
        assert_eq!(s.lens(), LineageLens::Coverage, "one `t` == coverage");
        s.cycle_lens();
        assert_eq!(s.lens(), LineageLens::DegreeHeat);
        s.cycle_lens();
        assert_eq!(s.lens(), LineageLens::Layer);
        s.cycle_lens();
        assert_eq!(s.lens(), LineageLens::LayerViolation);
        s.cycle_lens();
        assert_eq!(s.lens(), LineageLens::Off, "wraps back to Off");
    }

    #[test]
    fn q_signals_quit() {
        let mut s = state(45);
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Char('q'))),
            KeyOutcome::Quit
        );
    }

    #[test]
    fn ctrl_c_signals_quit() {
        let mut s = state(45);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ctrl_c), KeyOutcome::Quit);
    }

    #[test]
    fn empty_list_navigation_is_safe() {
        let mut s = state(0);
        handle_key(&mut s, press(KeyCode::Char('j')));
        handle_key(&mut s, press(KeyCode::Char('k')));
        handle_key(&mut s, press(KeyCode::Char('G')));
        assert_eq!(s.selected(), 0);
    }

    // ---- scroll-follow (ensure_visible), display-row space ----

    /// Display-row index of model `i` in the 49-row fixture layout. Mirrors
    /// `ModelList::row_of_model`.
    fn model_row(i: usize) -> usize {
        match i {
            0..=10 => i + 1,
            11..=18 => i + 2,
            19..=27 => i + 3,
            _ => i + 4,
        }
    }
    const TOTAL_ROWS: usize = 49;

    /// First-in-group reveal rows (header above). Mirrors `reveal_row_of_model`.
    fn reveal_row(i: usize) -> usize {
        match i {
            0 | 11 | 19 | 28 => model_row(i) - 1,
            _ => model_row(i),
        }
    }

    fn follow(s: &mut UiState, height: usize) {
        let sel = s.selected();
        s.ensure_visible(height, reveal_row(sel), model_row(sel), TOTAL_ROWS);
    }

    #[test]
    fn scroll_index_zero_keeps_top_visible() {
        let mut s = state(45);
        s.offset = 30;
        s.selected = 0;
        follow(&mut s, 10);
        let row = model_row(0);
        let off = s.offset();
        assert!(
            off <= row && row < off + 10,
            "model 0 (row {row}) in [{off},{})",
            off + 10
        );
        assert_eq!(off, 0, "selection at the top snaps the window to the top");
    }

    #[test]
    fn scroll_follows_selection_to_bottom_in_display_space() {
        let mut s = state(45);
        let h = 10;
        s.selected = 44;
        follow(&mut s, h);
        let row = model_row(44);
        let off = s.offset();
        assert!(
            off <= row && row < off + h,
            "model 44 (row {row}) in [{off},{})",
            off + h
        );
        assert_eq!(off, 39, "bottom-aligned window: total(49) - height(10)");
    }

    #[test]
    fn scroll_step_by_step_keeps_selected_row_visible() {
        let mut s = state(45);
        let h = 10;
        for _ in 0..44 {
            handle_key(&mut s, press(KeyCode::Char('j')));
            follow(&mut s, h);
            let row = model_row(s.selected());
            let off = s.offset();
            assert!(
                off <= row && row < off + h,
                "model {} (row {row}) out of [{off},{})",
                s.selected(),
                off + h
            );
        }
        assert_eq!(s.selected(), 44);
    }

    #[test]
    fn scroll_back_up_follows_and_clamps() {
        let mut s = state(45);
        let h = 10;
        s.selected = 44;
        follow(&mut s, h);
        assert_eq!(s.offset(), 39);
        for _ in 0..44 {
            handle_key(&mut s, press(KeyCode::Char('k')));
            follow(&mut s, h);
            let row = model_row(s.selected());
            let off = s.offset();
            assert!(
                off <= row && row < off + h,
                "model {} (row {row}) out of [{off},{}) going up",
                s.selected(),
                off + h
            );
        }
        assert_eq!(s.selected(), 0);
        assert_eq!(s.offset(), 0, "back at top, offset clamped to 0");
    }

    #[test]
    fn ensure_visible_height_one_keeps_selected_row_and_no_panic() {
        let mut s = state(45);
        for sel in [0usize, 11, 28, 44] {
            s.selected = sel;
            follow(&mut s, 1);
            assert_eq!(
                s.offset(),
                model_row(sel),
                "height 1: window sits on the selected row"
            );
        }
    }

    #[test]
    fn ensure_visible_height_zero_is_treated_as_one() {
        let mut s = state(45);
        s.selected = 44;
        s.ensure_visible(0, reveal_row(44), model_row(44), TOTAL_ROWS);
        assert_eq!(s.offset(), model_row(44), "height 0 behaves like height 1");
    }

    #[test]
    fn ensure_visible_clamps_when_everything_fits() {
        let mut s = state(45);
        s.selected = 44;
        s.offset = 5;
        s.ensure_visible(100, reveal_row(44), model_row(44), TOTAL_ROWS);
        assert_eq!(s.offset(), 0, "when all rows fit there is no scrolling");
    }

    #[test]
    fn first_in_group_selection_reveals_its_header() {
        let mut s = state(45);
        s.offset = 39;
        s.selected = 28;
        follow(&mut s, 10);
        let header = reveal_row(28);
        let model = model_row(28);
        let off = s.offset();
        assert!(
            off <= header,
            "header row {header} must be at/below offset {off}"
        );
        assert!(
            model < off + 10,
            "model row {model} must be inside [{off},{})",
            off + 10
        );
        assert_eq!(off, 31, "window snaps up to reveal the utilities header");
    }
}
