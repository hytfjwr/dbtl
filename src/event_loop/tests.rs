use super::*;

#[test]
fn split_editor_command_separates_program_and_args() {
    assert_eq!(
        split_editor_command("code --wait"),
        Some(("code".to_string(), vec!["--wait".to_string()]))
    );
    assert_eq!(
        split_editor_command("vim"),
        Some(("vim".to_string(), vec![]))
    );
    // Runs of whitespace (tabs included) never produce empty arguments.
    assert_eq!(
        split_editor_command("  emacs \t -nw  "),
        Some(("emacs".to_string(), vec!["-nw".to_string()]))
    );
}

#[test]
fn split_editor_command_rejects_blank_values() {
    assert_eq!(split_editor_command(""), None);
    assert_eq!(split_editor_command("   \t "), None);
}

/// The editor outcome→notice mapping: a clean exit is silent; a spawn
/// failure (missing binary) and a non-zero exit both produce a toast that
/// names the program — neither may tear the TUI down.
#[test]
fn editor_failure_notice_maps_outcomes() {
    let err: std::io::Result<std::process::ExitStatus> =
        Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    let note = editor_failure_notice("no-such-editor", &err).expect("spawn failure notices");
    assert!(note.contains("no-such-editor"), "{note}");

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let ok = Ok(std::process::ExitStatus::from_raw(0));
        assert_eq!(
            editor_failure_notice("vi", &ok),
            None,
            "clean exit is silent"
        );
        let fail = Ok(std::process::ExitStatus::from_raw(0x100)); // exit code 1
        let note = editor_failure_notice("vi", &fail).expect("non-zero exit notices");
        assert!(note.contains("vi"), "{note}");
    }
}

/// The yank pipe must reap its child unconditionally: a child that exits
/// without reading stdin breaks the pipe mid-write, and the old early `?`
/// skipped `wait()` (zombie). Now the write error comes back as `Err`
/// AFTER the reap — so this returns (no hang) with the failure visible.
#[cfg(unix)]
#[test]
fn feed_and_reap_reaps_child_and_reports_write_failure() {
    // Success path: a child that drains stdin.
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("cat spawns");
    assert!(feed_and_reap(child, "cat", "hello").is_ok());

    // `false` exits immediately; > pipe-buffer text forces a broken pipe.
    let child = Command::new("false")
        .stdin(Stdio::piped())
        .spawn()
        .expect("false spawns");
    let big = "x".repeat(1 << 20);
    assert!(feed_and_reap(child, "false", &big).is_err());
}

/// `restore` is the single teardown path; it must be safe to call multiple
/// times (the panic hook and normal exit may both reach it) and must not
/// panic in a non-TTY test environment.
#[test]
fn restore_is_idempotent_and_tty_free() {
    ratatui::restore();
    ratatui::restore();
}

/// Issue 08: the stack holds at most 3 toasts, newest on top — a 4th push
/// evicts the OLDEST entry so a burst never wedges on stale text.
#[test]
fn toast_stack_caps_at_three_and_evicts_oldest() {
    let item = |text: &str| ToastItem {
        level: dbtl::NoticeLevel::Info,
        text: text.into(),
        at: Instant::now(),
    };
    let mut s = ToastStack::default();
    for t in ["a", "b", "c", "d"] {
        s.push(item(t));
    }
    let texts: Vec<&str> = s.lines().iter().map(|l| l.text).collect();
    assert_eq!(texts, vec!["d", "c", "b"], "newest first, 'a' evicted");
}

/// Per-level TTL: info entries die at 2.5s while same-age warnings still
/// live (5s) — the retain pass only drops expired rows and reports the
/// flip so the loop redraws.
#[test]
fn toast_ttl_differs_by_level_and_retain_drops_expired() {
    assert_eq!(
        toast_ttl(dbtl::NoticeLevel::Info),
        Duration::from_millis(2500)
    );
    assert_eq!(
        toast_ttl(dbtl::NoticeLevel::Warning),
        Duration::from_millis(5000)
    );
    let mut s = ToastStack::default();
    // An info toast stamped at t0 is dead 3s later; a warning of the same
    // age is not. (Forwards-in-time `now` — no `Instant - Duration`
    // backdating, which can underflow-panic on some platforms.)
    let t0 = Instant::now();
    s.push(ToastItem {
        level: dbtl::NoticeLevel::Info,
        text: "old info".into(),
        at: t0,
    });
    s.push(ToastItem {
        level: dbtl::NoticeLevel::Warning,
        text: "same-age warning".into(),
        at: t0,
    });
    assert!(
        s.retain_fresh(t0 + Duration::from_secs(3)),
        "the expired info dropped"
    );
    let texts: Vec<&str> = s.lines().iter().map(|l| l.text).collect();
    assert_eq!(texts, vec!["same-age warning"]);
    assert!(
        !s.retain_fresh(t0 + Duration::from_secs(3)),
        "nothing left to expire"
    );
}

