//! Library half of `xtask`: the shared, testable logic behind the cargo
//! subcommands — the deterministic synthetic-project generator (`synth`,
//! reused by the Criterion benches via a dev-dependency) and the perf-report
//! renderer (`perf_report`). `main.rs` only wires clap args to these.
pub mod gen_docs;
pub mod perf_report;
pub mod synth;
