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
mod state;
mod terminal;
mod themes;
mod watch;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use cli::{build_app, load_baseline, run_check, run_columns, run_docs, run_impact, Cli, Commands};
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
    // clap's `conflicts_with` catches `--project-only --include-packages` at
    // the same level — but `--include-packages` is GLOBAL, so it can also
    // arrive via a subcommand context (`dbtl --project-only check
    // --include-packages`), which the declarative conflict does not reach.
    // Check the parsed pair so the two are exclusive wherever they were typed.
    if cli.project_only && cli.include_packages {
        anyhow::bail!("the argument '--project-only' cannot be used with '--include-packages'");
    }

    // --list-themes is a plain stdout listing, no data source / TTY needed.
    if cli.list_themes {
        print_theme_list();
        return Ok(());
    }

    // --project-only is a deprecated no-op: the DEFAULT package scope is
    // already the root project (installed-package nodes are pruned at load).
    // Warn on every invocation rather than silently swallowing — an old
    // script's author should learn the flag is dead and that
    // --include-packages is the widen knob.
    if cli.project_only {
        eprintln!(
            "warning: --project-only is deprecated and has no effect: the \
             default scope is already the root project only (use \
             --include-packages to widen it)"
        );
    }

    // Non-interactive subcommands run and return BEFORE any terminal init /
    // probe (same as --list-themes), so they work in a non-TTY / CI environment.
    match &cli.command {
        // `--include-packages` is a GLOBAL flag (parsed before OR after the
        // subcommand, landing on `cli.include_packages` either way), so the
        // subcommand variants carry no field of their own.
        Some(Commands::Docs {
            out,
            manifest,
            source,
            project,
            quiet,
            clean,
            check,
        }) => {
            return run_docs(
                out,
                manifest,
                source,
                project,
                *quiet,
                *clean,
                *check,
                cli.include_packages,
            );
        }
        Some(Commands::Impact {
            diff,
            format,
            manifest,
            source,
            project,
        }) => {
            return run_impact(
                diff,
                *format,
                manifest,
                source,
                project,
                cli.include_packages,
                cli.sql_dialect,
            );
        }
        Some(Commands::Check {
            diff,
            coverage_min,
            lint,
            no_breaking,
            rules,
            format,
            manifest,
            source,
            project,
            quiet,
        }) => {
            return run_check(
                diff,
                *coverage_min,
                *lint,
                rules,
                *format,
                manifest,
                source,
                project,
                *quiet,
                cli.include_packages,
                *no_breaking,
                cli.sql_dialect,
            );
        }
        Some(Commands::Columns {
            target,
            upstream,
            format,
            manifest,
            source,
            project,
            ..
        }) => {
            return run_columns(
                target,
                *upstream,
                *format,
                manifest,
                source,
                project,
                cli.include_packages,
                cli.sql_dialect,
            );
        }
        None => {}
    }

    // User defaults from ~/.config/dbtl/config.yml. A malformed file is a
    // warning, not a startup failure (the TUI still runs on built-in defaults).
    let config = match state::load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: ignoring dbtl config.yml: {e}");
            dbtl::Config::default()
        }
    };

    // Build the data model up front so load errors (missing manifest, bad
    // dbt_project.yml, YAML errors) are reported as plain text *before* we ever
    // touch the terminal.
    let mut app = build_app(cli)?;

    // The meta-driven options are config-only (no CLI/state layer): the model
    // list's `group_by_meta` grouping axis and the impact pack's
    // `owner_meta_key`. Applied BEFORE prefs so a saved sort change rebuilds
    // the already-grouped list rather than building twice.
    app.set_group_by_meta(config.group_by_meta.clone());
    if let Some(key) = &config.owner_meta_key {
        app.set_owner_meta_key(key.clone());
    }
    // `lineage_collapse`: the sibling-collapse threshold (also config-only —
    // a display policy like the meta keys, not a saved pref).
    if let Some(threshold) = config.lineage_collapse {
        app.set_collapse_threshold(threshold);
    }
    // Layered prefs: config defaults under the per-project saved state (the
    // state's `Some` fields win); --select/--diff below still override both.
    // The saved state is bound because `load_themes` below also consumes its
    // `theme` field (§13's precedence: flag > saved > config).
    app.apply_prefs(&config.as_prefs());
    let saved = state::load_state(app.project_root());
    app.apply_prefs(&saved);
    if let Some(name) = &cli.select {
        // Fail as plain text before the terminal is touched, like a load error.
        // Plain model name first; on a miss, try it as a dbt selector spec
        // (`--select 'tag:nightly'` → the first matching model, sorted).
        // Issue 03: `--select` starts on the model's TAB — activating it when
        // it is already open, appending one when it is not (§2/§13 H2).
        let found = app.open_tab_by_name(name)
            || dbtl::select(&app.dag, app.diff(), name)
                .iter()
                .next()
                .is_some_and(|uid| app.open_node_as_tab(uid));
        // A miss under the default scope might just be a pruned installed
        // package — name the escape hatch instead of leaving the user guessing
        // (--include-packages removes the hiding place, so no hint needed then).
        let hint = if cli.package_scope() == dbtl::PackageScope::Root {
            " (installed packages are hidden by the default scope — retry with \
             --include-packages if the model lives in one)"
        } else {
            ""
        };
        anyhow::ensure!(found, "--select: model '{name}' not found{hint}");
    }

    // Load the --diff baseline (a second Dag) and open on the Diff lens so the
    // differences are visible immediately. A bad baseline fails as plain text
    // pre-TTY, like every other load error. The baseline is pruned under the
    // same package scope as the current graph, so both sides agree.
    if let Some(base) = &cli.diff {
        let (dag, label) = load_baseline(base, cli.package_scope())?;
        app.set_diff_base(dag, label);
        app.ui_state.set_lens(LineageLens::Diff);
    }

    // Resolve the colour themes (presets + user files) and the start index.
    // §13 precedence: an explicit --theme/DBTL_THEME wins (a bad one fails as
    // plain text pre-TTY), then the saved theme, then config.yml's. A saved
    // or config name that no longer resolves (a deleted theme file) warns and
    // falls back to the default — state is best-effort, never a startup error.
    let theme_sel = cli
        .theme
        .as_deref()
        .or(saved.theme.as_deref())
        .or(config.theme.as_deref());
    let (themes, theme_idx) = match load_themes(theme_sel) {
        Ok(t) => t,
        Err(e) if cli.theme.is_none() => {
            eprintln!("warning: {e:#}");
            load_themes(None)?
        }
        Err(e) => return Err(e),
    };
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
    // append_notice, not set_notice: `with_source` may already have queued a
    // warning (a malformed `.dbtl.yml`) — both must show, not last-write-wins.
    if app.is_source_mode() {
        app.append_notice(if find_dbt().is_some() {
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

    // Persist per-project prefs (bookmarks/lens/density/sort/selection) for
    // the next run. Best-effort — an unwritable state dir is ignored.
    state::save_state(app.project_root(), &app.prefs_snapshot());

    loop_result
}
