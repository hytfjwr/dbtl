//! The interactive event loop: the only size-aware layer. Follows scroll
//! (list + lineage), draws, reads an event, maps it to an [`Action`] via the
//! keymap, applies it, runs any [`Effect`]s, and repeats until quit — plus the
//! impure helpers those effects need (editor suspend, clipboard, `dbt parse`).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use dbtl::action::{dispatch, Action};
use dbtl::app::{apply_action, App, LineageView};
use dbtl::effect::Effect;
use dbtl::layout::Layout;
use dbtl::ui::{
    draw, hit_test, lineage_content_rect, pane_interior, pane_rects, Focus, LineageLens, RenderCtx,
    StatusSegments,
};
use dbtl::{Mode, SortMode};

use ratatui::crossterm::event::{
    self, Event, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;

use crate::terminal::{set_keyboard_enhancement, set_mouse_capture};
use crate::watch::Watch;

/// The event loop: follow scroll (size-aware), draw, read an event, map it to an
/// [`Action`] via the keymap, apply it, run any effects, repeat until quit.
///
/// The size-aware steps (`ensure_visible`, `anchor_lineage`,
/// `ensure_lineage_visible`, `clamp_lineage`) stay here — the pure reducer
/// (`apply_action`) never sees pane geometry. Only `KeyEventKind::Press` drives
/// state, so Press+Release/Repeat terminals don't double-fire.
pub fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    watch: &mut Option<Watch>,
) -> Result<()> {
    // Track the selection (by unique_id, so a filter change keeping the same
    // node doesn't churn) and the lineage cursor, to re-anchor / follow on
    // change. `last_selection: None` forces an anchor on the first frame;
    // `force_anchor` is set by actions that reshape/recentre the lineage
    // without moving the selection.
    let mut last_selection: Option<String> = None;
    let mut last_cursor: Option<String> = None;
    let mut force_anchor = false;
    // The on-screen toast: the latest App notice stamped with its arrival time.
    // The loop owns the lifetime (the reducer stays clock-free): a new notice
    // replaces the current toast, and an expired one clears on the next
    // iteration — the 250ms idle tick guarantees that redraw happens promptly.
    const TOAST_TTL: Duration = Duration::from_millis(2500);
    let mut toast: Option<(String, std::time::Instant)> = None;
    // Dirty-flag redraw: the frame work below only runs when something
    // actually changed (first frame, any event, a toast flip, a --watch
    // reload). An idle 250ms tick with none of those skips straight back to
    // `event::poll` — before this, every tick recomputed the O(N) status
    // aggregates and rewrote the whole terminal.
    let mut dirty = true;
    // The last frame's lineage layout + full area: recomputed only inside a
    // dirty pass, but the mouse handler reads them after the draw section —
    // hit-testing against a stale layout would misattribute clicks.
    let mut lineage: Option<std::rc::Rc<dbtl::Layout>> = None;
    let mut area = ratatui::layout::Rect::default();

    loop {
        if let Some(text) = app.take_notice() {
            toast = Some((text, std::time::Instant::now()));
            dirty = true;
        }
        if toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= TOAST_TTL)
        {
            toast = None;
            dirty = true;
        }
        if dirty {
            let sel = app.ui_state.selected();
            let sel_uid = app.selected_unique_id();
            let selection_changed = last_selection != sel_uid;
            if selection_changed {
                // The single cursor-reset chokepoint: EVERY selection change (keys,
                // mouse click, search confirm, history, reload-restore) funnels
                // through here before the next draw, so the cursor always re-homes
                // to the new root — no per-call-site resets to forget.
                app.reset_lineage_cursor();
            }

            // Follow list scroll BEFORE drawing (display-row space) so a jump never
            // renders a stale frame with the selection off-screen. Read the row
            // numbers from the active list first (immutable borrow), then mutate.
            let (reveal_row, model_row, row_count) = {
                let list = app.active_list();
                (
                    list.reveal_row_of_model(sel),
                    list.row_of_model(sel),
                    list.row_count(),
                )
            };
            let list_height = list_pane_height(terminal)?;
            app.ui_state
                .ensure_visible(list_height, reveal_row, model_row, row_count);

            // The lineage layout for the current selection — memoized in App, so an
            // idle tick or a non-lineage keypress reuses the previous frame's
            // subgraph + layout + styles instead of recomputing them. The DISPLAY
            // subgraph carries the lineage cursor as its `selected`, so the
            // emphasis highlight and `selected_rect` below both track the cursor;
            // the viewport is then sized exactly as `draw` will size it, so
            // anchor/clamp matches the drawn window (the 2D display-row guarantee).
            lineage = app.styled_lineage_layout();
            let (view_w, view_h) = lineage_viewport(terminal, app.ui_state.list_visible())?;
            let cursor = app.lineage_cursor_uid();
            if let Some(lay) = lineage.as_deref() {
                // A lineage node-jump search anchors the viewport on the cycled match
                // (size-aware), overriding the usual selection-based anchor/clamp.
                // `current_lineage_match` is `None` for an empty query, so opening
                // search never jolts the view off the rooted node.
                let search_rect = app
                    .current_lineage_match()
                    .and_then(|uid| lay.rects.get(&uid).copied());
                if let Some(rect) = search_rect {
                    app.ui_state
                        .anchor_lineage(Some(rect), &lay.grid, view_w, view_h);
                } else if selection_changed || force_anchor {
                    // New root / reshaped view: centre on the display-selected node
                    // (the cursor, which a selection change just re-homed to the root).
                    app.ui_state
                        .anchor_lineage(lay.selected_rect, &lay.grid, view_w, view_h);
                } else if last_cursor != cursor {
                    // Cursor moved within the same graph: scroll MINIMALLY so the
                    // cursor's box shows — never a re-center (that would bring back
                    // the per-keypress viewport pan the cursor replaced).
                    if let Some(rect) = lay.selected_rect {
                        app.ui_state.ensure_lineage_visible(
                            rect,
                            lay.grid.width(),
                            lay.grid.height(),
                            view_w,
                            view_h,
                        );
                    }
                } else {
                    app.ui_state
                        .clamp_lineage(lay.grid.width(), lay.grid.height(), view_w, view_h);
                }
            }
            last_selection = sel_uid;
            last_cursor = cursor;
            force_anchor = false;

            // Overlay scroll clamp (size-aware), same seam as the lineage clamp: the
            // pure reducer records scroll intent, the loop bounds it to the screen.
            area = full_area(terminal)?;
            match &mut app.mode {
                dbtl::Mode::Help { scroll } => {
                    *scroll = dbtl::ui::clamp_help_scroll(area, *scroll);
                }
                dbtl::Mode::Detail(dv) => {
                    dv.scroll = dbtl::ui::clamp_detail_scroll(area, dv);
                }
                dbtl::Mode::Sql(sv) => {
                    sv.scroll = dbtl::ui::clamp_sql_scroll(area, sv);
                }
                dbtl::Mode::Stats(sv) => {
                    sv.scroll = dbtl::ui::clamp_stats_scroll(area, sv);
                }
                dbtl::Mode::Diff(dv) => {
                    dv.scroll = dbtl::ui::clamp_diff_scroll(area, dv);
                }
                _ => {}
            }

            // Precompute Dag-aware strings so RenderCtx stays Dag-free; the borrows
            // live for the draw call below.
            let status_note = app.selected_status_note();
            let view_label = app.lineage_view_label();
            // The re-root breadcrumb for the lineage pane title. `App::breadcrumb` is
            // size-unaware (max_width is a parameter); the FULL trail is handed to the
            // draw seam, which is the single width authority — it strictly re-truncates
            // the crumb so the trailing `[{v}]{lens}` title suffixes always survive.
            // `None` until the user re-roots.
            let breadcrumb = app.breadcrumb(usize::MAX);
            let overview_title = app.overview_title();

            // Status-bar segments (all owned `String`s precomputed here; the borrows
            // live to the draw call). Each is set only when meaningful — an absent
            // `Option` simply omits that segment (graceful degradation seam).
            let focus_s = match app.ui_state.focus() {
                Focus::List => "list",
                Focus::RightPane => "lineage",
            };
            // Position is meaningful only when the active list has selectable models;
            // an empty project or a zero-match search omits the segment entirely
            // (rather than papering over the empty case as a misleading `1/1`).
            let pos_s = if app.active_list().is_empty() {
                None
            } else {
                Some(format!(
                    "{}/{}",
                    app.ui_state.selected() + 1,
                    app.active_list().len()
                ))
            };
            let view_seg = if app.ui_state.overview() {
                Some("overview")
            } else if app.lineage_view != LineageView::default() {
                Some(view_label.as_str())
            } else {
                None
            };
            // The cov% segment keys on the COVERAGE lens specifically (the other
            // lenses recolour the lineage but quote no coverage number).
            let coverage_s = if app.ui_state.lens() == LineageLens::Coverage {
                let (tested, total) = app.coverage_summary();
                let pct = (tested * 100).checked_div(total).unwrap_or(0);
                Some(format!("cov {pct}% ({tested}/{total})"))
            } else {
                None
            };
            let bookmarks_s = if app.bookmarks.is_empty() {
                None
            } else {
                Some(format!("bm:{}", app.bookmarks.len()))
            };
            let sort_s = if app.sort != SortMode::default() {
                Some(format!("sort:{}", app.sort.label()))
            } else {
                None
            };
            // Blast radius of the selected node (always-on; arrows from the active
            // glyph mode's Chrome badges — never hardcoded). Built in App so the
            // string source is single and unit-testable.
            let impact_s = app.impact_status();
            // The baseline-diff chip (present only with a --diff baseline loaded).
            let diff_s = app.diff_status_label();
            // The run_results chip (coverage + freshness of the loaded artifact).
            let run_s = app.run_status_label();
            // Layer bands: the lineage pane's bottom-border column annotations,
            // present exactly while the Layer lens is active (cheap: one pass over
            // the layout's rects, only computed when the lens is on).
            let layer_bands = match (&lineage, app.ui_state.lens()) {
                (Some(lay), LineageLens::Layer) => Some(app.layer_bands(lay)),
                _ => None,
            };
            {
                let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, lineage.as_deref());
                ctx.mode = &app.mode;
                ctx.status = status_note.as_deref();
                ctx.stats = Some(&app.stats);
                ctx.lineage_label = Some(&view_label);
                ctx.breadcrumb = breadcrumb.as_deref();
                ctx.overview_title = overview_title.as_deref();
                ctx.glyphs = app.glyph_mode;
                ctx.bookmarks = Some(&app.bookmarks);
                // Full model count for the search title's N/M (M = full list len).
                ctx.full_model_count = Some(app.model_list.len());
                ctx.segments = StatusSegments {
                    impact: impact_s.as_deref(),
                    focus: Some(focus_s),
                    view: view_seg,
                    position: pos_s.as_deref(),
                    coverage: coverage_s.as_deref(),
                    bookmarks: bookmarks_s.as_deref(),
                    sort: sort_s.as_deref(),
                    diff: diff_s.as_deref(),
                    run: run_s.as_deref(),
                    filter: app.list_filter_label(),
                };
                ctx.minimap = app.ui_state.minimap_visible();
                ctx.filter_label = app.list_filter_label();
                ctx.layer_bands = layer_bands.as_deref();
                ctx.toast = toast.as_ref().map(|(text, _)| text.as_str());
                ctx.theme = app.active_theme();
                terminal
                    .draw(|frame| draw(frame, &ctx))
                    .context("failed to draw frame")?;
            }
            dirty = false;
        }

        // Block for the next event (no busy-poll). An idle tick (timeout) is
        // where `--watch` checks the data source for on-disk changes.
        if !event::poll(Duration::from_millis(250)).context("event poll failed")? {
            if let Some(w) = watch.as_mut() {
                if w.poll() {
                    // Same semantics as the `r` key: on failure the app keeps
                    // running on the old data; the reshaped graph re-centres.
                    let note = match app.reload() {
                        Ok(w) if w.is_empty() => "Reloaded (source changed)".to_string(),
                        Ok(w) => format!("Reloaded (source changed) — {}", warnings_tail(&w)),
                        Err(_) => "Reload failed (kept previous data)".to_string(),
                    };
                    app.set_notice(note);
                    force_anchor = true;
                    dirty = true;
                }
            }
            continue;
        }
        {
            dirty = true; // any delivered event may change the frame
            match event::read().context("event read failed")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = dispatch(&app.mode, key) {
                        // Actions that change the lineage *shape/centring* without
                        // moving the selection must force the anchor branch next
                        // frame (a size-aware loop concern). Reload rebuilds the
                        // Dag and restores selection by id, so the reshaped graph
                        // must re-centre even when the selected id is unchanged.
                        let reanchor = matches!(
                            action,
                            Action::Recenter
                                | Action::ToggleUpstream
                                | Action::ToggleDownstream
                                | Action::DepthDecrease
                                | Action::DepthIncrease
                                | Action::ResetView
                                | Action::Reload
                                | Action::ToggleListPane
                                | Action::DbtParse
                                | Action::ToggleDensity
                                | Action::ToggleOverview
                        );
                        let outcome = apply_action(app, action);
                        for effect in outcome.effects {
                            run_effect(terminal, app, effect)?;
                        }
                        // An effect can swap the data source (dbt parse adopts
                        // the manifest); re-baseline the watch poller so it
                        // follows the NEW origin instead of the stale one.
                        if let Some(w) = watch.as_mut() {
                            let (root, recursive) = app.watch_root();
                            if w.root != root || w.recursive != recursive {
                                *w = Watch::new(root.to_path_buf(), recursive);
                            }
                        }
                        if outcome.quit {
                            return Ok(());
                        }
                        if reanchor {
                            force_anchor = true;
                        }
                    }
                }
                Event::Mouse(me) => handle_mouse(app, me, lineage.as_deref(), area),
                _ => {}
            }
        }
    }
}

