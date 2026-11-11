use super::*;

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
pub(super) fn find_in_path_dirs(
    name: &str,
    dirs: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
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
pub(super) fn dbt_command(exe: &std::path::Path) -> Command {
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
pub(super) fn split_editor_command(value: &str) -> Option<(String, Vec<String>)> {
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
pub(super) fn open_in_editor(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    path: &str,
) -> Result<()> {
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
        app.set_notice_level(dbtl::NoticeLevel::Warning, note);
    }
    Ok(())
}

/// Map the editor subprocess outcome to a failure notice (`None` = clean exit).
/// Pure so the headless suite can cover it.
pub(super) fn editor_failure_notice(
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
pub(super) fn yank_to_clipboard(text: &str) -> Result<()> {
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
pub(super) fn feed_and_reap(
    mut child: std::process::Child,
    program: &str,
    text: &str,
) -> Result<()> {
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
