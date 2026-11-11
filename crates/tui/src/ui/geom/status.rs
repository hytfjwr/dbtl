use super::*;

// ---------------------------------------------------------------------------
// Status line + pane title placement: ONE pure calculation
// per surface — `draw_status`/`draw_lineage_pane` render from it and
// `hit_target` routes clicks through it, so a cell can never hit a span the
// screen did not draw (nor draw one it cannot hit).
// ---------------------------------------------------------------------------

/// Which droppable status chip a [`StatusSpan`] renders — the click-dispatch
/// key. The declaration order IS the drop order: `Impact` is the headline
/// (sheds last), `View` sheds first, and the `!N` notification badge sits
/// right after `Impact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusChip {
    /// The selected node's blast radius (`impact ↓1 ↑2`) — inert.
    Impact,
    /// Unread-warning badge (`!N`) — opens the notification centre.
    Notify,
    /// The live column trace (`column:m.c ↑4 ↓0`) — opens the column modal.
    Column,
    /// Baseline-diff summary (`diff +A ~M -R`) — opens the diff modal.
    Diff,
    /// `run_results` coverage/freshness (`run 12/34 @…`) — switches to the
    /// RunStatus lens.
    Run,
    /// The package-scope tag (`scope:root`) — inert.
    Scope,
    /// Coverage-lens percentage (`cov 42% (19/45)`) — toggles the lens.
    Coverage,
    /// Bookmark count (`bm:N`) — jumps to the next bookmark.
    Bookmarks,
    /// The persistent list filter (`marked`/`untested`) — inert readout
    /// (the chip table gives it no click action; `T`/`*` untoggle it).
    Filter,
    /// Non-default list sort (`sort:tests`) — cycles the sort.
    Sort,
    /// Cursor position (`12/45`) — inert.
    Position,
    /// The focused pane (`list`/`lineage`) — toggles focus.
    Focus,
    /// The rooted-view tag (`↑↓ ≤3`/`overview`) — resets the view, or leaves
    /// the overview when that is the current view.
    View,
}

/// One status-line span's placement — a hint on the left or a chip on the
/// right — as a status-rect-relative cell range `[x0, x1)` plus its
/// [`HitTarget`]. Owned data (copied from the hint/chip inputs) so the layout
/// outlives the frame's borrows — the loop stashes it for the hit-test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSpan {
    /// First covered cell (0-based, relative to the status rect's x).
    pub x0: u16,
    /// One past the last covered cell.
    pub x1: u16,
    /// What a click on this span resolves to.
    pub target: HitTarget,
}

/// The pure status-line placement: contextual hints left-anchored,
/// status chips right-anchored, both droppable at their low-priority ends.
/// `draw_status` renders exactly these spans; `hit_target` maps a cell to the
/// same ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusLayout {
    /// The kept hint spans (left→right), each `HitTarget::StatusHint`.
    pub hints: Vec<StatusSpan>,
    /// The kept chip spans (left→right, right-aligned as a block), each
    /// `HitTarget::StatusChip`.
    pub chips: Vec<StatusSpan>,
}

/// The display-cell width one hint occupies: ` {key}[ {desc}]`.
fn hint_w(h: &crate::action::StatusHint) -> u16 {
    use unicode_width::UnicodeWidthStr;
    (1 + UnicodeWidthStr::width(h.key.as_str())
        + if h.desc.is_empty() {
            0
        } else {
            1 + UnicodeWidthStr::width(h.desc.as_str())
        }) as u16
}

/// The display-cell width one chip occupies: ` {label}` — the label arrives
/// already bracketed (`[!3]`, `[cov 42%]`).
fn chip_w(label: &str) -> u16 {
    use unicode_width::UnicodeWidthStr;
    (1 + UnicodeWidthStr::width(label)) as u16
}

/// Place `hints` + `chips` on a `width`-column status line. The inputs
/// arrive PRIORITY-SORTED (highest-survival first): a too-narrow line sheds
/// the hints' tail first, then the chips' tail — the kept spans tile `[0,
/// hints_w)` left-to-right and `[width - chips_w, width)` right-aligned.
/// Pure — this is the single measurement both drawing and hit-testing use.
pub fn status_layout(
    width: u16,
    hints: &[crate::action::StatusHint],
    chips: &[(StatusChip, String)],
) -> StatusLayout {
    // The shed order on a short line: the HINTS' tail first (lowest
    // priority out), then — only once the hints are gone — the chips' tail.
    // The kept set is always a PREFIX of each priority-sorted input.
    let mut kept_h = hints.len();
    let mut kept_c = chips.len();
    let fits = |h: usize, c: usize| -> bool {
        hints[..h].iter().map(hint_w).sum::<u16>()
            + chips[..c].iter().map(|(_, l)| chip_w(l)).sum::<u16>()
            <= width
    };
    while !fits(kept_h, kept_c) {
        if kept_h > 0 {
            kept_h -= 1;
        } else if kept_c > 0 {
            kept_c -= 1;
        } else {
            break;
        }
    }
    let kept_hints = &hints[..kept_h];
    let kept_chips = &chips[..kept_c];
    let chips_w: u16 = kept_chips.iter().map(|(_, l)| chip_w(l)).sum();

    let mut x = 0u16;
    let hints_placed = kept_hints
        .iter()
        .map(|h| {
            let w = hint_w(h);
            let span = StatusSpan {
                x0: x,
                x1: x + w,
                target: HitTarget::StatusHint(h.action),
            };
            x += w;
            span
        })
        .collect();
    // Chips are a RIGHT-anchored block: the block's left edge is
    // `width - chips_w` and the spans tile inside it in priority order.
    let mut cx = width.saturating_sub(chips_w);
    let chips_placed = kept_chips
        .iter()
        .map(|(chip, label)| {
            let w = chip_w(label);
            let span = StatusSpan {
                x0: cx,
                x1: cx + w,
                target: HitTarget::StatusChip(*chip),
            };
            cx += w;
            span
        })
        .collect();
    StatusLayout {
        hints: hints_placed,
        chips: chips_placed,
    }
}

/// Which clickable span of a pane title a [`TitleSpan`] marks — currently
/// the lineage title's breadcrumb entries and its `[{v}]` / `[lens:…]` tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleKind {
    /// A breadcrumb entry `steps` re-root hops back: 1 = the direct parent,
    /// `n` = an earlier ancestor. A click replays `App::history_back` that
    /// many times — the same behaviour `b` gives, no second history model.
    Crumb(usize),
    /// The `[{v}]` view tag (`↑ ≤3`/`overview`) — a click resets the rooted
    /// view filters, the `0` key's [`Action::ResetView`](crate::Action::ResetView).
    View,
    /// The `[lens:…]` tag — a click cycles the lineage lens (`t`).
    Lens,
}

/// One title span's placement: the cell range `[x0, x1)` it occupies inside
/// the pane's top border, measured from the title TEXT's first cell (the
/// pane's `x + 1` — Block titles start one cell in from the corner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleSpan {
    /// What a click here dispatches.
    pub kind: TitleKind,
    /// First covered cell.
    pub x0: u16,
    /// One past the last covered cell.
    pub x1: u16,
}

/// The pure lineage-title placement — the composed text plus the cell ranges
/// of its clickable parts. `draw_lineage_pane` sets `text` verbatim (byte-
/// identical to the old `compose_lineage_title` output); `hit_target` maps a
/// title-row cell through `spans`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleLayout {
    /// The full title text as the Block draws it.
    pub text: String,
    /// The clickable spans in display order.
    pub spans: Vec<TitleSpan>,
}
