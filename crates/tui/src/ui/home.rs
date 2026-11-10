//! Issue 10's home screen renderer — draws the loop-built [`HomeData`] into
//! the whole body band while the fixed home tab is active (§3). Pure
//! painting: the line sequence comes from [`super::geom::home_lines`] (the
//! same plan `hit_target` resolves), the rows/strings arrive precomputed —
//! nothing here reads a `Dag`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::geom::{home_lines, home_window, HomeLine};
use super::theme::Theme;
use super::RenderCtx;
use crate::action::{Mode, SearchTarget};
use crate::app::HomeTone;

/// The detail column's reserved width (`{label}  {detail}`): labels get the
/// rest of the band, middle-truncated only when a terminal is narrower than
/// the row needs (a full-width row never truncates its label).
const DETAIL_W: usize = 34;
/// The coverage gauge's cell count (`cov [#######---] 80%`-style — the bar
/// is `ok`/`surface`-background SPACES, never glyphs, per the §15 rule).
const GAUGE_W: usize = 12;

/// Draw the home body into `area` (the `list ∪ lineage ∪ inspector` band).
/// The line plan is `home_lines(data, area.height)` — clipped with a
/// `+{n} more` footer — so every drawn row is exactly the one `hit_target`
/// reports at that cell.
pub(crate) fn draw_home(frame: &mut Frame, area: Rect, data: &crate::HomeData, ctx: &RenderCtx) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let t = ctx.theme;
    let chrome = super::chrome(ctx.glyphs);
    // Fill the band with the plain bg first — the panes' borders don't exist
    // under home, so nothing else covers these cells.
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.surface)),
        area,
    );

    let lines = home_window(&home_lines(data), area.height as usize, ctx.home_scroll);
    // The result cursor while a home search is open (read straight from the
    // mode — it changes without touching the row data).
    let search_sel = match ctx.mode {
        Mode::Search(s) if s.target == SearchTarget::Home => s.match_idx,
        _ => 0,
    };
    let sel = if data.search.is_some() {
        search_sel
    } else {
        ctx.home_sel
    };

    // The label column: widest visible label, capped so a detail always has
    // room (a narrow terminal shrinks the label, never the columns apart).
    let label_w = rows_width(data, &lines, area.width);

    for (i, line) in lines.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let row = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        match line {
            HomeLine::Title => {
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            " dbtl home",
                            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" - {}", data.project),
                            Style::default().fg(t.text_dim),
                        ),
                    ])),
                    row,
                );
            }
            HomeLine::Health => {
                let mut spans: Vec<Span> = Vec::new();
                for (i, cell) in data.health.iter().enumerate() {
                    let (text, style) = if i == 0 {
                        (
                            format!(" {cell}"),
                            Style::default()
                                .fg(t.text_bright)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        (format!("   {cell}"), Style::default().fg(t.text_dim))
                    };
                    spans.push(Span::styled(text, style));
                }
                // The coverage gauge: `ok`/`surface` background SPACES — the
                // §15 "styled spaces, never new gauge glyphs" rule.
                if data.coverage.1 > 0 {
                    let (tested, total) = data.coverage;
                    let pct = tested * 100 / total;
                    let filled = (GAUGE_W * pct / 100).min(GAUGE_W);
                    spans.push(Span::styled("   [", Style::default().fg(t.text_faint)));
                    spans.push(Span::styled(
                        " ".repeat(filled),
                        Style::default().bg(if pct >= 80 { t.ok } else { t.warn }),
                    ));
                    spans.push(Span::styled(
                        " ".repeat(GAUGE_W - filled),
                        Style::default().bg(t.surface_hi),
                    ));
                    spans.push(Span::styled("]", Style::default().fg(t.text_faint)));
                }
                frame.render_widget(Paragraph::new(Line::from(spans)), row);
            }
            HomeLine::Pad => {}
            HomeLine::SearchBar => {
                let text = match &data.search {
                    Some(s) => Line::from(vec![
                        Span::styled(" / ", Style::default().fg(t.accent)),
                        Span::styled(s.query.clone(), Style::default().fg(t.text_bright)),
                        Span::styled(chrome.caret, Style::default().fg(t.accent)),
                        Span::styled(
                            s.hist.map(|_| "  <history>").unwrap_or_default(),
                            Style::default().fg(t.text_faint),
                        ),
                    ]),
                    None => Line::from(vec![
                        Span::styled(" / ", Style::default().fg(t.text_faint)),
                        Span::styled(
                            "search models - Enter opens a tab",
                            Style::default().fg(t.text_faint),
                        ),
                    ]),
                };
                frame.render_widget(Paragraph::new(text), row);
            }
            HomeLine::SectionHeader(title) => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!(" [{title}]"),
                        Style::default().fg(t.text_dim).add_modifier(Modifier::BOLD),
                    ))),
                    row,
                );
            }
            HomeLine::Row(idx) => {
                if let Some((label, detail, tone)) = row_at(data, *idx) {
                    draw_home_row(
                        frame,
                        row,
                        label,
                        detail,
                        tone,
                        *idx == sel,
                        label_w,
                        t,
                        chrome.ellipsis,
                    );
                }
            }
            HomeLine::Note => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "   no matches - Esc to clear",
                        Style::default().fg(t.text_faint),
                    ))),
                    row,
                );
            }
            HomeLine::More(n) => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("   +{n} more"),
                        Style::default().fg(t.text_faint),
                    ))),
                    row,
                );
            }
        }
    }
}

