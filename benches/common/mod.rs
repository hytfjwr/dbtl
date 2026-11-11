//! Shared benchmark inputs. The synthetic-manifest generator lives in
//! `xtask::synth` — the same implementation `cargo xtask synth` writes to
//! disk — re-exported here so bench code keeps reading
//! `common::synth_manifest` and there is exactly one generator.
pub use xtask::synth::{manifest_json, synth_manifest, write_project, SynthParams};
