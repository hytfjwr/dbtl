use super::*;

/// A `menu` annotation resolved per node-menu item: the label the row shows.
/// The menu never invents actions — every item resolves through the
/// [`BINDINGS`] row its [`MenuSlot`] names, so the action, its key label, and
/// its triggers stay the single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuMark {
    /// Show the row with this display label.
    Label(&'static str),
    /// The `Enter` row, whose honest label depends on the target: "Re-root
    /// here" when `Enter` would re-root to a different node, plain
    /// "Structure view" when the target is already the re-rooted node (the
    /// re-root is a no-op — the Enter really just opens the structure view).
    EnterContext,
}

/// One row inside the node context menu: either a separator rule or an
/// actionable item. Items carry NO action — `binding` indexes [`BINDINGS`]
/// (the row's action, trigger, and `key_label` are all read from there), so
/// the menu can never drift from the keymap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A rule between item groups.
    Separator,
    /// A selectable row: `binding` is the [`BINDINGS`] index the row was
    /// resolved from; `label` is the [`MenuMark`] text resolved against the
    /// target at open.
    Item { binding: usize, label: String },
}

/// Where the menu box anchors: the pointer cell it was
/// right-clicked on, or the target node's drawn box for the `a` keyboard
/// path. The box grows down-right of the anchor and flips on overflow —
/// the actual cells are computed by the size-aware layer (draw / hit-test).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAnchor {
    /// The screen cell the menu opened on (the right-click point).
    Cell { col: u16, row: u16 },
    /// The target node's lineage box — resolved to cells at draw/hit time
    /// (the event loop owns screen geometry).
    Node,
}

/// The node context menu's state (the `Mode::Menu` payload). Everything the
/// renderer needs is snapshotted here — the render layer never consults a
/// `Dag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    /// The `unique_id` every item acts on (the clicked / cursored node).
    pub target: String,
    /// The node's display name — the box's title.
    pub name: String,
    /// The rows (items + separators), already filtered for availability —
    /// e.g. a source node omits "SQL preview" because it has no SQL.
    pub items: Vec<MenuEntry>,
    /// The selection cursor: an index into `items`, always lands on `Item`
    /// rows.
    pub selected: usize,
    /// The mode restored on Esc/execute (`None` = Selection) — the same
    /// return mechanism `Mode::Yank` carries for nested overlays.
    pub back: Option<Box<Mode>>,
    /// Where the box anchors on screen.
    pub anchor: MenuAnchor,
}

/// One slot in the node menu's layout table ([`NODE_MENU_LAYOUT`]): the
/// ORDER and grouping only. Each item slot names the `(mode, action)` pair
/// its [`BINDINGS`] row carries — the action, key label, and triggers all
/// come from that row, so a layout reorder can never fork the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSlot {
    /// An actionable item resolved from the `(ModeKind, Action)` row.
    Item(ModeKind, Action),
    /// A group separator.
    Separator,
}

/// The node context menu's layout — the shape: open/view
/// actions, a rule, the yanks, a rule, the bookmarks/path toggles. This table
/// carries NO item data; `menu_layout_resolves` asserts every item slot names
/// a real `(mode, action)` [`BINDINGS`] row.
///
/// `Copy unique_id` resolves the `y` row from `ModeKind::Yank` (its trigger
/// is plain `y` — the second-key of the yank prefix); every other item names
/// its `ModeKind::Selection` row.
pub static NODE_MENU_LAYOUT: &[MenuSlot] = &[
    MenuSlot::Item(ModeKind::Selection, Action::DetailOpen),
    MenuSlot::Item(ModeKind::Selection, Action::SqlOpen),
    MenuSlot::Item(ModeKind::Selection, Action::OpenEditor),
    MenuSlot::Separator,
    MenuSlot::Item(ModeKind::Selection, Action::YankName),
    MenuSlot::Item(ModeKind::Yank, Action::YankId),
    MenuSlot::Item(ModeKind::Selection, Action::YankSql),
    MenuSlot::Item(ModeKind::Yank, Action::YankSelector),
    MenuSlot::Separator,
    MenuSlot::Item(ModeKind::Selection, Action::BookmarkToggle),
    MenuSlot::Item(ModeKind::Selection, Action::PathToggle),
];

/// The display label a menu slot resolves to — `re_roots` is whether the
/// target differs from the current re-rooted node (see
/// [`MenuMark::EnterContext`]).
pub(crate) fn menu_slot_label(mark: MenuMark, re_roots: bool) -> String {
    match mark {
        MenuMark::Label(l) => l.to_string(),
        MenuMark::EnterContext => {
            if re_roots {
                "Re-root here".to_string()
            } else {
                "Structure view".to_string()
            }
        }
    }
}

/// The mark each layout slot displays — kept next to the layout so the label
/// choice lives in one place (the `MenuSlot` stays a pure `(mode, action)`
/// key into [`BINDINGS`]).
pub(crate) fn menu_slot_mark(action: Action) -> MenuMark {
    match action {
        Action::DetailOpen => MenuMark::EnterContext,
        Action::SqlOpen => MenuMark::Label("SQL preview"),
        Action::OpenEditor => MenuMark::Label("Open in editor"),
        Action::YankName => MenuMark::Label("Copy name"),
        Action::YankId => MenuMark::Label("Copy unique_id"),
        Action::YankSql => MenuMark::Label("Copy raw SQL"),
        Action::YankSelector => MenuMark::Label("Copy dbt selector"),
        Action::BookmarkToggle => MenuMark::Label("Bookmark"),
        Action::PathToggle => MenuMark::Label("Path anchor here"),
        other => panic!("menu_slot_mark: no label for {other:?}"),
    }
}
