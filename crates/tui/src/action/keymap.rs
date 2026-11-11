use super::*;

/// A single key that fires a binding. `Key` matches the `KeyCode` ignoring
/// modifiers (so `G` = Shift+g still matches); `Ctrl` matches Ctrl+char;
/// `Super` matches Cmd+char (delivered only by terminals speaking the kitty
/// keyboard protocol — `main` opts in at startup; a `Ctrl` fallback should
/// accompany every `Super` binding).
#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    Key(KeyCode),
    Ctrl(char),
    Super(char),
}

impl Trigger {
    pub(super) fn matches(self, key: KeyEvent) -> bool {
        match self {
            // A plain-key trigger must NOT fire on a chorded press: without
            // this, Ctrl-d would match the `d` binding (table order decided
            // the winner, so Ctrl-d / Ctrl-u could never mean paging). SHIFT
            // stays allowed — capitals arrive as Char('G') + SHIFT.
            Trigger::Key(code) => {
                key.code == code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
            }
            Trigger::Ctrl(c) => {
                key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(c)
            }
            Trigger::Super(c) => {
                key.modifiers.contains(KeyModifiers::SUPER) && key.code == KeyCode::Char(c)
            }
        }
    }

    /// Short human label for the `?` help.
    pub(super) fn label(self) -> String {
        match self {
            Trigger::Ctrl(c) => format!("Ctrl-{c}"),
            Trigger::Super(c) => format!("Cmd-{c}"),
            Trigger::Key(code) => match code {
                // Space prints as an invisible glyph; name it so `?` is legible.
                KeyCode::Char(' ') => "Space".into(),
                KeyCode::Char(c) => c.to_string(),
                KeyCode::Enter => "Enter".into(),
                KeyCode::Esc => "Esc".into(),
                KeyCode::Tab => "Tab".into(),
                KeyCode::BackTab => "S-Tab".into(),
                KeyCode::Backspace => "Bksp".into(),
                // ASCII names (arrow glyphs are ambiguous-width on CJK terminals).
                KeyCode::Up => "Up".into(),
                KeyCode::Down => "Down".into(),
                KeyCode::Left => "Left".into(),
                KeyCode::Right => "Right".into(),
                KeyCode::PageDown => "PgDn".into(),
                KeyCode::PageUp => "PgUp".into(),
                other => format!("{other:?}"),
            },
        }
    }
}

/// One keyboard binding: which mode it applies in, the keys that trigger it, the
/// resulting action, and the help text. The `?` overlay is generated from this.
pub struct KeyBinding {
    pub mode: ModeKind,
    pub triggers: &'static [Trigger],
    pub action: Action,
    pub help: &'static str,
    /// The home screen's Actions row: `true` marks this row eligible for
    /// the home "Actions" line — the row's `key_label()`/`help` render there
    /// verbatim, and [`crate::App::unavailable_reason`] still filters the unusable
    /// ones out (a baseline-less `D` never shows).
    pub home: bool,
    /// Status-line hint annotations: offer this row's action
    /// as a `{key} {desc}` hint while `ctx` is the on-screen context. Rows
    /// sharing `(ctx, prio, desc)` merge into ONE hint (`j` + `k` →
    /// `j/k move`); `prio` is the drop-priority — higher values shed first on
    /// a narrow line. An empty slice opts the row out of hints entirely.
    pub hints: &'static [Hint],
}

/// The on-screen context a status hint is offered in — the "situation"
/// column. `Any` emits in every context (the `?` escape hatch every screen
/// keeps); the pane contexts are Selection-mode only and supersede each
/// other: a live column trace or the whole-graph overview replaces the pane
/// key story with its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HintCtx {
    /// Every context (modal modes only ever emit these).
    Any,
    /// The model list owns the focus.
    List,
    /// The lineage pane owns the focus.
    Lineage,
    /// A column trace is live (`h/l` step columns there).
    Traced,
    /// The whole-graph overview (`w`) — the browser-like home page.
    Overview,
    /// The inspector pane owns the focus (the third pane).
    Inspector,
    /// The home tab is active (the `j/k move ⏎ open / search
    /// 1-9 tabs q quit` row).
    Home,
}