/// Handle a mouse event (size-aware, so it lives in the loop, not the reducer).
/// Click selects the model under the cursor in the list, or re-roots to the
/// clicked lineage node (via the pure [`hit_test`]); the wheel scrolls the pane
/// under the cursor. Acts only in `Selection` mode — overlays own the screen.
fn handle_mouse(app: &mut App, me: MouseEvent, lineage: Option<&Layout>, area: Rect) {
    if !matches!(app.mode, Mode::Selection) {
        return;
    }
    let rects = pane_rects(area, app.ui_state.list_visible());
    let list_inner = pane_interior(rects.list);
    let lin_inner = pane_interior(rects.lineage);
    let (col, row) = (me.column, me.row);

    match me.kind {
        MouseEventKind::Down(_) => {
            if within(list_inner, col, row) {
                app.ui_state.set_focus(Focus::List);
                let r = app.ui_state.offset() + (row - list_inner.y) as usize;
                let mi = app.active_list().model_at_row(r);
                if let Some(mi) = mi {
                    app.ui_state.set_selected(mi);
                }
            } else if within(lin_inner, col, row) {
                app.ui_state.set_focus(Focus::RightPane);
                if let Some(lay) = lineage {
                    // Hit-test against the SAME centered content rect the renderer
                    // draws into, so a click lands on the node actually under it.
                    let content =
                        lineage_content_rect(lin_inner, lay.grid.width(), lay.grid.height());
                    let hit = hit_test(
                        &lay.rects,
                        app.ui_state.lineage_scroll_x(),
                        app.ui_state.lineage_scroll_y(),
                        content,
                        col,
                        row,
                    );
                    if let Some(uid) = hit {
                        // Re-root for a model (records history) / move the
                        // cursor for a source-seed-snapshot; the domain method
                        // also re-homes the cursor when the root is clicked.
                        app.click_lineage_node(&uid);
                    }
                }
            }
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let down = me.kind == MouseEventKind::ScrollDown;
            let shift = me.modifiers.contains(KeyModifiers::SHIFT);
            wheel(app, list_inner, lin_inner, col, row, down, shift);
        }
        // A native horizontal notch (trackpad swipe / tilt wheel). Only the
        // lineage diagram scrolls horizontally; the list pane ignores it.
        MouseEventKind::ScrollRight => wheel_x(app, lin_inner, col, row, true),
        MouseEventKind::ScrollLeft => wheel_x(app, lin_inner, col, row, false),
        _ => {}
    }
}

