//! End-to-end tests that drive the real `dbtl` binary inside a real
//! pseudo-terminal — the coverage the `TestBackend` suite cannot give:
//! startup terminal-capability queries, raw key and SGR mouse byte streams,
//! SIGWINCH resizes, and a clean process exit.
//!
//! Every test spawns its own child in its own PTY with isolated XDG dirs.
//! They are `#[ignore]`d so the default `cargo test`/`nextest` runs skip them;
//! run the suite explicitly with:
//!
//! ```sh
//! cargo test --test tui_pty -- --ignored
//! ```

#![cfg(unix)]

mod harness;

use harness::{TuiSession, FIXTURE_MANIFEST};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// Per-scenario upper bound: covers cold launches on loaded CI runners while
/// still failing a wedged session in reasonable time.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The name on the home row carrying the ` > ` cursor marker, or `None`
/// while the marker isn't painted — scraped from the screen so the
/// assertion tracks whichever row is selected, never a hardcoded
/// coordinate. The marker starts the row, so `strip_prefix` can't match a
/// ` > ` inside a row's detail text.
fn selected_home_name(screen: &str) -> Option<String> {
    screen.lines().find_map(|line| {
        let rest = line.strip_prefix(" > ")?;
        rest.split_whitespace().next().map(str::to_string)
    })
}

