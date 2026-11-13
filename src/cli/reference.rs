//! The generated `docs/reference/cli.md` — rendered from the live clap
//! model plus the flag-consumption table and exit-code mapping, so the
//! documented CLI cannot drift from the real one. The hidden
//! `cli-reference` subcommand prints the Markdown on stdout; the
//! `cargo xtask gen-docs` task is its only caller.

use std::collections::BTreeMap;

use clap::{Arg, ArgAction, Command, CommandFactory};

use super::exit::ExitKind;
use super::flags::GLOBAL_FLAG_COMMANDS;
use super::{exit_code, Cli, CliError};

/// The generated-file banner every `gen-docs` output carries.
pub const GENERATED_MARKER: &str =
    "<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->";

/// Print the generated CLI reference (Markdown) to stdout.
pub fn run_cli_reference() -> Result<(), CliError> {
    super::write_stdout(&cli_reference_markdown(), anstream::ColorChoice::Auto)
}

/// Render the whole reference document.
fn cli_reference_markdown() -> String {
    let mut cmd = Cli::command();
    // `build` propagates the `global = true` args into every subcommand and
    // installs the auto `-h`/`--version` args — the same model parsing sees.
    cmd.build();

    let mut out = String::new();
    out.push_str(GENERATED_MARKER);
    out.push_str("\n\n# `dbtl` command-line reference\n\n");
    out.push_str(&cell(&about(&cmd)));
    out.push_str(
        "\n\nRunning `dbtl` with no subcommand launches the interactive TUI — the \
         top-level flags below apply to it. Every subcommand resolves its data \
         source the same way (`--manifest` / `--source` / `--project` / \
         auto-detect) and prints the same `error: …` line shape on stderr.\n\n",
    );

    // Global flags work beside any subcommand on the command line — but each
    // subcommand lists the ones it consumes; an explicit occurrence anywhere
    // else is a usage error (the single consumption table below).
    out.push_str("## Global flags\n\n");
    out.push_str(
        "Global flags parse on either side of a subcommand — but a subcommand \
         only honors the ones it consumes; typing another explicitly is a \
         usage error (exit 2). The per-command accepted sets are the same \
         table the ignored-flag check reads.\n\n",
    );
    out.push_str("| Flag | Value | Env | Default | Description |\n");
    out.push_str("| ---- | ----- | --- | ------- | ----------- |\n");
    for arg in cmd.get_arguments() {
        if !arg.is_global_set() || arg.is_hide_set() {
            continue;
        }
        out.push_str(&arg_row(arg));
    }

    out.push_str("\n## TUI flags\n\n");
    out.push_str(
        "Top-level flags that configure the interactive app. An explicit \
         occurrence next to a subcommand is a usage error (exit 2).\n\n",
    );
    out.push_str("| Flag | Value | Env | Default | Description |\n");
    out.push_str("| ---- | ----- | --- | ------- | ----------- |\n");
    for arg in cmd.get_arguments() {
        if arg.is_global_set() || arg.is_hide_set() {
            continue;
        }
        out.push_str(&arg_row(arg));
    }

    // flag id (`include_packages`) → the actual `--long` name.
    let global_longs: BTreeMap<&str, String> = cmd
        .get_arguments()
        .filter(|a| a.is_global_set())
        .filter_map(|a| a.get_long().map(|l| (a.get_id().as_str(), l.to_string())))
        .collect();

    out.push_str("\n## Subcommands\n\n");
    let (visible, hidden): (Vec<&Command>, Vec<&Command>) =
        cmd.get_subcommands().partition(|s| !s.is_hide_set());
    for sub in &visible {
        out.push_str(&subcommand_section(sub, &global_longs));
    }
    if !hidden.is_empty() {
        out.push_str("\n### Hidden subcommands\n\n");
        out.push_str("Real commands kept out of `--help` because they feed tooling, not interactive discovery.\n\n");
        out.push_str("| Command | Description |\n| ------- | ----------- |\n");
        for sub in &hidden {
            out.push_str(&format!(
                "| `dbtl {}` | {} |\n",
                sub.get_name(),
                cell(&about(sub))
            ));
        }
    }

    out.push_str("\n## Exit codes\n\n");
    out.push_str("| Code | Meaning |\n| ---- | ------- |\n");
    out.push_str("| `0` | Success — every gate passed, output written. |\n");
    out.push_str(&format!(
        "| `{}` | Gate failure — a check tripped (a failing gate, a lint finding at or above `--fail-on`, or a docs `--check` diff). |\n",
        exit_code(Some(ExitKind::Gate))
    ));
    out.push_str(&format!(
        "| `{}` | Usage / configuration error — clap misuse, unknown rule or gate names, a malformed `.dbtl.yml`, an unresolvable selector, or a global flag the subcommand does not consume. |\n",
        exit_code(Some(ExitKind::Usage))
    ));
    out.push_str(&format!(
        "| `{}` | Input error — a named input (manifest, project, baseline, catalog, diff source, git rev) could not be found or read. Unclassified errors report as this code. |\n",
        exit_code(Some(ExitKind::Input))
    ));
    out.push_str("| `101` | Panic. |\n");
    out
}