/// Whether screen cell `(x, y)` is inside `r`.
fn within(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Route a vertical wheel notch to whichever pane the cursor is over. Over the
/// lineage pane, Shift remaps the notch to a horizontal pan (down = right) —
/// the usual convention for mice without a horizontal wheel; over the list,
/// Shift changes nothing (the list has no horizontal axis).
fn wheel(
    app: &mut App,
    list_inner: Rect,
    lin_inner: Rect,
    col: u16,
    row: u16,
    down: bool,
    shift: bool,
) {
    if within(list_inner, col, row) {
        app.ui_state.wheel_list(down);
    } else if within(lin_inner, col, row) {
        if shift {
            app.ui_state.wheel_lineage_x(down);
        } else {
            app.ui_state.wheel_lineage(down);
        }
    }
}

/// Route a horizontal wheel notch to the lineage pane — the only horizontally
/// scrollable surface. The saturating pan intent is bounded by the loop's
/// `clamp_lineage` next frame, same as the vertical wheel.
fn wheel_x(app: &mut App, lin_inner: Rect, col: u16, row: u16, right: bool) {
    if within(lin_inner, col, row) {
        app.ui_state.wheel_lineage_x(right);
    }
}

/// Perform a side effect requested by the reducer. The only impure surface:
/// it suspends/reinits the terminal for `$EDITOR`, shells out to the clipboard,
/// or reloads the manifest. Effects that fail non-fatally (editor, clipboard,
/// reload, file write) keep the TUI running and surface the failure on the
/// notice channel — overwriting the reducer's optimistic intent toast, so the
/// toast never claims a copy/export that didn't happen.
fn run_effect(terminal: &mut DefaultTerminal, app: &mut App, effect: Effect) -> Result<()> {
    match effect {
        Effect::OpenEditor(path) => open_in_editor(terminal, app, &path)?,
        Effect::Yank(text) => {
            if yank_to_clipboard(&text).is_err() {
                app.set_notice("Copy failed (clipboard unavailable)");
            }
        }
        Effect::ReloadManifest => {
            // On failure the app is left unchanged (reload `?`-returns before
            // mutating), so we keep running on the old data.
            let note = match app.reload() {
                Ok(w) if w.is_empty() => "Reloaded".to_string(),
                Ok(w) => format!("Reloaded — {}", warnings_tail(&w)),
                Err(_) => "Reload failed (kept previous data)".to_string(),
            };
            app.set_notice(note);
        }
        Effect::WriteFile { path, contents } => {
            if std::fs::write(&path, contents).is_err() {
                app.set_notice(format!("Export failed: {path}"));
            }
        }
        Effect::DbtParse => run_dbt_parse(terminal, app)?,
    }
    Ok(())
}

/// Run `dbt parse` in the project root and adopt the regenerated
/// `target/manifest.json` as the data source. Every failure (no dbt on PATH, no
/// project file, non-zero exit, missing/corrupt manifest) keeps the TUI running
/// on the current data and reports via the notice toast.
///
/// The TUI is suspended around the subprocess (same dance as `$EDITOR`) so
/// dbt's own progress/error output is visible while it runs.
fn run_dbt_parse(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let Some(dbt) = find_dbt() else {
        app.set_notice("dbt not found in PATH");
        return Ok(());
    };
    let root = app.project_root().to_path_buf();
    if !root.join("dbt_project.yml").is_file() {
        app.set_notice(format!("No dbt_project.yml in {}", root.display()));
        return Ok(());
    }

    // Leave the TUI exactly like the editor suspend; always re-init even if
    // the subprocess failed, or the terminal stays corrupted.
    set_keyboard_enhancement(false);
    set_mouse_capture(false);
    ratatui::restore();
    println!("dbtl: running `dbt parse` in {} ...", root.display());
    let status = dbt_command(&dbt).arg("parse").current_dir(&root).status();
    *terminal = ratatui::try_init().context("failed to re-init terminal after dbt parse")?;
    set_keyboard_enhancement(true);
    set_mouse_capture(true);
    let _ = terminal.clear();

    match status {
        Ok(s) if s.success() => {}
        Ok(_) => {
            app.set_notice("dbt parse failed (kept current data)");
            return Ok(());
        }
        Err(_) => {
            app.set_notice("Failed to launch dbt (kept current data)");
            return Ok(());
        }
    }
    let manifest = root.join("target").join("manifest.json");
    if !manifest.is_file() {
        app.set_notice("dbt parse wrote no target/manifest.json");
        return Ok(());
    }
    // On a load error `adopt_manifest` restores the previous source, so the
    // app keeps running on the pre-parse data. Load warnings (unknown
    // resource_types) ride the toast like a manual reload.
    let note = match app.adopt_manifest(manifest) {
        Ok(w) if w.is_empty() => "dbt parse OK: now reading target/manifest.json".to_string(),
        Ok(w) => format!("dbt parse OK — {}", warnings_tail(&w)),
        Err(_) => "dbt parse OK but the manifest failed to load (kept current data)".to_string(),
    };
    app.set_notice(note);
    Ok(())
}

/// The reload/`dbt parse` toast tail for a non-empty load-warnings vec:
/// every warning joined (`"1 warning: <w>"` / `"N warnings: <w1>; <w2>"`) —
/// quoting only the first would hide the rest of what the manifest carries.
fn warnings_tail(warnings: &[String]) -> String {
    format!(
        "{} warning{}: {}",
        warnings.len(),
        if warnings.len() == 1 { "" } else { "s" },
        warnings.join("; ")
    )
}

/// Locate the `dbt` executable on `PATH`, cross-platform. Returns the full
/// path so the caller can run it without re-resolving (and so Windows `.bat`/
/// `.cmd` launchers can be routed through `cmd /C`). `pub(crate)` because the
/// source-mode startup notice in `main` checks the same probe.
pub(crate) fn find_dbt() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_in_path_dirs("dbt", std::env::split_paths(&path))
}

