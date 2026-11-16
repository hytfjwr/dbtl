use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Version to cut, without the leading `v` (e.g. `0.6.3`).
    #[arg(long)]
    version: String,
    /// Release date stamped on the new section (`YYYY-MM-DD`).
    #[arg(long)]
    date: String,
    /// Changelog file to rewrite in place.
    #[arg(long, default_value = "CHANGELOG.md")]
    file: PathBuf,
    /// Cut the section even when `## [Unreleased]` holds no entries (a
    /// release with no user-facing changes gets a `- No notable changes.`
    /// line).
    #[arg(long)]
    allow_empty: bool,
}

pub fn run(args: Args) -> Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .with_context(|| format!("read {}", args.file.display()))?;
    let out = xtask::changelog::release_section(&text, &args.version, &args.date, args.allow_empty)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::write(&args.file, out).with_context(|| format!("write {}", args.file.display()))?;
    eprintln!(
        "cut [{}] - {} in {}",
        args.version,
        args.date,
        args.file.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// run()'s file plumbing: the changelog is rewritten in place and a
    /// bad input leaves it untouched.
    #[test]
    fn run_rewrites_the_file_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("CHANGELOG.md");
        std::fs::write(
            &file,
            "# Changelog\n\n## [Unreleased]\n\n- New thing.\n\n## [0.6.2] - 2024-01-01\n\n- Old.\n\n[Unreleased]: https://x/compare/v0.6.2...HEAD\n[0.6.2]: https://x/compare/v0.6.1...v0.6.2\n",
        )
        .unwrap();
        run(Args {
            version: "0.6.3".to_string(),
            date: "2024-02-01".to_string(),
            file: file.clone(),
            allow_empty: false,
        })
        .expect("run changelog-release");
        let out = std::fs::read_to_string(&file).unwrap();
        assert!(out.contains("## [0.6.3] - 2024-02-01"), "{out}");
        assert!(out.contains("- New thing."), "{out}");
        assert!(
            out.contains("[Unreleased]: https://x/compare/v0.6.3...HEAD"),
            "{out}"
        );

        // A malformed version errors without touching the file.
        let err = run(Args {
            version: "nope".to_string(),
            date: "2024-02-01".to_string(),
            file: file.clone(),
            allow_empty: false,
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid version"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), out, "unchanged");
    }
}
