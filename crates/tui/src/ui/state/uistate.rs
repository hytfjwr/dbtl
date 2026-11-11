use super::*;

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
    pub(super) selected: usize,
    /// Index of the first visible *display row* (display-row space, i.e. into
    /// `ModelList::rows`). NOTE: row space, not model space — headers force it.
    pub(super) offset: usize,
    /// Currently focused pane.
    pub(super) focus: Focus,
    /// Whether the left model-list pane is shown — the USER'S PREF (the
    /// `Ctrl-b` toggle's persisted value), not the on-screen answer: below
    /// the 80-column band edge the pane is always hidden and the list lives
    /// only as the [`list_drawer`](UiState::list_drawer) overlay. While the
    /// resolved surface is hidden, focus is pinned to [`Focus::RightPane`]
    /// (the toggles and [`toggle_focus`](UiState::toggle_focus) all read
    /// [`list_shown`](UiState::list_shown)), so no key can ever drive an
    /// invisible list.
    list_visible: bool,
    /// Whether the narrow-band drawer is open — the ONLY list surface below
    /// 80 columns. A transient UI state, never persisted: above the
    /// band `pane_layout` ignores the flag outright, and in the band it is
    /// independent of `list_visible` (the pref governs the inline pane that
    /// can only exist ≥80).
    list_drawer: bool,
    /// The event loop's published answer to "is the terminal in the narrow
    /// band" — the <80-columns verdict that turns `list_visible` into a
    /// drawer-only surface. Only the loop writes it (the reducer and the
    /// prefs stay size-blind); [`list_shown`](UiState::list_shown) and the
    /// focus ring read it.
    list_auto_hidden: bool,
    /// Number of selectable models; the selection space is `0..model_count`.
    model_count: usize,
    /// Horizontal scroll offset of the lineage pane (CharGrid x).
    pub(super) lineage_scroll_x: usize,
    /// Vertical scroll offset of the lineage pane (CharGrid y).
    pub(super) lineage_scroll_y: usize,
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
    /// Whether the lineage pane shows the WHOLE graph instead of the rooted
    /// subgraph (a pure view-pref like `density`). Toggled by
    /// `Action::ToggleOverview` directly in `apply_action`. Defaulting OFF
    /// keeps every existing render/golden untouched.
    overview: bool,
    /// The list pane's saved width in columns — a view-pref like
    /// `list_visible`. Edited by the `<`/`>` keys, the splitter drag, and the
    /// `=` reset (all direct `apply_action` mutations, never
    /// `reduce_selection`); the DISPLAYED width is `geom::pane_layout`'s
    /// clamp of this value to the terminal.
    list_width: u16,
    /// The inspector pane's saved width in columns. Stored ahead of Issue
    /// 04's pane so `=` and the persisted state already carry it.
    inspector_width: u16,
    /// The inspector visibility pref (`I` cycles it; persisted). Pure data —
    /// the width-aware `auto` resolution happens in the event loop, which
    /// then publishes the result through [`UiState::set_inspector_shown`].
    inspector_policy: InspectorPolicy,
    /// Whether the inspector pane is actually ON SCREEN this frame — the
    /// event loop's resolved value, not a user toggle. Only the loop writes
    /// it; `pane_layout`/`hit_test`/focus-filtering read it.
    inspector_shown: bool,
    /// The inspector pane's scroll offset in content lines (loop-clamped,
    /// like every scroll slot).
    inspector_scroll: usize,
    /// The inspector's column-row cursor — an index into
    /// [`InspectorView::detail`]'s `col_names`, or `usize::MAX` when no row
    /// is cursored (the default — `j`/`k` then scroll instead of moving a
    /// cursor, per the "cursor stays a mouse/pick affordance" reading).
    inspector_col: usize,
    /// One-shot "reveal the picked row" intent: [`set_inspector_col`] arms
    /// it, the event loop's scroll clamp consumes it via
    /// [`take_inspector_reveal`]. Without the one-shot flag the every-frame
    /// clamp would keep dragging the viewport back to the cursored row —
    /// a single pick would cage `j`/`k`/`d`/`u`/`g`/`G`/wheel inside
    /// `inner_h` of that row forever.
    inspector_reveal: bool,
    /// The unique_id the currently-built [`InspectorView`] describes, so the
    /// reducer can reset scroll + cursor when the inspected node changes.
    inspector_target: String,
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
            list_drawer: false,
            list_auto_hidden: false,
            model_count,
            lineage_scroll_x: 0,
            lineage_scroll_y: 0,
            lens: LineageLens::default(),
            minimap_visible: false,
            density: crate::Density::default(),
            overview: false,
            list_width: super::geom::DEFAULT_LIST_WIDTH,
            inspector_width: super::geom::DEFAULT_INSPECTOR_WIDTH,
            inspector_policy: InspectorPolicy::default(),
            inspector_shown: false,
            inspector_scroll: 0,
            inspector_col: usize::MAX,
            inspector_reveal: false,
            inspector_target: String::new(),
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

    /// Show / hide the left model-list pane (the `Ctrl-b` pref). In the
    /// narrow band the inline surface cannot exist, so the toggle means the
    /// drawer — the event loop's `Ctrl-b` reroute hits this same branch, and
    /// so does every other dispatcher (palette confirm, menu): checking the
    /// published band verdict HERE keeps `list_visible` — a persisted pref —
    /// untouched no matter how the action arrived. Flipping to shown hands
    /// focus to the list; flipping to hidden moves it off the list — to the
    /// lineage, or keeps the inspector if that's where it was — the same pin
    /// [`set_list_drawer`] and [`set_list_auto_hidden`] keep: the decision
    /// reads [`list_shown`], so a pref flip in the band can't land focus on
    /// a surface that isn't drawn.
    ///
    /// [`set_list_drawer`]: UiState::set_list_drawer
    /// [`set_list_auto_hidden`]: UiState::set_list_auto_hidden
    /// [`list_shown`]: UiState::list_shown
    pub fn toggle_list_pane(&mut self) {
        if self.list_auto_hidden {
            self.toggle_list_drawer();
            return;
        }
        self.list_visible = !self.list_visible;
        self.focus = if self.list_shown() {
            Focus::List
        } else if self.focus == Focus::List {
            Focus::RightPane
        } else {
            self.focus
        };
    }

    /// Whether any list surface is on screen this frame — the inline pane
    /// above the narrow band, the overlay drawer inside it. The single
    /// "is the list reachable" verdict the focus ring reads: a hidden list
    /// can never hold focus, and only while this is true may `Focus::List`
    /// stand.
    pub fn list_shown(&self) -> bool {
        if self.list_auto_hidden {
            self.list_drawer
        } else {
            self.list_visible
        }
    }

    /// Whether the narrow-band drawer is open. The event loop writes
    /// it; [`pane_config`](UiState::pane_config) relays it to `pane_layout`
    /// so the drawn rect and the resolved rect stay one value.
    pub fn list_drawer(&self) -> bool {
        self.list_drawer
    }

    /// Whether the loop's <80-columns verdict is published — see
    /// [`set_list_auto_hidden`](UiState::set_list_auto_hidden).
    pub fn list_auto_hidden(&self) -> bool {
        self.list_auto_hidden
    }

    /// Open / close the narrow-band drawer. Opening takes list focus — the
    /// drawer IS the list surface in the band, and you open it to pick from
    /// it; a stray open above the band is inert (no focus steal, no rect).
    /// Closing drops focus to the lineage only when the closed surface held
    /// it and nothing list-shaped remains — the same pin
    /// [`toggle_list_pane`](UiState::toggle_list_pane) keeps.
    pub fn set_list_drawer(&mut self, open: bool) {
        self.list_drawer = open;
        if open {
            if self.list_auto_hidden {
                self.focus = Focus::List;
            }
        } else if !self.list_shown() && self.focus == Focus::List {
            self.focus = Focus::RightPane;
        }
    }

    /// Toggle the narrow-band drawer — the <80-columns meaning of `Ctrl-b`.
    /// Above the band the flag is inert, so the loop keeps the pref
    /// meaning there instead of reaching this.
    pub fn toggle_list_drawer(&mut self) {
        self.set_list_drawer(!self.list_drawer);
    }

    /// Publish the event loop's <80-columns verdict (the ONLY writer — the
    /// reducer and the prefs stay size-blind). In the band the inline pane
    /// is always hidden and the list exists only as the drawer, so a
    /// `Focus::List` left over from the wide band pins to the lineage; the
    /// drawer flag itself is left untouched — leaving the band hands the
    /// verdict back to `list_visible` and the flag simply goes inert. A
    /// drawer left open on the way out re-materializes on re-entry — that
    /// IS a fresh open, so it takes list focus like [`set_list_drawer`]
    /// does.
    ///
    /// [`set_list_drawer`]: UiState::set_list_drawer
    pub fn set_list_auto_hidden(&mut self, hidden: bool) {
        self.list_auto_hidden = hidden;
        if hidden && self.list_drawer {
            self.focus = Focus::List;
        } else if !self.list_shown() && self.focus == Focus::List {
            self.focus = Focus::RightPane;
        }
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

    /// Set the lens directly (the `--diff` startup seam: a loaded baseline
    /// opens on the Diff lens so the differences are visible immediately).
    pub fn set_lens(&mut self, lens: LineageLens) {
        self.lens = lens;
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

    /// Set the lineage box density directly (state restore / config default —
    /// the interactive path still goes through [`toggle_density`](Self::toggle_density)).
    pub fn set_density(&mut self, density: crate::Density) {
        self.density = density;
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

    /// Whether the whole-graph overview is active.
    pub fn overview(&self) -> bool {
        self.overview
    }

    /// Flip the whole-graph overview. A pure view-pref toggle like the density;
    /// the cached subgraph/layout rebuild next frame (the flag is part of both
    /// keys) and the loop force-anchors the reshaped diagram.
    pub fn toggle_overview(&mut self) {
        self.overview = !self.overview;
    }

    /// The list pane's saved width in columns.
    pub fn list_width(&self) -> u16 {
        self.list_width
    }

    /// The inspector pane's saved width in columns.
    pub fn inspector_width(&self) -> u16 {
        self.inspector_width
    }

    /// The saved width behind a splitter (the drag status hint's readout).
    pub fn pane_width(&self, which: super::geom::Splitter) -> u16 {
        match which {
            super::geom::Splitter::List => self.list_width,
            super::geom::Splitter::Inspector => self.inspector_width,
        }
    }

    /// Set the list pane's saved width directly (the `<`/`>` post-clamp and
    /// state restore). A stored width below the floor still displays at
    /// the floor — `pane_layout` clamps — so this only guards a literal 0.
    pub fn set_list_width(&mut self, w: u16) {
        self.list_width = w.max(1);
    }

    /// Set the inspector pane's saved width directly (state restore / `=`).
    pub fn set_inspector_width(&mut self, w: u16) {
        self.inspector_width = w.max(1);
    }

    /// `<`/`>`: nudge the list pane's saved width by `delta` columns (signed),
    /// floored at the minimum. The size-aware maximum (60% of the
    /// terminal) is the layout's clamp and the loop's post-clamp — this layer
    /// never sees pane size (same size-blind split as `wheel_lineage`).
    pub fn nudge_list_width(&mut self, delta: i32) {
        self.list_width = (i32::from(self.list_width) + delta)
            .clamp(i32::from(super::geom::MIN_LIST_WIDTH), i32::from(u16::MAX))
            as u16;
    }

    /// `<`/`>` on a focused inspector: the inspector-side counterpart of
    /// [`nudge_list_width`](UiState::nudge_list_width) — same fixed step,
    /// same size-blind floor-only clamp.
    pub fn nudge_inspector_width(&mut self, delta: i32) {
        self.inspector_width = (i32::from(self.inspector_width) + delta).clamp(
            i32::from(super::geom::MIN_INSPECTOR_WIDTH),
            i32::from(u16::MAX),
        ) as u16;
    }

    /// Apply a splitter drag's resolved width to the pane pref, clamped to the
    /// min/max for `terminal_w` — the size-aware caller (the event loop, or
    /// a headless test) resolves the pointer to `w` via
    /// `geom::splitter_drag_width`; the write is the same one the keys make.
    pub fn set_pane_width(&mut self, which: super::geom::Splitter, w: u16, terminal_w: u16) {
        match which {
            super::geom::Splitter::List => {
                self.list_width = super::geom::clamp_list_width(w, terminal_w);
            }
            super::geom::Splitter::Inspector => {
                self.inspector_width = super::geom::clamp_inspector_width(w, terminal_w);
            }
        }
    }

    /// Reset a pane's saved width to its default (double-click on its
    /// splitter, or `=` for both).
    pub fn reset_pane_width(&mut self, which: super::geom::Splitter) {
        match which {
            super::geom::Splitter::List => {
                self.list_width = super::geom::DEFAULT_LIST_WIDTH;
            }
            super::geom::Splitter::Inspector => {
                self.inspector_width = super::geom::DEFAULT_INSPECTOR_WIDTH;
            }
        }
    }

    /// The inspector visibility pref (`I` cycles it; a persisted view-pref).
    /// NOT the on-screen state — see [`inspector_shown`](UiState::inspector_shown).
    pub fn inspector_policy(&self) -> InspectorPolicy {
        self.inspector_policy
    }

    /// Set the inspector pref directly (state restore / config default).
    pub fn set_inspector_policy(&mut self, policy: InspectorPolicy) {
        self.inspector_policy = policy;
    }

    /// `I`: cycle the inspector pref `auto → on → off`. Pure pref mutation —
    /// the width-aware resolution happens in the event loop, which then
    /// publishes the result through [`set_inspector_shown`](UiState::set_inspector_shown).
    pub fn cycle_inspector(&mut self) {
        self.inspector_policy = self.inspector_policy.next();
    }

    /// Whether the inspector pane is ON SCREEN right now — the value the
    /// event loop publishes per frame after resolving the pref against the
    /// terminal width. `pane_layout`, `hit_test`, and the focus ring read it.
    pub fn inspector_shown(&self) -> bool {
        self.inspector_shown
    }

    /// Publish the event loop's resolved inspector visibility (the ONLY
    /// writer — reducers and prefs never touch this directly). Hiding the
    /// pane moves focus off it so no key can drive an invisible pane, the
    /// same pin `toggle_list_pane` keeps on the list.
    pub fn set_inspector_shown(&mut self, shown: bool) {
        self.inspector_shown = shown;
        if !shown && self.focus == Focus::Inspector {
            // Land on the lineage — the pane you were inspecting INTO — never
            // the list (which may itself be hidden).
            self.focus = Focus::RightPane;
        }
    }

    /// The inspector pane's scroll offset (content lines).
    pub fn inspector_scroll(&self) -> usize {
        self.inspector_scroll
    }

    /// Set the inspector scroll offset directly — the wheel/key scroll
    /// intent, a home/end jump's target, and the event loop's clamp + the
    /// cursor's ensure-visible all write here (same size-blind split as
    /// `wheel_lineage`: intents land unclamped; the loop bounds them).
    pub fn set_inspector_scroll(&mut self, scroll: usize) {
        self.inspector_scroll = scroll;
    }

    /// The inspector's column-row cursor (an index into the built
    /// `InspectorView`'s `col_names`), or `usize::MAX` for "no row cursored".
    pub fn inspector_col(&self) -> usize {
        self.inspector_col
    }

    /// Set the column-row cursor directly (a mouse pick — the index comes
    /// from the event loop's screen→row mapping) and arm the one-shot
    /// reveal so the next clamped frame scrolls it into view. `usize::MAX`
    /// clears the cursor (and the intent).
    pub fn set_inspector_col(&mut self, index: usize) {
        self.inspector_col = index;
        self.inspector_reveal = index != usize::MAX;
    }

    /// Drain the one-shot reveal intent [`set_inspector_col`] armed — the
    /// event loop's scroll clamp passes the cursored row to
    /// `clamp_inspector_scroll` for THIS frame only, so a pick reveals its
    /// row once and `j`/`k`/wheel scroll freely ever after (the highlight
    /// itself lives on in `inspector_col`, pinned or not).
    pub fn take_inspector_reveal(&mut self) -> bool {
        std::mem::take(&mut self.inspector_reveal)
    }

    /// The unique_id the current `InspectorView` describes ("" = none yet).
    pub fn inspector_target(&self) -> &str {
        &self.inspector_target
    }

    /// Record the inspected node's uid; a CHANGE resets scroll + column
    /// cursor (the new node's content starts at the top — the alternative,
    /// carrying a stale offset into different-length content, reads as a
    /// bug). Loop-written every frame.
    pub fn set_inspector_target(&mut self, uid: &str) {
        if self.inspector_target != uid {
            self.inspector_target.clear();
            self.inspector_target.push_str(uid);
            self.inspector_scroll = 0;
            self.inspector_col = usize::MAX;
            self.inspector_reveal = false;
        }
    }

    /// Set list-pane visibility directly (state restore), reusing the toggle's
    /// focus pinning so a hidden list can never keep the cursor.
    pub fn set_list_visible(&mut self, visible: bool) {
        if self.list_visible != visible {
            self.toggle_list_pane();
        }
    }

    /// Set the minimap flag directly (state restore — the interactive path
    /// still goes through [`toggle_minimap`](Self::toggle_minimap)).
    pub fn set_minimap(&mut self, on: bool) {
        self.minimap_visible = on;
    }

    /// The `geom::PaneConfig` for the current view prefs — the ONE derivation
    /// every `pane_layout` caller (draw, hit-test, viewport math) shares. The
    /// tab bar claims its row; the inspector reads the loop's
    /// RESOLVED `inspector_shown` (the width-aware answer), never the raw
    /// pref; `list_drawer` relays the narrow-band overlay flag (pane_layout
    /// itself gates it on the band, so the flag is harmless above 80).
    pub fn pane_config(&self) -> super::geom::PaneConfig {
        super::geom::PaneConfig {
            list_visible: self.list_visible,
            list_width: self.list_width,
            inspector_visible: self.inspector_shown,
            inspector_width: self.inspector_width,
            tab_bar: true,
            list_drawer: self.list_drawer,
        }
    }

    /// Advance focus one step in the `Tab` ring (`List → RightPane →
    /// Inspector → List`), skipping any pane that isn't on screen — so a
    /// hidden list still yields the lineage↔inspector pair, and a hidden
    /// inspector collapses the ring back to the original two-pane toggle.
    pub(super) fn toggle_focus(&mut self) {
        // The `Tab` ring walks the FIXED `List → RightPane → Inspector` order,
        // skipping panes that aren't on screen (`Focus::Inspector` is skipped
        // while the pane is hidden, `Focus::List` while the list is hidden) —
        // the List -> Lineage -> Inspector cycle.
        for _ in 0..3 {
            self.focus = self.focus.next();
            match self.focus {
                Focus::List if !self.list_shown() => continue,
                Focus::Inspector if !self.inspector_shown => continue,
                _ => return,
            }
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

    /// Write both lineage scroll offsets verbatim — the tab-switch restore
    /// half of the per-tab scroll snapshot ([`App`](crate::App) swaps them
    /// through its active tab's `scroll_x`/`scroll_y`). Values are treated as
    /// unvalidated intent exactly like the wheel setters: the loop's
    /// [`clamp_lineage`](UiState::clamp_lineage) bounds them next frame, so a
    /// restored offset that no longer fits the grid can never draw out of
    /// range. `pub(crate)` — the only caller is `App::enter_active_tab`.
    pub(crate) fn set_lineage_scroll(&mut self, x: usize, y: usize) {
        self.lineage_scroll_x = x;
        self.lineage_scroll_y = y;
    }

    /// Write both lineage scroll offsets as pan/minimap intent — the pointer
    /// gestures' write path (blank-space drag pans, minimap click/drag
    /// scrubs). Same unvalidated-intent contract as the wheel setters: the
    /// caller resolves the pure cell math (geom's `pan_offset` /
    /// `minimap_scroll_to`) and this stores it; the loop's
    /// [`clamp_lineage`](UiState::clamp_lineage) bounds it next frame.
    /// `pub` (not `pub(crate)`) because the only caller is the ROOT crate's
    /// event loop — a different crate than `dbtl-tui`.
    pub fn pan_lineage(&mut self, x: usize, y: usize) {
        self.lineage_scroll_x = x;
        self.lineage_scroll_y = y;
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

    /// Scroll the inspector pane vertically by a wheel notch (focus-independent;
    /// the loop's `clamp_inspector_scroll` bounds it next frame). Used by a
    /// wheel over the inspector pane.
    pub fn wheel_inspector(&mut self, down: bool) {
        self.inspector_scroll = if down {
            self.inspector_scroll.saturating_add(WHEEL_STEP)
        } else {
            self.inspector_scroll.saturating_sub(WHEEL_STEP)
        };
    }

    /// Scroll the lineage pane horizontally by a wheel notch (focus-independent;
    /// the loop's `clamp_lineage` bounds it next frame). Used by a horizontal
    /// wheel notch (trackpad swipe / tilt wheel) or Shift+vertical wheel over
    /// the lineage pane — together with [`wheel_lineage`](UiState::wheel_lineage)
    /// this gives free 2-axis panning of the diagram.
    pub fn wheel_lineage_x(&mut self, right: bool) {
        self.lineage_scroll_x = if right {
            self.lineage_scroll_x.saturating_add(WHEEL_STEP)
        } else {
            self.lineage_scroll_x.saturating_sub(WHEEL_STEP)
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
    pub(super) fn move_down(&mut self) {
        if self.focus != Focus::List || self.model_count == 0 {
            return;
        }
        if self.selected < self.max_index() {
            self.selected += 1;
        }
    }

    /// Move the selection up by one, clamped at the first model. No-op when the
    /// list pane is not focused or the list is empty. Never panics.
    pub(super) fn move_up(&mut self) {
        if self.focus != Focus::List || self.model_count == 0 {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the selection a fixed page (10) down / up, clamped at the list
    /// edges. Fixed-step (not viewport-sized) so the reducer stays size-unaware.
    /// No-op when the list pane is not focused or the list is empty.
    pub(super) fn page_move(&mut self, down: bool) {
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
    pub(super) fn jump_top(&mut self) {
        if self.focus != Focus::List {
            return;
        }
        self.selected = 0;
    }

    /// Jump to the last model. No-op unless the list pane is focused or empty.
    pub(super) fn jump_bottom(&mut self) {
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