/// PATH-lookup core, parameterized over the directory list so tests never
/// mutate the process-global `PATH` (env mutation races parallel tests).
fn find_in_path_dirs(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    for dir in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for cand in candidate_names(name) {
            let p = dir.join(&cand);
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// The filenames that count as "the `name` command" on this platform: the bare
/// name on Unix; on Windows, `name` + each `PATHEXT` extension (`dbt.exe`,
/// `dbt.cmd`, …) the way `where` resolves commands.
#[cfg(not(windows))]
fn candidate_names(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[cfg(windows)]
fn candidate_names(name: &str) -> Vec<String> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .map(|e| format!("{name}{}", e.to_ascii_lowercase()))
        .collect()
}

/// Whether `path` is an executable file. Unix checks the execute bits (a
/// plain data file named `dbt` on PATH is not a command); Windows has no
/// execute bit — the `PATHEXT` extension (already applied by
/// [`candidate_names`]) is what marks executability there.
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Build the `Command` for a resolved dbt executable. On Windows, `.bat`/`.cmd`
/// launchers (common for pip-installed tools) cannot be spawned directly by
/// `CreateProcess` — route them through `cmd /C`.
fn dbt_command(exe: &std::path::Path) -> Command {
    #[cfg(windows)]
    {
        let ext = exe
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("bat" | "cmd")) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(exe);
            return c;
        }
    }
    Command::new(exe)
}

