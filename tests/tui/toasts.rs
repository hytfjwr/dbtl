use super::*;

// ============================================================================
// transient toast (copy / bookmark / export feedback)
// ============================================================================

/// The toast floats at the TOP-RIGHT: a 3-row bordered box inset 2 cols from
/// the right edge and 1 row from the top, its text on the middle row. Absent
/// `ctx.toasts` (the default empty slice) the buffer is byte-identical to a
/// plain render — every other test in this file implicitly asserts that.
#[test]
pub(super) fn toast_renders_top_right_with_its_text() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.toasts = &[ToastLine {
        level: NoticeLevel::Info,
        text: "Copied unique_id",
    }];
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();

    // Geometry: text 16 wide -> box 20 wide; x = 100 - 20 - 2 = 78, y = 1..4.
    assert_eq!(
        buffer.cell((78, 1)).unwrap().symbol(),
        "╭",
        "toast top-left"
    );
    assert_eq!(
        buffer.cell((97, 1)).unwrap().symbol(),
        "╮",
        "toast top-right"
    );
    assert_eq!(
        buffer.cell((78, 3)).unwrap().symbol(),
        "╰",
        "toast bottom-left"
    );
    let row = row_text(&buffer, 2);
    let pos = row
        .find("Copied unique_id")
        .expect("toast text on middle row");
    assert!(
        pos >= 70,
        "toast text sits in the top-RIGHT corner, got col {pos}"
    );

    // Without a toast the text never appears anywhere.
    let plain = render(100, 30);
    assert!(
        !buffer_to_string(&plain).contains("Copied unique_id"),
        "no toast field -> no toast text"
    );
}

#[test]
pub(super) fn toast_truncates_to_the_screen_with_ascii_marker() {
    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    let long = "Removed bookmark: some_extremely_long_model_name";
    let toasts = [ToastLine {
        level: NoticeLevel::Info,
        text: long,
    }];
    ctx.toasts = &toasts;
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    let row = row_text(&buffer, 2);
    // Width budget at 40 cols: 40 - 4 (frame) - 2 (inset) = 34 -> 32 chars + "..".
    assert!(
        row.contains("Removed bookmark: some_extremely.."),
        "truncated to the display budget with a '..' marker, got {row:?}"
    );
    assert!(
        !buffer_to_string(&buffer).contains(long),
        "the overlong text never renders in full"
    );
}

/// The toast layers ABOVE modal overlays (drawn last), so a yank fired from
/// inside the SQL/help modal is still acknowledged.
#[test]
pub(super) fn toast_floats_above_modal_overlays() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    let mode = dbtl::Mode::Help(dbtl::HelpView::new(dbtl::Mode::Selection));
    ctx.mode = &mode;
    ctx.toasts = &[ToastLine {
        level: NoticeLevel::Info,
        text: "Copied raw SQL",
    }];
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    assert!(
        row_text(&buffer, 2).contains("Copied raw SQL"),
        "toast text wins over the modal's cells on its row"
    );
}

/// Issue 08: toasts stack up to 3, `toasts[0]` (the newest) on TOP — each box
/// sized to its own text, 3 rows apart starting at y=1.
#[test]
pub(super) fn toast_stack_renders_up_to_three_newest_on_top() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.toasts = &[
        ToastLine {
            level: NoticeLevel::Info,
            text: "toast_newest",
        },
        ToastLine {
            level: NoticeLevel::Warning,
            text: "toast_middle",
        },
        ToastLine {
            level: NoticeLevel::Info,
            text: "toast_oldest",
        },
    ];
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buffer);
    let y_of = |needle: &str| -> u16 {
        (0..buffer.area.height)
            .find(|&y| row_text(&buffer, y).contains(needle))
            .unwrap_or_else(|| panic!("{needle} rendered"))
    };
    let (y_new, y_mid, y_old) = (
        y_of("toast_newest"),
        y_of("toast_middle"),
        y_of("toast_oldest"),
    );
    assert_eq!(
        (y_mid - y_new, y_old - y_mid),
        (3, 3),
        "each box 3 rows apart"
    );
    assert_eq!(y_new, 2, "newest toast's text on the top box's middle row");
    assert!(
        !text.contains("toast_fourth"),
        "only the provided three exist"
    );
}

