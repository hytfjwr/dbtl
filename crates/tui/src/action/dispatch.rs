use super::*;

/// Map `(Mode, KeyEvent)` to an [`Action`], or `None` if the key is unbound.
///
/// Ctrl-C always quits (any mode). While the help `/`-filter is being edited,
/// printable/Backspace/Enter keys route to the `Search*` editing actions BEFORE
/// the table. Then [`BINDINGS`] is consulted for the mode; finally, in Search
/// or Palette mode, any leftover plain printable char becomes
/// [`Action::SearchType`].
pub fn dispatch(mode: &Mode, key: KeyEvent) -> Option<Action> {
    // Global: Ctrl-C quits regardless of mode (never a printable char).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    // While the help `/`-filter is being edited, printable text types INTO it
    // and Backspace/Enter edit/commit it — the same "printable is text"
    // contract Search/Palette live by, reusing the Search* actions. This runs
    // BEFORE the BINDINGS table so `j`/`q`/`?`/`g`/`/` edit the filter instead
    // of scrolling or closing; Esc isn't printable, so it still reaches
    // HelpToggle (which the reducer reads as "clear the filter first").
    if let Mode::Help(h) = mode {
        if h.editing {
            match key.code {
                KeyCode::Char(c) => {
                    let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::SUPER);
                    if plain {
                        return Some(Action::SearchType(c));
                    }
                }
                KeyCode::Backspace => return Some(Action::SearchBackspace),
                KeyCode::Enter => return Some(Action::SearchConfirm),
                _ => {}
            }
        }
    }
    let mk = mode.kind();
    for b in BINDINGS {
        if b.mode == mk && b.triggers.iter().any(|t| t.matches(key)) {
            return Some(b.action);
        }
    }
    // Dynamic: in search OR the command palette, printable text (no Ctrl/Alt/
    // Super) edits the query — so a printable like 'q' filters and never quits.
    // Super-modified chars are commands, not text — they can actually arrive
    // once the kitty keyboard protocol is active.
    if mk == ModeKind::Search || mk == ModeKind::Palette {
        if let KeyCode::Char(c) = key.code {
            let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && !key.modifiers.contains(KeyModifiers::SUPER);
            if plain {
                return Some(Action::SearchType(c));
            }
        }
    }
    // Dynamic: inside the node menu, an item's OWN shortcut invokes it
    // directly (the trigger of the BINDINGS row its layout slot resolved to —
    // `s` runs SQL preview, `Space` bookmarks, without moving the cursor).
    // The nav/close keys all matched M::Menu rows above, so only item keys
    // reach here.
    if let Mode::Menu(ms) = mode {
        for (i, entry) in ms.items.iter().enumerate() {
            if let MenuEntry::Item { binding, .. } = entry {
                if BINDINGS[*binding].triggers.iter().any(|t| t.matches(key)) {
                    return Some(Action::MenuInvoke(i));
                }
            }
        }
    }
    None
}

/// The [`Action`] `Esc` produces in `mode` — resolved through [`BINDINGS`], so
/// an outside click sends literally the same action the key sends (the
/// same-as-Esc rule). `None` for modes with no `Esc` trigger.
pub fn esc_action(mode: &Mode) -> Option<Action> {
    let mk = mode.kind();
    BINDINGS
        .iter()
        .find(|b| {
            b.mode == mk
                && b.triggers
                    .iter()
                    .any(|t| matches!(t, Trigger::Key(KeyCode::Esc)))
        })
        .map(|b| b.action)
}

/// The yank menu's BINDINGS rows (`ModeKind::Yank`, in table order) —
/// the single enumeration the yank menu's renderer, the event loop's click→row
/// map, and the tests all share, so a new `y<key>` binding rows every surface
/// at once.
pub fn yank_bindings() -> impl Iterator<Item = &'static KeyBinding> {
    BINDINGS.iter().filter(|b| b.mode == ModeKind::Yank)
}

/// One line of the `?` help overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpLine {
    pub mode: ModeKind,
    pub keys: String,
    pub desc: String,
}