/// Split an editor value like `code --wait` into program + leading arguments.
/// Values such as `$VISUAL` commonly carry flags, so the whole string must not
/// be treated as a single program name. Whitespace-only values yield `None`.
fn split_editor_command(value: &str) -> Option<(String, Vec<String>)> {
    let mut tokens = value.split_whitespace().map(String::from);
    let program = tokens.next()?;
    Some((program, tokens.collect()))
}

/// Suspend the TUI, open `path` in `$VISUAL`/`$EDITOR` (default `vi`), then
/// re-enter the alternate screen and force a redraw. A missing re-init would
/// leave the terminal corrupted, so we always re-init even if the editor failed.
/// Editor spawn/exit failures are NOT propagated — tearing the TUI down over a
/// missing editor binary would discard the whole session — they surface on the
/// notice channel like every other non-fatal effect failure.
fn open_in_editor(terminal: &mut DefaultTerminal, app: &mut App, path: &str) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_default();
    let (program, args) =
        split_editor_command(&editor).unwrap_or_else(|| ("vi".to_string(), Vec::new()));

    // Leave the TUI (pop the keyboard enhancement and disable mouse first, then
    // restore raw mode + alt screen) — the editor gets the terminal in the same
    // plain state a shell would.
    set_keyboard_enhancement(false);
    set_mouse_capture(false);
    ratatui::restore();
    let status = Command::new(&program).args(&args).arg(path).status();
    // Re-enter: re-init the terminal, re-arm keyboard + mouse, force a redraw.
    *terminal = ratatui::try_init().context("failed to re-init terminal after editor")?;
    set_keyboard_enhancement(true);
    set_mouse_capture(true);
    let _ = terminal.clear();

    if let Some(note) = editor_failure_notice(&program, &status) {
        app.set_notice(note);
    }
    Ok(())
}