/// The notification stamp is a parseable `HH:MM:SS` from the platform
/// `date` (with the std UTC fallback never producing garbage either).
#[test]
fn now_hhmmss_is_well_formed() {
    let s = now_hhmmss();
    assert_eq!(s.len(), 8, "HH:MM:SS shape: {s:?}");
    let parts: Vec<&str> = s.split(':').collect();
    assert_eq!(parts.len(), 3);
    let h: u32 = parts[0].parse().expect("hour");
    let m: u32 = parts[1].parse().expect("minute");
    let sec: u32 = parts[2].parse().expect("second");
    assert!(h < 24 && m < 60 && sec < 60, "{s} out of range");
}

/// The warnings toast tail pluralizes correctly and joins EVERY warning —
/// a manifest carrying several unknown kinds must not quietly drop all
/// but the first line.
#[test]
fn warnings_tail_pluralizes_and_joins_all() {
    assert_eq!(
        warnings_tail(&["w1".to_string()]),
        "1 warning: w1",
        "singular"
    );
    assert_eq!(
        warnings_tail(&["w1".to_string(), "w2".to_string(), "w3".to_string()]),
        "3 warnings: w1; w2; w3",
        "plural, all warnings joined"
    );
}

#[test]
fn find_in_path_dirs_resolves_executables_only() {
    let dir = std::env::temp_dir().join(format!("dbtl_path_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(if cfg!(windows) { "dbt.exe" } else { "dbt" });
    std::fs::write(&exe, "#!/bin/sh\n").unwrap();

    #[cfg(unix)]
    {
        // Without the execute bit, a file named `dbt` on PATH is data, not a command.
        assert_eq!(find_in_path_dirs("dbt", [dir.clone()]), None);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(find_in_path_dirs("dbt", [dir.clone()]), Some(exe));
    // A dir without the command (and empty path entries) yields None.
    assert_eq!(
        find_in_path_dirs("dbt", [PathBuf::new(), dir.join("missing")]),
        None
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------
// §5 splitter gestures through `handle_mouse`: MouseEvent -> MouseRouter
// -> pane_layout hit-test -> set_pane_width — the same path the loop feeds
// (headless: the router is pure, only `Instant::now()` comes from here).
// ------------------------------------------------------------------

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

fn fixture_app() -> App {
    let dag = dbtl::load_dag(FIXTURE).expect("fixture loads");
    App::new(dag, PathBuf::from(FIXTURE))
}

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

/// Feed one event through the production handler (Selection mode, no
/// lineage layout needed — the splitter strip is pane geometry only).
fn feed(
    app: &mut App,
    router: &mut MouseRouter,
    drag: &mut Option<PaneDrag>,
    kind: MouseEventKind,
    x: u16,
    y: u16,
) {
    feed_l(app, router, drag, kind, x, y, None);
}

/// [`feed`] with a real lineage `Layout` — the Issue 05 gestures (lineage
/// pan, minimap, node hover) hit-test against the drawn diagram. Geometry
/// at 100x30 with the fixture: tab strip row 0, body rows 1..29, list
/// 0..34 (splitter cols 33-34), lineage interior 35..99 x 2..28; the
/// 114x7 grid centres into a 64x7 content rect at y 11..18 — so (40,12)
/// lands the seed box, (50,20) is interior blank, and the enabled
/// minimap sits at 83..99 x 2..8.
fn feed_l(
    app: &mut App,
    router: &mut MouseRouter,
    drag: &mut Option<PaneDrag>,
    kind: MouseEventKind,
    x: u16,
    y: u16,
    lineage: Option<&Layout>,
) {
    handle_mouse(
        app,
        router,
        drag,
        mouse(kind, x, y),
        lineage,
        Rect::new(0, 0, 100, 30),
        &ChromeGeom::default(),
    );
}

/// [`feed_l`] at a caller-chosen terminal `area` — the §12 drawer only
/// exists below 80 columns, so its gestures feed a narrow band.
#[allow(clippy::too_many_arguments)]
fn feed_area(
    app: &mut App,
    router: &mut MouseRouter,
    drag: &mut Option<PaneDrag>,
    kind: MouseEventKind,
    x: u16,
    y: u16,
    lineage: Option<&Layout>,
    area: Rect,
) {
    handle_mouse(
        app,
        router,
        drag,
        mouse(kind, x, y),
        lineage,
        area,
        &ChromeGeom::default(),
    );
}

/// The narrow band the §12 drawer tests feed: 60x24 puts `pane_layout`
/// under `LIST_AUTO_HIDE_W`, so the flag-backed rect actually resolves.
const BAND: Rect = Rect::new(0, 0, 60, 24);

/// Open the drawer the way the loop's per-frame band publish does.
fn open_band_drawer(app: &mut App) {
    app.ui_state.set_list_auto_hidden(true);
    app.ui_state.set_list_drawer(true);
}

/// The first cell hit-testing as `ListRow` under `g` (left-to-right,
/// top-to-bottom) — aimed by the test instead of hardcoded, so a fixture
/// row reshuffle can't silently move the target.
fn list_row_cell(app: &App, lineage: Option<&Layout>, area: Rect) -> (u16, u16) {
    let chrome = ChromeGeom::default();
    let g = chrome.hit_geometry(app, lineage, area, None);
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(HitTarget::ListRow(_)) = hit_target(&g, x, y) {
                return (x, y);
            }
        }
    }
    panic!("no ListRow cell resolved in the geometry");
}

/// Every `(x, y, target)` the geometry resolves — OWNED results, so the
/// borrow on `app` ends here and the caller may mutate state between an
/// open- and closed-surface scan (the drawer reveal test needs both).
fn resolved_targets(app: &App, lineage: Option<&Layout>, area: Rect) -> Vec<(u16, u16, HitTarget)> {
    let chrome = ChromeGeom::default();
    let g = chrome.hit_geometry(app, lineage, area, None);
    let mut out = Vec::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(t) = hit_target(&g, x, y) {
                out.push((x, y, t));
            }
        }
    }
    out
}

#[test]
fn splitter_drag_resizes_the_list_and_tracks_the_grab_offset() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    use MouseEventKind as K;
    // The 34-column default: the boundary strip is columns 33-34, so a
    // press on 33 records grab offset 0 (the list's own border cell).
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Down(MouseButton::Left),
        33,
        10,
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Left),
        50,
        10,
    );
    assert_eq!(
        app.ui_state.list_width(),
        51,
        "grab 0: pointer col 50 wants the border there -> width 51"
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Left),
        45,
        10,
    );
    assert_eq!(app.ui_state.list_width(), 46);
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Up(MouseButton::Left),
        45,
        10,
    );
    assert_eq!(app.ui_state.list_width(), 46, "release commits the width");

    // Grabbing the strip's RIGHT cell (the lineage pane's left border,
    // offset 1) keeps that border under the pointer instead of snapping.
    // Width 46: the strip is columns 45-46.
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Down(MouseButton::Left),
        46,
        10,
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Left),
        60,
        10,
    );
    assert_eq!(
        app.ui_state.list_width(),
        60,
        "grab 1: pointer col 60 wants the right border there -> width 60"
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Up(MouseButton::Left),
        60,
        10,
    );
}

