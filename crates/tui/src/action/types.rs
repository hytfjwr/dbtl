use super::*;

/// A movement / scroll direction, reused across actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Every user intent. `Copy` (all payloads are `char`/`u16`/[`Direction`]), so
/// it can live in the static [`BINDINGS`] table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // --- movement keys: focus-routed in `app::apply_action` (list move via
    // `ui::reduce_selection` when the list is focused, lineage CURSOR move when
    // the lineage pane is focused — never a viewport pan).
    MoveDown,
    MoveUp,
    MoveLeft,
    MoveRight,
    JumpTop,
    JumpBottom,
    ToggleFocus,
    /// Show / hide the left model-list pane (the lineage pane takes the full
    /// width while hidden; focus is forced to the lineage).
    ToggleListPane,
    Quit,
    // --- Overlays / domain (handled by `app::apply_action`).
    DetailOpen,
    DetailClose,
    DetailScroll(Direction),
    SearchOpen,
    SearchType(char),
    SearchBackspace,
    SearchCancel,
    SearchConfirm,
    SearchMove(Direction),
    /// Open the command palette (a fuzzy finder over every Selection-mode
    /// command). Its editing keys REUSE the `Search*` actions — only the opener
    /// is its own action, so the keymap stays the single source of truth.
    PaletteOpen,
    HelpToggle,
    Recenter,
    OpenEditor,
    YankId,
    YankName,
    /// Copy the current lineage subgraph to the clipboard as Mermaid `graph LR`.
    YankMermaid,
    /// Copy the current lineage subgraph to the clipboard as Graphviz DOT.
    YankDot,
    Reload,
    // --- lineage view (direction / depth) ---
    /// Toggle showing the upstream side of the lineage.
    ToggleUpstream,
    /// Toggle showing the downstream side of the lineage.
    ToggleDownstream,
    /// Decrease / increase the lineage hop-depth limit.
    DepthDecrease,
    DepthIncrease,
    /// Reset the lineage view to the default (both directions, unlimited depth).
    ResetView,
    // --- selection (re-root) history ---
    /// Go back / forward through the re-root history.
    HistoryBack,
    HistoryForward,
    // --- new feature commands (Step A wires ALL of them; only the two modal
    // openers are implemented here — the five toggles get no-op stubs filled by
    // their own later steps, so the keymap is never edited again). ---
    /// Open the SQL preview modal for the selected / cursored node.
    SqlOpen,
    /// Inside the SQL preview: switch the displayed text between the raw
    /// (uncompiled) SQL and the compiled (Jinja-expanded) one — when the node
    /// has compiled SQL at all (`target/compiled/` or manifest `compiled_code`).
    SqlToggleCompiled,
    /// Open the project stats dashboard modal.
    StatsOpen,
    /// Cycle the lineage colour lens (Off → Coverage → DegreeHeat → Layer →
    /// LayerViolation → Off).
    CycleLens,
    /// Set the lineage lens directly — the status chips' click path names a
    /// lens (the `run …` chip jumps to RunStatus rather than pedalling the
    /// `t` cycle). Never key-bound: only hit targets produce it.
    SetLens(LineageLens),
    /// Toggle a bookmark on the selected model.
    BookmarkToggle,
    /// Cycle the selection through the bookmarked models.
    BookmarkCycle,
    /// Cycle the left-pane sort order.
    SortCycle,
    /// Toggle the lineage minimap.
    ToggleMinimap,
    /// Toggle the lineage density (Comfortable 3-row boxes <-> Compact 1-row).
    ToggleDensity,
    /// Toggle the whole-graph overview (the lineage pane shows the ENTIRE DAG
    /// instead of the rooted subgraph; always Compact density).
    ToggleOverview,
    /// Two-node path mode: first `p` anchors a path start at the selection;
    /// the root↔cursor path highlight then runs anchor↔cursor (dbt's
    /// `A+,+B` answer to "why does X reach Y"). `p` again clears the anchor.
    PathToggle,
    /// Copy the current lineage diagram to the clipboard as plain text (the
    /// same glyphs the pane draws).
    YankAscii,
    /// Copy the selected node's raw (uncompiled) SQL to the clipboard.
    YankSql,
    /// Copy a Markdown blast-radius report for the selected node.
    YankImpact,
    /// Copy a runnable `dbt build --select …` command for the current lineage
    /// view (root + direction toggles + depth limit as graph operators).
    YankSelector,
    /// Copy the selected node's compiled (Jinja-expanded) SQL — the
    /// `target/compiled/` / `compiled_code` counterpart of [`Action::YankSql`], which
    /// stays raw-only. Bound as `ys` inside the yank-prefix menu.
    YankCompiledSql,
    /// Inside the yank menu (`yl`): copy the selected column's DOWNSTREAM
    /// column-level lineage closure as a deterministic Markdown table (the
    /// same [`ColumnReport`](crate::ColumnReport) `dbtl columns` prints). The
    /// "selected column" is the structure modal's column cursor, stashed on
    /// the App when `y` opened the menu; without it the action explains how
    /// to pick one instead of writing noise to the clipboard.
    YankColumnLineage,
    /// Open the yank prefix menu (a which-key over `Mode::Yank`): every yank
    /// action is also reachable as `y` + the same letter it always used
    /// (`ym` = Mermaid, `yi` = impact, `yy` = unique_id). The single-key
    /// bindings stay bound for the migration period.
    YankPrefix,
    /// Write the current lineage diagram to `<name>_lineage.txt` in the cwd.
    ExportLineage,
    /// Move the list selection a fixed page (10 models) down / up. List-side
    /// only (forwarded to `reduce_selection`); fixed-step so the reducer stays
    /// size-unaware.
    PageDown,
    PageUp,
    /// Scroll an overlay modal a fixed page (10 lines); the loop clamps.
    DetailScrollPage(Direction),
    /// Jump an overlay modal's scroll to the top / bottom (bottom records
    /// `usize::MAX`; the loop's per-frame clamp bounds it to the content).
    DetailScrollHome,
    DetailScrollEnd,
    /// Move the structure modal's column cursor one row (`Up` = previous
    /// column, `Down` = next) — the `yl` yank targets the column under it.
    /// Distinct from `DetailScroll`, which pans the viewport: this is a
    /// SELECTION, and the loop's ensure-visible follows it. While the tests
    /// section owns the cursor (`Tab`), the same move walks the jumpable
    /// (relationships-target) test rows.
    DetailColumnMove(Direction),
    /// Toggle the structure modal's h/l cursor between the columns section and
    /// the tests section (`Tab`) — the tests side only offers rows Enter can
    /// jump to (relationships `to:` targets).
    DetailSectionToggle,
    /// Cycle the selection to the next / previous untested model (the
    /// `coverage_gap` set — same predicate as the Coverage lens).
    GapNext,
    GapPrev,
    /// Cycle the selection backward through the bookmarked models.
    BookmarkCycleBack,
    /// Send the lineage cursor to the leftmost (most-upstream) / rightmost
    /// (most-downstream) column, nearest row.
    LineageLeftmost,
    LineageRightmost,
    /// Toggle the persistent untested-only / bookmarked-only list filter.
    ToggleUntestedFilter,
    ToggleBookmarkFilter,
    /// Fold/unfold the list group the cursor sits in (`-`; the group-header
    /// click's keyboard twin).
    ToggleGroupFold,
    /// Run `dbt parse` in the project root and adopt the regenerated
    /// `target/manifest.json` (upgrade source-mode's inferred lineage to the
    /// compiled manifest's full fidelity; in manifest mode it refreshes).
    DbtParse,
    /// Cycle the colour theme through the App's loaded list (built-in presets
    /// plus any user themes); the toast names the one that landed.
    ThemeCycle,
    /// Open the baseline-diff summary modal (`--diff`); a toast explains how to
    /// load a baseline when none is.
    DiffOpen,
    /// Export the PR impact report (the diff modal's full content) as Markdown to
    /// `<project>_pr_impact.md`. Bound INSIDE the Diff modal (the `e`-style
    /// export); reads the open [`DiffView`] snapshot so the file matches screen.
    ExportPrImpact,
    /// Copy the suggested `dbt build --select …` command for the changed-node set
    /// to the clipboard. Bound INSIDE the Diff modal; reads the open
    /// [`DiffView`]'s [`PrImpact`]. Copies the column-narrowed
    /// [`PrImpact::ci_command_columns`] when the analysis produced one —
    /// the most precise buildable set — falling back to the wide
    /// [`PrImpact::ci_command`]. `Y` ([`Action::YankPrSelectorWide`]) takes the wide
    /// command unconditionally.
    YankPrSelector,
    /// The wide counterpart of [`Action::YankPrSelector`]: always copies
    /// [`PrImpact::ci_command`] (every changed node with `+` fan-out),
    /// ignoring the column narrowing.
    YankPrSelectorWide,
    // --- column-level lineage focus ---
    /// Enter column focus on the structure modal's cursored column: the
    /// lineage pane filters to the column's related models and draws the
    /// related columns inside each box. Falls back to `DetailClose`
    /// behaviour when no column is under the cursor (so Enter still closes
    /// a column-less modal).
    ColumnFocusEnter,
    /// Leave column focus (Esc in Selection mode) — the lineage returns to
    /// the normal rooted view. No-op while unfocused.
    ColumnFocusExit,
    /// Open the column-lineage modal (`Mode::Columns`): the traced /
    /// cursored column's upstream+downstream column tree, scrollable and
    /// jumpable (Enter re-roots to the row's model). Toasts a hint when no
    /// column is in scope.
    ColumnsOpen,
    /// Move the column modal's row cursor one selectable row (`Up`/`Left` =
    /// previous, `Down`/`Right` = next) — a SELECTION like
    /// `DetailColumnMove`, not a viewport scroll; the loop's
    /// `clamp_columns_scroll` follows it.
    ColumnRowMove(Direction),
    /// Re-root to the model of the column-modal row under the cursor
    /// (records re-root history, like a lineage click). A non-model row
    /// notices instead of jumping.
    ColumnJump,
    // --- pane widths ---
    /// `<`: shrink the focused pane's saved width by the 4-column step —
    /// the list, until the inspector gains a focus target.
    PaneNarrow,
    /// `>`: grow the focused pane's saved width by the same step.
    PaneWiden,
    /// `=`: every resizable pane's saved width back to its default.
    PaneWidthsReset,
    /// Open the notification centre (`Ctrl-n`): the scrollable log of recent
    /// notifications (`HH:MM:SS  level  text`, newest first).
    /// Opening it is the read gesture that zeroes the `!N` unread-warning
    /// count.
    NotificationsOpen,
    // --- node context menu ---
    /// Open the node context menu (`a`, or a right-click on a node): a
    /// BINDINGS-derived action list over the node under the cursor/pointer.
    /// Over the structure / SQL modal the menu targets the modal's node.
    MenuOpen,
    /// Move the node menu's selection cursor (j/k, ↑/↓).
    MenuMove(Direction),
    /// Execute the cursored menu item.
    MenuConfirm,
    /// Execute the menu item at `items` index `i` — produced by the
    /// dispatcher's in-menu shortcut lookup (an item's own key fires it) and
    /// by a click on that row; never a fixed BINDINGS row.
    MenuInvoke(usize),
    /// Highlight the menu item at `items` index `i` (pointer hover) — pure
    /// cursor move, no execution.
    MenuSelect(usize),
    /// Highlight the `y` which-key row `i` (a row click selects, per the
    /// row-bearing-modal rule) — the index into [`yank_bindings`].
    YankPick(usize),
    /// Execute the `y` which-key row `i` (a double-click), resolving through
    /// the same [`yank_bindings`] table the renderer draws.
    YankInvoke(usize),
    /// Highlight palette candidate `i` (a row click; double-click then fires
    /// the existing [`SearchConfirm`](Action::SearchConfirm)).
    PalettePick(usize),
    /// Move the columns modal's row cursor to `i` (a row click; double-click
    /// then fires the existing [`ColumnJump`](Action::ColumnJump)).
    ColumnPick(usize),
    // --- tabs ---
    /// `O`: open the focus target's model in a NEW tab right of the active
    /// one (already-open roots just activate their tab — dedupe).
    TabOpen,
    /// `X` / `Ctrl-w`: close the ACTIVE tab — the right neighbour (else the
    /// left) takes over; the last tab refuses with a toast.
    TabClose,
    /// `{` / `}`: previous / next tab, wrapping at both ends.
    TabPrev,
    TabNext,
    /// `1`..=`9`: activate the tab at that 1-based position (tabs from
    /// position 10 carry no number and are unreachable by digit). Position
    /// counts MODEL tabs only — the home tab sits left of position 1 and is
    /// reached via `~`/`{`/`}`.
    TabActivate(u8),
    // --- home tab ---
    /// `~`: activate the home tab (no-op while already on it).
    HomeGo,
    /// `j`/`k`/`↑`/`↓` on the home screen: move the item cursor over the
    /// flattened selectable rows (resume → recent → bookmarks → attention;
    /// the search-results list while `/` search is open).
    HomeMove(Direction),
    /// `Enter` on the home screen: open the item under the cursor — already
    /// open activates its tab, else a new tab (the open rule).
    HomeOpen,
    /// A click on home item `i` (its flattened selectable index): select it
    /// and open it in one gesture (items open on click).
    HomePick(usize),
    // --- inspector pane ---
    /// `I`: cycle the inspector visibility pref `auto → on → off` (the
    /// persisted value; the width-aware auto rule resolves at draw time).
    ToggleInspector,
    /// Move the inspector's column-row cursor to `i` (a row click in the
    /// pane — the event loop maps the cell to the column index). `Enter`
    /// then traces the row's column, exactly like the structure modal's.
    InspectorColumnPick(usize),
}

/// Which pane a search targets (decided from focus when search opens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTarget {
    /// Filter the left model list.
    List,
    /// Jump to a matching node within the current lineage subgraph.
    Lineage,
    /// The home screen's `/` field: same syntax as `List` (`!` selector /
    /// `?` full-text), results replace the home sections. Enter opens the
    /// picked model in a tab rather than re-rooting.
    Home,
}

/// Live search state (query text + which pane it drives + the selection to
/// restore on cancel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub target: SearchTarget,
    pub query: String,
    /// The `unique_id` that was selected when search opened, restored on cancel.
    pub origin_uid: Option<String>,
    /// For a lineage-target search: index into the (ordered) match list, advanced
    /// by Up/Down to cycle matches. Reset to 0 whenever the query changes.
    pub match_idx: usize,
    /// For a LIST-target search: while `Some(i)`, the query is a recalled
    /// `App::search_history` entry — the browse cursor Up/Down walk (entered
    /// only from an EMPTY query). `None` = the live query the user is typing;
    /// any edit drops the browse back to `None`.
    pub hist: Option<usize>,
}
