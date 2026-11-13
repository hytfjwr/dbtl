//! Prints the lint output the engine produces for a bundled fixture, as
//! JSON (`{"text": …, "json": …}`) — the expected bytes the docs site's
//! Node-side engine check compares the wasm build against
//! (`site/scripts/check-engine.mjs`).
//!
//! Usage: `fixture_lint <jaffle_finance|sample>` — the `.yml` files lint
//! positions resolve against are supplied exactly like the site does:
//! none for `jaffle_finance`, the project's own `.yml` files for `sample`.

use dbtl_wasm::engine::{Engine, LintOptions, LoadInput};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Every `.yml`/`.yaml` under `root`, keyed by its `/`-separated
/// project-relative path — the `files` map `Engine::load` expects.
fn yml_files(root: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read fixture dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
                let rel = path
                    .strip_prefix(root)
                    .expect("under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(rel, std::fs::read_to_string(&path).expect("read yml"));
            }
        }
    }
    files
}

fn main() {
    let name = std::env::args()
        .nth(1)
        .expect("usage: fixture_lint <jaffle_finance|sample>");
    let fixtures = fixture_dir();
    let (manifest, project) = match name.as_str() {
        "jaffle_finance" => (fixtures.join("manifest.json"), None),
        "sample" => (
            fixtures.join("sample_manifest.json"),
            Some(fixtures.join("sample_project")),
        ),
        other => panic!("unknown fixture '{other}'"),
    };
    let input = LoadInput {
        manifest: std::fs::read_to_string(&manifest).expect("read manifest"),
        files: project.map(|p| yml_files(&p)).unwrap_or_default(),
        ..LoadInput::default()
    };
    let (engine, _) = Engine::load(&input).expect("fixture loads");
    let out = engine.lint(&LintOptions::default()).expect("lint runs");
    print!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "text": out.text,
            "json": out.json,
        }))
        .expect("serializes")
    );
}
