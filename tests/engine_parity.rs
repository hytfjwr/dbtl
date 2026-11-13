//! Byte-for-byte parity between the in-memory engine (`dbtl-wasm`'s
//! `engine` module — the same code the docs site builds to wasm) and the
//! CLI: the engine must emit exactly what `dbtl ls`/`dbtl lint`/
//! `dbtl columns` print — stdout bodies, the gate verdict, the exit code —
//! and lay out lineage exactly like `layout_mode`, the placement the TUI
//! draws. If any of these drifted, the site would silently show different
//! results than the binary, so the suite compares bytes, not shapes.
//!
//! Two fixtures run through both paths:
//! - the committed `jaffle_finance` manifest (no project files — findings
//!   keep only their manifest-carried data, like a manifest-only CLI run);
//! - `sample_project` copied into a tempdir with `sample_manifest.json`
//!   dropped at `target/manifest.json`, so the CLI resolves the project's
//!   `.yml` files while the engine gets the same texts through its `files`
//!   map — schema-declared findings must anchor identically either way.
//!
//! No field is environment-dependent under these arrangements: every path
//! in the reports is project-relative, `tool_version` comes from the
//! workspace version, and the ordering is deterministic — the comparisons
//! below are raw `assert_eq!` on the byte strings, nothing normalized.

mod common;