#[test]
fn splitter_drag_clamps_to_the_pane_bounds() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    use MouseEventKind as K;
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Down(MouseButton::Left),
        33,
        10,
    );
    // Past the 60%-of-terminal cap at 100 cols.
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Left),
        95,
        10,
    );
    assert_eq!(
        app.ui_state.list_width(),
        60,
        "the cap wins over the pointer"
    );
    // Below the 18-column floor.
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Left),
        2,
        10,
    );
    assert_eq!(
        app.ui_state.list_width(),
        18,
        "the floor wins over the pointer"
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Up(MouseButton::Left),
        2,
        10,
    );
    assert_eq!(app.ui_state.list_width(), 18);
}

#[test]
fn splitter_flick_without_drag_events_still_resizes() {
    // A fast press -> release-farther-right reports no Drag motion at all;
    // the DragEnd fallback resolves the press cell and commits anyway.
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    use MouseEventKind as K;
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Down(MouseButton::Left),
        33,
        10,
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Up(MouseButton::Left),
        45,
        10,
    );
    assert_eq!(app.ui_state.list_width(), 46, "release col 45 -> width 46");
    assert!(drag.is_none(), "the drag state cleared on release");
}

#[test]
fn splitter_double_click_resets_the_saved_width() {
    let mut app = fixture_app();
    app.ui_state.set_pane_width(Splitter::List, 50, 100);
    let mut router = MouseRouter::new();
    let mut drag = None;
    use MouseEventKind as K;
    // Width 50: the strip is columns 49-50. A plain click is inert — only
    // the double-click resets.
    for (kind, x) in [
        (K::Down(MouseButton::Left), 49),
        (K::Up(MouseButton::Left), 49),
    ] {
        feed(&mut app, &mut router, &mut drag, kind, x, 10);
    }
    assert_eq!(
        app.ui_state.list_width(),
        50,
        "a single click keeps the width"
    );
    for (kind, x) in [
        (K::Down(MouseButton::Left), 49),
        (K::Up(MouseButton::Left), 49),
    ] {
        feed(&mut app, &mut router, &mut drag, kind, x, 10);
    }
    assert_eq!(
        app.ui_state.list_width(),
        dbtl::ui::DEFAULT_LIST_WIDTH,
        "the double-click resets to the §1 default"
    );
}

