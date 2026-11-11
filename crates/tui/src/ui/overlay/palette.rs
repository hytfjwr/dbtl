use super::*;

// ---- command palette ------------------------------------------------------

/// Draw the command palette: a centered fuzzy-finder over every Selection-mode
/// command. `pub(crate)` for `render::draw`. The candidate list is derived from
/// the keymap ([`palette_candidates`]) at draw time — keyed off `BINDINGS`, so
/// the palette can never drift from the keys. No `Dag` is consulted (the keymap
/// is static), so `RenderCtx` stays Dag-free.
///
/// Layout: a query line (with the [`Chrome`](super::Chrome) caret) on the first
/// interior row, then one row per candidate below. The selected row is reversed
/// via [`selected_style`]; the matched query chars are highlighted Yellow+bold
/// (via [`match_indices`](crate::model_list::match_indices)); the key label is
/// right-aligned and dim. The scroll window is DERIVED from `selected` (no stored
/// scroll, so nothing for the loop to clamp), and the right-border scrollbar thumb
/// is stamped on overflow. Chrome glyphs only — ASCII-safe in both modes.
pub(crate) fn draw_palette(
    frame: &mut Frame,
    area: Rect,
    state: &PaletteState,
    rows: Option<&[PaletteRow]>,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    // ~60% × ~60%, min-clamped so the box never collapses on a tiny terminal.
    let rect = centered_rect(60, 60, area).intersection(area);
    let chrome = super::chrome(glyphs);
    // The `@` model-search mode re-titles the box: the `+`
    // palette's `new_tab` arm lands Enter in a NEW tab, a plain `@` query
    // re-roots — neither runs a command.
    let title = if state.new_tab {
        " open in new tab  (Esc to close, Enter to open) "
    } else if state.query.starts_with('@') {
        " models  (Esc to close, Enter to open) "
    } else {
        " commands  (Esc to close, Enter to run) "
    };
    let inner = super::draw_accent_modal(frame, rect, title, glyphs, t);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The query line (row 0 of the interior), with the caret after the text.
    let query_line = Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.query.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(chrome.caret.to_string(), Style::default().fg(t.accent)),
    ]);
    let query_rect = Rect { height: 1, ..inner };
    frame.render_widget(Paragraph::new(query_line), query_rect);

    // The candidate list fills the rows below the query line.
    let list_rect = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let view_h = list_rect.height as usize;
    // The Dag-aware row model is precomputed at the loop seam (`App::palette_rows`
    // → `RenderCtx::palette_rows`). Headless renders that never computed it fall
    // back to plain keymap command rows (no recents, no availability marks) so
    // they still draw something.
    let fallback;
    let rows: &[PaletteRow] = match rows {
        Some(r) => r,
        None => {
            fallback = palette_candidates(&state.query)
                .iter()
                .map(|b| PaletteRow::Command {
                    action: b.action,
                    help: b.help,
                    keys: b.key_label(),
                    unavailable: None,
                })
                .collect::<Vec<_>>();
            &fallback
        }
    };
    // `@` model search: the query text after `@` is the name pattern rows were
    // built from (`App::palette_rows` trims it); it is also the string the
    // match highlight measures against.
    let model_q = state
        .query
        .strip_prefix('@')
        .map(str::trim)
        .unwrap_or(&state.query);
    if view_h == 0 {
        return;
    }
    // `selected` indexes the SELECTABLE rows (headers never count). It can
    // overshoot when the candidates shrank under a fixed query outside a
    // mutation the reducer sees (a --watch reload mid-palette): clamp it for
    // display like the confirm arm does, or the highlight row vanishes and
    // the scroll window loses the cursor entirely.
    let selectable = rows
        .iter()
        .filter(|r| !matches!(r, PaletteRow::Header(_)))
        .count();
    let sel = if selectable == 0 {
        usize::MAX // nothing to land on — no row may claim the cursor
    } else {
        state.selected.min(selectable - 1)
    };
    // Find the clamped selection's display position for the scroll window.
    let mut sel_i = 0usize;
    let mut sel_display = 0usize;
    for (di, r) in rows.iter().enumerate() {
        if !matches!(r, PaletteRow::Header(_)) {
            if sel_i == sel {
                sel_display = di;
            }
            sel_i += 1;
        }
    }
    let total = rows.len();
    let off = sel_display
        .saturating_sub(view_h - 1)
        .min(total.saturating_sub(view_h));

    let label_w = list_rect.width as usize;
    let hi = Style::default().fg(t.gold).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::with_capacity(view_h);
    let mut sel_i = 0usize;
    for (di, row) in rows.iter().enumerate() {
        let header = matches!(row, PaletteRow::Header(_));
        let selected = !header && sel_i == sel;
        if !header {
            sel_i += 1;
        }
        if di < off || di >= off + view_h {
            continue;
        }
        match row {
            PaletteRow::Header(label) => {
                lines.push(Line::styled(format!(" {label}"), header_style(t)));
            }
            PaletteRow::Command {
                help,
                keys,
                unavailable,
                ..
            } => {
                // Grey = "would do nothing right now": the whole row (help, key,
                // appended reason) is faint; the reversed selected row still
                // reads as the cursor.
                let grey = unavailable.is_some();
                let base = if selected {
                    selected_style(t)
                } else if grey {
                    Style::default().fg(t.text_faint)
                } else {
                    Style::default()
                };
                // The help text, with matched query chars highlighted
                // (yellow+bold). The highlight wins over the base except for the
                // reversed selected row, which keeps the reverse.
                let hits = if state.query.starts_with('@') {
                    Vec::new()
                } else {
                    crate::model_list::match_indices(help, &state.query)
                };
                let mut spans: Vec<Span> = vec![Span::styled("  ", base)];
                for (ci, ch) in help.chars().enumerate() {
                    let st = if hits.contains(&ci) && !selected && !grey {
                        base.patch(hi)
                    } else {
                        base
                    };
                    spans.push(Span::styled(ch.to_string(), st));
                }
                // Right-align the dim key label within the row width
                // (best-effort: pad between help and key when there is room).
                let reason = unavailable
                    .as_ref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default();
                let used = 2 + help.chars().count() + keys.chars().count() + reason.chars().count();
                if label_w > used {
                    spans.push(Span::styled(" ".repeat(label_w - used), base));
                } else {
                    spans.push(Span::styled(" ", base));
                }
                let key_base = if selected {
                    base
                } else if grey {
                    Style::default().fg(t.text_faint)
                } else {
                    Style::default().fg(t.text_dim)
                };
                // A row can be admitted by a KEY-LABEL match (e.g. "tab" matches
                // the "Tab" label, not the help text). When the help has NO hits
                // but the query is non-empty, highlight the matched key-label
                // chars so that row's match cue is visible too.
                let key_hits = if hits.is_empty() && !state.query.trim().is_empty() && !grey {
                    crate::model_list::match_indices(keys, &state.query)
                } else {
                    Vec::new()
                };
                if key_hits.is_empty() {
                    spans.push(Span::styled(keys.clone(), key_base));
                } else {
                    for (ci, ch) in keys.chars().enumerate() {
                        let st = if key_hits.contains(&ci) && !selected {
                            key_base.patch(hi)
                        } else {
                            key_base
                        };
                        spans.push(Span::styled(ch.to_string(), st));
                    }
                }
                if !reason.is_empty() {
                    spans.push(Span::styled(reason, Style::default().fg(t.text_faint)));
                }
                lines.push(Line::from(spans));
            }
            PaletteRow::Model { name, .. } => {
                let base = if selected {
                    selected_style(t)
                } else {
                    Style::default()
                };
                let hits = crate::model_list::match_indices(name, model_q);
                let mut spans: Vec<Span> = vec![Span::styled("  ", base)];
                for (ci, ch) in name.chars().enumerate() {
                    let st = if hits.contains(&ci) && !selected {
                        base.patch(hi)
                    } else {
                        base
                    };
                    spans.push(Span::styled(ch.to_string(), st));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    if selectable == 0 {
        // No candidates at all — the one-line zero-match message.
        let msg = if state.query.starts_with('@') {
            "no matching models"
        } else {
            "no matching commands"
        };
        lines.push(Line::styled(
            format!("  {msg}"),
            Style::default().fg(t.text_faint),
        ));
    }
    frame.render_widget(Paragraph::new(lines), list_rect);

    // Right-border scrollbar thumb on overflow (display-row units — header rows
    // scroll with the commands).
    super::stamp_scrollbar(frame, rect, total, off, view_h, glyphs, t);
}

/// Draw the `y` yank-prefix which-key menu into `rect` (precomputed by
/// [`super::geom::yank_menu_rect`] so the hit-test targets the same box):
/// a row per `y<key>` binding, GENERATED from the `M::Yank` keymap entries
/// (same source of truth as dispatch + `?` help). `sel` is the pointer's
/// highlighted row — reversed like any other selection marker.
pub(crate) fn draw_yank_menu(
    frame: &mut Frame,
    rect: Rect,
    sel: usize,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    use crate::action::yank_bindings;
    let inner = super::draw_accent_modal(frame, rect, " yank + key ", glyphs, t);
    let rows: Vec<Line> = yank_bindings()
        .enumerate()
        .map(|(i, b)| {
            let key = Span::styled(
                format!("  y{:<3}", b.key_label()),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            );
            let help = Span::raw(format!(" {}", b.help));
            if i == sel {
                Line::from(vec![
                    key,
                    Span::styled(format!(" {}", b.help), Style::default().fg(t.text_bright)),
                ])
                .style(selected_style(t))
            } else {
                Line::from(vec![key, help])
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

/// Dim every buffer cell OUTSIDE `rect`: a style-only pass — `fg` becomes
/// `text_faint`, glyphs untouched, so ASCII mode loses nothing (the
/// dim-the-modal's-exterior rule). Runs right before the overlay paints, so the
/// box itself keeps full colour. One buffer scan per frame while an overlay
/// is up.
pub(crate) fn dim_exterior(frame: &mut Frame, except: Rect, t: &theme::Theme) {
    let buf = frame.buffer_mut();
    let area = *buf.area();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if except.contains(Position { x, y }) {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_fg(t.text_faint);
            }
        }
    }
}