use common::{
    copy_tree, dbtl_cmd, manifest_path, sample_manifest_path, SAMPLE_PROJECT,
};
use dbtl_wasm::engine::{ColumnsInput, Engine, LineageInput, LintOptions, LoadInput};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    let mut cmd: Command = dbtl_cmd();
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.output().expect("run dbtl")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Every `.yml`/`.yaml` under `root`, keyed by its `/`-separated
/// project-relative path — the `files` map `Engine::load` expects (the
/// same keys `location.file` carries).
fn yml_files(root: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(rel, std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    files
}

/// An engine loaded with the jaffle_finance manifest (no `.yml` texts —
/// the fixture dir has none, like a manifest-only CLI run).
fn jaffle_engine() -> Engine {
    let (engine, _) = Engine::load(&LoadInput {
        manifest: std::fs::read_to_string(manifest_path()).unwrap(),
        ..LoadInput::default()
    })
    .expect("jaffle fixture loads");
    engine
}

/// `sample_project` copied to a tempdir plus `sample_manifest.json` at
/// `target/manifest.json` — returns `(tempdir, manifest path)`. The temp
/// home keeps the CLI's `.yml` lookups inside the test.
fn sample_project_dir(tmp: &tempfile::TempDir) -> PathBuf {
    let proj = tmp.path().join("proj");
    copy_tree(Path::new(SAMPLE_PROJECT), &proj);
    std::fs::create_dir_all(proj.join("target")).unwrap();
    std::fs::copy(sample_manifest_path(), proj.join("target/manifest.json")).unwrap();
    proj
}

/// An engine loaded with `sample_manifest.json` and the project's `.yml`
/// texts — the engine-side twin of `sample_project_dir`.
fn sample_engine() -> Engine {
    let (engine, _) = Engine::load(&LoadInput {
        manifest: std::fs::read_to_string(sample_manifest_path()).unwrap(),
        files: yml_files(Path::new(SAMPLE_PROJECT)),
        ..LoadInput::default()
    })
    .expect("sample fixture loads");
    engine
}

// ---- selector parity: `dbtl ls --output uids` -------------------------

/// `dbtl ls <spec> --manifest <m> --output uids` returns the engine's
/// uid set, in the same order.
fn assert_selector_parity(manifest: &Path, engine: &Engine, spec: &str) {
    let cli = run(&[
        "ls",
        spec,
        "--manifest",
        manifest.to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert!(cli.status.success(), "ls stderr: {}", stderr(&cli));
    let want: Vec<String> = stdout(&cli).lines().map(str::to_string).collect();
    let got: Vec<String> = engine
        .select(spec)
        .unwrap_or_else(|e| panic!("engine select '{spec}': {e}"))
        .nodes
        .iter()
        .map(|n| n.unique_id.clone())
        .collect();
    assert_eq!(got, want, "uid set for '{spec}'");
}

#[test]
fn select_matches_ls_uids_on_both_fixtures() {
    let jaffle = jaffle_engine();
    let jaffle_manifest = manifest_path();
    for spec in [
        "tag:marts",
        "fct_subscription_process+",
        "config.materialized:table,tag:marts",
        "+fct_subscription_process",
        "path:models/intermediate",
    ] {
        assert_selector_parity(&jaffle_manifest, &jaffle, spec);
    }

    let sample = sample_engine();
    let sample_manifest = sample_manifest_path();
    for spec in ["stg_*", "fct_orders+", "tag:marts", "fct_orders stg_orders"] {
        assert_selector_parity(&sample_manifest, &sample, spec);
    }
}

#[test]
fn select_errors_match_the_cli_wording() {
    let engine = jaffle_engine();
    let err = engine.select("bad:spec:nop").expect_err("invalid spec");
    let cli = run(&[
        "ls",
        "bad:spec:nop",
        "--manifest",
        manifest_path().to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert_eq!(cli.status.code(), Some(2), "usage error class");
    assert_eq!(stderr(&cli).trim(), format!("error: {err}"));

    let err = engine.select("state:modified").expect_err("state spec");
    let cli = run(&[
        "ls",
        "state:modified",
        "--manifest",
        manifest_path().to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert_eq!(cli.status.code(), Some(2));
    assert_eq!(stderr(&cli).trim(), format!("error: {err}"));
}

// ---- lint parity: text/ansi/json + the gate ---------------------------

/// Assert the engine's lint output is the CLI's: `stdout` per format and
/// color, the exit code, and the gate message on stderr.
fn assert_lint_parity(manifest: &Path, engine: &Engine, opts: &LintOptions, extra: &[&str]) {
    let m = manifest.to_str().unwrap();
    let out = engine.lint(opts).expect("engine lint runs");

    let mut args = vec![
        "lint",
        "--manifest",
        m,
        "--format",
        "text",
        "--color",
        "never",
    ];
    args.extend_from_slice(extra);
    let cli = run(&args);
    assert_eq!(stdout(&cli), out.text, "text body");
    assert_eq!(cli.status.code(), Some(out.exit_code), "exit code");
    match &out.error {
        Some(e) => assert_eq!(stderr(&cli).trim(), format!("error: {e}")),
        None => assert_eq!(stderr(&cli).trim(), "", "a passing gate prints no error"),
    }

    let mut args = vec!["lint", "--manifest", m, "--format", "json"];
    args.extend_from_slice(extra);
    let cli = run(&args);
    assert_eq!(stdout(&cli), out.json, "json body");

    let mut args = vec![
        "lint",
        "--manifest",
        m,
        "--format",
        "text",
        "--color",
        "always",
    ];
    args.extend_from_slice(extra);
    let cli = run(&args);
    assert_eq!(stdout(&cli), out.ansi, "styled text body");
}

#[test]
fn lint_matches_the_cli_on_the_jaffle_manifest() {
    let engine = jaffle_engine();
    assert_lint_parity(&manifest_path(), &engine, &LintOptions::default(), &[]);
}

#[test]
fn lint_matches_the_cli_on_the_copied_sample_project() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = sample_project_dir(&tmp);
    let engine = sample_engine();
    assert_lint_parity(
        &proj.join("target/manifest.json"),
        &engine,
        &LintOptions::default(),
        &[],
    );
}

#[test]
fn lint_matches_the_cli_under_a_selector_and_fail_on() {
    let engine = jaffle_engine();
    assert_lint_parity(
        &manifest_path(),
        &engine,
        &LintOptions {
            selector: Some("tag:marts".to_string()),
            ..LintOptions::default()
        },
        &["--select", "tag:marts"],
    );
    assert_lint_parity(
        &manifest_path(),
        &engine,
        &LintOptions {
            fail_on: Some(dbtl::FailOn::Never),
            ..LintOptions::default()
        },
        &["--fail-on", "never"],
    );
}

// ---- column-lineage parity: `dbtl columns` ----------------------------

/// `dbtl columns <m> <target> [--upstream] [--format json]` against the
/// engine's `columns` — both output bodies and the fallback warning.
fn assert_columns_parity(manifest: &Path, engine: &Engine, target: &str, upstream: bool) {
    let m = manifest.to_str().unwrap();
    let mut args = vec!["columns", "--manifest", m, target];
    if upstream {
        args.push("--upstream");
    }
    let cli = run(&args);
    assert!(cli.status.success(), "columns stderr: {}", stderr(&cli));

    let out = engine
        .columns(&ColumnsInput {
            target: target.to_string(),
            upstream,
        })
        .expect("engine columns runs");
    assert_eq!(stdout(&cli), out.markdown, "markdown body");

    let mut args = vec!["columns", "--manifest", m, target, "--format", "json"];
    if upstream {
        args.push("--upstream");
    }
    let cli = run(&args);
    assert_eq!(stdout(&cli), out.json, "json body");
}

#[test]
fn columns_matches_the_cli() {
    let sample = sample_engine();
    let sample_manifest = sample_manifest_path();
    for (target, upstream) in [
        ("stg_orders.id", false),
        ("stg_orders.id", true),
        ("fct_orders.id", true),
        ("model.sample.fct_orders.id", true),
    ] {
        assert_columns_parity(&sample_manifest, &sample, target, upstream);
    }

    let jaffle = jaffle_engine();
    let jaffle_manifest = manifest_path();
    assert_columns_parity(
        &jaffle_manifest,
        &jaffle,
        "fct_subscription_process.subscription_id",
        false,
    );
}

#[test]
fn columns_errors_match_the_cli() {
    let engine = jaffle_engine();
    let m = manifest_path();
    for target in [
        "fct_subscription_process",
        "fct_subscription_process.no_such_col",
        "no_such_model.id",
    ] {
        let cli = run(&["columns", "--manifest", m.to_str().unwrap(), target]);
        let err = engine
            .columns(&ColumnsInput {
                target: target.to_string(),
                upstream: false,
            })
            .expect_err("a bad target errors");
        assert_eq!(cli.status.code(), Some(2), "{target}");
        assert_eq!(stderr(&cli).trim(), format!("error: {err}"), "{target}");
    }
}

// ---- lineage-layout parity: the TUI's own `layout_mode` ----------------

/// The engine's `rows` join into the exact text `layout_mode` would draw
/// for the same subgraph — the placement the TUI paints, so a browser can
/// never lay out the same graph differently.
#[test]
fn lineage_rows_match_layout_mode() {
    let engine = jaffle_engine();
    let dag = common::fixture_dag();
    for (root, glyphs) in [
        (
            "model.jaffle_finance.fct_subscription_process",
            dbtl::GlyphMode::Unicode,
        ),
        (
            "model.jaffle_finance.fct_subscription_process",
            dbtl::GlyphMode::Ascii,
        ),
    ] {
        let out = engine
            .lineage(&LineageInput {
                root: Some(root.to_string()),
                glyphs: match glyphs {
                    dbtl::GlyphMode::Unicode => dbtl_wasm::engine::GlyphMode::Unicode,
                    dbtl::GlyphMode::Ascii => dbtl_wasm::engine::GlyphMode::Ascii,
                },
                ..LineageInput::default()
            })
            .unwrap();
        let want = dbtl::layout_mode(&dag.subgraph(root), glyphs)
            .grid
            .to_text();
        assert_eq!(out.rows.join("\n"), want, "{root} {glyphs:?}");
    }
}

/// A selector's match set as one diagram — `roots` unions each root's
/// neighborhood; the rows equal what the union subgraph lays out as.
#[test]
fn lineage_roots_unions_the_neighborhoods() {
    let engine = jaffle_engine();
    let roots = vec![
        "model.jaffle_finance.fct_subscription_process".to_string(),
        "model.jaffle_finance.dim_delivery_lanes".to_string(),
    ];
    let out = engine
        .lineage(&LineageInput {
            roots: roots.clone(),
            direction: dbtl_wasm::engine::Direction::Upstream,
            depth: Some(1),
            ..LineageInput::default()
        })
        .unwrap();
    // Every root appears in the laid-out diagram.
    for root in &roots {
        assert!(
            out.rects.iter().any(|r| &r.uid == root),
            "{root} missing from rects"
        );
    }
    // Same shape as laying out the same union through dbtl-core directly.
    let dag = engine.dag();
    let mut nodes = std::collections::BTreeMap::new();
    let mut edges = std::collections::BTreeSet::new();
    let mut selected = String::new();
    for root in &roots {
        let sg = dag.subgraph_view(root, true, false, Some(1));
        if selected.is_empty() {
            selected = sg.selected.clone();
        }
        for n in sg.nodes {
            nodes.insert(n.unique_id.clone(), n);
        }
        for e in sg.edges {
            edges.insert((e.parent, e.child));
        }
    }
    let union = dbtl::Subgraph {
        selected,
        nodes: nodes.into_values().collect(),
        edges: edges
            .into_iter()
            .map(|(parent, child)| dbtl::Edge { parent, child })
            .collect(),
    };
    let want = dbtl::layout_mode(&union, dbtl::GlyphMode::Unicode)
        .grid
        .to_text();
    assert_eq!(out.rows.join("\n"), want);
}