#[test]
fn non_splitter_drag_and_right_button_are_inert() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    use MouseEventKind as K;
    // A drag that begins on a list row: no splitter, no width change.
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Down(MouseButton::Left),
        5,
        5,
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Left),
        60,
        5,
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Up(MouseButton::Left),
        60,
        5,
    );
    assert_eq!(app.ui_state.list_width(), dbtl::ui::DEFAULT_LIST_WIDTH);
    assert!(drag.is_none());
    // A right-button drag on the boundary is not a resize either.
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Down(MouseButton::Right),
        33,
        10,
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Right),
        60,
        10,
    );
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Up(MouseButton::Right),
        60,
        10,
    );
    assert_eq!(app.ui_state.list_width(), dbtl::ui::DEFAULT_LIST_WIDTH);
    assert!(drag.is_none());
}

#[test]
fn wheel_over_the_list_adopts_the_cursor_model_as_tab_root() {
    // §2 T1 review: a wheel notch over the list moves the CURSOR, and
    // like a j/k move the cursor's model becomes the active tab's root
    // (with the history push) — root≠cursor must not detach here.
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let root0 = app.selected_unique_id().unwrap();
    // (10,10) at 100x30 sits inside the list pane's interior.
    feed(
        &mut app,
        &mut router,
        &mut drag,
        MouseEventKind::ScrollDown,
        10,
        10,
    );
    let landed = app.active_list().model_at(3).unwrap().unique_id.clone();
    assert_eq!(
        app.selected_unique_id().as_deref(),
        Some(landed.as_str()),
        "the wheeled cursor's model became the active tab's root"
    );
    assert_eq!(
        app.tab().back,
        vec![root0.clone()],
        "the wheel re-root pushed the old root onto back"
    );
    app.history_back();
    assert_eq!(
        app.selected_unique_id().as_deref(),
        Some(root0.as_str()),
        "`b` walks the wheel's re-root back"
    );
}

// ------------------------------------------------------------------
// §6 Issue 05 gestures: lineage pan (blank AND node-origin), the
// minimap's click-to-centre + drag-scrub, the hover resolution the
// accent frame keys off, and the abandoned-drag carry-over fix.
// ------------------------------------------------------------------

/// §6/M3: a drag that began on lineage blank space pans the diagram 1:1
/// in BOTH axes — the release is a DragEnd, so it can never become the
/// click that would refocus the pane.
#[test]
fn lineage_blank_drag_pans_horizontally_and_vertically() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let lineage = app.styled_lineage_layout();
    use MouseEventKind as K;
    // Seed the offsets so both directions have room to move.
    app.ui_state.pan_lineage(10, 4);
    for (kind, x, y) in [
        (K::Down(MouseButton::Left), 50, 20),
        (K::Drag(MouseButton::Left), 45, 17),
    ] {
        feed_l(
            &mut app,
            &mut router,
            &mut drag,
            kind,
            x,
            y,
            lineage.as_deref(),
        );
    }
    // Pointer moved (-5, -3) from the press: the picture follows, so the
    // scroll offsets grow by the same delta.
    assert_eq!(app.ui_state.lineage_scroll_x(), 15);
    assert_eq!(app.ui_state.lineage_scroll_y(), 7);
    for (kind, x, y) in [
        (K::Drag(MouseButton::Left), 55, 22),
        (K::Up(MouseButton::Left), 55, 22),
    ] {
        feed_l(
            &mut app,
            &mut router,
            &mut drag,
            kind,
            x,
            y,
            lineage.as_deref(),
        );
    }
    // +5/+2 from the press: saturating pan leaves the offsets clamped at
    // zero (the frame's clamp_lineage bounds the far edge).
    assert_eq!(app.ui_state.lineage_scroll_x(), 5);
    assert_eq!(app.ui_state.lineage_scroll_y(), 2);
    assert!(drag.is_none(), "release ends the pan");
}

/// §6/M3: a drag that began ON a node box is a pan — not a click. The
/// cursor the click would have committed stays home, and the release
/// (a DragEnd) never turns into one either.
#[test]
fn lineage_node_drag_pans_and_never_clicks() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let lineage = app.styled_lineage_layout();
    use MouseEventKind as K;
    // (40,12) is inside the seed box (grid 0..26 x 0..3, content origin
    // 35,11 at scroll 0).
    let cursor_before = app.lineage_cursor_uid();
    for (kind, x, y) in [
        (K::Down(MouseButton::Left), 40, 12),
        (K::Drag(MouseButton::Left), 38, 12),
        (K::Up(MouseButton::Left), 38, 12),
    ] {
        feed_l(
            &mut app,
            &mut router,
            &mut drag,
            kind,
            x,
            y,
            lineage.as_deref(),
        );
    }
    assert_eq!(app.ui_state.lineage_scroll_x(), 2, "the node drag panned");
    assert_eq!(
        app.lineage_cursor_uid(),
        cursor_before,
        "no click happened: the cursor never committed to the dragged node"
    );
}

