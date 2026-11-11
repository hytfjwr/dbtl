//! Text generated from the clap definition itself: `dbtl completions`
//! (shell completion scripts) and `dbtl man` (a roff man page). Both
//! render to stdout and return before any data source or terminal is
//! touched — they exist so packaging can ship completions and a man page
//! built from the exact CLI definition they describe.

use std::io::{self, Write};

use anyhow::Context;
use clap::CommandFactory;

use super::{Cli, CliError};

/// Write the completion script for `shell` to stdout.
pub fn run_completions(shell: clap_complete::Shell) -> Result<(), CliError> {
    let mut cmd = Cli::command();
    let mut out = io::stdout().lock();
    clap_complete::generate(shell, &mut cmd, "dbtl", &mut out);
    out.flush()
        .context("failed to write the completion script")?;
    Ok(())
}

/// Write the man page (roff) for the top-level `dbtl` command to stdout.
/// Its SUBCOMMANDS section is generated from the same `Commands` enum the
/// dispatch reads, so the page can't drift from the real CLI.
pub fn run_man() -> Result<(), CliError> {
    let man = clap_mangen::Man::new(Cli::command());
    let mut out = io::stdout().lock();
    man.render(&mut out)
        .context("failed to render the man page")?;
    out.flush().context("failed to write the man page")?;
    Ok(())
}
