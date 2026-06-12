//! Thin CLI/TUI wrapper around the `dbtl` library. Owns three things:
//! - the ratatui terminal lifecycle (panic-restoring hook + idempotent teardown),
//! - the event loop's size-aware scroll-follow / lineage anchoring — the seam the
//!   pure reducer must never touch — before dispatching to [`Action`]/[`apply_action`],
//! - running the [`Effect`]s the reducer requests (editor / yank / reload): the
//!   only impure surface.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use dbtl::action::{dispatch, Action};
use dbtl::app::{apply_action, App, LineageView};
use dbtl::effect::Effect;
use dbtl::layout::{GlyphMode, Layout};
use dbtl::ui::theme::{self, Theme};
use dbtl::ui::{
    draw, hit_test, lineage_content_rect, pane_interior, pane_rects, Focus, LineageLens, RenderCtx,
    StatusSegments,
};
use dbtl::{load_dag, load_dag_from_source, Mode, SortMode};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;

/// dbtl: browse dbt models and their lineage — from a compiled manifest.json
/// or straight from project source (no `dbt compile`).
#[derive(Parser, Debug)]
#[command(
    name = "dbtl",
    version,
    about = "Browse dbt model lineage from a manifest or project source"
)]
struct Cli {
    /// Path to a compiled dbt manifest.json (forces manifest mode).
    #[arg(long, conflicts_with = "source")]
    manifest: Option<String>,
    /// Path to a dbt project dir, parsed from source with no compile (forces source mode).
    #[arg(long, conflicts_with = "manifest")]
    source: Option<String>,
    /// dbt project dir for auto-detect: prefer <dir>/target/manifest.json, else parse source.
    #[arg(long, default_value = ".")]
    project: String,
    /// Force pure-ASCII glyphs (boxes/borders/markers). Default: probe the terminal.
    #[arg(long, conflicts_with = "unicode")]
    ascii: bool,
    /// Force Unicode box-drawing glyphs, skipping the terminal probe.
    #[arg(long, conflicts_with = "ascii")]
    unicode: bool,
    /// Start with this model selected (by name, e.g. `stg_orders`).
    #[arg(long)]
    select: Option<String>,
    /// Auto-reload when the data source changes on disk (the manifest file, or
    /// any .sql/.yml/.csv under the project dir in source mode).
    #[arg(long)]
    watch: bool,
    /// Color theme: a preset name (see --list-themes), a theme under
    /// ~/.config/dbtl/themes/<name>.yml, or a path to a theme YAML file.
    #[arg(long, env = "DBTL_THEME")]
    theme: Option<String>,
    /// List the available color themes (presets + user theme files) and exit.
    #[arg(long)]
    list_themes: bool,
    /// Baseline to diff against: a manifest.json file, or a dbt project dir
    /// (its target/manifest.json if compiled, else parsed from source). Adds
    /// the Diff lens (added/modified tints), the `D` summary modal, and a
    /// status diff chip.
    #[arg(long)]
    diff: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Print the full context chain (cause is named), not a panic backtrace.
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    // --list-themes is a plain stdout listing, no data source / TTY needed.
    if cli.list_themes {
        print_theme_list();
        return Ok(());
    }

    // Build the data model up front so load errors (missing manifest, bad
    // dbt_project.yml, YAML errors) are reported as plain text *before* we ever
    // touch the terminal.
    let mut app = build_app(cli)?;
    if let Some(name) = &cli.select {
        // Fail as plain text before the terminal is touched, like a load error.
        anyhow::ensure!(
            app.select_by_name(name),
            "--select: model '{name}' not found"
        );
    }

    // Load the --diff baseline (a second Dag) and open on the Diff lens so the
    // differences are visible immediately. A bad baseline fails as plain text
    // pre-TTY, like every other load error.
    if let Some(base) = &cli.diff {
        let (dag, label) = load_baseline(base)?;
        app.set_diff_base(dag, label);
        app.ui_state.set_lens(LineageLens::Diff);
    }

    // Resolve the colour themes (presets + user files) and the --theme start
    // index. A bad EXPLICIT selection fails as plain text pre-TTY; user themes
    // get palette-lint WARNINGS inside load_themes, never errors.
    let (themes, theme_idx) = load_themes(cli.theme.as_deref())?;
    app.set_themes(themes, theme_idx);

