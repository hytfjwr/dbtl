//! Prints the lint output the engine produces for a manifest, as JSON
//! (`{"text": …, "json": …}`) — the expected bytes the docs site's
//! Node-side engine check compares the wasm build against
//! (`site/scripts/check-engine.mjs`).
//!
//! Usage: `fixture_lint <manifest.json> [files.json]` — `files.json` is
//! the `{project-relative path: text}` map `Engine::load` expects (the
//! `.yml` files lint positions resolve against), exactly like the site
//! stages under `public/samples/<name>/`.

use dbtl_wasm::engine::{Engine, LintOptions, LoadInput};
use std::collections::BTreeMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let manifest = args
        .next()
        .expect("usage: fixture_lint <manifest.json> [files.json]");
    let files: BTreeMap<String, String> = match args.next() {
        Some(path) => {
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read files.json"))
                .expect("files.json parses")
        }
        None => BTreeMap::new(),
    };
    let input = LoadInput {
        manifest: std::fs::read_to_string(&manifest).expect("read manifest"),
        files,
        ..LoadInput::default()
    };
    let (engine, _) = Engine::load(&input).expect("manifest loads");
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