/// Synthetic help for actions NOT bound to a fixed key (text input, mouse).
/// These are hand-maintained — add a row whenever a mode gains key-free
/// input behaviour so the `?` overlay documents it.
static DYNAMIC_HELP: &[(ModeKind, &str, &str)] = &[
    (ModeKind::Search, "a-z..", "type to filter / search"),
    (ModeKind::Palette, "a-z..", "type to filter commands"),
    (ModeKind::Palette, "@name", "search models / re-root"),
    (ModeKind::Help, "a-z..", "type to filter help lines"),
    // The pointer table: clicks confirm on release; a second click on the
    // same cell inside 400ms is a double-click.
    (
        ModeKind::Selection,
        "click",
        "list row selects · node moves cursor · tab / chip / hint / title / minimap act",
    ),
    (
        ModeKind::Selection,
        "double-click",
        "node commits (re-root / structure / expand) · list row: structure · border: reset",
    ),
    (
        ModeKind::Selection,
        "right click",
        "node actions menu (list row / lineage node)",
    ),
    (
        ModeKind::Selection,
        "middle click",
        "list row / node opens in a new tab · tab closes",
    ),
    (
        ModeKind::Selection,
        "drag",
        "pan lineage · resize a pane border · scrub the minimap",
    ),
    (
        ModeKind::Selection,
        "hover",
        "accent node + direct edges · rest 0.5s for a tooltip",
    ),
    (
        ModeKind::Selection,
        "wheel",
        "list selects · lineage pans (Shift: sideways) · inspector scrolls · strip scrolls",
    ),
    (
        ModeKind::Home,
        "click / wheel",
        "open the row / move the item cursor",
    ),
    (
        ModeKind::Menu,
        "item key",
        "an item's own key runs it directly",
    ),
    (
        ModeKind::Menu,
        "click item",
        "run the action (outside click closes)",
    ),
    (
        ModeKind::Yank,
        "mouse",
        "row click picks · double-click yanks · outside closes",
    ),
    (
        ModeKind::Palette,
        "mouse",
        "row click picks · double-click runs · wheel scrolls · outside closes",
    ),
    (
        ModeKind::Columns,
        "mouse",
        "row click picks · double-click jumps · wheel scrolls · outside closes",
    ),
    (
        ModeKind::Detail,
        "mouse",
        "wheel scrolls · outside click closes",
    ),
    (
        ModeKind::Sql,
        "mouse",
        "wheel scrolls · outside click closes",
    ),
    (
        ModeKind::Stats,
        "mouse",
        "wheel scrolls · outside click closes",
    ),
    (
        ModeKind::Diff,
        "mouse",
        "wheel scrolls · outside click closes",
    ),
    (
        ModeKind::Notifications,
        "mouse",
        "wheel scrolls · outside click closes",
    ),
    (
        ModeKind::Help,
        "mouse",
        "wheel scrolls · outside click closes",
    ),
];

/// All help lines for the `?` overlay, derived from [`BINDINGS`] plus the
/// synthetic dynamic entries. Single source of truth → cannot drift from
/// [`dispatch`].
pub fn help_lines() -> Vec<HelpLine> {
    let mut lines: Vec<HelpLine> = BINDINGS
        .iter()
        .map(|b| HelpLine {
            mode: b.mode,
            keys: b.key_label(),
            desc: b.help.to_string(),
        })
        .collect();
    for (mode, keys, desc) in DYNAMIC_HELP {
        lines.push(HelpLine {
            mode: *mode,
            keys: (*keys).to_string(),
            desc: (*desc).to_string(),
        });
    }
    lines
}

/// The command-palette candidates for `query`: every Selection-mode binding
/// (excluding [`Action::PaletteOpen`] itself), filtered by a fuzzy subsequence
/// match. The match runs over the binding's `help` text first, falling back to
/// its key label ([`KeyBinding::key_label`]) — so both "minimap" and "Ctrl-p"
/// can find a row. An empty query lists ALL candidates.
///
/// Order is [`BINDINGS`] table order, preserved by the single-pass filter (no
/// sort) — so the palette is deterministic and mirrors the help layout. The
/// single source of palette candidates: it reads [`BINDINGS`], so the palette can
/// never drift from the keymap.
pub fn palette_candidates(query: &str) -> Vec<&'static KeyBinding> {
    // `match_indices` returns empty for BOTH an empty query AND a non-match, so
    // the empty-query case must be handled explicitly or the palette opens blank.
    let empty = query.trim().is_empty();
    BINDINGS
        .iter()
        .filter(|b| b.mode == ModeKind::Selection && b.action != Action::PaletteOpen)
        .filter(|b| {
            empty
                || !crate::model_list::match_indices(b.help, query).is_empty()
                || !crate::model_list::match_indices(&b.key_label(), query).is_empty()
        })
        .collect()
}

/// "This action reshapes or recentres the lineage without moving the
/// selection" — the event loop forces the anchor branch after it (the loop's
/// dispatched-key check AND the `Outcome.reanchor` flag a palette-confirmed
/// command carries: one list, no drift between the key and palette paths).
pub fn reanchors_lineage(action: Action) -> bool {
    matches!(
        action,
        Action::Recenter
            | Action::ToggleUpstream
            | Action::ToggleDownstream
            | Action::DepthDecrease
            | Action::DepthIncrease
            | Action::ResetView
            | Action::Reload
            | Action::ToggleListPane
            | Action::ToggleInspector
            | Action::DbtParse
            | Action::ToggleDensity
            | Action::ToggleOverview
            | Action::ColumnFocusEnter
            | Action::ColumnFocusExit
    )
}

/// Resolve a persisted palette-recent NAME back to its live [`BINDINGS`] row.
/// Recents are stored as `Action`'s `Debug` name (`"ToggleMinimap"`); a name no
/// longer present in the Selection-mode keymap (renamed/removed action) drops
/// rather than pointing at a dead row — the Recent section mirrors [`BINDINGS`],
/// it never becomes a second command source.
pub fn palette_binding_named(name: &str) -> Option<&'static KeyBinding> {
    BINDINGS.iter().find(|b| {
        b.mode == ModeKind::Selection
            && b.action != Action::PaletteOpen
            && format!("{:?}", b.action) == name
    })
}