    // Enter raw mode + alternate screen and install the panic-restoring hook.
    // In a non-TTY environment this returns Err; we surface it as a normal
    // error rather than panicking.
    let mut terminal = ratatui::try_init().context("failed to initialize terminal (no TTY?)")?;

    // Pick the glyph repertoire on the LIVE terminal (the probe needs raw mode
    // + the alternate screen, so it must follow try_init) — and BEFORE mouse
    // capture is enabled, so no mouse-report bytes can interleave with the
    // cursor-position exchange. The keyboard-enhancement probe is a
    // query/response exchange too, so it shares the same window.
    app.glyph_mode = resolve_glyph_mode(cli);
    detect_keyboard_enhancement();
    set_keyboard_enhancement(true); // kitty protocol → Cmd-modified keys arrive
    set_mouse_capture(true); // ratatui's init does NOT enable mouse; we do.

    // ratatui's panic hook restores raw mode + the alt screen but NOT mouse
    // capture or the keyboard-enhancement push, so chain a hook that also
    // unwinds those on the panic path — keeping the panic teardown consistent
    // with the normal one below.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        set_keyboard_enhancement(false);
        set_mouse_capture(false);
        prev_hook(info);
    }));

    // Source mode infers lineage from regex-parsed SQL (no Jinja rendering), so
    // macro-generated refs and the like are invisible — recommend the compiled
    // manifest up front. One-shot: the toast shows on the first frames only.
    if app.is_source_mode() {
        app.set_notice(if find_dbt().is_some() {
            "Source mode: lineage is inferred - press P to run dbt parse"
        } else {
            "Source mode: lineage is inferred (a compiled manifest is exact)"
        });
    }

    // The --watch poller: baseline the on-disk stamp before the loop starts so
    // only LATER edits trigger a reload.
    let mut watch = cli.watch.then(|| {
        let (root, recursive) = app.watch_root();
        Watch::new(root.to_path_buf(), recursive)
    });

    let loop_result = event_loop(&mut terminal, &mut app, &mut watch);

    // Single teardown path: also the one the panic hook runs. Idempotent.
    set_keyboard_enhancement(false);
    set_mouse_capture(false);
    ratatui::restore();

    loop_result
}

/// Resolve the CLI into a loaded [`App`]. Explicit `--manifest` / `--source` win;
/// otherwise auto-detect under `--project`: a compiled `target/manifest.json` if
/// present (full fidelity), else parse the project from source.
fn build_app(cli: &Cli) -> Result<App> {
    if let Some(path) = &cli.manifest {
        let dag = load_dag(path)?;
        return Ok(App::new(dag, PathBuf::from(path)));
    }
    if let Some(dir) = &cli.source {
        let dag = load_dag_from_source(dir)?;
        return Ok(App::from_source(dag, PathBuf::from(dir)));
    }
    let project = PathBuf::from(&cli.project);
    let manifest = project.join("target").join("manifest.json");
    if manifest.is_file() {
        let dag = load_dag(&manifest)?;
        Ok(App::new(dag, manifest))
    } else if project.join("dbt_project.yml").is_file() {
        let dag = load_dag_from_source(&project)?;
        Ok(App::from_source(dag, project))
    } else {
        anyhow::bail!(
            "no data source under {}: neither target/manifest.json nor dbt_project.yml found \
             (pass --manifest <file> or --source <dir>)",
            project.display()
        )
    }
}

/// Resolve the `--diff` baseline into a loaded `Dag` + a display label: a
/// manifest.json FILE directly, or a dbt project DIR through the same
/// auto-detect `build_app` uses (compiled `target/manifest.json` first, else
/// source parse) — so "diff against that other worktree" just works.
fn load_baseline(path: &str) -> Result<(dbtl::Dag, String)> {
    let p = PathBuf::from(path);
    if p.is_file() {
        let dag = load_dag(&p).with_context(|| format!("--diff: failed to load {path}"))?;
        return Ok((dag, path.to_string()));
    }
    if p.is_dir() {
        let manifest = p.join("target").join("manifest.json");
        if manifest.is_file() {
            let dag = load_dag(&manifest)
                .with_context(|| format!("--diff: failed to load {}", manifest.display()))?;
            return Ok((dag, manifest.display().to_string()));
        }
        if p.join("dbt_project.yml").is_file() {
            let dag = load_dag_from_source(&p)
                .with_context(|| format!("--diff: failed to parse project {path}"))?;
            return Ok((dag, path.to_string()));
        }
    }
    anyhow::bail!(
        "--diff: {path} is neither a manifest.json file nor a dbt project dir \
         (no target/manifest.json or dbt_project.yml found)"
    )
}