/// The widest label across the rows the plan draws — bounds the detail
/// column's start. Capped so `detail` always keeps [`DETAIL_W`] (on a band
/// narrower than that, the label just takes what's left).
fn rows_width(data: &crate::HomeData, lines: &[HomeLine], width: u16) -> usize {
    let widest = lines
        .iter()
        .filter_map(|l| match l {
            HomeLine::Row(i) => row_at(data, *i).map(|(label, _, _)| label.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let budget = (width as usize).saturating_sub(DETAIL_W + 4);
    widest.min(budget.max(8))
}

/// The `(label, detail, tone)` the flat selectable index `i` renders — read
/// back through the same mapping [`home_lines`] used (search results while
/// searching, the section rows in order otherwise).
fn row_at(data: &crate::HomeData, i: usize) -> Option<(&str, &str, HomeTone)> {
    if let Some(s) = &data.search {
        return s
            .results
            .get(i)
            .map(|r| (r.label.as_str(), r.detail.as_str(), r.tone));
    }
    let mut flat = 0;
    for sec in &data.sections {
        for row in &sec.rows {
            if flat == i {
                return Some((row.label.as_str(), row.detail.as_str(), row.tone));
            }
            flat += 1;
        }
    }
    None
}

/// One selectable row: `  {label:<label_w}  {detail}` with the tone's colour
/// on the label, the selection bar (`REVERSED` accent) on the whole row when
/// `sel`, and a `>` marker in the gutter.
#[allow(clippy::too_many_arguments)]
fn draw_home_row(
    frame: &mut Frame,
    row: Rect,
    label: &str,
    detail: &str,
    tone: HomeTone,
    sel: bool,
    label_w: usize,
    t: &Theme,
    ellipsis: &str,
) {
    let tone_fg = match tone {
        HomeTone::Normal => t.text_bright,
        HomeTone::Bad => t.danger,
        HomeTone::Warn => t.warn,
        HomeTone::Dim => t.text_faint,
    };
    // Middle-truncate a too-long label (the `…`/`..` repertoire is chrome's).
    let label = truncate_label(label, label_w, ellipsis);
    let base = if sel {
        super::selected_style(t)
    } else {
        Style::default().fg(tone_fg)
    };
    let detail_st = if sel {
        base
    } else {
        Style::default().fg(t.text_faint)
    };
    let text = Line::from(vec![
        Span::styled(if sel { " > " } else { "   " }, base),
        Span::styled(format!("{label:<label_w$}"), base),
        Span::styled(format!("  {detail}"), detail_st),
    ]);
    // Paint the whole row's background so the selection bar spans it.
    if sel {
        frame.render_widget(Paragraph::new("").style(base), row);
    }
    frame.render_widget(Paragraph::new(text), row);
}

/// Truncate `label` to `w` display columns, middle-eliding with the active
/// chrome's ellipsis when it doesn't fit (dbt names share prefixes — the
/// tail is the identifying half).
fn truncate_label(label: &str, w: usize, ellipsis: &str) -> String {
    let len = label.chars().count();
    if len <= w {
        return label.to_string();
    }
    let ew = ellipsis.chars().count();
    if w <= ew + 1 {
        return label.chars().take(w).collect();
    }
    let head = (w - ew).div_ceil(2);
    let tail = w - ew - head;
    format!(
        "{}{}{}",
        label.chars().take(head).collect::<String>(),
        ellipsis,
        label
            .chars()
            .skip(len.saturating_sub(tail))
            .collect::<String>()
    )
}