/// §6: a left click on the minimap centres the viewport on the clicked
/// cell's grid block — (95,4) is 12 cells into the 16-wide inset.
#[test]
fn minimap_click_centres_the_viewport() {
    let mut app = fixture_app();
    app.ui_state.set_minimap(true);
    let mut router = MouseRouter::new();
    let mut drag = None;
    let lineage = app.styled_lineage_layout();
    use MouseEventKind as K;
    for kind in [K::Down(MouseButton::Left), K::Up(MouseButton::Left)] {
        feed_l(
            &mut app,
            &mut router,
            &mut drag,
            kind,
            95,
            4,
            lineage.as_deref(),
        );
    }
    // mx=12, sx=ceil(114/16)=8 -> 12*8+4-32 = 68 (stored raw; the frame's
    // clamp_lineage bounds it to the far edge like every scroll writer).
    assert_eq!(app.ui_state.lineage_scroll_x(), 68);
}

/// §6: a minimap drag scrubs the viewport — every tracked cell recentres
/// through the same `minimap_scroll_to` mapping the click uses.
#[test]
fn minimap_drag_scrubs_the_viewport() {
    let mut app = fixture_app();
    app.ui_state.set_minimap(true);
    let mut router = MouseRouter::new();
    let mut drag = None;
    let lineage = app.styled_lineage_layout();
    use MouseEventKind as K;
    for (kind, x, y) in [
        (K::Down(MouseButton::Left), 85, 4),
        (K::Drag(MouseButton::Left), 90, 4),
        (K::Drag(MouseButton::Left), 95, 4),
        (K::Up(MouseButton::Left), 95, 4),
    ] {
        feed_l(
            &mut app,
            &mut router,
            &mut drag,
            kind,
            x,
            y,
            lineage.as_deref(),
        );
    }
    assert_eq!(
        app.ui_state.lineage_scroll_x(),
        68,
        "the scrub tracked the release cell's block centre"
    );
    assert!(drag.is_none());
}

/// The accent frame's input: a `Moved` event over a node resolves the
/// hover target the renderer accents — deduped on the same cell, cleared
/// off the node.
#[test]
fn hover_tracks_the_node_under_the_pointer() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let lineage = app.styled_lineage_layout();
    use MouseEventKind as K;
    feed_l(
        &mut app,
        &mut router,
        &mut drag,
        K::Moved,
        40,
        12,
        lineage.as_deref(),
    );
    assert_eq!(
        router.hover(),
        Some(&HitTarget::LineageNode(
            "seed.jaffle_finance.source_datetime_policy".to_string()
        ))
    );
    // Moving to blank space clears the node target.
    feed_l(
        &mut app,
        &mut router,
        &mut drag,
        K::Moved,
        50,
        20,
        lineage.as_deref(),
    );
    assert_eq!(router.hover(), Some(&HitTarget::LineageBlank));
}

/// Issue 02 carry-over: a splitter drag abandoned by a superseding press
/// can never resize on the new press's motion. The router keeps the
/// newest press, so the in-flight drag is orphaned — the gesture gate
/// must clear it, not keep it tracking.
#[test]
fn abandoned_splitter_drag_clears_on_a_superseding_press() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    use MouseEventKind as K;
    // Arm a splitter drag, then press the RIGHT button mid-drag (a real
    // multi-button press or a terminal that dropped the left release).
    for (kind, x) in [
        (K::Down(MouseButton::Left), 33),
        (K::Drag(MouseButton::Left), 50),
    ] {
        feed(&mut app, &mut router, &mut drag, kind, x, 10);
    }
    assert_eq!(app.ui_state.list_width(), 51, "the left drag resized");
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Down(MouseButton::Right),
        10,
        5,
    );
    for x in [15, 20] {
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Right),
            x,
            5,
        );
    }
    assert_eq!(
        app.ui_state.list_width(),
        51,
        "the abandoned drag stopped tracking instead of following the right drag"
    );
    assert!(drag.is_none());
}

/// Issue 02 carry-over, second half: a mode change mid-drag abandons it
/// too — the stored drag is cleared the moment pointer routing leaves
/// `Selection`, so a later inert-mode move can't resize either.
#[test]
fn abandoned_splitter_drag_clears_on_a_mode_change() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    use MouseEventKind as K;
    for (kind, x) in [
        (K::Down(MouseButton::Left), 33),
        (K::Drag(MouseButton::Left), 50),
    ] {
        feed(&mut app, &mut router, &mut drag, kind, x, 10);
    }
    assert_eq!(app.ui_state.list_width(), 51);
    app.mode = dbtl::Mode::Help(dbtl::HelpView::new(dbtl::Mode::Selection));
    feed(
        &mut app,
        &mut router,
        &mut drag,
        K::Drag(MouseButton::Left),
        60,
        10,
    );
    assert_eq!(
        app.ui_state.list_width(),
        51,
        "the modal mode abandoned the drag"
    );
    assert!(drag.is_none());
}

