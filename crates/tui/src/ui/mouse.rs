//! Mouse gesture detection: raw press/release/move events plus a clock in,
//! semantic gestures out. The event loop owns both crossterm and the wall
//! clock — it feeds [`MouseInput`]s with an `Instant` and routes the produced
//! [`MouseOutcome`]s — so this state machine stays pure and headlessly
//! testable (no `Instant::now()` inside).
//!
//! The rules (screen-spec §6):
//! - a **click** is confirmed on RELEASE, only when the pointer stayed on the
//!   press cell (sub-cell movement can't be observed anyway — coordinates are
//!   whole cells);
//! - a **drag** begins when the pointer moves ≥1 cell with a button held, and
//!   ends on release;
//! - a **double-click** is a click that lands on the same cell within
//!   [`DOUBLE_CLICK`] of the previous click.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::Position;

use super::HitTarget;

/// Two clicks on the same cell within this window count as a double-click.
/// 400ms is the classic X11/Windows default.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// A raw pointer event fed to [`MouseRouter::feed`]. Translated from
/// `MouseEventKind` at the loop boundary — scroll kinds are not gestures
/// (they route straight to the wheel handlers) so they produce `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseInput {
    /// A button went down at `(col, row)`.
    Press {
        /// Which button.
        button: MouseButton,
        /// Press column.
        col: u16,
        /// Press row.
        row: u16,
    },
    /// A button came up at `(col, row)`.
    Release {
        /// Which button.
        button: MouseButton,
        /// Release column.
        col: u16,
        /// Release row.
        row: u16,
    },
    /// The pointer moved to `(col, row)` with a button held (`Drag` events).
    /// The held button is the tracked press's — the reported one is ignored:
    /// terminals deliver one Drag kind per motion, and single-button drags
    /// are the only supported gesture.
    Drag {
        /// Current column.
        col: u16,
        /// Current row.
        row: u16,
    },
    /// The pointer moved to `(col, row)` with NO button held (`Moved` events —
    /// plain hover).
    Move {
        /// Current column.
        col: u16,
        /// Current row.
        row: u16,
    },
}

impl MouseInput {
    /// Translate a crossterm [`MouseEventKind`] at `(col, row)`. Scroll kinds
    /// (vertical AND horizontal) return `None` — they are wheel notches, not
    /// gestures, and the loop routes them to its wheel helpers.
    pub fn from_kind(kind: MouseEventKind, col: u16, row: u16) -> Option<MouseInput> {
        match kind {
            MouseEventKind::Down(button) => Some(MouseInput::Press { button, col, row }),
            MouseEventKind::Up(button) => Some(MouseInput::Release { button, col, row }),
            MouseEventKind::Drag(_) => Some(MouseInput::Drag { col, row }),
            MouseEventKind::Moved => Some(MouseInput::Move { col, row }),
            _ => None,
        }
    }
}

/// A recognized gesture. `at` positions are screen cells; drag outcomes also
/// carry `origin` (the press cell) so a consumer can compute the full delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseOutcome {
    /// A completed click: press and release on the same cell.
    Click {
        /// Which button.
        button: MouseButton,
        /// The clicked cell.
        at: Position,
    },
    /// A second click on the same cell within [`DOUBLE_CLICK`].
    DoubleClick {
        /// Which button.
        button: MouseButton,
        /// The clicked cell.
        at: Position,
    },
    /// A drag began: the pointer left the press cell with the button held.
    /// `at` is where the pointer is now (≥1 cell from `origin`).
    DragStart {
        /// Which button is held.
        button: MouseButton,
        /// The press cell.
        origin: Position,
        /// Current position.
        at: Position,
    },
    /// The pointer moved to another cell mid-drag.
    DragMove {
        /// The press cell.
        origin: Position,
        /// Current position.
        at: Position,
    },
    /// The button was released, ending the drag.
    DragEnd {
        /// Which button was held.
        button: MouseButton,
        /// The press cell.
        origin: Position,
        /// Release position.
        at: Position,
    },
    /// The pointer hovered a new cell with no button held. The loop resolves
    /// it to a [`HitTarget`] before deciding to redraw.
    Hover {
        /// The hovered cell.
        at: Position,
    },
}

/// The tracked press: button, cell, and time.
#[derive(Debug, Clone, Copy)]
struct Press {
    button: MouseButton,
    at: Position,
    when: Instant,
}

