//! Terminal lifecycle: the glyph-mode probe, mouse capture, and the kitty
//! keyboard-enhancement push/pop (the pieces `run` toggles around the event
//! loop, the `$EDITOR` suspend, and the panic path).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use dbtl::layout::GlyphMode;

use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;

use crate::cli::Cli;

/// Decide the glyph repertoire: explicit `--ascii` / `--unicode` win; otherwise
/// probe the live terminal. Inconclusive probes (terminal didn't answer the
/// cursor-position query) default to Unicode, matching most terminals.
pub fn resolve_glyph_mode(cli: &Cli) -> GlyphMode {
    if cli.ascii {
        return GlyphMode::Ascii;
    }
    if cli.unicode {
        return GlyphMode::Unicode;
    }
    probe_ambiguous_width().unwrap_or(GlyphMode::Unicode)
}

/// Measure how wide the terminal ACTUALLY renders an East-Asian-Ambiguous
/// box-drawing glyph (`─`, U+2500): write it at the origin of the (already
/// entered) alternate screen and read the cursor advance via a cursor-position
/// report. CJK-configured terminals/fonts commonly render ambiguous-width
/// characters 2 cells wide; every Unicode border would then desync ratatui's
/// 1-cell buffer model into doubled/ghosted lines, so 2-cell advance selects
/// the pure-ASCII glyph set instead. The probe cells are repainted before the
/// first frame draws, so nothing of it is visible.
///
/// A terminal that never answers the CPR query costs crossterm's internal 2s
/// poll timeout once at startup and falls back to `None` (→ Unicode) — no
/// hang. `--ascii` / `--unicode` skip the probe entirely.
fn probe_ambiguous_width() -> Option<GlyphMode> {
    use ratatui::crossterm::cursor::{position, MoveTo};
    let mut out = std::io::stdout();
    execute!(out, MoveTo(0, 0)).ok()?;
    write!(out, "─").ok()?;
    out.flush().ok()?;
    let (x, _) = position().ok()?;
    execute!(out, MoveTo(0, 0)).ok()?;
    write!(out, "  ").ok()?;
    out.flush().ok()?;
    Some(if x >= 2 {
        GlyphMode::Ascii
    } else {
        GlyphMode::Unicode
    })
}

/// Enable/disable terminal mouse capture (best-effort; ratatui's `init`/`restore`
/// don't manage it). Toggled around the editor suspend too.
pub fn set_mouse_capture(on: bool) {
    let mut out = std::io::stdout();
    let _ = if on {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    };
}

/// Whether the live terminal speaks the kitty keyboard protocol, probed once at
/// startup. Gates [`set_keyboard_enhancement`] so unsupported terminals are
/// never sent push/pop sequences.
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// Probe the terminal for kitty-keyboard-protocol support (a query/response
/// exchange like the glyph probe — call it in the same no-mouse-capture
/// window). A terminal that never answers costs crossterm's internal poll
/// timeout once at startup and reads as unsupported.
pub fn detect_keyboard_enhancement() {
    let supported = matches!(supports_keyboard_enhancement(), Ok(true));
    KEYBOARD_ENHANCED.store(supported, Ordering::Relaxed);
}

/// Push/pop the kitty keyboard-protocol enhancement (best-effort, gated on the
/// startup probe). `DISAMBIGUATE_ESCAPE_CODES` makes the terminal encode
/// modified keys it would otherwise swallow or fold — this is what lets the
/// `Cmd-b` (Super) binding actually arrive as `SUPER + 'b'`. Toggled around
/// the editor suspend and on the panic path, mirroring mouse capture.
pub fn set_keyboard_enhancement(on: bool) {
    if !KEYBOARD_ENHANCED.load(Ordering::Relaxed) {
        return;
    }
    let mut out = std::io::stdout();
    let _ = if on {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
    } else {
        execute!(out, PopKeyboardEnhancementFlags)
    };
}
