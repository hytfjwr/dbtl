//! The "which global flags does each subcommand honor" table — the single
//! place the ignored-flag check and its drift tests read, so the clap
//! definitions and the validation can't disagree silently.
//!
//! Global flags are declared once on [`Cli`](crate::cli::Cli) with
//! `global = true`: clap accepts them on either side of a subcommand, and a
//! command that doesn't consume one must reject it instead of silently
//! dropping it. [`check_flag_usage`] does the rejection via
//! `ArgMatches::value_source` — only an explicit command-line occurrence
//! counts (env-fed values like `DBTL_THEME` are still allowed to configure
//! the app under a subcommand).

use clap::parser::ValueSource;
use clap::ArgMatches;

use super::CliError;

/// Global (`global = true`) flag ids → the subcommand names that consume
/// them. A global flag the subcommand doesn't list is an error when typed
/// explicitly, no matter which side of the subcommand it sits on.
pub const GLOBAL_FLAG_COMMANDS: &[(&str, &[&str])] = &[
    ("manifest", &["docs", "impact", "check", "columns", "lint"]),
    ("source", &["docs", "impact", "check", "columns", "lint"]),
    ("project", &["docs", "impact", "check", "columns", "lint"]),
    (
        "include_packages",
        &["docs", "impact", "check", "columns", "lint"],
    ),
    ("diff", &["impact", "check", "lint"]),
    // `--run-results` feeds the TUI and `docs` — every other subcommand
    // rejects it (an empty list would mean TUI-only).
    ("run_results", &["docs"]),
    // `lint`/`docs` consume both: docs' Lint section runs the same
    // evaluation `dbtl lint` does, so without the flags their results could
    // disagree with the report's.
    ("catalog", &["docs", "impact", "check", "columns", "lint"]),
    (
        "sql_dialect",
        &["docs", "impact", "check", "columns", "lint"],
    ),
];

/// TUI-only top-level flags: valid in the app, meaningless under a
/// subcommand — an explicit occurrence next to a subcommand is rejected.
pub const TUI_ONLY_FLAGS: &[&str] = &[
    "select",
    "watch",
    "theme",
    "list_themes",
    "ascii",
    "unicode",
];

/// Top-level flags a subcommand run may carry even though the subcommand
/// doesn't read them: `--project-only` is a frozen deprecated no-op — its
/// own warning already tells the user it does nothing.
#[cfg(test)]
const IGNORED_FLAG_EXEMPT: &[&str] = &["project_only"];

/// Reject explicitly supplied flags the dispatched subcommand ignores:
/// global flags outside its [`GLOBAL_FLAG_COMMANDS`] row, and the
/// [`TUI_ONLY_FLAGS`] set. Exits are usage errors — the invocation could
/// never have meant what it says.
pub fn check_flag_usage(matches: &ArgMatches) -> Result<(), CliError> {
    let Some((sub_name, sub_matches)) = matches.subcommand() else {
        return Ok(());
    };
    // A global flag lands in `matches` wherever it was typed — and in
    // `sub_matches` when typed after the subcommand — so "explicitly
    // supplied" means a command-line source at either level. `value_source`
    // panics on an id the Command never defined: the top level knows every
    // top-level arg, `sub_matches` only the globals, so each table gets its
    // own query.
    let top = |id: &str| matches.value_source(id) == Some(ValueSource::CommandLine);
    for (flag, commands) in GLOBAL_FLAG_COMMANDS {
        let explicit =
            top(flag) || sub_matches.value_source(flag) == Some(ValueSource::CommandLine);
        if !commands.contains(&sub_name) && explicit {
            return Err(invalid_flag(flag, sub_name));
        }
    }
    for flag in TUI_ONLY_FLAGS {
        if top(flag) {
            return Err(invalid_flag(flag, sub_name));
        }
    }
    Ok(())
}

fn invalid_flag(flag: &str, sub_name: &str) -> CliError {
    CliError::usage(anyhow::anyhow!(
        "flag '--{}' is not supported by subcommand '{}'",
        flag.replace('_', "-"),
        sub_name
    ))
}

/// Whether a non-global top-level flag is exempt from the ignored-flag
/// check — the deprecated `--project-only` is the only one.
#[cfg(test)]
pub fn is_ignored_flag_exempt(id: &str) -> bool {
    IGNORED_FLAG_EXEMPT.contains(&id)
}