/// Map the editor subprocess outcome to a failure notice (`None` = clean exit).
/// Pure so the headless suite can cover it.
fn editor_failure_notice(
    program: &str,
    status: &std::io::Result<std::process::ExitStatus>,
) -> Option<String> {
    match status {
        Ok(s) if s.success() => None,
        Ok(s) => Some(format!("Editor failed ({program}): {s}")),
        Err(_) => Some(format!("Failed to launch editor ({program})")),
    }
}

/// The clipboard commands to try on this platform, in order. Linux has no
/// single standard tool, so Wayland's `wl-copy` is probed first, then the two
/// common X11 ones — the first that spawns wins.
#[cfg(target_os = "macos")]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[("pbcopy", &[])];

#[cfg(target_os = "linux")]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

#[cfg(target_os = "windows")]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[("clip", &[])];

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[];

/// Copy `text` to the system clipboard via the platform's clipboard command
/// ([`CLIPBOARD_COMMANDS`]). Best-effort; the caller surfaces errors as a
/// toast so a missing clipboard tool never breaks the TUI.
fn yank_to_clipboard(text: &str) -> Result<()> {
    for (program, args) in CLIPBOARD_COMMANDS {
        let Ok(child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .spawn()
        else {
            continue;
        };
        return feed_and_reap(child, program, text);
    }
    anyhow::bail!("no clipboard command available")
}

/// Feed `text` to a spawned clipboard child's stdin and ALWAYS reap it: an
/// early `?` on the write would skip `wait()` and leave a zombie for the rest
/// of the process lifetime. The write error (e.g. a broken pipe from a child
/// that exited without reading) is reported after the reap.
fn feed_and_reap(mut child: std::process::Child, program: &str, text: &str) -> Result<()> {
    let write_result = match child.stdin.take() {
        // `stdin` drops at the end of this arm, closing the pipe so the child
        // sees EOF and `wait()` below cannot deadlock.
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Ok(()),
    };
    let wait_result = child.wait();
    write_result.with_context(|| format!("failed to write to {program}"))?;
    wait_result.with_context(|| format!("{program} failed"))?;
    Ok(())
}

/// Compute the number of visible *display rows* in the list pane, mirroring the
/// draw layout: total height minus the status line (1) minus the pane's top and
/// bottom borders (2). `max(1)` guards tiny terminals so we never produce a
/// zero-height window.
fn list_pane_height(terminal: &DefaultTerminal) -> Result<usize> {
    let size = terminal.size().context("failed to read terminal size")?;
    let body = size.height.saturating_sub(1); // status line
    let inner = body.saturating_sub(2); // top + bottom border
    Ok(inner.max(1) as usize)
}

/// The lineage pane's interior size `(width, height)` for the current terminal,
/// computed through the SAME pure geometry (`pane_rects` + `pane_interior`) that
/// `draw` uses — including the list pane's visibility — so the anchor/clamp
/// viewport equals the blitted/drawn viewport.
fn lineage_viewport(terminal: &DefaultTerminal, list_visible: bool) -> Result<(usize, usize)> {
    let interior = pane_interior(pane_rects(full_area(terminal)?, list_visible).lineage);
    Ok((interior.width as usize, interior.height as usize))
}

/// The full terminal rect (origin at 0,0). Used for overlay geometry / clamps.
fn full_area(terminal: &DefaultTerminal) -> Result<ratatui::layout::Rect> {
    let size = terminal.size().context("failed to read terminal size")?;
    Ok(ratatui::layout::Rect::new(0, 0, size.width, size.height))
}

#[cfg(test)]
mod tests {
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
}