/// Spawn the binary and wait for the home screen's body — the launch every
/// scenario starts from. The title row reaches the screen before the body
/// does (a frame arrives at the reader in chunks, so a snapshot taken right
/// after the title can show it with the rows still blank). Waiting for the
/// ` > ` selection marker means the body has painted — every scenario then
/// starts from a complete screen.
fn launch_home() -> TuiSession {
    let mut t = TuiSession::spawn();
    if let Err(e) = t.wait_until("the home body's selected row".to_string(), TIMEOUT, |s| {
        s.contains("dbtl home") && selected_home_name(s).is_some()
    }) {
        panic!("{e}");
    }
    t
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn startup_shows_the_home_screen() {
    let t = launch_home();
    let screen = t.screen_text();
    assert!(
        screen.contains("jaffle_finance"),
        "home title names the project:\n{screen}"
    );
    assert!(
        screen.contains("scope:root"),
        "home title names the scope chip:\n{screen}"
    );
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn enter_opens_the_selected_model_tab() {
    let mut t = launch_home();
    // The launch already waited for the ` > ` marker — read which model
    // row it sits on (the marker persists until a key is pressed).
    let name = selected_home_name(&t.screen_text()).expect("the selected row the launch saw");
    t.send_str("\r");
    // The home body leaves the screen and the model-tab panes draw in its
    // place — the list pane's title stats render only there.
    if let Err(e) = t.wait_gone("dbtl home", TIMEOUT) {
        panic!("{e}");
    }
    if let Err(e) = t.wait_for("model:45", TIMEOUT) {
        panic!("{e}");
    }
    // The lineage pane's title names the active tab's root — the full name,
    // unlike the tab strip's length-capped labels. It must be the exact
    // model the ` > ` row was pointing at when Enter went out.
    if let Err(e) = t.wait_for(&format!("Lineage: {name}"), TIMEOUT) {
        panic!("{e}");
    }
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn slash_search_filters_and_esc_restores() {
    let mut t = launch_home();
    t.send_str("/");
    t.send_str("fct_");
    // Wait for the whole filtered frame, not just the match: a mid-frame
    // read can show the match while a stale row from the unfiltered body
    // (`pos_tax`'s Attention entry) is still painted.
    if let Err(e) = t.wait_until("the filtered results".to_string(), TIMEOUT, |s| {
        s.contains("fct_delivery_monthly_snapshot") && !s.contains("pos_tax")
    }) {
        panic!("{e}");
    }
    t.send_str("\x1b"); // Esc
    if let Err(e) = t.wait_for("[Health]", TIMEOUT) {
        panic!("{e}");
    }
}

/// The name on the list row carrying the `> ` selection marker — scraped the
/// same way rows are found, so selection assertions compare tokens exactly.
fn selected_list_name(screen: &str) -> Option<&str> {
    screen
        .lines()
        .find_map(|l| l.strip_prefix("│  > ")?.split_whitespace().next())
}

/// The Enter-hint the status line shows for the currently focused pane —
/// the on-screen signal that focus actually moved. (Three panes exist at
/// 160 cols, so the focus target differs by pane.)
fn enter_hint(screen: &str) -> Option<&'static str> {
    ["Enter open", "Enter re-root", "Enter trace column"]
        .into_iter()
        .find(|h| screen.contains(h))
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn tab_cycles_the_pane_focus() {
    let mut t = launch_home();
    t.send_str("\r"); // open a model tab
    if let Err(e) = t.wait_for("model:45", TIMEOUT) {
        panic!("{e}");
    }
    // Same chunked-frame shape as the launch: the pane's `model:45` title
    // can be on screen while the status line's Enter hint isn't yet —
    // poll for the hint instead of reading once.
    let first = t
        .wait_until(
            "a focus hint on the status line".to_string(),
            TIMEOUT,
            |s| enter_hint(s).is_some(),
        )
        .map(|s| enter_hint(&s).expect("the hint the wait saw"))
        .unwrap_or_else(|e| panic!("{e}"));
    // Each Tab moves focus to a different pane (the Enter hint changes);
    // three presses cycle back to the starting pane.
    t.send_str("\t");
    let second = t
        .wait_until("a different focus hint".to_string(), TIMEOUT, |s| {
            enter_hint(s).is_some_and(|h| h != first)
        })
        .map(|s| enter_hint(&s).expect("a hint is visible"))
        .unwrap_or_else(|e| panic!("{e}"));
    t.send_str("\t");
    if let Err(e) = t.wait_until("a third focus hint".to_string(), TIMEOUT, |s| {
        enter_hint(s).is_some_and(|h| h != second)
    }) {
        panic!("{e}");
    }
    t.send_str("\t");
    if let Err(e) = t.wait_until("the original focus hint".to_string(), TIMEOUT, |s| {
        enter_hint(s) == Some(first)
    }) {
        panic!("{e}");
    }
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn question_mark_opens_and_esc_closes_help() {
    let mut t = launch_home();
    t.send_str("?");
    if let Err(e) = t.wait_for("Keybindings - dbtl", TIMEOUT) {
        panic!("{e}");
    }
    t.send_str("\x1b"); // Esc
                        // The home title only repaints once the overlay is gone — waiting for
                        // it covers both halves of "closing help returns to the home screen",
                        // and can't trip on a mid-restore frame the way a single read can.
    if let Err(e) = t.wait_until(
        "the home screen behind the closed help".to_string(),
        TIMEOUT,
        |s| s.contains("dbtl home") && !s.contains("Keybindings - dbtl"),
    ) {
        panic!("{e}");
    }
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn shrinking_past_the_floor_shows_too_small_and_restoring_recovers() {
    let mut t = launch_home();
    t.resize(40, 10);
    if let Err(e) = t.wait_for("terminal too small", TIMEOUT) {
        panic!("{e}");
    }
    t.resize(160, 44);
    if let Err(e) = t.wait_for("dbtl home", TIMEOUT) {
        panic!("{e}");
    }
    if let Err(e) = t.wait_gone("terminal too small", TIMEOUT) {
        panic!("{e}");
    }
}

/// The first unselected model row on the list pane — `(name, x, y)` in
/// 1-based SGR cell coords — or `None` while the body hasn't painted (no
/// `> ` marker row yet) or no second row exists.
///
/// An unselected row is `│` + 2-indent + 2 marker spaces + the name, and
/// the list pane's border always sits at column 0 — `strip_prefix` keeps
/// pane interiors elsewhere on the row from matching. Names may be
/// mid-truncated (`stg_financ…scal_years`); the rendered text is what both
/// the click target and the marker assertion use.
fn unselected_list_row(screen: &str) -> Option<(String, u16, u16)> {
    // The marker row being absent means the list body hasn't painted — the
    // first row could secretly BE the selected one, so don't pick yet.
    let selected = selected_list_name(screen)?.to_string();
    for (y, line) in screen.lines().enumerate() {
        let Some(rest) = line.strip_prefix("│    ") else {
            continue;
        };
        if let Some(name) = rest.split_whitespace().next() {
            let model_shaped = name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_…".contains(c));
            if model_shaped && name != selected {
                // The click column comes from the screen too: the display
                // width of everything before the name in this row (+1 —
                // SGR coords are 1-based), never a hardcoded cell.
                let byte_off = line.len() - rest.len() + rest.find(name).unwrap();
                let x = UnicodeWidthStr::width(&line[..byte_off]) as u16 + 1;
                return Some((name.to_string(), x, y as u16 + 1));
            }
        }
    }
    None
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn sgr_click_on_a_list_row_selects_that_model() {
    let mut t = launch_home();
    t.send_str("\r"); // open a model tab → list pane visible
    if let Err(e) = t.wait_for("model:45", TIMEOUT) {
        panic!("{e}");
    }
    // The pane title can arrive before the rows do (chunked frame) — wait
    // until a selectable row is actually on screen, then click it.
    let (name, x, y) = {
        let screen = t
            .wait_until("an unselected list row".to_string(), TIMEOUT, |s| {
                unselected_list_row(s).is_some()
            })
            .unwrap_or_else(|e| panic!("{e}"));
        unselected_list_row(&screen).expect("the row the wait saw")
    };
    t.click(x, y);
    // Exact-match the selected marker's row token — a `> {name}` substring
    // could prefix-collide with a longer name or land inside a breadcrumb.
    if let Err(e) = t.wait_until(format!("{name:?} selected"), TIMEOUT, |s| {
        selected_list_name(s) == Some(name.as_str())
    }) {
        panic!("{e}");
    }
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn q_quits_with_exit_code_zero() {
    let mut t = launch_home();
    t.send_str("q");
    match t.wait_exit(TIMEOUT) {
        Ok(status) => assert_eq!(status.exit_code(), 0, "q must exit cleanly, got {status:?}"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
#[ignore = "spawns the real binary on a PTY — run with --ignored"]
fn an_unanswered_deadline_still_kills_the_child() {
    // Queries never answered: the launch takes the crossterm probe's own
    // ~2s fallback, so a short deadline on impossible text must still fire —
    // and Drop must reap the child rather than leak it.
    let mut t = TuiSession::spawn_at(160, 44, FIXTURE_MANIFEST, false);
    let err = t
        .wait_for("zzz-this-text-never-renders", Duration::from_secs(4))
        .expect_err("the impossible needle must time out");
    assert!(err.contains("timed out"), "{err}");
    let pid = t.child_pid().expect("the child has a pid");
    drop(t);
    // kill+wait reaped it: `ps` prints nothing at all for the pid.
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p"])
        .arg(pid.to_string())
        .output()
        .expect("ps");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "child {pid} survived Drop (still in ps): {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
