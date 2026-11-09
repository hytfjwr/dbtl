//! Thin CLI/TUI wrapper around the `dbtl` library — the orchestrator only.
//! The responsibilities live in the sibling modules:
//! - [`cli`]: the `clap` argument model, the non-interactive `docs` subcommand,
//!   and the shared data-source resolution.
//! - [`terminal`]: the glyph-mode probe, mouse capture, and the kitty
//!   keyboard-enhancement push/pop (the terminal lifecycle pieces).
//! - [`watch`]: the `--watch` on-disk-change poller.
//! - [`themes`]: colour-theme loading + `--theme`/`--list-themes`.
//! - [`event_loop`]: the size-aware scroll-follow / lineage anchoring — the
//!   seam the pure reducer must never touch — plus the impure [`Effect`]
//!   runners (editor / yank / reload / `dbt parse`).
//!
//! `main` itself keeps only: parse → dispatch → terminal init + panic hook →
//! the loop → the single teardown path.

mod cli;
mod event_loop;
mod terminal;
mod themes;
mod watch;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use cli::{build_app, load_baseline, run_check, run_docs, run_impact, Cli, Commands};
use event_loop::{event_loop, find_dbt};
use terminal::{
    detect_keyboard_enhancement, resolve_glyph_mode, set_keyboard_enhancement, set_mouse_capture,
};
use themes::{load_themes, print_theme_list};
use watch::Watch;

use dbtl::ui::LineageLens;

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

    // Non-interactive subcommands run and return BEFORE any terminal init /
    // probe (same as --list-themes), so they work in a non-TTY / CI environment.
    match &cli.command {
        Some(Commands::Docs {
            out,
            manifest,
            source,
            project,
            quiet,
            clean,
            check,
        }) => return run_docs(out, manifest, source, project, *quiet, *clean, *check),
        Some(Commands::Impact {
            diff,
            format,
            manifest,
            source,
            project,
        }) => return run_impact(diff, *format, manifest, source, project),
        Some(Commands::Check {
            diff,
            coverage_min,
            manifest,
            source,
            project,
            quiet,
        }) => {
            return run_check(diff, *coverage_min, manifest, source, project, *quiet);
        }
        None => {}
    }

    // Build the data model up front so load errors (missing manifest, bad
    // dbt_project.yml, YAML errors) are reported as plain text *before* we ever
    // touch the terminal.
    let mut app = build_app(cli)?;
    if let Some(name) = &cli.select {
        // Fail as plain text before the terminal is touched, like a load error.
        // Plain model name first; on a miss, try it as a dbt selector spec
        // (`--select 'tag:nightly'` → the first matching model, sorted).
        let found = app.select_by_name(name)
            || dbtl::select(&app.dag, app.diff(), name)
                .iter()
                .next()
                .is_some_and(|uid| app.select_by_unique_id(uid));
        anyhow::ensure!(found, "--select: model '{name}' not found");
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