/// Gesture state between raw events: the live press (and whether it has
/// crossed the drag threshold), the last click (the double-click window
/// reference), and the last reported hover target (the redraw gate).
#[derive(Debug, Default)]
pub struct MouseRouter {
    /// The button currently held (`Some` between `Down` and `Up`). A second
    /// `Down` mid-press replaces it — terminals rarely deliver multi-button
    /// presses, and treating the newest press as authoritative keeps the
    /// release pairing simple.
    press: Option<Press>,
    /// The last cell a pointer move reported — during a press it is the last
    /// dragged cell (`Some` doubles as the "dragging" flag, cleared on
    /// release); with no press it is the last hovered cell (dedupes hover).
    last_pointer: Option<Position>,
    /// The last completed click's cell + release time — what a second click
    /// is measured against for the double-click window.
    last_click: Option<(Position, Instant)>,
    /// The last hover target reported to the UI via [`track_hover`]. Held so
    /// hover moves can gate redraws on actual target CHANGES.
    ///
    /// [`track_hover`]: MouseRouter::track_hover
    hover: Option<HitTarget>,
}

impl MouseRouter {
    /// Fresh state: nothing pressed, never clicked, nothing hovered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one raw event (with the loop's clock). Returns at most one
    /// gesture — every input maps to ≤1 outcome.
    pub fn feed(&mut self, input: MouseInput, now: Instant) -> Option<MouseOutcome> {
        match input {
            MouseInput::Press { button, col, row } => {
                // A press is not yet anything: clicks confirm on RELEASE, so a
                // Down alone produces no outcome. (It does arm the drag
                // threshold and clear stale hover-dedup state implicitly.)
                self.press = Some(Press {
                    button,
                    at: Position { x: col, y: row },
                    when: now,
                });
                self.last_pointer = None;
                None
            }
            MouseInput::Drag { col, row } => {
                let Some(press) = self.press else {
                    return None; // held move without a tracked press — ignore
                };
                let at = Position { x: col, y: row };
                match self.last_pointer {
                    // First cell outside the press cell: the drag begins.
                    None if at != press.at => {
                        self.last_pointer = Some(at);
                        Some(MouseOutcome::DragStart {
                            button: press.button,
                            origin: press.at,
                            at,
                        })
                    }
                    // Already dragging: report only real cell changes (terminals
                    // can re-report the same cell on sub-cell motion).
                    Some(last) if at != last => {
                        self.last_pointer = Some(at);
                        Some(MouseOutcome::DragMove {
                            origin: press.at,
                            at,
                        })
                    }
                    _ => None,
                }
            }
            MouseInput::Release { button, col, row } => {
                let Some(press) = self.press.take() else {
                    return None; // release without a press — ignore
                };
                let at = Position { x: col, y: row };
                if self.last_pointer.take().is_some() {
                    return Some(MouseOutcome::DragEnd {
                        button: press.button,
                        origin: press.at,
                        at,
                    });
                }
                if at != press.at || button != press.button {
                    // Off-cell release with no drag events seen (a slipped
                    // click — terminals always report held motion as Drag, so
                    // this is defensive) or a different button released:
                    // neither a click nor a drag. Report nothing.
                    return None;
                }
                let double = self.last_click.is_some_and(|(cell, when)| {
                    cell == at && now.saturating_duration_since(when) <= DOUBLE_CLICK
                });
                self.last_click = Some((at, now));
                Some(if double {
                    MouseOutcome::DoubleClick {
                        button: press.button,
                        at,
                    }
                } else {
                    MouseOutcome::Click {
                        button: press.button,
                        at,
                    }
                })
            }
            MouseInput::Move { col, row } => {
                if self.press.is_some() {
                    return None; // a button is held: Drag owns the motion
                }
                let at = Position { x: col, y: row };
                if self.last_pointer == Some(at) {
                    return None; // same hover cell as last reported — dedup
                }
                self.last_pointer = Some(at);
                Some(MouseOutcome::Hover { at })
            }
        }
    }

    /// The live press — button, cell, and the `Instant` it went down — while
    /// a button is held. Consumers needing the press age (a hold threshold,
    /// Issue 02's drag) read it through here; the router still owns no clock.
    pub fn press(&self) -> Option<(MouseButton, Position, Instant)> {
        self.press.map(|p| (p.button, p.at, p.when))
    }