/// The user theme directory: `$XDG_CONFIG_HOME/dbtl/themes`, falling back to
/// `~/.config/dbtl/themes`. `None` when neither env var resolves (the built-in
/// presets still work).
fn user_theme_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("dbtl").join("themes"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("dbtl")
                .join("themes")
        })
}

/// The `*.yml` / `*.yaml` files under `dir`, sorted by file name so the theme
/// list (and the Ctrl-t cycle order) is deterministic. A missing/unreadable
/// dir is simply empty.
fn theme_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "yml" || e == "yaml")
        })
        .collect();
    files.sort();
    files
}

/// Insert a named theme, REPLACING a same-name entry in place (so a user file
/// named like a preset overrides it without duplicating the cycle). Returns
/// the index of the slot it wrote.
fn upsert_theme(themes: &mut Vec<(String, Theme)>, name: String, theme: Theme) -> usize {
    match themes.iter().position(|(n, _)| *n == name) {
        Some(i) => {
            themes[i].1 = theme;
            i
        }
        None => {
            themes.push((name, theme));
            themes.len() - 1
        }
    }
}

/// A sanity cap on theme files: anything bigger is certainly not a palette
/// (e.g. a stray binary copied into the themes dir) and would otherwise be
/// slurped whole before YAML rejects it.
const THEME_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// Read + parse one theme YAML file; the ERROR POLICY (warn-and-skip vs hard
/// error) stays at the call sites.
fn read_theme(path: &std::path::Path) -> Result<Theme, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > THEME_FILE_MAX_BYTES {
        return Err(format!("{size} bytes is too large for a theme file"));
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    theme::parse_theme(&text)
}

/// Surface a user theme's palette-contract findings as stderr warnings
/// (pre-TTY, so they land in the scrollback). Warnings, never errors — a
/// custom theme may knowingly bend the contract. Presets are lint-clean by
/// test, so only loaded user themes pass through here.
fn warn_theme_lint(name: &str, theme: &Theme) {
    for warning in theme::lint(theme) {
        eprintln!("warning: theme '{name}': {warning}");
    }
}