// ------------------------------------------------------------------
// §9 Issue 05 review: the settled tooltip drops on press / scroll /
// leave, and never arms for a `+M more` display-chrome marker.
// ------------------------------------------------------------------

/// The reconcile's press half lives in `handle_mouse`: a Down feeds the
/// router no gesture, so the event must itself dirty — otherwise no
/// reconcile pass ever observes `mouse.press()` (the router clears it
/// on release before the Click gesture) and a click slides under a
/// visible tip.
#[test]
fn a_press_dirties_so_the_reconcile_observes_the_held_button() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let lineage = app.styled_lineage_layout();
    let handled = handle_mouse(
        &mut app,
        &mut router,
        &mut drag,
        mouse(MouseEventKind::Down(MouseButton::Left), 40, 12),
        lineage.as_deref(),
        Rect::new(0, 0, 100, 30),
        &ChromeGeom::default(),
    );
    assert!(
        handled.dirty,
        "a press dirties so the held button drops the tip"
    );
    assert!(router.press().is_some(), "the press is now observable");
}

/// §9's "消す条件": the tip drops on a press (even though the same node
/// still sits under the pointer) and on a viewport move that leaves the
/// SAME node under the pointer — the anchored box went stale.
#[test]
fn tooltip_clears_on_press_and_on_same_node_scroll() {
    let app = fixture_app();
    let node = HitTarget::LineageNode("seed.jaffle_finance.source_datetime_policy".to_string());
    let mut tip = Some(HoverTip {
        uid: "seed.jaffle_finance.source_datetime_policy".to_string(),
        scroll: (0, 0),
        since: Instant::now(),
        visible: true,
    });
    // A held button: hover_uid would still resolve the node, but the
    // pressed check drops the tip outright.
    reconcile_hover_tip(&mut tip, Some(&node), true, (0, 0), &app.dag);
    assert!(tip.is_none(), "a press drops the tip");

    // Re-arm, settle, then scroll with the same node still hovered: the
    // visible tip can't ride along at its stale anchor — it re-arms.
    reconcile_hover_tip(&mut tip, Some(&node), false, (0, 0), &app.dag);
    tip.as_mut().unwrap().visible = true;
    reconcile_hover_tip(&mut tip, Some(&node), false, (4, 0), &app.dag);
    let t = tip.as_ref().expect("a same-node hover re-arms");
    assert!(
        !t.visible,
        "a scroll under the pointer drops the visible tip"
    );
    assert_eq!(t.scroll, (4, 0), "the re-arm tracked the new viewport");
    // Same node AND same viewport: the rest continues undisturbed.
    tip.as_mut().unwrap().visible = true;
    reconcile_hover_tip(&mut tip, Some(&node), false, (4, 0), &app.dag);
    assert!(tip.as_ref().unwrap().visible, "no scroll → the tip stays");
}

/// `+M more` markers hit-test as `LineageNode` but are display chrome —
/// `dag.get` has nothing to describe, so the tip must never arm (it
/// would only dirty the 500ms flip for a `tip_view` that is `None`).
#[test]
fn tooltip_never_arms_for_a_collapse_marker() {
    let app = fixture_app();
    let marker = HitTarget::LineageNode("model.jaffle_finance.stg_x+more".to_string());
    let mut tip = None;
    reconcile_hover_tip(&mut tip, Some(&marker), false, (0, 0), &app.dag);
    assert!(tip.is_none(), "a marker uid arms no tooltip");
    // And a REAL node arms normally — the guard doesn't over-filter.
    let node = HitTarget::LineageNode("seed.jaffle_finance.source_datetime_policy".to_string());
    reconcile_hover_tip(&mut tip, Some(&node), false, (0, 0), &app.dag);
    assert!(tip.is_some(), "a real node still arms the tip");
}

// ---- §12 narrow-band drawer: loop-level gestures ----

/// A left press+release at a cell — one full click through the router.
fn click(
    app: &mut App,
    router: &mut MouseRouter,
    drag: &mut Option<PaneDrag>,
    x: u16,
    y: u16,
    lineage: Option<&Layout>,
) {
    feed_area(
        app,
        router,
        drag,
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
        lineage,
        BAND,
    );
    feed_area(
        app,
        router,
        drag,
        MouseEventKind::Up(MouseButton::Left),
        x,
        y,
        lineage,
        BAND,
    );
}