/// One hint annotation on a [`KeyBinding`] row (see [`KeyBinding::hints`]).
/// `desc` is the dim label after the accent key; `prio` orders the emitted
/// row left-to-right and decides the drop order (higher sheds first).
#[derive(Debug, Clone, Copy)]
pub struct Hint {
    pub prio: u8,
    pub ctx: HintCtx,
    pub desc: &'static str,
}

impl Hint {
    /// Shorthand constructors keep the one-line [`BINDINGS`] rows readable.
    pub const fn any(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Any,
            desc,
        }
    }
    /// Offer in the list-focus context.
    pub const fn list(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::List,
            desc,
        }
    }
    /// Offer in the lineage-focus context.
    pub const fn lineage(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Lineage,
            desc,
        }
    }
    /// Offer while a column trace is live.
    pub const fn traced(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Traced,
            desc,
        }
    }
    /// Offer in the whole-graph overview.
    pub const fn overview(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Overview,
            desc,
        }
    }
    /// Offer while the inspector pane owns the focus.
    pub const fn inspector(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Inspector,
            desc,
        }
    }
    /// Offer while the home tab is active.
    pub const fn home(prio: u8, desc: &'static str) -> Self {
        Hint {
            prio,
            ctx: HintCtx::Home,
            desc,
        }
    }
}

/// One status-line hint's data (its cell range lives in
/// [`StatusSpan`](crate::ui::StatusSpan)): the accent key part, the dim
/// description, and the [`Action`] a click dispatches. `action: None` marks
/// the loop's transient readouts (splitter hover/drag — the pointer IS the
/// gesture, so the text teaches rather than buttons). `prio` is the layout's
/// drop rank: higher sheds first on a narrow line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusHint {
    /// The accent-coloured key label (or the whole text for a transient).
    pub key: String,
    /// The dim description after the key (empty for transients).
    pub desc: String,
    /// The action a left click dispatches (`None` = a readout, not a button).
    pub action: Option<Action>,
    /// The drop rank — the row's right-to-left shed order is by this field.
    pub prio: u8,
}

