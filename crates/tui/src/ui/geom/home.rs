// Home screen line plan: ONE pure sequence — the renderer
// draws it top-down into the body band and `hit_target` resolves a cell the
// same way, so "computed window == drawn window" holds for the home too.
// ---------------------------------------------------------------------------

/// One line in the home body's vertical plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeLine {
    /// `dbtl home - {project} · {source} · scope:{scope}`.
    Title,
    /// A blank spacer row.
    Pad,
    /// The `/` search bar (live query + caret while open, dim hint while
    /// closed).
    SearchBar,
    /// A section's non-selectable content row — the middle rows (Health's
    /// coverage gauge, Last run's status counts, Diff's delta). The index
    /// addresses [`HomeData::info_flat`] — never selectable.
    Info(usize),
    /// A section header (`[Resume]` / `[Attention]` / …).
    SectionHeader(&'static str),
    /// A selectable row — the flat index into `HomeData::selectable` (while
    /// a home search is open, the result rows).
    Row(usize),
    /// A dim note row (`no matches — Esc to clear`).
    Note,
    /// A capped section's `+{n} more` overflow line — non-selectable
    /// (`S`/`'` reach the full sets; the Attention overflow points at `S`).
    Tail(usize),
    /// The trailing `+{n} more` elision when the body clipped rows.
    More(usize),
    /// The fixed `Tip:` line — closes the plan (never selectable).
    Tip,
}

/// The home body's FULL line sequence (unclipped) — the plan
/// [`home_window`] scrolls/clips and `hit_target` re-derives, so a `Row(i)`
/// cell always lands on the row the screen painted. Pure + mode-independent
/// in both glyph modes.
///
/// Order (fixed): title → pad → search bar → pad → either the open
/// search's result rows (which own the cursor) or the section list in
/// `HomeData::sections` order — each section a header, its `info` content
/// rows (the Health/Last run/Diff middle rows sit here in section order),
/// its selectable `rows`, then a `+N more` tail — padded between.
pub fn home_lines(data: &crate::app::HomeData) -> Vec<HomeLine> {
    let mut lines = vec![
        HomeLine::Title,
        HomeLine::Pad,
        HomeLine::SearchBar,
        HomeLine::Pad,
    ];
    match &data.search {
        Some(s) => {
            lines.extend((0..s.results.len()).map(HomeLine::Row));
            if s.results.is_empty() {
                lines.push(HomeLine::Note);
            }
        }
        None => {
            let mut flat = 0;
            let mut info_i = 0;
            for (si, sec) in data.sections.iter().enumerate() {
                if si > 0 {
                    lines.push(HomeLine::Pad);
                }
                lines.push(HomeLine::SectionHeader(sec.title));
                for _ in &sec.info {
                    lines.push(HomeLine::Info(info_i));
                    info_i += 1;
                }
                for _ in &sec.rows {
                    lines.push(HomeLine::Row(flat));
                    flat += 1;
                }
                if let Some(n) = sec.tail {
                    lines.push(HomeLine::Tail(n));
                }
            }
            // The fixed closing line (`Actions` always exists, so the pad
            // before it can never lead the plan).
            lines.push(HomeLine::Pad);
            lines.push(HomeLine::Tip);
        }
    }
    lines
}

/// The `scroll`-offset, `height`-tall window into a [`home_lines`] plan —
/// the SAME sequence the renderer draws and `hit_target` routes. Rows past
/// the window's bottom collapse into a trailing `+{n} more` line (the count
/// names every row the screen dropped, the replaced line included).
pub fn home_window(lines: &[HomeLine], height: usize, scroll: usize) -> Vec<HomeLine> {
    let scroll = scroll.min(lines.len().saturating_sub(1));
    let end = (scroll + height).min(lines.len());
    let mut window: Vec<HomeLine> = lines[scroll.min(end)..end].to_vec();
    let below = lines.len() - end;
    if below > 0 && !window.is_empty() {
        let last = window.len() - 1;
        window[last] = HomeLine::More(below + 1);
    }
    window
}

/// The plan index of `Row(sel)` — the loop's ensure-visible input (which
/// line must be inside the scroll window). Falls back to the plan's end so
/// an out-of-range cursor scrolls to the bottom rather than wrapping.
pub fn home_line_of(lines: &[HomeLine], sel: usize) -> usize {
    lines
        .iter()
        .position(|l| matches!(l, HomeLine::Row(i) if *i == sel))
        .unwrap_or_else(|| lines.len().saturating_sub(1))
}