    /// Record the hover target a `Hover` outcome resolved to. Returns whether
    /// it CHANGED — the redraw gate (`Moved` events dirty the frame only when
    /// the cell under the pointer is a different [`HitTarget`]).
    pub fn track_hover(&mut self, target: Option<HitTarget>) -> bool {
        let changed = self.hover != target;
        self.hover = target;
        changed
    }

    /// The hover target currently under the pointer, if any. A render input
    /// (hover highlights overlay the frame buffer; never the `CharGrid` or
    /// its cache key).
    pub fn hover(&self) -> Option<&HitTarget> {
        self.hover.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn press(b: MouseButton, x: u16, y: u16) -> MouseInput {
        MouseInput::Press {
            button: b,
            col: x,
            row: y,
        }
    }
    fn release(b: MouseButton, x: u16, y: u16) -> MouseInput {
        MouseInput::Release {
            button: b,
            col: x,
            row: y,
        }
    }
    fn drag(x: u16, y: u16) -> MouseInput {
        MouseInput::Drag { col: x, row: y }
    }
    fn moved(x: u16, y: u16) -> MouseInput {
        MouseInput::Move { col: x, row: y }
    }
    fn at(x: u16, y: u16) -> Position {
        Position { x, y }
    }
    fn click(b: MouseButton, x: u16, y: u16) -> MouseOutcome {
        MouseOutcome::Click {
            button: b,
            at: at(x, y),
        }
    }

    /// Event sequence → outcome sequence, the acceptance-table style.
    #[test]
    fn click_confirms_on_release_at_the_press_cell() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        assert_eq!(r.feed(press(MouseButton::Left, 5, 3), t0), None);
        // Moving within the cell is invisible — the coords ARE cells.
        assert_eq!(r.feed(drag(5, 3), t0), None, "0-cell move is not a drag");
        assert_eq!(
            r.feed(release(MouseButton::Left, 5, 3), t0),
            Some(click(MouseButton::Left, 5, 3))
        );
    }

