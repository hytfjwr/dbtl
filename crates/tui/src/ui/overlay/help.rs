use super::*;

/// The help overlay content as display lines, GENERATED from the keymap's
/// [`help_lines`](crate::action::help_lines) — the single source of truth — so
/// the `?` overlay can never drift from what the keys actually do. Grouped by
/// mode with section headers; the section for `from` (the mode the overlay was
/// opened over) leads, the rest keep table order. A non-empty `filter`
/// fuzzy-matches each row's keys+desc — a fully filtered-out section leaves no
/// dangling header, and a zero-match filter shows a one-line message.
fn help_display_lines(t: &theme::Theme, filter: &str, from: ModeKind) -> Vec<Line<'static>> {
    use crate::action::{help_lines, HelpLine};
    let mut sections: Vec<(ModeKind, &str)> = vec![
        (ModeKind::Selection, "Normal"),
        (ModeKind::Home, "Home"),
        (ModeKind::Search, "Search / filter"),
        (ModeKind::Detail, "Structure modal"),
        (ModeKind::Sql, "SQL preview"),
        (ModeKind::Stats, "Stats dashboard"),
        (ModeKind::Diff, "Diff summary"),
        (ModeKind::Columns, "Column lineage modal"),
        (ModeKind::Notifications, "Notification center"),
        (ModeKind::Palette, "Command palette"),
        (ModeKind::Yank, "Yank (y + key)"),
        (ModeKind::Menu, "Node menu"),
        (ModeKind::Help, "Help"),
    ];
    if let Some(i) = sections.iter().position(|(mk, _)| *mk == from) {
        let lead = sections.remove(i);
        sections.insert(0, lead);
    }
    let pat = filter.trim();
    let all = help_lines();
    let matches = |hl: &HelpLine| {
        pat.is_empty()
            || !crate::model_list::match_indices(&hl.desc, pat).is_empty()
            || !crate::model_list::match_indices(&hl.keys, pat).is_empty()
    };
    let mut out: Vec<Line> = Vec::new();
    for (mk, title) in sections {
        let rows: Vec<&HelpLine> = all.iter().filter(|l| l.mode == mk && matches(l)).collect();
        if !pat.is_empty() && rows.is_empty() {
            continue;
        }
        out.push(Line::styled(format!("{title}:"), header_style(t)));
        for hl in rows {
            // The key column in the accent, the description default — same text.
            out.push(Line::from(vec![
                Span::styled(format!("  {:<10} ", hl.keys), Style::default().fg(t.accent)),
                Span::raw(hl.desc.to_string()),
            ]));
        }
        out.push(Line::from(""));
    }
    if !pat.is_empty() && out.is_empty() {
        out.push(Line::styled(
            "  no matching bindings".to_string(),
            Style::default().fg(t.text_faint),
        ));
    }
    out
}

/// Clamp a help-overlay scroll offset to the valid range for `area` (size-aware;
/// the event loop calls this on the stored scroll, mirroring `clamp_lineage`).
/// Counts with the DEFAULT theme — the line COUNT is theme-independent (a theme
/// only recolours), so the clamp never needs the active one.
pub fn clamp_help_scroll(area: Rect, hv: &HelpView) -> usize {
    clamp_modal_scroll(
        area,
        help_display_lines(&theme::DEFAULT, hv.filter.as_deref().unwrap_or(""), hv.from).len(),
        hv.scroll,
    )
}

/// Draw the `?` help overlay: a centered, scrollable box listing every binding
/// (from the keymap). `scroll` is clamped here for display; the loop also clamps
/// the stored value so up/down stay responsive. `pub(crate)` for `render::draw`.
pub(crate) fn draw_help(
    frame: &mut Frame,
    area: Rect,
    hv: &HelpView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    let ver = env!("CARGO_PKG_VERSION");
    let title = match &hv.filter {
        // While the `/`-filter is live its text rides the title — the caret
        // only while keys are still typing into it.
        Some(f) => {
            let chrome = super::chrome(glyphs);
            let caret = if hv.editing { chrome.caret } else { "" };
            let hint = if hv.editing {
                "(Enter to apply, Esc to clear)"
            } else {
                "(/ to edit, Esc to clear)"
            };
            format!(" Keybindings - dbtl v{ver}  /{f}{caret}  {hint} ")
        }
        None => format!(" Keybindings - dbtl v{ver}  (?/Esc/q to close, j/k to scroll) "),
    };
    draw_scrollable_modal(
        frame,
        area,
        title,
        help_display_lines(t, hv.filter.as_deref().unwrap_or(""), hv.from),
        hv.scroll,
        glyphs,
        t,
    );
}