/// A click on a drawer row picks the model and closes the surface —
/// and the closed surface hands list focus back to the lineage.
#[test]
fn drawer_row_click_picks_and_closes() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    open_band_drawer(&mut app);
    let (x, y) = list_row_cell(&app, None, BAND);
    click(&mut app, &mut router, &mut drag, x, y, None);
    assert!(
        !app.ui_state.list_drawer(),
        "a drawer-row pick closes the drawer"
    );
    assert_eq!(
        app.ui_state.focus(),
        Focus::RightPane,
        "the closed surface hands focus to the lineage"
    );
}

/// The pick's follow-up `DoubleClick` must be inert: the row cell it
/// resolves on the REVEALED surface is a lineage node the user never
/// aimed at — firing its arm would re-root or open the structure modal.
#[test]
fn drawer_pick_makes_the_follow_up_double_click_inert() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    open_band_drawer(&mut app);
    let lineage = app.styled_lineage_layout();
    // Aim at a cell that is a drawer row while open AND a non-model
    // lineage node once revealed (a seed/source double-click opens the
    // structure modal — the failure is observable in `app.mode`).
    let (x, y) = {
        let rows: Vec<(u16, u16)> = resolved_targets(&app, lineage.as_deref(), BAND)
            .into_iter()
            .filter_map(|(x, y, t)| matches!(t, HitTarget::ListRow(_)).then_some((x, y)))
            .collect();
        app.ui_state.set_list_drawer(false);
        let revealed = resolved_targets(&app, lineage.as_deref(), BAND);
        app.ui_state.set_list_drawer(true);
        let mut fallback = None;
        let mut aim = None;
        for (x, y) in rows {
            let under = revealed
                .iter()
                .find(|(rx, ry, _)| *rx == x && *ry == y)
                .map(|(_, _, t)| t);
            if let Some(HitTarget::LineageNode(uid)) = under {
                if !uid.starts_with("model.") {
                    aim = Some((x, y));
                    break;
                }
                fallback = fallback.or(Some((x, y)));
            }
        }
        aim.or(fallback).expect("a drawer row over a lineage node")
    };
    // The pick: drawer closes, model adopted.
    click(&mut app, &mut router, &mut drag, x, y, lineage.as_deref());
    assert!(!app.ui_state.list_drawer(), "the pick closes the drawer");
    // The double-click that gesture-wise follows MUST be inert.
    click(&mut app, &mut router, &mut drag, x, y, lineage.as_deref());
    assert!(
        matches!(app.mode, Mode::Selection),
        "the follow-up double must not fire on the revealed node — got {:?}",
        app.mode
    );
}

/// A click outside the open drawer dismisses it (`ModalExterior`), and
/// a right-click INSIDE keeps it for the context menu.
#[test]
fn drawer_exterior_click_dismisses_right_click_keeps() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    open_band_drawer(&mut app);
    let (x, y) = list_row_cell(&app, None, BAND);
    // Right-click a row: the context menu is not a pick — drawer stays.
    feed_area(
        &mut app,
        &mut router,
        &mut drag,
        MouseEventKind::Down(MouseButton::Right),
        x,
        y,
        None,
        BAND,
    );
    feed_area(
        &mut app,
        &mut router,
        &mut drag,
        MouseEventKind::Up(MouseButton::Right),
        x,
        y,
        None,
        BAND,
    );
    assert!(
        app.ui_state.list_drawer(),
        "a context-menu right-click keeps the drawer"
    );
}

#[test]
fn drawer_exterior_click_dismisses() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    open_band_drawer(&mut app);
    // Far right of the 34-wide drawer: plain lineage interior.
    click(&mut app, &mut router, &mut drag, 50, 12, None);
    assert!(
        !app.ui_state.list_drawer(),
        "a click outside the drawer dismisses it"
    );
}

/// The wheel over the drawer interior scrolls the list (cursor + adopt),
/// the same notch the inline pane takes.
#[test]
fn drawer_wheel_scrolls_the_list() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    open_band_drawer(&mut app);
    let (x, y) = list_row_cell(&app, None, BAND);
    let before = app.ui_state.selected();
    feed_area(
        &mut app,
        &mut router,
        &mut drag,
        MouseEventKind::ScrollDown,
        x,
        y,
        None,
        BAND,
    );
    assert_ne!(
        app.ui_state.selected(),
        before,
        "a wheel notch over the drawer interior moves the list cursor"
    );
}

// ---- Issue 10: the home body is a panes-owning surface -------------

/// The first cell hit-testing as `HomeRow(i)` under `g` — aimed by the
/// test instead of hardcoded, like [`list_row_cell`].
fn home_row_cell(app: &App, area: Rect) -> (u16, u16, usize) {
    let chrome = ChromeGeom::default();
    let g = chrome.hit_geometry(app, None, area, None);
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(HitTarget::HomeRow(i)) = hit_target(&g, x, y) {
                return (x, y, i);
            }
        }
    }
    panic!("no HomeRow cell resolved in the geometry");
}