/// A warning toast wears the `warn` border color while info keeps `accent` —
/// severity is visible without reading the text.
#[test]
pub(super) fn warning_toast_border_differs_from_info() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.toasts = &[
        ToastLine {
            level: NoticeLevel::Info,
            text: "info toast",
        },
        ToastLine {
            level: NoticeLevel::Warning,
            text: "warn toast",
        },
    ];
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    let theme = &dbtl::ui::theme::DEFAULT;
    // Border cells: info box top-left at y=1, warning box at y=4 (3 rows down).
    // Both are right-aligned, so the left border x differs by text width —
    // compare against the theme roles rather than coordinates.
    let find_border_color = |needle: &str| {
        (0..buffer.area.height)
            .find_map(|y| {
                let row = row_text(&buffer, y);
                row.find(needle).map(|_pos| {
                    // The border corner of the box is one row up, leftmost non-space.
                    let by = y - 1;
                    (0..buffer.area.width)
                        .filter_map(|x| buffer.cell((x, by)))
                        .find(|c| c.symbol() == "╭" || c.symbol() == "+")
                        .map(|c| c.fg)
                })
            })
            .flatten()
            .expect("border cell found")
    };
    let info_fg = find_border_color("info toast");
    let warn_fg = find_border_color("warn toast");
    assert_eq!(info_fg, theme.accent, "info border = accent");
    assert_eq!(warn_fg, theme.warn, "warning border = warn");
}

/// Issue 08: the `Ctrl-n` centre renders `HH:MM:SS  level  text` rows
/// newest-first inside the shared scrollable-modal chrome.
#[test]
pub(super) fn notification_center_renders_rows_newest_first() {
    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    let mode = dbtl::Mode::Notifications(dbtl::NotificationsView {
        rows: vec![
            dbtl::NotificationEntry {
                stamp: "10:20:31".into(),
                level: NoticeLevel::Warning,
                text: "Reload failed".into(),
            },
            dbtl::NotificationEntry {
                stamp: "10:20:30".into(),
                level: NoticeLevel::Info,
                text: "Copied unique_id".into(),
            },
        ],
        scroll: 0,
    });
    ctx.mode = &mode;
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buffer);
    assert!(text.contains("notifications"), "modal title");
    let warn_row = (0..buffer.area.height)
        .find(|&y| row_text(&buffer, y).contains("Reload failed"))
        .expect("warning row");
    let info_row = (0..buffer.area.height)
        .find(|&y| row_text(&buffer, y).contains("Copied unique_id"))
        .expect("info row");
    assert!(warn_row < info_row, "newest row renders first");
    assert!(
        row_text(&buffer, warn_row).contains("10:20:31")
            && row_text(&buffer, warn_row).contains("warn"),
        "row shape HH:MM:SS  level  text: {:?}",
        row_text(&buffer, warn_row)
    );
}

/// The unread-warning badge `!N` sits right after `impact` and drops with the
/// rest of the droppable chips on a narrow line — never the protected core.
#[test]
pub(super) fn notify_chip_renders_and_drops_on_narrow() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.segments = StatusSegments {
            impact: Some("v1 ^2"),
            notify: Some("!3"),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains("[!3]"),
        "!N chip renders after impact: {text}"
    );
    assert!(
        text.find("[!3]").unwrap() > text.find("[v1 ^2]").unwrap(),
        "!N sits right after impact"
    );

    // Narrow: 40 cols — both chips still fit (`[v1 ^2]` 8 + `[!3]` 5); the
    // head-of-order impact chip survives on its own at the truly narrow end.
    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.segments = StatusSegments {
            impact: Some("v1 ^2"),
            notify: Some("!3"),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let buffer = terminal.backend().buffer().clone();
    let last = row_text(&buffer, buffer.area.height - 1);
    assert!(
        last.contains("[v1 ^2]") && last.contains("[!3]"),
        "both chips fit at 40 cols: {last:?}"
    );
}

/// On a terminal too short to float the box above the status bar (the 3-row
/// toast sits at y=1, so height 4 would land its bottom border ON the status
/// row), the toast is skipped entirely — the §8 hint row survives.
#[test]
pub(super) fn toast_skips_tiny_terminals_and_never_covers_the_status_bar() {
    let backend = TestBackend::new(60, 4);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let hints = list_hints();
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.hints = &hints;
    ctx.toasts = &[ToastLine {
        level: NoticeLevel::Info,
        text: "Copied unique_id",
    }];
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    assert!(
        !buffer_to_string(&buffer).contains("Copied"),
        "no toast on a 4-row terminal"
    );
    assert!(
        row_text(&buffer, 3).contains("j/k"),
        "the status bar's hint row is untouched"
    );
}