/// One `## dbtl <name>` section: about text, a usage line, the local-args
/// table, and the accepted global flags (the same consumption table the
/// ignored-flag check reads).
fn subcommand_section(sub: &Command, global_longs: &BTreeMap<&str, String>) -> String {
    let mut out = String::new();
    out.push_str(&format!("\n### `dbtl {}`\n\n", sub.get_name()));
    let about = about(sub);
    if !about.is_empty() {
        out.push_str(&cell(&about));
        out.push_str("\n\n");
    }
    out.push_str(&format!("```text\n{}\n```\n\n", sub.clone().render_usage()));

    let mut rows = String::new();
    for arg in sub.get_arguments() {
        // Global args show as inherited copies after `build` — they belong
        // to the global table, and the accepted set is listed separately.
        if arg.is_global_set() || arg.is_hide_set() {
            continue;
        }
        rows.push_str(&arg_row(arg));
    }
    if !rows.is_empty() {
        out.push_str("| Flag | Value | Env | Default | Description |\n");
        out.push_str("| ---- | ----- | --- | ------- | ----------- |\n");
        out.push_str(&rows);
        out.push('\n');
    }

    let accepted: Vec<String> = GLOBAL_FLAG_COMMANDS
        .iter()
        .filter(|(_, commands)| commands.contains(&sub.get_name()))
        .map(|(flag, _)| {
            let long = global_longs.get(flag).map_or(*flag, String::as_str);
            format!("`--{long}`")
        })
        .collect();
    if accepted.is_empty() {
        out.push_str("Accepts no global flags.\n");
    } else {
        out.push_str(&format!("Accepts global flags: {}.\n", accepted.join(", ")));
    }
    out
}

/// The flag row for `arg`: `` `--long` `` / `` `-s`, `--long` `` / a bare
/// `` `<POSITIONAL>` ``, its value shape, env, default, and description.
fn arg_row(arg: &Arg) -> String {
    format!(
        "| {} | {} | {} | {} | {} |\n",
        names_cell(arg),
        value_cell(arg),
        env_cell(arg),
        default_cell(arg),
        cell(&help_text(arg)),
    )
}

fn names_cell(arg: &Arg) -> String {
    let mut names = Vec::new();
    if let Some(short) = arg.get_short() {
        names.push(format!("`-{short}`"));
    }
    if let Some(long) = arg.get_long() {
        names.push(format!("`--{long}`"));
    }
    if names.is_empty() {
        return format!("`<{}>`", value_name(arg));
    }
    names.join(" ")
}

