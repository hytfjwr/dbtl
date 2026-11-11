//! Process exit-code classification for non-interactive paths.
//!
//! [`ExitKind`] is attached at the point an error originates (a gate failed,
//! the invocation was wrong, an input could not be read); [`CliError`]
//! carries it next to the underlying `anyhow::Error` so the ONE conversion in
//! `main` stays a pure classification → code mapping.

/// How a `dbtl` failure maps onto a process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// The invocation ran to completion and the checked invariant did not
    /// hold (a gate tripped, a `--check` comparison diverged).
    Gate,
    /// The invocation itself was invalid — unknown/unsupported flags,
    /// missing required flags, malformed config, an unresolvable selector.
    Usage,
    /// An explicitly named input file could not be found or read.
    Input,
}

/// The pure classification → process exit code: `Gate` is a failure (`1`),
/// `Usage` is `2`, `Input` is `3`, and any error that reaches `main`
/// unclassified defaults to `Input` (`3`) — an unexpected data/source
/// failure looks like a bad input, never like a clean gate trip.
pub fn exit_code(kind: Option<ExitKind>) -> i32 {
    match kind {
        Some(ExitKind::Gate) => 1,
        Some(ExitKind::Usage) => 2,
        Some(ExitKind::Input) | None => 3,
    }
}

/// An `anyhow::Error` plus the [`ExitKind`] decided where it was raised.
/// `main` prints `error:` via the error's `Debug`/`Display` exactly like a
/// bare `anyhow::Error` and reads `kind` for the exit code.
#[derive(Debug)]
pub struct CliError {
    error: anyhow::Error,
    kind: Option<ExitKind>,
}

impl CliError {
    /// Any error (unclassified — maps to `3`).
    pub fn new(error: anyhow::Error) -> Self {
        Self { error, kind: None }
    }

    /// A gate trip — the invocation was fine, the check did not pass.
    pub fn gate(error: anyhow::Error) -> Self {
        Self::with_kind(error, ExitKind::Gate)
    }

    /// The caller passed something the CLI cannot use.
    pub fn usage(error: anyhow::Error) -> Self {
        Self::with_kind(error, ExitKind::Usage)
    }

    /// A named input could not be found or read.
    pub fn input(error: anyhow::Error) -> Self {
        Self::with_kind(error, ExitKind::Input)
    }

    fn with_kind(error: anyhow::Error, kind: ExitKind) -> Self {
        Self {
            error,
            kind: Some(kind),
        }
    }

    /// The classification `main` maps to an exit code.
    pub fn kind(&self) -> Option<ExitKind> {
        self.kind
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

impl From<anyhow::Error> for CliError {
    /// Bare `anyhow::Result` helpers (`?`-propagated contexts, shared loader
    /// errors) keep working — the error surfaces unclassified.
    fn from(error: anyhow::Error) -> Self {
        Self::new(error)
    }
}

/// Attach an [`ExitKind`] to a bare `anyhow::Result` at the site the error
/// originates — `res.input()`/`res.usage()` instead of wrapping manually.
pub trait Classify<T> {
    /// The named input could not be found or read.
    fn input(self) -> Result<T, CliError>;
    /// The caller's invocation or config is invalid.
    fn usage(self) -> Result<T, CliError>;
}

impl<T> Classify<T> for anyhow::Result<T> {
    fn input(self) -> Result<T, CliError> {
        self.map_err(CliError::input)
    }
    fn usage(self) -> Result<T, CliError> {
        self.map_err(CliError::usage)
    }
}