/// The contextual status-line hints for `mode`/`focus`/`traced`/`overview` —
/// generated from [`BINDINGS`] (the single source: key labels come from
/// [`KeyBinding::key_label`]'s primary trigger, actions are the rows' own),
/// so the status line can never teach a key that doesn't work. Emission is
/// priority-sorted; bindings sharing `(ctx, prio, desc)` merge into one hint
/// (the `j`/`k` pair → `j/k move`) whose click action survives only while
/// every member agrees on it (a direction family dispatches nothing). Only
/// the hint's OWN row is listed — the event loop may REPLACE this list with
/// a transient readout (splitter hover/drag).
pub fn status_hints(mode: ModeKind, focus: Focus, traced: bool, overview: bool) -> Vec<StatusHint> {
    // Which contexts emit. Modal modes are context-free (`Any` only). In
    // Selection the focused pane's ctx emits — except a live column trace or
    // the overview, which supersede the pane story with their own (traced
    // wins over overview: a trace pins a rooted graph).
    let ctxs: &[HintCtx] = match mode {
        ModeKind::Selection => {
            if traced {
                // A live trace supersedes even the `?` escape hatch — the
                // traced row is a self-contained story (`h/l`, `K`, `Esc`).
                &[HintCtx::Traced]
            } else if overview {
                &[HintCtx::Overview]
            } else {
                match focus {
                    Focus::List => &[HintCtx::Any, HintCtx::List],
                    Focus::RightPane => &[HintCtx::Any, HintCtx::Lineage],
                    Focus::Inspector => &[HintCtx::Any, HintCtx::Inspector],
                }
            }
        }
        // The home row: `j/k move  ⏎ open  / search  1-9 tabs  q quit`.
        ModeKind::Home => &[HintCtx::Any, HintCtx::Home],
        _ => &[HintCtx::Any],
    };
    // Group rows by (ctx, prio, desc). `BTreeMap` iteration IS the priority
    // sort. A group's click action survives only while every member agrees
    // on it — a merged family (`j/k move`, `h/l step column`, `1-9 tabs`)
    // names no single command, so it goes inert rather than firing an
    // arbitrary member (`1-9` jumping to tab 1 would be a quiet surprise).
    type Groups = std::collections::BTreeMap<
        (u8, HintCtx, &'static str),
        (usize, Vec<String>, Option<Action>),
    >;
    let mut groups: Groups = Groups::new();
    for (i, b) in BINDINGS.iter().enumerate() {
        if b.mode != mode {
            continue;
        }
        for h in b.hints {
            if !ctxs.contains(&h.ctx) {
                continue;
            }
            let key = b.triggers.first().map(|t| t.label()).unwrap_or_default();
            let e = groups
                .entry((h.prio, h.ctx, h.desc))
                .or_insert_with(|| (i, Vec::new(), Some(b.action)));
            e.0 = e.0.min(i);
            e.1.push(key);
            if e.2 != Some(b.action) {
                e.2 = None;
            }
        }
    }
    groups
        .into_iter()
        .map(|((prio, _ctx, desc), (_first, mut keys, action))| {
            keys.sort();
            keys.dedup();
            StatusHint {
                // Compact the merged key part: single-char members form a
                // contiguous run (`1`-`9` → `1-9`) or join bare (`hjkl`),
                // pairs keep the `/` convention, mixed shapes join `/`.
                key: compact_keys(&keys),
                desc: desc.to_string(),
                action,
                prio,
            }
        })
        .collect()
}

/// Join a merged hint's member key labels: 3+ single-char labels
/// that form a contiguous ASCII range collapse to `a-z` (`1..9` → `1-9`), 3+
/// single chars join bare (`hjkl`), exactly two join `/` (`j/k`), and
/// anything else joins `/` (`Down/Up`). Deterministic — labels arrive sorted.
pub(super) fn compact_keys(keys: &[String]) -> String {
    let singles: Option<Vec<u8>> = keys
        .iter()
        .map(|k| (k.len() == 1).then(|| k.as_bytes()[0]))
        .collect();
    match singles {
        Some(cs) if cs.len() >= 3 && cs.windows(2).all(|w| w[1] == w[0] + 1) => {
            format!("{}-{}", cs[0] as char, cs[cs.len() - 1] as char)
        }
        Some(cs) if cs.len() >= 3 => cs.iter().map(|&c| c as char).collect(),
        _ => keys.join("/"),
    }
}

impl KeyBinding {
    /// The binding's trigger labels joined with `/` (e.g. `"Ctrl-p/Cmd-p"`) — the
    /// SAME formatting [`help_lines`] uses, reused by the command palette's
    /// right-aligned key column. `Trigger::label` stays private; this is the one
    /// public accessor for it.
    pub fn key_label(&self) -> String {
        self.triggers
            .iter()
            .map(|t| t.label())
            .collect::<Vec<_>>()
            .join("/")
    }
}

use Action as A;
use KeyCode::{BackTab, Backspace, Char, Down, Enter, Esc, Left, PageDown, PageUp, Right, Tab, Up};
use ModeKind as M;
use Trigger::{Ctrl, Key, Super};

/// THE binding table — the single source of truth for dispatch AND help.
/// Kept as a one-row-per-binding table (rustfmt would explode it to ~6 lines each).
#[rustfmt::skip]
pub static BINDINGS: &[KeyBinding] = &[
    // ---- Selection mode ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('j')), Key(Down)], action: A::MoveDown, help: "down (list / lineage cursor)", hints: &[Hint::list(10, "move"), Hint::lineage(10, "move"), Hint::overview(10, "move"), Hint::inspector(10, "scroll")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('k')), Key(Up)], action: A::MoveUp, help: "up (list / lineage cursor)", hints: &[Hint::list(10, "move"), Hint::lineage(10, "move"), Hint::overview(10, "move"), Hint::inspector(10, "scroll")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('h')), Key(Left)], action: A::MoveLeft, help: "lineage cursor left (upstream)", hints: &[Hint::lineage(10, "move"), Hint::traced(10, "step column")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('l')), Key(Right)], action: A::MoveRight, help: "lineage cursor right (downstream)", hints: &[Hint::lineage(10, "move"), Hint::traced(10, "step column")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('g'))], action: A::JumpTop, help: "jump to first model (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('G'))], action: A::JumpBottom, help: "jump to last model (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Tab)], action: A::ToggleFocus, help: "switch list / lineage focus", hints: &[Hint::list(50, "lineage"), Hint::inspector(30, "list")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Super('b'), Ctrl('b')], action: A::ToggleListPane, help: "show / hide the model list pane", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Enter)], action: A::DetailOpen, help: "open structure / re-root to cursor / toggle +more", hints: &[Hint::list(20, "open"), Hint::lineage(20, "re-root"), Hint::overview(20, "open"), Hint::inspector(20, "trace column")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('/'))], action: A::SearchOpen, help: "search (list filter / lineage jump)", hints: &[Hint::list(30, "search"), Hint::overview(30, "search")], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "toggle this help", hints: &[Hint::any(90, "help")], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('o'))], action: A::OpenEditor, help: "open SQL in $EDITOR", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('y'))], action: A::YankPrefix, help: "yank prefix menu", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('Y'))], action: A::YankName, help: "copy model name", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('m'))], action: A::YankMermaid, help: "copy lineage as Mermaid", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('x'))], action: A::YankDot, help: "copy lineage as Graphviz DOT", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('r'))], action: A::Reload, help: "reload manifest", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('z'))], action: A::Recenter, help: "cursor home + re-center lineage", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('u'))], action: A::ToggleUpstream, help: "toggle upstream side", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('d'))], action: A::ToggleDownstream, help: "toggle downstream side", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('['))], action: A::DepthDecrease, help: "limit lineage depth (-)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char(']'))], action: A::DepthIncrease, help: "widen lineage depth (+)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('0'))], action: A::ResetView, help: "reset lineage view", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('b'))], action: A::HistoryBack, help: "jump back (re-root history)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('f'))], action: A::HistoryForward, help: "jump forward (re-root history)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('q')), Ctrl('c')], action: A::Quit, help: "quit", hints: &[Hint::overview(80, "quit")], home: false },
    // ---- new feature commands: placed AFTER `quit` so `quit` stays inside the
    // first help page at 100x40 (help_overlay_renders_keybindings_from_keymap).
    // Reorder these past `quit` only if that test's TestBackend height grows. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('s'))], action: A::SqlOpen, help: "preview SQL", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('S'))], action: A::StatsOpen, help: "stats dashboard", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('t'))], action: A::CycleLens, help: "cycle lineage lens", hints: &[Hint::lineage(50, "lens")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char(' '))], action: A::BookmarkToggle, help: "toggle bookmark", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('\''))], action: A::BookmarkCycle, help: "cycle bookmarks", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('.'))], action: A::SortCycle, help: "cycle list sort", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('M'))], action: A::ToggleMinimap, help: "toggle minimap", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('v'))], action: A::ToggleDensity, help: "toggle compact lineage", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('p'), Super('p')], action: A::PaletteOpen, help: "command palette", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('c'))], action: A::YankAscii, help: "copy lineage as text diagram", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('C'))], action: A::YankSql, help: "copy raw SQL", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('i'))], action: A::YankImpact, help: "copy impact report (Markdown)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('e'))], action: A::ExportLineage, help: "export lineage to <name>_lineage.txt", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(PageDown), Ctrl('d')], action: A::PageDown, help: "page down (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(PageUp), Ctrl('u')], action: A::PageUp, help: "page up (list)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('n'))], action: A::GapNext, help: "next untested model (Lint lens: next lint finding)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('N'))], action: A::GapPrev, help: "previous untested model (Lint lens: previous lint finding)", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('"'))], action: A::BookmarkCycleBack, help: "cycle bookmarks backward", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('H'))], action: A::LineageLeftmost, help: "lineage cursor to first column", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('L'))], action: A::LineageRightmost, help: "lineage cursor to last column", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('T'))], action: A::ToggleUntestedFilter, help: "filter list: untested only", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('*'))], action: A::ToggleBookmarkFilter, help: "filter list: bookmarked only", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('F'))], action: A::ToggleLintFilter, help: "filter list: lint findings only", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('-'))], action: A::ToggleGroupFold, help: "fold/unfold model group", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('P'))], action: A::DbtParse, help: "run dbt parse (build manifest)", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('t')], action: A::ThemeCycle, help: "cycle color theme", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('D'))], action: A::DiffOpen, help: "diff vs baseline (--diff)", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('!'))], action: A::YankSelector, help: "copy dbt --select command", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('w'))], action: A::ToggleOverview, help: "toggle whole-graph overview", hints: &[], home: true },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('p'))], action: A::PathToggle, help: "anchor/clear two-node path", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('K'))], action: A::ColumnsOpen, help: "column lineage modal (traced column)", hints: &[Hint::traced(20, "column modal")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('a'))], action: A::MenuOpen, help: "node actions menu", hints: &[Hint::lineage(40, "actions")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Esc)], action: A::ColumnFocusExit, help: "exit column focus", hints: &[Hint::traced(30, "exit trace")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('<'))], action: A::PaneNarrow, help: "narrow pane 4 cols", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('>'))], action: A::PaneWiden, help: "widen pane 4 cols", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('='))], action: A::PaneWidthsReset, help: "reset pane widths", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Ctrl('n')], action: A::NotificationsOpen, help: "notification center", hints: &[], home: false },
    // ---- tabs: `O` opens the cursor's model in a new tab,
    // `X`/Ctrl-w closes, `{`/`}` cycle, `1`-`9` jump straight to a tab. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('O'))], action: A::TabOpen, help: "open node in new tab", hints: &[Hint::list(40, "new tab"), Hint::lineage(30, "new tab")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('X')), Ctrl('w')], action: A::TabClose, help: "close tab", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('{'))], action: A::TabPrev, help: "previous tab", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('}'))], action: A::TabNext, help: "next tab", hints: &[], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('1'))], action: A::TabActivate(1), help: "go to tab 1", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('2'))], action: A::TabActivate(2), help: "go to tab 2", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('3'))], action: A::TabActivate(3), help: "go to tab 3", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('4'))], action: A::TabActivate(4), help: "go to tab 4", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('5'))], action: A::TabActivate(5), help: "go to tab 5", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('6'))], action: A::TabActivate(6), help: "go to tab 6", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('7'))], action: A::TabActivate(7), help: "go to tab 7", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('8'))], action: A::TabActivate(8), help: "go to tab 8", hints: &[Hint::overview(40, "tabs")], home: false },
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('9'))], action: A::TabActivate(9), help: "go to tab 9", hints: &[Hint::overview(40, "tabs")], home: false },
    // ---- inspector: `I` cycles the pane's visibility pref. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('I'))], action: A::ToggleInspector, help: "inspector pane auto / on / off", hints: &[], home: false },
    // ---- home tab: `~` jumps to the fixed left tab. ----
    KeyBinding { mode: M::Selection, triggers: &[Key(Char('~'))], action: A::HomeGo, help: "go to home tab", hints: &[], home: false },
    // ---- Yank prefix mode (`y` + one key): the second key is the SAME letter
    // the single-key binding always used, so muscle memory carries over as
    // `ym`/`yC`/…; `yy` copies the unique_id (what bare `y` used to do). ----
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('y'))], action: A::YankId, help: "copy unique_id", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('Y'))], action: A::YankName, help: "copy model name", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('m'))], action: A::YankMermaid, help: "copy lineage as Mermaid", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('x'))], action: A::YankDot, help: "copy lineage as Graphviz DOT", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('c'))], action: A::YankAscii, help: "copy lineage as text diagram", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('C'))], action: A::YankSql, help: "copy raw SQL", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('s'))], action: A::YankCompiledSql, help: "copy compiled SQL", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('i'))], action: A::YankImpact, help: "copy impact report (Markdown)", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('l'))], action: A::YankColumnLineage, help: "copy column lineage (Markdown)", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('!'))], action: A::YankSelector, help: "copy dbt --select command", hints: &[], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "cancel", hints: &[Hint::any(80, "cancel")], home: false },
    KeyBinding { mode: M::Yank, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    // ---- Search mode (printable chars are handled dynamically -> SearchType) ----
    KeyBinding { mode: M::Search, triggers: &[Key(Esc)], action: A::SearchCancel, help: "cancel search", hints: &[Hint::any(80, "cancel")], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Enter)], action: A::SearchConfirm, help: "confirm", hints: &[Hint::any(20, "confirm")], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Backspace)], action: A::SearchBackspace, help: "delete char", hints: &[], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Up)], action: A::SearchMove(Direction::Up), help: "previous match", hints: &[Hint::any(10, "match")], home: false },
    KeyBinding { mode: M::Search, triggers: &[Key(Down)], action: A::SearchMove(Direction::Down), help: "next match", hints: &[Hint::any(10, "match")], home: false },
    // ---- Command palette (printable chars are handled dynamically -> SearchType;
    // the editing actions REUSE the Search* arms, routed by mode in apply_action) ----
    KeyBinding { mode: M::Palette, triggers: &[Key(Esc)], action: A::SearchCancel, help: "close palette", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Enter)], action: A::SearchConfirm, help: "run command", hints: &[Hint::any(20, "run")], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Backspace)], action: A::SearchBackspace, help: "delete char", hints: &[], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Up)], action: A::SearchMove(Direction::Up), help: "prev command", hints: &[Hint::any(10, "pick")], home: false },
    KeyBinding { mode: M::Palette, triggers: &[Key(Down)], action: A::SearchMove(Direction::Down), help: "next command", hints: &[Hint::any(10, "pick")], home: false },
    // ---- Detail modal ----
    KeyBinding { mode: M::Detail, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    // `?` opens help over the modal (HelpView.back restores it) — the
    // structure-modal section then leads the layout.
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    // Enter/`c` commits the column under the h/l cursor to column focus (the
    // arm closes the modal instead when the node has no columns — the classic
    // Enter-to-close survives there). While the tests section owns the cursor
    // (Tab), Enter re-roots to the relationships test's `to:` target instead.
    KeyBinding { mode: M::Detail, triggers: &[Key(Enter), Key(Char('c'))], action: A::ColumnFocusEnter, help: "trace column / jump to test target", hints: &[Hint::any(50, "trace")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Tab), Key(BackTab)], action: A::DetailSectionToggle, help: "columns / tests section", hints: &[Hint::any(40, "sections")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('K'))], action: A::ColumnsOpen, help: "column lineage modal (this column)", hints: &[Hint::any(60, "column modal")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('h')), Key(Char('H'))], action: A::DetailColumnMove(Direction::Up), help: "previous column / test", hints: &[Hint::any(30, "column")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('l')), Key(Char('L'))], action: A::DetailColumnMove(Direction::Down), help: "next column / test", hints: &[Hint::any(30, "column")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('y'))], action: A::YankPrefix, help: "yank prefix menu", hints: &[Hint::any(70, "yank")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('a'))], action: A::MenuOpen, help: "node actions menu", hints: &[Hint::any(75, "actions")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Detail, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- SQL preview modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Sql, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('c'))], action: A::SqlToggleCompiled, help: "toggle raw / compiled SQL", hints: &[Hint::any(30, "compiled")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('a'))], action: A::MenuOpen, help: "node actions menu", hints: &[Hint::any(40, "actions")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Sql, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Stats dashboard modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Stats, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Stats, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Diff summary modal (reuses DetailClose + DetailScroll) ----
    KeyBinding { mode: M::Diff, triggers: &[Key(Esc), Key(Enter), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('e'))], action: A::ExportPrImpact, help: "export PR impact report (Markdown)", hints: &[Hint::any(30, "export")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('y'))], action: A::YankPrSelector, help: "copy dbt build --select (column-narrowed)", hints: &[Hint::any(40, "copy selector")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('Y'))], action: A::YankPrSelectorWide, help: "copy dbt build --select (wide)", hints: &[], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Diff, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Column lineage modal: j/k move the ROW cursor (a
    // selection the loop scroll-follows), d/u/g/G page the viewport like every
    // other modal, Enter re-roots to the row's model. ----
    KeyBinding { mode: M::Columns, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Enter)], action: A::ColumnJump, help: "jump to this model", hints: &[Hint::any(30, "open")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('j')), Key(Down)], action: A::ColumnRowMove(Direction::Down), help: "next column row", hints: &[Hint::any(10, "row")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('k')), Key(Up)], action: A::ColumnRowMove(Direction::Up), help: "previous column row", hints: &[Hint::any(10, "row")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Columns, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Notification centre (reuses DetailClose + DetailScroll) ----
    // `Ctrl-n` opens it from Selection and toggles it closed while open.
    KeyBinding { mode: M::Notifications, triggers: &[Key(Esc), Key(Enter), Key(Char('q')), Ctrl('n')], action: A::DetailClose, help: "close", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Notifications, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // ---- Node context menu: only nav / run / close live here —
    // an item's OWN shortcut (the trigger of the BINDINGS row its layout
    // slot names) is resolved by the dispatcher's item lookup, never a
    // fixed row. ----
    KeyBinding { mode: M::Menu, triggers: &[Key(Char('j')), Key(Down)], action: A::MenuMove(Direction::Down), help: "next item", hints: &[Hint::any(10, "move")], home: false },
    KeyBinding { mode: M::Menu, triggers: &[Key(Char('k')), Key(Up)], action: A::MenuMove(Direction::Up), help: "previous item", hints: &[Hint::any(10, "move")], home: false },
    KeyBinding { mode: M::Menu, triggers: &[Key(Enter)], action: A::MenuConfirm, help: "run the selected action", hints: &[Hint::any(20, "run")], home: false },
    KeyBinding { mode: M::Menu, triggers: &[Key(Esc), Key(Char('q'))], action: A::DetailClose, help: "close menu", hints: &[Hint::any(80, "close")], home: false },
    // ---- Help overlay ----
    KeyBinding { mode: M::Help, triggers: &[Key(Esc), Key(Char('?')), Key(Char('q'))], action: A::HelpToggle, help: "close help", hints: &[Hint::any(80, "close")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('j')), Key(Down)], action: A::DetailScroll(Direction::Down), help: "scroll down", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('k')), Key(Up)], action: A::DetailScroll(Direction::Up), help: "scroll up", hints: &[Hint::any(10, "scroll")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('d')), Key(PageDown), Ctrl('d')], action: A::DetailScrollPage(Direction::Down), help: "page down", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('u')), Key(PageUp), Ctrl('u')], action: A::DetailScrollPage(Direction::Up), help: "page up", hints: &[Hint::any(15, "page")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('g'))], action: A::DetailScrollHome, help: "jump to top", hints: &[Hint::any(20, "jump")], home: false },
    KeyBinding { mode: M::Help, triggers: &[Key(Char('G'))], action: A::DetailScrollEnd, help: "jump to bottom", hints: &[Hint::any(20, "jump")], home: false },
    // `/` opens the line filter. While the filter is being edited, printable
    // keys are TEXT (they route through the Search* editing actions — the same
    // reuse the palette/search rows use), so `?`/`q` close is suspended until
    // the filter is committed (Enter) or cleared (Esc); see dispatch().
    KeyBinding { mode: M::Help, triggers: &[Key(Char('/'))], action: A::SearchOpen, help: "filter help lines", hints: &[Hint::any(30, "filter")], home: false },
    // ---- Home tab: j/k move over model items, Enter
    // opens (activate-or-new-tab), `/` types into the search field, `Esc`
    // lives in the Search rows. The Actions-row commands rebind here so the
    // same keys work while the home tab is active. ----
    KeyBinding { mode: M::Home, triggers: &[Key(Char('j')), Key(Down)], action: A::HomeMove(Direction::Down), help: "next item", hints: &[Hint::home(10, "move")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('k')), Key(Up)], action: A::HomeMove(Direction::Up), help: "previous item", hints: &[Hint::home(10, "move")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Enter)], action: A::HomeOpen, help: "open item in a tab", hints: &[Hint::home(20, "open")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('/'))], action: A::SearchOpen, help: "search models", hints: &[Hint::home(30, "search")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('1'))], action: A::TabActivate(1), help: "go to tab 1", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('2'))], action: A::TabActivate(2), help: "go to tab 2", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('3'))], action: A::TabActivate(3), help: "go to tab 3", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('4'))], action: A::TabActivate(4), help: "go to tab 4", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('5'))], action: A::TabActivate(5), help: "go to tab 5", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('6'))], action: A::TabActivate(6), help: "go to tab 6", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('7'))], action: A::TabActivate(7), help: "go to tab 7", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('8'))], action: A::TabActivate(8), help: "go to tab 8", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('9'))], action: A::TabActivate(9), help: "go to tab 9", hints: &[Hint::home(40, "tabs")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('{'))], action: A::TabPrev, help: "previous tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('}'))], action: A::TabNext, help: "next tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('~'))], action: A::HomeGo, help: "go to home tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('X')), Ctrl('w')], action: A::TabClose, help: "close tab", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('w'))], action: A::ToggleOverview, help: "toggle whole-graph overview", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('S'))], action: A::StatsOpen, help: "stats dashboard", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('D'))], action: A::DiffOpen, help: "diff vs baseline (--diff)", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('P'))], action: A::DbtParse, help: "run dbt parse (build manifest)", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Ctrl('n')], action: A::NotificationsOpen, help: "notification center", hints: &[], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('?'))], action: A::HelpToggle, help: "toggle this help", hints: &[Hint::any(90, "help")], home: false },
    KeyBinding { mode: M::Home, triggers: &[Key(Char('q')), Ctrl('c')], action: A::Quit, help: "quit", hints: &[Hint::home(80, "quit")], home: false },
];
