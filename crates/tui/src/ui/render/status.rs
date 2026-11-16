use super::*;

/// Convert a [`StatusSegments`] into the priority-ordered chip list
/// [`status_layout`](super::status_layout) and `draw_status` share — the
/// `(chip, bracketed-label)` pairs in the keep order (`Impact` survives
/// longest, `View` sheds first). The single producer for chips, so the
/// draw and the hit-test can never disagree on which chips exist.
pub fn status_chips(segments: &StatusSegments) -> Vec<(super::StatusChip, String)> {
    use super::StatusChip as C;
    let mut v = Vec::with_capacity(13);
    let mut push = |chip: C, txt: Option<&str>| {
        if let Some(txt) = txt {
            v.push((chip, format!("[{txt}]")));
        }
    };
    // Declaration order == the survival order (see [`StatusChip`]).
    push(C::Impact, segments.impact);
    push(C::Notify, segments.notify);
    push(C::Column, segments.column);
    push(C::Diff, segments.diff);
    push(C::Run, segments.run);
    push(C::Scope, segments.scope);
    push(C::Coverage, segments.coverage);
    push(C::Bookmarks, segments.bookmarks);
    push(C::Filter, segments.filter);
    push(C::Sort, segments.sort);
    push(C::Position, segments.position);
    push(C::Focus, segments.focus);
    push(C::View, segments.view);
    v
}

/// A chip's semantic colour — the same per-segment colour assignments the
/// legacy segment list used, keyed by [`StatusChip`] now.
fn chip_color(chip: super::StatusChip, t: &theme::Theme) -> Color {
    use super::StatusChip as C;
    match chip {
        C::Impact | C::Notify => t.danger,
        C::Column | C::Bookmarks => t.gold,
        C::Diff => t.diff_mod,
        C::Run | C::Coverage => t.warn,
        C::Scope | C::Filter | C::Sort => t.accent,
        C::Position => t.text_faint,
        C::Focus => t.ok,
        C::View => t.chip_view,
    }
}

/// Draw the bottom status line: the contextual hints
/// left-anchored (key in `accent`, description in `text_dim`), the chips
/// right-anchored in their semantic colours — all placed by the shared
/// [`status_layout`](super::status_layout) the hit-test also consumed, so
/// every drawn span answers the click its `HitTarget` promises. The hovered
/// span draws BOLD (the emphasis cue); the inert `StatusBar` cells keep
/// the plain surface band.
pub(super) fn draw_status(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    use unicode_width::UnicodeWidthStr;

    let t = ctx.theme;
    let chips = status_chips(&ctx.segments);
    let layout = super::status_layout(area.width, ctx.hints, &chips);

    // The surface band first (the whole row is chrome, not content rows).
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.surface)),
        area,
    );
    if area.height == 0 {
        return;
    }
    let row = area.y;
    let buf = frame.buffer_mut();
    let mut stamp = |x: u16, s: &str, st: Style| {
        buf.set_stringn(
            area.x + x,
            row,
            s,
            (area.width.saturating_sub(x)) as usize,
            st,
        );
    };

    // Hints — `layout.hints` is a PREFIX of `ctx.hints` (the drop keeps the
    // head of the priority-sorted input), so the zip pairs each span with
    // its hint.
    for (span, hint) in layout.hints.iter().zip(ctx.hints.iter()) {
        let hot = ctx.hover == Some(&span.target);
        let mut x = span.x0;
        let key_st = Style::default()
            .fg(t.accent)
            .bg(t.surface)
            .add_modifier(if hot {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        stamp(x, &format!(" {}", hint.key), key_st);
        x += 1 + UnicodeWidthStr::width(hint.key.as_str()) as u16;
        if !hint.desc.is_empty() {
            let desc_st = Style::default()
                .fg(t.text_dim)
                .bg(t.surface)
                .add_modifier(if hot {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            stamp(x, &format!(" {}", hint.desc), desc_st);
        }
    }
    // Chips — same prefix-pairing over the right-anchored block.
    for (span, (chip, label)) in layout.chips.iter().zip(chips.iter()) {
        let hot = ctx.hover == Some(&span.target);
        let st = Style::default()
            .fg(chip_color(*chip, t))
            .bg(t.surface)
            .add_modifier(if hot {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        stamp(span.x0, &format!(" {label}"), st);
    }
}
