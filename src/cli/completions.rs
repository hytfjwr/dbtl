//! Text generated from the clap definition itself: `dbtl completions`
//! (shell completion scripts) and `dbtl man` (a roff man page). Both
//! render to stdout and return before any data source or terminal is
//! touched — they exist so packaging can ship completions and a man page
//! built from the exact CLI definition they describe.

use anyhow::Context;
use clap::CommandFactory;

use super::{Cli, CliError};

/// Write the completion script for `shell` to stdout.
pub fn run_completions(shell: clap_complete::Shell) -> Result<(), CliError> {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut cmd, "dbtl", &mut buf);
    let script = String::from_utf8(buf).context("the generated completion script is not UTF-8")?;
    super::write_stdout(&script, anstream::ColorChoice::Auto)
}

/// Write the man page (roff) for the top-level `dbtl` command to stdout.
/// Its SUBCOMMANDS section is generated from the same `Commands` enum the
/// dispatch reads, so the page can't drift from the real CLI.
pub fn run_man() -> Result<(), CliError> {
    let man = clap_mangen::Man::new(Cli::command());
    let mut buf = Vec::new();
    man.render(&mut buf)
        .context("failed to render the man page")?;
    let page = String::from_utf8(buf).context("the rendered man page is not UTF-8")?;
    super::write_stdout(&page, anstream::ColorChoice::Auto)
}
