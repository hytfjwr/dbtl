//! Per-project UI state persistence (`$XDG_STATE_HOME/dbtl/`) and the optional
//! user config (`~/.config/dbtl/config.yml`).
//!
//! The data types live in `dbtl_tui::persist`; this module owns the filesystem
//! side only: XDG path resolution (same env-var rules as [`crate::themes`]),
//! a stable one-file-per-project key, and best-effort load/save — a corrupt
//! state file or unwritable dir degrades to "no saved state", never an error.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use dbtl::{Config, PersistedState};

/// The user's home directory: `$HOME` on unix-likes, `%USERPROFILE%` on
/// Windows (CI runners on Windows don't set `HOME`).
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The user config file: `$XDG_CONFIG_HOME/dbtl/config.yml`, falling back to
/// `~/.config/dbtl/config.yml`. `None` when neither env var resolves.
fn config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("dbtl").join("config.yml"));
    }
    home_dir().map(|home| home.join(".config/dbtl/config.yml"))
}

/// The per-project state directory: `$XDG_STATE_HOME/dbtl`, falling back to
/// `~/.local/state/dbtl` (the XDG default when `XDG_STATE_HOME` is unset).
fn state_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("dbtl"));
    }
    home_dir().map(|home| home.join(".local/state/dbtl"))
}

/// The state file for one project: `<state_dir>/<slug>-<hash>.json`, where the
/// slug is the sanitized project dir basename (human-readable) and the hash
/// keeps two projects sharing a basename from colliding.
fn project_state_path(project_root: &Path) -> Option<PathBuf> {
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let slug: String = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    let name = format!("{}-{:016x}.json", slug, hasher.finish());
    Some(state_dir()?.join(name))
}

/// Load `~/.config/dbtl/config.yml`. Returns `Ok(Config::default())` when the
/// file is absent; a malformed file is an error the caller reports as a
/// startup warning (never a panic, never aborts the TUI).
pub fn load_config() -> std::io::Result<Config> {
    match config_path() {
        Some(path) => Config::load(&path),
        None => Ok(Config::default()),
    }
}

/// Load the saved state for this project. Any failure — missing file, corrupt
/// JSON, unresolvable dirs — silently yields the default (a fresh run behaves
/// exactly as before persistence existed).
pub fn load_state(project_root: &Path) -> PersistedState {
    project_state_path(project_root)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Save the prefs snapshot for this project. Best-effort: an unwritable state
/// dir is ignored rather than turning an exit into an error.
pub fn save_state(project_root: &Path, state: &PersistedState) {
    let Some(path) = project_state_path(project_root) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_state_path_slugs_and_hashes() {
        let p = project_state_path(Path::new("/tmp/My dbt-Project_1"))
            .expect("HOME/XDG resolves in tests");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("My_dbt-Project_1-"), "{name}");
        assert!(name.ends_with(".json"));
        // Different roots → different files; same root → same file.
        assert_ne!(
            project_state_path(Path::new("/tmp/My dbt-Project_1")),
            project_state_path(Path::new("/tmp/other"))
        );
        assert_eq!(
            project_state_path(Path::new("/tmp/My dbt-Project_1")),
            project_state_path(Path::new("/tmp/My dbt-Project_1"))
        );
    }
}
