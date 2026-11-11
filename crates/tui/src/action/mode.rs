use super::*;

/// The interaction mode. Overlays are plain enum variants (not boxed trait
/// objects): adding one is a variant + a render arm.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Selection,
    Search(SearchState),
    Detail(DetailView),
    /// SQL source preview of a node (scrollable, syntax-coloured plain text).
    Sql(SqlView),
    /// Project stats dashboard (scrollable sections).
    Stats(StatsView),
    /// Baseline-diff summary (scrollable sections; needs a `--diff` baseline).
    Diff(DiffView),
    /// Column-lineage tree for one column (scrollable, Enter jumps to the
    /// row's model — Issue 09 C).
    Columns(ColumnView),
    /// Notification centre (`Ctrl-n`, Issue 08): the recent-notification log,
    /// scrollable, newest first.
    Notifications(NotificationsView),
    /// Command palette: a fuzzy finder over every Selection-mode command.
    Palette(PaletteState),
    /// The `y` yank-prefix which-key menu: any key dispatched here runs its
    /// yank action and closes the menu (one-shot); `Esc`/`q` cancel. `back`
    /// restores the overlay it was opened over (structure modal → `y` → Esc
    /// returns to the structure modal, not the panes); `sel` is the
    /// pointer-selectable row index (a click selects, a double-click runs).
    Yank {
        back: Option<Box<Mode>>,
        sel: usize,
    },
    /// The node context menu (Issue 06): right-click on a node or `a`.
    Menu(MenuState),
    /// The `?` keybindings overlay (scrollable, `/`-filterable; `back`
    /// restores the mode it was opened over — Issue 09).
    Help(HelpView),
    /// The home tab is ACTIVE (Issue 10, §3): the body draws the home screen
    /// instead of the three panes. NOT a modal — it is the idle mode while
    /// `tabs[0]` (the unclosable home tab) is active; overlays (help, stats,
    /// search) still stack on top and close back to it. The item cursor
    /// lives on `App::home_sel` so modal round-trips keep it.
    Home,
}

/// The mode discriminant, used for keymap lookup (the binding table keys on this,
/// not on the payload-carrying [`Mode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Selection,
    Search,
    Detail,
    Sql,
    Stats,
    Diff,
    Columns,
    Notifications,
    Palette,
    Yank,
    Menu,
    Help,
    Home,
}

impl Mode {
    pub fn kind(&self) -> ModeKind {
        match self {
            Mode::Selection => ModeKind::Selection,
            Mode::Search(_) => ModeKind::Search,
            Mode::Detail(_) => ModeKind::Detail,
            Mode::Sql(_) => ModeKind::Sql,
            Mode::Stats(_) => ModeKind::Stats,
            Mode::Diff(_) => ModeKind::Diff,
            Mode::Columns(_) => ModeKind::Columns,
            Mode::Notifications(_) => ModeKind::Notifications,
            Mode::Palette(_) => ModeKind::Palette,
            Mode::Yank { .. } => ModeKind::Yank,
            Mode::Menu(_) => ModeKind::Menu,
            Mode::Help(_) => ModeKind::Help,
            Mode::Home => ModeKind::Home,
        }
    }

    /// Whether pointer input is live in this mode: clicks route to the
    /// panes (Selection) or the modal/menu hit-test (everything else with a
    /// surface). `Search` keeps mouse inert — its query renders inline in
    /// the pane titles and typing owns the keys, so a stray click must not
    /// hijack the search.
    pub fn mouse_live(&self) -> bool {
        !matches!(self, Mode::Search(_))
    }
}