    #[test]
    fn one_cell_move_is_a_drag_not_a_click() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        r.feed(press(MouseButton::Left, 5, 3), t0);
        assert_eq!(
            r.feed(drag(6, 3), t0),
            Some(MouseOutcome::DragStart {
                button: MouseButton::Left,
                origin: at(5, 3),
                at: at(6, 3),
            })
        );
        assert_eq!(
            r.feed(drag(9, 3), t0),
            Some(MouseOutcome::DragMove {
                origin: at(5, 3),
                at: at(9, 3),
            })
        );
        // Returning over the press cell keeps it a drag — threshold already crossed.
        assert_eq!(
            r.feed(drag(5, 3), t0),
            Some(MouseOutcome::DragMove {
                origin: at(5, 3),
                at: at(5, 3),
            })
        );
        assert_eq!(
            r.feed(release(MouseButton::Left, 5, 3), t0),
            Some(MouseOutcome::DragEnd {
                button: MouseButton::Left,
                origin: at(5, 3),
                at: at(5, 3),
            })
        );
        // The completed drag does NOT become a click — and must not arm a
        // double-click either.
        assert_eq!(r.feed(press(MouseButton::Left, 5, 3), t0), None);
        assert_eq!(
            r.feed(release(MouseButton::Left, 5, 3), t0),
            Some(click(MouseButton::Left, 5, 3)),
            "a fresh click after a drag is a plain click"
        );
    }

    #[test]
    fn drag_dedupes_same_cell_reports() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        r.feed(press(MouseButton::Left, 5, 3), t0);
        r.feed(drag(6, 3), t0); // DragStart
        assert_eq!(
            r.feed(drag(6, 3), t0),
            None,
            "same-cell re-report while dragging emits nothing"
        );
    }

    #[test]
    fn double_click_window() {
        let t0 = t0();
        let cases = [
            (399u64, true),
            (400u64, true),  // boundary: "400ms以内" is inclusive
            (401u64, false), // over the window → second click
        ];
        for (ms, expect_double) in cases {
            let mut r = MouseRouter::new();
            r.feed(press(MouseButton::Left, 5, 3), t0);
            r.feed(release(MouseButton::Left, 5, 3), t0);
            r.feed(
                press(MouseButton::Left, 5, 3),
                t0 + Duration::from_millis(ms),
            );
            // Release at the SAME instant: the window is measured between the
            // two release timestamps, so this makes the gap exactly `ms`.
            let out = r.feed(
                release(MouseButton::Left, 5, 3),
                t0 + Duration::from_millis(ms),
            );
            if expect_double {
                assert_eq!(
                    out,
                    Some(MouseOutcome::DoubleClick {
                        button: MouseButton::Left,
                        at: at(5, 3),
                    }),
                    "{ms}ms should be a double-click"
                );
            } else {
                assert_eq!(
                    out,
                    Some(click(MouseButton::Left, 5, 3)),
                    "{ms}ms exceeds the window"
                );
            }
        }
    }

    #[test]
    fn second_click_on_a_different_cell_is_not_a_double() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        r.feed(press(MouseButton::Left, 5, 3), t0);
        r.feed(release(MouseButton::Left, 5, 3), t0);
        r.feed(
            press(MouseButton::Left, 6, 3),
            t0 + Duration::from_millis(100),
        );
        assert_eq!(
            r.feed(
                release(MouseButton::Left, 6, 3),
                t0 + Duration::from_millis(110)
            ),
            Some(click(MouseButton::Left, 6, 3))
        );
    }

    #[test]
    fn second_click_after_the_window_is_not_a_double() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        r.feed(press(MouseButton::Left, 5, 3), t0);
        r.feed(release(MouseButton::Left, 5, 3), t0);
        r.feed(press(MouseButton::Left, 5, 3), t0 + Duration::from_secs(2));
        assert_eq!(
            r.feed(
                release(MouseButton::Left, 5, 3),
                t0 + Duration::from_secs(2)
            ),
            Some(click(MouseButton::Left, 5, 3))
        );
    }

    #[test]
    fn buttons_are_distinguished() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        r.feed(press(MouseButton::Right, 5, 3), t0);
        assert_eq!(
            r.feed(release(MouseButton::Right, 5, 3), t0),
            Some(click(MouseButton::Right, 5, 3))
        );
        r.feed(press(MouseButton::Middle, 2, 2), t0);
        assert_eq!(
            r.feed(release(MouseButton::Middle, 2, 2), t0),
            Some(click(MouseButton::Middle, 2, 2))
        );
        // A release of a DIFFERENT button than the press is not a click.
        r.feed(press(MouseButton::Left, 1, 1), t0);
        assert_eq!(r.feed(release(MouseButton::Right, 1, 1), t0), None);
    }

    #[test]
    fn stray_events_are_ignored() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        assert_eq!(r.feed(release(MouseButton::Left, 1, 1), t0), None);
        assert_eq!(r.feed(drag(1, 1), t0), None, "drag with no press");
    }

    #[test]
    fn off_cell_release_without_drag_is_not_a_click() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        r.feed(press(MouseButton::Left, 5, 3), t0);
        assert_eq!(r.feed(release(MouseButton::Left, 6, 3), t0), None);
    }

    #[test]
    fn hover_moves_report_once_per_cell() {
        let t0 = t0();
        let mut r = MouseRouter::new();
        assert_eq!(
            r.feed(moved(1, 1), t0),
            Some(MouseOutcome::Hover { at: at(1, 1) })
        );
        assert_eq!(r.feed(moved(1, 1), t0), None, "same-cell hover is deduped");
        assert_eq!(
            r.feed(moved(2, 1), t0),
            Some(MouseOutcome::Hover { at: at(2, 1) })
        );
        // While a button is held, plain moves are not hovers (Drag owns motion).
        r.feed(press(MouseButton::Left, 2, 1), t0);
        assert_eq!(r.feed(moved(3, 1), t0), None);
    }

    #[test]
    fn hover_tracking_gates_redraw_on_change() {
        let mut r = MouseRouter::new();
        assert!(r.track_hover(Some(HitTarget::LineageBlank)), "first hover");
        assert!(
            !r.track_hover(Some(HitTarget::LineageBlank)),
            "same target stays clean"
        );
        assert!(r.track_hover(Some(HitTarget::Minimap)), "changed target");
        assert!(r.track_hover(None), "leaving all targets");
        assert!(!r.track_hover(None), "still on nothing");
    }
}
