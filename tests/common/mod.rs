//! Shared fixture plumbing for the integration tests.
//!
//! Each `tests/*.rs` target is its own crate, so helpers that every suite
//! needs — locating the committed fixtures, loading the jaffle_finance
//! manifest, spawning the built `dbtl` binary — live here instead of being
//! copy-pasted per file. Not every target uses every helper, so dead code
//! is allowed crate-wide.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

/// The committed jaffle_finance manifest (schema v12) most suites load.
pub const FIXTURE_MANIFEST: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

/// The small synthetic manifest the CLL/consistency suites pin.
pub const SAMPLE_MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/sample_manifest.json"
);

/// The `dbt_source`-mode project dir matching `SAMPLE_MANIFEST`.
pub const SAMPLE_PROJECT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_project");

/// Absolute path to a file under `tests/fixtures/`.
pub fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// The fixture manifest as a `PathBuf` (CLI-spawning suites want paths).
pub fn manifest_path() -> PathBuf {
    fixture_path("manifest.json")
}

/// `SAMPLE_MANIFEST` as a `PathBuf`.
pub fn sample_manifest_path() -> PathBuf {
    fixture_path("sample_manifest.json")
}

/// Load the fixture manifest into a `Dag`.
pub fn fixture_dag() -> dbtl::Dag {
    dbtl::load_dag(FIXTURE_MANIFEST).expect("fixture manifest must load")
}

/// A `Command` bound to the `dbtl` binary cargo just built.
pub fn dbtl_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dbtl"))
}