/// Resolve the loaded theme list (built-in presets + user files, user names
/// winning) and the `--theme`/`DBTL_THEME` start index. `selected` resolves
/// as: a loaded theme name → else a path to a theme YAML file → else an error
/// listing the available names. A user file that fails to parse is a warning +
/// skip, UNLESS it is the explicitly selected one — then it is the startup error.
fn load_themes(selected: Option<&str>) -> Result<(Vec<(String, Theme)>, usize)> {
    let mut themes: Vec<(String, Theme)> = theme::presets()
        .iter()
        .map(|(n, t)| (n.to_string(), *t))
        .collect();
    if let Some(dir) = user_theme_dir() {
        for path in theme_files(&dir) {
            let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            match read_theme(&path) {
                Ok(t) => {
                    warn_theme_lint(&name, &t);
                    upsert_theme(&mut themes, name, t);
                }
                Err(e) if selected == Some(name.as_str()) => {
                    anyhow::bail!("theme '{name}' ({}): {e}", path.display())
                }
                Err(e) => eprintln!("warning: skipping theme file {}: {e}", path.display()),
            }
        }
    }
    let index = match selected {
        None => 0,
        Some(sel) => match themes.iter().position(|(n, _)| n == sel) {
            Some(i) => i,
            // Not a loaded name: accept a direct path to a theme YAML file.
            None if std::path::Path::new(sel).is_file() => {
                let t = read_theme(std::path::Path::new(sel))
                    .map_err(|e| anyhow::anyhow!("theme '{sel}': {e}"))?;
                let name = std::path::Path::new(sel)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("custom")
                    .to_string();
                warn_theme_lint(&name, &t);
                upsert_theme(&mut themes, name, t)
            }
            None => anyhow::bail!(
                "unknown theme '{sel}' — set a preset name, a name under the user theme \
                 dir, or a YAML file path via --theme / DBTL_THEME (available: {})",
                themes
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
    };
    Ok((themes, index))
}

/// Print the `--list-themes` listing: the built-in presets, then the user theme
/// dir's files (or where to put them).
fn print_theme_list() {
    println!("built-in presets:");
    for name in theme::preset_names() {
        println!("  {name}");
    }
    let Some(dir) = user_theme_dir() else {
        return;
    };
    let files = theme_files(&dir);
    if files.is_empty() {
        println!(
            "\nuser themes: none (put YAML theme files under {})",
            dir.display()
        );
        return;
    }
    println!("\nuser themes ({}):", dir.display());
    for path in files {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            println!("  {stem}");
        }
    }
}

/// Decide the glyph repertoire: explicit `--ascii` / `--unicode` win; otherwise
/// probe the live terminal. Inconclusive probes (terminal didn't answer the
/// cursor-position query) default to Unicode, matching most terminals.
fn resolve_glyph_mode(cli: &Cli) -> GlyphMode {
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
fn set_mouse_capture(on: bool) {
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
fn detect_keyboard_enhancement() {
    let supported = matches!(supports_keyboard_enhancement(), Ok(true));
    KEYBOARD_ENHANCED.store(supported, Ordering::Relaxed);
}

/// Push/pop the kitty keyboard-protocol enhancement (best-effort, gated on the
/// startup probe). `DISAMBIGUATE_ESCAPE_CODES` makes the terminal encode
/// modified keys it would otherwise swallow or fold — this is what lets the
/// `Cmd-b` (Super) binding actually arrive as `SUPER + 'b'`. Toggled around
/// the editor suspend and on the panic path, mirroring mouse capture.
fn set_keyboard_enhancement(on: bool) {
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

/// The `--watch` poller: a polled (not notified) change detector over the data
/// source. Manifest mode stamps the single file's mtime; source mode stamps the
/// MAX `(mtime, file count)` over the project's `.sql`/`.yml`/`.yaml`/`.csv`
/// files (the count catches a deletion, which can only lower the max mtime).
/// Checks are throttled to one scan per 2s of idle ticks, so the 250ms event
/// poll never busy-walks a big project tree.
struct Watch {
    root: PathBuf,
    recursive: bool,
    stamp: Option<(std::time::SystemTime, usize)>,
    last_check: std::time::Instant,
}

impl Watch {
    fn new(root: PathBuf, recursive: bool) -> Self {
        let stamp = fs_stamp(&root, recursive);
        Watch {
            root,
            recursive,
            stamp,
            last_check: std::time::Instant::now(),
        }
    }

    /// Whether the source changed since the last accepted stamp (throttled).
    /// A vanished source (`None` stamp — e.g. mid-`dbt compile` manifest swap)
    /// is NOT a change; the next successful stamp triggers the reload.
    fn poll(&mut self) -> bool {
        if self.last_check.elapsed() < Duration::from_secs(2) {
            return false;
        }
        self.last_check = std::time::Instant::now();
        let cur = fs_stamp(&self.root, self.recursive);
        if cur.is_some() && cur != self.stamp {
            self.stamp = cur;
            return true;
        }
        false
    }
}

/// The on-disk stamp `(max mtime, file count)` for a watch root: the file's own
/// mtime (count 1) when `recursive` is false, else over every data file
/// (`.sql`/`.yml`/`.yaml`/`.csv`) under the tree — skipping dot-dirs and
/// `target/` (dbt writes artifacts there on every invocation, which would
/// self-trigger). `None` when the root is unreadable.
fn fs_stamp(root: &std::path::Path, recursive: bool) -> Option<(std::time::SystemTime, usize)> {
    fn walk(dir: &std::path::Path, best: &mut Option<std::time::SystemTime>, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // file_type() does not follow symlinks, so a symlinked directory
            // (e.g. a link loop back into the tree) is never walked into.
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if !name.starts_with('.') && name != "target" {
                    walk(&path, best, count);
                }
                continue;
            }
            let data_file = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("sql" | "yml" | "yaml" | "csv")
            );
            if !data_file {
                continue;
            }
            *count += 1;
            if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                if best.is_none_or(|b| mtime > b) {
                    *best = Some(mtime);
                }
            }
        }
    }
    if !recursive {
        return std::fs::metadata(root)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| (t, 1));
    }
    let mut best = None;
    let mut count = 0;
    walk(root, &mut best, &mut count);
    best.map(|t| (t, count))
}