/// [`click`] at a caller-chosen `area` (the home tests run the wide
/// 100x30 layout, not the band).
fn click_at(
    app: &mut App,
    router: &mut MouseRouter,
    drag: &mut Option<PaneDrag>,
    x: u16,
    y: u16,
    area: Rect,
) {
    feed_area(
        app,
        router,
        drag,
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
        None,
        area,
    );
    feed_area(
        app,
        router,
        drag,
        MouseEventKind::Up(MouseButton::Left),
        x,
        y,
        None,
        area,
    );
}

/// The wide layout the home tests share (tab strip + body band).
const WIDE: Rect = Rect::new(0, 0, 100, 30);

/// §3's 項目はクリックでも開く: a release-confirmed click through
/// `handle_mouse` on a `HomeRow` cell IS the row's Enter — the model
/// tab activates (this is the routing the mode gate used to drop).
#[test]
fn home_row_click_opens_the_model_tab() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let m1 = app.active_list().models[1].unique_id.clone();
    app.open_node_as_tab(&m1);
    app.enter_home();
    assert!(app.home_active(), "the fixture lands on the home tab");
    let (x, y, _i) = home_row_cell(&app, WIDE);
    click_at(&mut app, &mut router, &mut drag, x, y, WIDE);
    assert!(
        !app.home_active(),
        "the row click opened a model tab like Enter"
    );
    assert!(matches!(app.mode, Mode::Selection));
}

/// A row pick swaps the surface under the pointer — the follow-up
/// `DoubleClick` must be inert (a list row under the revealed panes
/// would fire `DetailOpen`).
#[test]
fn home_row_pick_makes_the_follow_up_double_click_inert() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let m1 = app.active_list().models[1].unique_id.clone();
    app.open_node_as_tab(&m1);
    app.enter_home();
    let (x, y, _i) = home_row_cell(&app, WIDE);
    click_at(&mut app, &mut router, &mut drag, x, y, WIDE);
    assert!(!app.home_active(), "the pick opened the model tab");
    // Same cell, inside the double-click window: the router emits a
    // DoubleClick — suppressed, so it can't fire on the revealed panes.
    click_at(&mut app, &mut router, &mut drag, x, y, WIDE);
    assert!(
        matches!(app.mode, Mode::Selection),
        "the follow-up double must stay inert — got {:?}",
        app.mode
    );
}

/// The wheel over the home body moves the home cursor — the same
/// notch→cursor semantics the list wheel keeps; the tab strip still
/// scrolls the strip.
#[test]
fn home_wheel_moves_the_home_cursor() {
    let mut app = fixture_app();
    let mut router = MouseRouter::new();
    let mut drag = None;
    let m1 = app.active_list().models[1].unique_id.clone();
    app.open_node_as_tab(&m1);
    app.enter_home();
    let (x, y, _) = home_row_cell(&app, WIDE);
    assert_eq!(app.home_sel, 0);
    feed_area(
        &mut app,
        &mut router,
        &mut drag,
        MouseEventKind::ScrollDown,
        x,
        y,
        None,
        WIDE,
    );
    assert_eq!(app.home_sel, 1, "a notch down moved the home cursor");
    feed_area(
        &mut app,
        &mut router,
        &mut drag,
        MouseEventKind::ScrollUp,
        x,
        y,
        None,
        WIDE,
    );
    assert_eq!(app.home_sel, 0);
}

/// §12's drawer flag can never shadow the home body: entering home
/// clears it, and even a stale flag loses to the home arm's hit order.
#[test]
fn drawer_flag_cannot_shadow_the_home_body() {
    let mut app = fixture_app();
    open_band_drawer(&mut app);
    app.enter_home();
    assert!(
        !app.ui_state.list_drawer(),
        "entering home clears the drawer flag"
    );
    // Even a flag re-set out from under it loses: the home arm owns the
    // band in `hit_target`, so body cells stay `HomeRow`/inert, never
    // `ListRow`/`ModalExterior`.
    app.ui_state.set_list_drawer(true);
    let targets: Vec<HitTarget> = resolved_targets(&app, None, BAND)
        .into_iter()
        .map(|(_, _, t)| t)
        .collect();
    assert!(
        targets.iter().any(|t| matches!(t, HitTarget::HomeRow(_))),
        "home rows still hit-test under the phantom flag"
    );
    assert!(
        !targets
            .iter()
            .any(|t| matches!(t, HitTarget::ModalExterior | HitTarget::ModalInterior)),
        "no drawer targets leak over the home body: {targets:?}"
    );
}
