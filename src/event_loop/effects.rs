use super::*;

/// Perform a side effect requested by the reducer. The only impure surface:
/// it suspends/reinits the terminal for `$EDITOR`, shells out to the clipboard,
/// or reloads the manifest. Effects that fail non-fatally (editor, clipboard,
/// reload, file write) keep the TUI running and surface the failure on the
/// notice channel — overwriting the reducer's optimistic intent toast, so the
/// toast never claims a copy/export that didn't happen.
pub(super) fn run_effect(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    effect: Effect,
) -> Result<()> {
    match effect {
        Effect::OpenEditor(path) => open_in_editor(terminal, app, &path)?,
        Effect::Yank(text) => {
            if yank_to_clipboard(&text).is_err() {
                app.set_notice_level(
                    dbtl::NoticeLevel::Warning,
                    "Copy failed (clipboard unavailable)",
                );
            }
        }
        Effect::ReloadManifest => {
            // On failure the app is left unchanged (reload `?`-returns before
            // mutating), so we keep running on the old data. Load warnings and
            // failures are WARNING-level; a clean reload stays info.
            let (level, note) = match app.reload() {
                Ok(w) if w.is_empty() => (dbtl::NoticeLevel::Info, "Reloaded".to_string()),
                Ok(w) => (
                    dbtl::NoticeLevel::Warning,
                    format!("Reloaded — {}", warnings_tail(&w)),
                ),
                Err(_) => (
                    dbtl::NoticeLevel::Warning,
                    "Reload failed (kept previous data)".to_string(),
                ),
            };
            app.set_notice_level(level, note);
        }
        Effect::WriteFile { path, contents } => {
            if std::fs::write(&path, contents).is_err() {
                app.set_notice_level(dbtl::NoticeLevel::Warning, format!("Export failed: {path}"));
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
pub(super) fn run_dbt_parse(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let Some(dbt) = find_dbt() else {
        app.set_notice_level(dbtl::NoticeLevel::Warning, "dbt not found in PATH");
        return Ok(());
    };
    let root = app.project_root().to_path_buf();
    if !root.join("dbt_project.yml").is_file() {
        app.set_notice_level(
            dbtl::NoticeLevel::Warning,
            format!("No dbt_project.yml in {}", root.display()),
        );
        return Ok(());
    }

    // Leave the TUI exactly like the editor suspend; always re-init even if
    // the subprocess failed, or the terminal stays corrupted.
    set_keyboard_enhancement(false);
    set_mouse_capture(false);
    ratatui::restore();
    // `--vars` rides verbatim — dbt parses the same text `--vars` the launch
    // took, so the generated manifest's `enabled`/rendered values match the
    // source-mode graph the app is replacing.
    let mut cmd = dbt_command(&dbt);
    cmd.arg("parse");
    let vars = app.cli_vars().map(|v| v.raw().to_string());
    if let Some(v) = &vars {
        cmd.arg("--vars").arg(v);
    }
    println!(
        "dbtl: running `dbt parse{}` in {} ...",
        vars.as_deref()
            .map(|v| format!(" --vars '{v}'"))
            .unwrap_or_default(),
        root.display()
    );
    let status = cmd.current_dir(&root).status();
    *terminal = ratatui::try_init().context("failed to re-init terminal after dbt parse")?;
    set_keyboard_enhancement(true);
    set_mouse_capture(true);
    let _ = terminal.clear();

    match status {
        Ok(s) if s.success() => {}
        Ok(_) => {
            app.set_notice_level(
                dbtl::NoticeLevel::Warning,
                "dbt parse failed (kept current data)",
            );
            return Ok(());
        }
        Err(_) => {
            app.set_notice_level(
                dbtl::NoticeLevel::Warning,
                "Failed to launch dbt (kept current data)",
            );
            return Ok(());
        }
    }
    // `<root>/target/manifest.json` — `default_target_dir` names the `target`
    // half so every auto-detect spells it the same way.
    let manifest = match dbtl::default_target_dir(None, Some(&root))
        .map(|target| target.join("manifest.json"))
    {
        Some(m) if m.is_file() => m,
        _ => {
            app.set_notice_level(
                dbtl::NoticeLevel::Warning,
                "dbt parse wrote no target/manifest.json",
            );
            return Ok(());
        }
    };
    // On a load error `adopt_manifest` restores the previous source, so the
    // app keeps running on the pre-parse data. Load warnings (unhandled
    // resource_types, scope warnings) ride the toast like a manual reload.
    let (level, note) = match app.adopt_manifest(manifest) {
        Ok(w) if w.is_empty() => (
            dbtl::NoticeLevel::Info,
            "dbt parse OK: now reading target/manifest.json".to_string(),
        ),
        Ok(w) => (
            dbtl::NoticeLevel::Warning,
            format!("dbt parse OK — {}", warnings_tail(&w)),
        ),
        Err(_) => (
            dbtl::NoticeLevel::Warning,
            "dbt parse OK but the manifest failed to load (kept current data)".to_string(),
        ),
    };
    app.set_notice_level(level, note);
    Ok(())
}
