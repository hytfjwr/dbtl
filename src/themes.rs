//! Colour-theme loading: the built-in presets plus user YAML files under the
//! XDG config dir, the `--theme`/`DBTL_THEME` resolution, and `--list-themes`.

use std::path::PathBuf;

use anyhow::Result;

use dbtl::ui::theme::{self, Theme};

/// The user theme directory: `$XDG_CONFIG_HOME/dbtl/themes`, falling back to
/// `~/.config/dbtl/themes`. `None` when neither env var resolves (the built-in
/// presets still work).
fn user_theme_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("dbtl").join("themes"));
    }
    crate::state::home_dir().map(|home| home.join(".config").join("dbtl").join("themes"))
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
pub fn load_themes(selected: Option<&str>) -> Result<(Vec<(String, Theme)>, usize)> {
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
pub fn print_theme_list() {
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