/// The value shape: an enum's accepted spellings, `<VALUE>`, or `—` for a
/// plain switch. `[VALUE]` marks a flag whose value is optional
/// (`--write-baseline [PATH]`), and `(repeatable)` marks appendable flags.
fn value_cell(arg: &Arg) -> String {
    match arg.get_action() {
        ArgAction::SetTrue
        | ArgAction::SetFalse
        | ArgAction::Count
        | ArgAction::Help
        | ArgAction::HelpShort
        | ArgAction::HelpLong
        | ArgAction::Version => return "—".to_string(),
        _ => {}
    }
    let mut value = match possible_values(arg) {
        Some(values) => values
            .iter()
            .map(|v| format!("`{v}`"))
            .collect::<Vec<_>>()
            .join(" \\| "),
        None => {
            let bracketed = arg.get_num_args().is_some_and(|r| r.min_values() == 0);
            if bracketed {
                format!("`[<{}>]`", value_name(arg))
            } else {
                format!("`<{}>`", value_name(arg))
            }
        }
    };
    if matches!(arg.get_action(), ArgAction::Append)
        || arg.get_num_args().is_some_and(|r| r.max_values() > 1)
    {
        value.push_str(" (repeatable)");
    }
    value
}

fn value_name(arg: &Arg) -> String {
    arg.get_value_names()
        .and_then(|names| names.first())
        .map(|n| esc(n.as_ref()))
        .unwrap_or_else(|| esc(&arg.get_id().to_string().to_uppercase()))
}

fn possible_values(arg: &Arg) -> Option<Vec<String>> {
    let values: Vec<String> = arg
        .get_value_parser()
        .possible_values()?
        .map(|p| p.get_name().to_string())
        .collect();
    (!values.is_empty()).then_some(values)
}

fn env_cell(arg: &Arg) -> String {
    arg.get_env()
        .map(|e| format!("`{}`", e.to_string_lossy()))
        .unwrap_or_else(|| "—".to_string())
}

fn default_cell(arg: &Arg) -> String {
    let defaults = arg.get_default_values();
    if defaults.is_empty() {
        return "—".to_string();
    }
    defaults
        .iter()
        .map(|d| format!("`{}`", esc(&d.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first description paragraph — `about`/`long_about` on commands,
/// `help`/`long_help` on args.
fn about(cmd: &Command) -> String {
    cmd.get_long_about()
        .or_else(|| cmd.get_about())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn help_text(arg: &Arg) -> String {
    arg.get_long_help()
        .or_else(|| arg.get_help())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// One Markdown table cell: whitespace flattened to single spaces, `|`
/// escaped (GitHub renders `\|` as a literal pipe even inside code spans).
fn cell(text: &str) -> String {
    esc(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Escape a table-breaking `|` inside a cell body.
fn esc(text: &str) -> String {
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(cli_reference_markdown(), cli_reference_markdown());
    }

    #[test]
    fn covers_the_real_cli() {
        let doc = cli_reference_markdown();
        assert!(doc.starts_with(GENERATED_MARKER));
        for name in ["docs", "impact", "check", "lint", "columns", "completions"] {
            assert!(
                doc.contains(&format!("### `dbtl {name}`")),
                "missing {name}"
            );
        }
        // Hidden subcommands exist but stay out of `dbtl --help`.
        assert!(doc.contains("`dbtl man`"));
        assert!(doc.contains("`dbtl cli-reference`"));
        assert!(doc.contains("## Exit codes"));
        assert!(doc.contains("## Global flags"));
        // The flag-consumption table: docs takes --manifest; completions
        // (no data source) takes nothing.
        let docs = doc.split("### `dbtl docs`").nth(1).unwrap();
        let docs = docs.split("### `dbtl").next().unwrap();
        assert!(docs.contains("`--manifest`"));
        let completions = doc.split("### `dbtl completions`").nth(1).unwrap();
        let completions = completions.split("### `dbtl").next().unwrap();
        assert!(completions.contains("Accepts no global flags"));
    }
}
