//! The CLI's output taps: [`write_stdout`] is the ONE writer every
//! stdout-bound byte goes through, and [`warn_each`] is the ONE stderr loop
//! the loaders' warning vectors take — so no command can disagree with
//! another on pipe behavior or warning wording.

use std::io::Write;

use super::CliError;

/// Write `body` to stdout through `anstream` — ANSI escapes pass through
/// or strip per `choice` (`auto` follows the terminal/environment
/// detection; commands without a `--color` flag pass `Auto`). A closed
/// pipe (`dbtl ls … | head -1`) is a normal end of output — `BrokenPipe`
/// exits 0, never a panic; any other write error surfaces to the caller.
pub(crate) fn write_stdout(body: &str, choice: anstream::ColorChoice) -> Result<(), CliError> {
    let mut out = anstream::AutoStream::new(std::io::stdout(), choice);
    match out.write_all(body.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(CliError::new(e.into())),
    }
}

/// Print each entry of `warnings` to stderr as `warning{tag}: {w}` — the
/// shared wording every loader warning takes (`""` for the current source,
/// `" (--diff baseline)"` / `" (scope)"` for the tagged sides), so the text
/// can't drift between the commands that drive the same loaders.
pub(crate) fn warn_each(tag: &str, warnings: &[String]) {
    for w in warnings {
        eprintln!("warning{tag}: {w}");
    }
}