/// The event loop: follow scroll (size-aware), draw, read an event, map it to an
/// [`Action`] via the keymap, apply it, run any effects, repeat until quit.
///
/// The size-aware steps (`ensure_visible`, `anchor_lineage`,
/// `ensure_lineage_visible`, `clamp_lineage`) stay here — the pure reducer
/// (`apply_action`) never sees pane geometry. Only `KeyEventKind::Press` drives
/// state, so Press+Release/Repeat terminals don't double-fire.
fn event_loop(
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

    loop {
        if let Some(text) = app.take_notice() {
            toast = Some((text, std::time::Instant::now()));
        }
        if toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= TOAST_TTL)
        {
            toast = None;
        }
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
        let lineage = app.styled_lineage_layout();
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
        let area = full_area(terminal)?;
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
        let view_seg = if app.lineage_view != LineageView::default() {
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

        // Block for the next event (no busy-poll). An idle tick (timeout) is
        // where `--watch` checks the data source for on-disk changes.
        if !event::poll(Duration::from_millis(250)).context("event poll failed")? {
            if let Some(w) = watch.as_mut() {
                if w.poll() {
                    // Same semantics as the `r` key: on failure the app keeps
                    // running on the old data; the reshaped graph re-centres.
                    let note = match app.reload() {
                        Ok(()) => "Reloaded (source changed)",
                        Err(_) => "Reload failed (kept previous data)",
                    };
                    app.set_notice(note);
                    force_anchor = true;
                }
            }
            continue;
        }
        {
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
                Ok(()) => "Reloaded",
                Err(_) => "Reload failed (kept previous data)",
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
    // app keeps running on the pre-parse data.
    let note = match app.adopt_manifest(manifest) {
        Ok(()) => "dbt parse OK: now reading target/manifest.json",
        Err(_) => "dbt parse OK but the manifest failed to load (kept current data)",
    };
    app.set_notice(note);
    Ok(())
}

/// Locate the `dbt` executable on `PATH`, cross-platform. Returns the full
/// path so the caller can run it without re-resolving (and so Windows `.bat`/
/// `.cmd` launchers can be routed through `cmd /C`).
fn find_dbt() -> Option<PathBuf> {
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

    #[test]
    fn find_in_path_dirs_resolves_executables_only() {
        let dir = std::env::temp_dir().join(format!("dbtl_path_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(if cfg!(windows) { "dbt.exe" } else { "dbt" });
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();

        #[cfg(unix)]
        {
            // Without the execute bit, a file named `dbt` is data, not a command.
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

    #[test]
    fn fs_stamp_tracks_data_files_and_watch_detects_change() {
        let dir = std::env::temp_dir().join(format!("dbtl_watch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("models/a.sql"), "select 1").unwrap();
        std::fs::write(dir.join("dbt_project.yml"), "name: t").unwrap();
        // Artifacts under target/ and non-data files never count.
        std::fs::write(dir.join("target/manifest.json"), "{}").unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();

        let (_, count) = fs_stamp(&dir, true).expect("stamp exists");
        assert_eq!(count, 2, "a.sql + dbt_project.yml only");

        let mut w = Watch::new(dir.clone(), true);
        assert!(!w.poll(), "throttled: no immediate re-scan");
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(!w.poll(), "unchanged tree is not a change");
        // A new model file changes the count (even with a coarse mtime clock).
        std::fs::write(dir.join("models/b.sql"), "select 2").unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(w.poll(), "new data file detected");
        // Deleting it lowers the count back — also a change.
        std::fs::remove_file(dir.join("models/b.sql")).unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(w.poll(), "deletion detected via the file count");

        // Single-file (manifest) mode stamps that file alone.
        let manifest = dir.join("target/manifest.json");
        assert_eq!(fs_stamp(&manifest, false).map(|(_, c)| c), Some(1));
        assert_eq!(fs_stamp(&dir.join("missing"), false), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
