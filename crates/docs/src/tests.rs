use super::*;
use dbtl_core::load_dag;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/manifest.json"
);

fn fixture_dag() -> Dag {
    load_dag(FIXTURE).expect("fixture manifest must load")
}

/// Find the contents of the generated file at `rel_path`.
fn file<'a>(out: &'a [(String, String)], rel_path: &str) -> &'a str {
    out.iter()
        .find(|(p, _)| p == rel_path)
        .map(|(_, c)| c.as_str())
        .unwrap_or_else(|| panic!("expected a generated file at {rel_path}"))
}

/// Resolve a relative link target the way the filesystem would: interpret
/// `href` against the directory `from_page` lives in, collapsing `..`
/// segments. Generated pages only ever use `..`/plain segments (no `.`, no
/// leading `/`, no `http:`), so the join is this simple.
fn resolve_link(from_page: &str, href: &str) -> String {
    assert!(
        !href.starts_with('/'),
        "generated links are never root-absolute: {href}"
    );
    let mut parts: Vec<&str> = from_page
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .into_iter()
        .collect();
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                assert!(parts.pop().is_some(), "link escapes out_dir: {href}");
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[test]
fn generates_one_page_per_node_plus_readme() {
    let dag = fixture_dag();
    let out = generate_docs(
        &dag,
        "jaffle_finance",
        "tests/fixtures/manifest.json",
        PackageScope::Root,
    );
    // 93 nodes (45 model + 38 source + 7 seed + 1 snapshot + 2 exposure)
    // plus the README index.
    assert_eq!(out.len(), dag.len() + 1, "one page per node + README");
    let readme_count = out.iter().filter(|(p, _)| p == "README.md").count();
    assert_eq!(readme_count, 1, "exactly one README");
    // Every node page lives under its resource-type directory — and
    // `node_files` places each page exactly there.
    let files = node_files(&dag);
    for (path, _) in &out {
        assert!(
            path == "README.md" || files.values().any(|p| p == path),
            "unexpected path {path}"
        );
    }
    for (uid, rel) in &files {
        let node = dag.get(uid).expect("node_files only emits real uids");
        let dir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        assert_eq!(
            dir,
            docs_dir(&node.resource_type),
            "{uid}'s page must sit under {}",
            docs_dir(&node.resource_type)
        );
    }
    // The fixture exercises every shipped directory.
    for dir in ["models", "sources", "seeds", "snapshots", "exposures"] {
        assert!(
            files.values().any(|p| p.starts_with(&format!("{dir}/"))),
            "fixture has nodes under {dir}/"
        );
    }
}

#[test]
fn every_generated_file_leads_with_the_marker() {
    // The marker is the `--clean`/`--check` identification contract: if any
    // file skipped it, a stale page could survive a clean or a hand-written
    // file could be mistaken for generated.
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    for (path, contents) in &out {
        assert!(
            contents.starts_with(GENERATED_MARKER),
            "{path} must lead with the generated marker"
        );
    }
}

#[test]
fn node_paths_are_unique_no_collisions() {
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let mut paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
    let count = paths.len();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), count, "no two files share a path");
}

#[test]
fn deterministic_byte_identical_across_runs() {
    let dag = fixture_dag();
    let a = generate_docs(
        &dag,
        "jaffle_finance",
        "tests/fixtures/manifest.json",
        PackageScope::Root,
    );
    let b = generate_docs(
        &dag,
        "jaffle_finance",
        "tests/fixtures/manifest.json",
        PackageScope::Root,
    );
    assert_eq!(a, b, "two generations must be byte-identical");
}

#[test]
fn readme_links_resolve_to_real_generated_files() {
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let generated: HashSet<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
    let readme = file(&out, "README.md");
    // Pull every `*.md` target out of the README's Markdown links and
    // confirm each resolves to a file we actually generated (no broken
    // links). README sits at the root, so targets are root-relative.
    let mut checked = 0usize;
    for chunk in readme.split("](") {
        if let Some(end) = chunk.find(')') {
            let target = &chunk[..end];
            if target.ends_with(".md") {
                let resolved = resolve_link("README.md", target);
                assert!(
                    generated.contains(resolved.as_str()),
                    "README links to missing file {target}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked >= dag.len(), "every node is linked from the README");
}

#[test]
fn readme_has_summary_counts_and_mermaid() {
    let dag = fixture_dag();
    let out = generate_docs(&dag, "jaffle_finance", "manifest.json", PackageScope::Root);
    let readme = file(&out, "README.md");
    assert!(readme.contains("# jaffle_finance"), "project heading");
    assert!(
        readme.contains("Generated from: `manifest.json`"),
        "provenance"
    );
    assert!(readme.contains("- total nodes: 93"), "total count");
    assert!(readme.contains("- model: 45"), "model count");
    assert!(readme.contains("- source: 38"), "source count");
    assert!(readme.contains("- seed: 7"), "seed count");
    assert!(readme.contains("- snapshot: 1"), "snapshot count");
    assert!(
        readme.contains("```mermaid\ngraph LR\n"),
        "project mermaid fence"
    );
}

#[test]
fn node_page_has_all_sections_and_a_mermaid_fence() {
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    // Pick a model that the fixture freezes: fct_subscription_process.
    let uid = dag
        .nodes()
        .values()
        .find(|n| n.name == "fct_subscription_process")
        .map(|n| n.unique_id.clone())
        .expect("fixture has fct_subscription_process");
    let files = node_files(&dag);
    let page = file(&out, &files[&uid]);
    assert!(
        page.starts_with(GENERATED_MARKER),
        "generated pages lead with the marker"
    );
    assert!(
        page.contains("\n# fct_subscription_process"),
        "heading follows the marker"
    );
    for section in [
        "## Description",
        "## Columns",
        "## Tests",
        "## SQL",
        "## Upstream dependencies",
        "## Downstream dependencies",
        "## Lineage",
        "## Meta",
    ] {
        assert!(page.contains(section), "missing section {section}");
    }
    // The section order is part of the page contract.
    let headings: Vec<&str> = page.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(
        headings,
        vec![
            "## Description",
            "## Columns",
            "## Tests",
            "## SQL",
            "## Upstream dependencies",
            "## Downstream dependencies",
            "## Lineage",
            "## Meta",
        ],
        "model section order"
    );
    assert!(page.contains("- unique_id: `"), "unique_id metadata");
    assert!(
        page.contains("```mermaid\ngraph LR\n"),
        "per-node mermaid fence"
    );
    // It has upstream dependencies, so the closure-count lines are present.
    assert!(
        page.contains("- transitive ancestors:"),
        "transitive up count"
    );
    assert!(
        page.contains("- transitive descendants:"),
        "transitive down count"
    );
}

#[test]
fn seed_with_accepted_values_test_lists_it() {
    // The fixture's fiscal_years seed carries an accepted_values test.
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let uid = dag
        .nodes()
        .values()
        .find(|n| n.name == "fiscal_years" && n.resource_type == "seed")
        .map(|n| n.unique_id.clone())
        .expect("fixture has the fiscal_years seed");
    let files = node_files(&dag);
    let page = file(&out, &files[&uid]);
    assert!(page.contains("## Tests"), "tests section present");
    assert!(
        page.contains("accepted_values"),
        "lists the accepted_values test"
    );
}

#[test]
fn relationships_test_target_is_linked_in_the_tests_table() {
    // pos_ord's relationships test targets the pos_pay source —
    // the Tests table resolves the `to:` name and links its page.
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let page = file(&out, &files["model.jaffle_finance.pos_ord"]);
    assert!(
        page.contains("| Test | Column | Target | Rows |"),
        "the 4-column tests table: {page}"
    );
    assert!(
        page.contains("[pos_pay]("),
        "the relationships row links its target: {page}"
    );
}

#[test]
fn empty_sections_are_labeled_not_dropped() {
    // A source with no tests still shows the Tests heading + "No tests."
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let uid = dag
        .nodes()
        .values()
        .find(|n| n.resource_type == "source" && dag.tests(&n.unique_id).is_empty())
        .map(|n| n.unique_id.clone())
        .expect("fixture has an untested source");
    let files = node_files(&dag);
    let page = file(&out, &files[&uid]);
    assert!(
        page.contains("## Tests\n\nNo tests."),
        "labeled empty tests"
    );
}

#[test]
fn meta_renders_sorted_key_value_table() {
    // The Meta section is a `Key|Value` table sorted by key (BTreeMap
    // iteration); nested values render compact JSON; `|` escapes.
    use dbtl_core::{RawManifest, RawNode};
    use std::collections::HashMap;
    let meta = serde_json::Map::from_iter([
        (
            "zone".to_string(),
            serde_json::json!({"b": 2, "a": 1}), // object → compact JSON
        ),
        ("owner".to_string(), serde_json::json!("a|b")), // pipe escapes
        ("team".to_string(), serde_json::json!("eng")),
    ]);
    let nodes = HashMap::from([(
        "model.p.m".to_string(),
        RawNode {
            name: "m".into(),
            resource_type: "model".into(),
            path: Some("staging/m.sql".into()),
            meta,
            ..Default::default()
        },
    )]);
    let manifest = RawManifest {
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
        macros: HashMap::new(),
    };
    let dag = Dag::build(&manifest);
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let page = file(&out, &files["model.p.m"]);
    let section = page
        .split("## Meta\n\n")
        .nth(1)
        .and_then(|s| s.split("\n\n").next())
        .expect("meta section present");
    let rows: Vec<&str> = section
        .lines()
        .filter(|l| l.starts_with("| "))
        .skip(2) // header + separator
        .collect();
    assert_eq!(
        rows,
        vec![
            "| owner | a\\|b |",
            "| team | eng |",
            "| zone | {\"b\":2,\"a\":1} |",
        ],
        "rows sorted by key, nested compact JSON, pipe escaped"
    );
}

#[test]
fn hostile_names_and_descriptions_do_not_break_markdown_or_mermaid() {
    // A node whose name/description carry a `"`, a `|`, a `]`, a `\`, and a
    // newline must not break the Mermaid label, the Markdown table cell, the
    // link label, or the fence.
    use dbtl_core::{RawDependsOn, RawManifest, RawNode};
    use std::collections::HashMap;
    let mut columns = serde_json::Map::new();
    columns.insert(
        "id".to_string(),
        serde_json::json!({
            "name": "id",
            "data_type": "integer",
            "description": "a | piped \"quoted\" column"
        }),
    );
    let nodes = HashMap::from([
        (
            "model.p.evil".to_string(),
            RawNode {
                name: "ev\"]il\\ | model".into(),
                resource_type: "model".into(),
                path: Some("staging/evil.sql".into()),
                description: Some("line one\nline \"two\" | pipe".into()),
                columns,
                depends_on: RawDependsOn {
                    nodes: vec!["model.p.up".to_string()],
                    macros: vec![],
                },
                ..Default::default()
            },
        ),
        (
            "model.p.up".to_string(),
            RawNode {
                name: "up".into(),
                resource_type: "model".into(),
                path: Some("staging/up.sql".into()),
                ..Default::default()
            },
        ),
    ]);
    let parent_map = HashMap::from([("model.p.evil".to_string(), vec!["model.p.up".to_string()])]);
    let child_map = HashMap::from([("model.p.up".to_string(), vec!["model.p.evil".to_string()])]);
    let manifest = RawManifest {
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
        macros: HashMap::new(),
    };
    let dag = Dag::build(&manifest);
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let page = file(&out, &files["model.p.evil"]);
    // The Mermaid label escapes the quote (#quot;) and drops control chars,
    // so the raw `"` never reaches a Mermaid `["..."]` string nor the fence.
    assert!(page.contains("#quot;"), "mermaid label escaped the quote");
    // The hostile column description carries a `|`; the table cell escapes
    // it to `\|` so the row keeps its column count.
    assert!(
        page.contains("a \\| piped"),
        "table cell escaped the pipe in the column description"
    );
    // The whole-project README must also survive the hostile name: the
    // table cell escapes the `|` (the bare `]`/`\` are inert in cell text),
    // and the link label escapes `]`/`\`/`|` so the `[…](…)` can't snap.
    let readme = file(&out, "README.md");
    assert!(
        readme.contains("ev\"]il\\ \\| model"),
        "README cell escaped the pipe"
    );
    assert!(
        readme.contains("[ev\"\\]il\\\\ \\| model]("),
        "README link label escaped ] \\ and |"
    );
    // The dep list on the parent's page (same directory → basename href)
    // carries the same escaping.
    let up = file(&out, &files["model.p.up"]);
    assert!(
        up.contains("- [ev\"\\]il\\\\ \\| model](model.p.evil.md)"),
        "dep-list link label escaped ] \\ and |"
    );
    // Determinism still holds with the hostile data.
    let again = generate_docs(&dag, "p", "src", PackageScope::Root);
    assert_eq!(out, again, "deterministic even with hostile data");
}

// ---- Doc-page additions ----

/// Pull every `models/…md` target out of a `click <id> "<href>"` line.
fn click_targets(markdown: &str) -> Vec<String> {
    markdown
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let rest = l.strip_prefix("click ")?;
            // `click <id> "<href>"` — the href is the only quoted span.
            let start = rest.find('"')? + 1;
            let end = rest[start..].find('"')? + start;
            Some(rest[start..end].to_string())
        })
        .collect()
}

#[test]
fn diagram_click_links_target_real_generated_files() {
    // Every `click` link in EVERY generated page (node pages + README) must
    // resolve to a file we actually generated — no broken navigation, in
    // either direction (README → a page, or a page → any directory).
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let generated: HashSet<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
    let mut total_clicks = 0usize;
    let mut saw_parent_dir = false;
    for (page, contents) in &out {
        for target in click_targets(contents) {
            total_clicks += 1;
            saw_parent_dir |= target.starts_with("../");
            let resolved = resolve_link(page, &target);
            assert!(
                generated.contains(resolved.as_str()),
                "{page}: click links to missing file {target} (resolved {resolved})"
            );
        }
    }
    assert!(total_clicks > 0, "at least some click links were emitted");
    // Models depend on sources, so some page-level click must cross
    // directories via `../` — proving the href isn't a bare basename
    // resolving to a non-existent sibling.
    assert!(saw_parent_dir, "cross-directory click links exist");
}

#[test]
fn every_diagram_has_a_legend_pointing_at_real_files() {
    // The legend ("Nodes in this diagram:" + bullet links) is the
    // click-independent fallback; its links must resolve too.
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let generated: HashSet<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
    for (page, contents) in &out {
        // A page with lineage carries a legend.
        if !contents.contains("```mermaid") {
            continue;
        }
        assert!(
            contents.contains("Nodes in this diagram:"),
            "{page}: a diagram is missing its legend"
        );
        // Legend links are Markdown `](href)`; resolve like click targets.
        for chunk in contents.split("](") {
            if let Some(end) = chunk.find(')') {
                let target = &chunk[..end];
                if !target.ends_with(".md") {
                    continue;
                }
                let resolved = resolve_link(page, target);
                assert!(
                    generated.contains(resolved.as_str()),
                    "{page}: legend/link target {target} missing (resolved {resolved})"
                );
            }
        }
    }
}

#[test]
fn readme_coverage_summary_matches_coverage_counts() {
    // The README's coverage numbers must be exactly the shared
    // `coverage_counts` base (the same source the TUI's `t` lens / `cov`
    // segment / `dbtl check` use): testable = model|snapshot|seed in the
    // root project, tested = those with ≥1 test.
    let dag = fixture_dag();
    let (tested, testable) = coverage_counts(&dag);
    let pct = tested * 100 / testable;
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let readme = file(&out, "README.md");
    assert!(
        readme.contains(&format!(
            "- tested: {tested} / {testable} testable resources"
        )),
        "coverage tested/total line, expected {tested}/{testable}"
    );
    assert!(
        readme.contains(&format!("- coverage: {pct}%")),
        "coverage percent line, expected {pct}%"
    );
    assert!(
        readme.contains(&format!("- untested: {}", testable - tested)),
        "untested count line"
    );
}

#[test]
fn readme_column_coverage_matches_column_coverage_counts() {
    // The README's column-coverage line must be exactly the shared
    // `column_coverage_counts` base the TUI stats dashboard quotes —
    // declared columns carrying ≥1 test over the root-project testable
    // set (212/322 on the frozen fixture).
    let dag = fixture_dag();
    let (tested, declared) = column_coverage_counts(&dag);
    let pct = tested * 100 / declared;
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let readme = file(&out, "README.md");
    assert!(
        readme.contains(&format!(
            "- columns tested: {tested} / {declared} declared columns ({pct}%)"
        )),
        "column coverage line, expected {tested}/{declared}, got: {readme}"
    );
}

#[test]
fn readme_orphan_list_matches_direct_down_zero_models() {
    // The orphan list is exactly the root-project models with
    // `direct_down == 0`, sorted (the stats dashboard's predicate).
    let dag = fixture_dag();
    let mut expected: Vec<&str> = dag
        .nodes()
        .values()
        .filter(|n| dag.is_root_project(n) && n.resource_type == "model" && n.direct_down == 0)
        .map(|n| n.name.as_str())
        .collect();
    expected.sort_unstable();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let readme = file(&out, "README.md");
    assert!(readme.contains("## Orphan models"), "orphan section");
    // The section after "## Orphan models" lists every expected name; pull
    // the bullet names that appear under it.
    let section = readme
        .split("## Orphan models")
        .nth(1)
        .expect("orphan section present");
    for name in &expected {
        assert!(
            section.contains(&format!("[{name}](")) || section.contains(&format!("- {name}\n")),
            "orphan model {name} missing from the README list"
        );
    }
    assert!(!expected.is_empty(), "fixture has orphan models to assert");
}

#[test]
fn small_project_renders_one_whole_graph_diagram() {
    // Below the threshold (the 93-node fixture), the README lineage is a
    // SINGLE diagram (no per-layer `###` split headings).
    let dag = fixture_dag();
    assert!(dag.len() < FULL_GRAPH_NODE_LIMIT, "fixture is small");
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let readme = file(&out, "README.md");
    let lineage = readme
        .split("## Lineage")
        .nth(1)
        .expect("README has a Lineage section");
    let fences = lineage.matches("```mermaid").count();
    assert_eq!(fences, 1, "small project = exactly one whole-graph diagram");
    assert!(
        !lineage.contains("split by layer"),
        "no split note below the threshold"
    );
}

/// A synthetic dag of `count` chained models spread across the four logical
/// layers, used to cross the [`FULL_GRAPH_NODE_LIMIT`] threshold in tests.
fn large_layered_dag(count: usize) -> Dag {
    use dbtl_core::{RawDependsOn, RawManifest, RawNode};
    use std::collections::HashMap;
    let layers = ["staging", "intermediate", "marts", "utilities"];
    let mut nodes = HashMap::new();
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    for i in 0..count {
        let layer = layers[i % layers.len()];
        let uid = format!("model.big.m{i:04}");
        // Chain each node to the previous one in the SAME layer so every
        // group carries some intra-group edges.
        let depends: Vec<String> = if i >= layers.len() {
            vec![format!("model.big.m{:04}", i - layers.len())]
        } else {
            Vec::new()
        };
        for p in &depends {
            parent_map.entry(uid.clone()).or_default().push(p.clone());
            child_map.entry(p.clone()).or_default().push(uid.clone());
        }
        nodes.insert(
            uid.clone(),
            RawNode {
                name: format!("m{i:04}"),
                resource_type: "model".into(),
                path: Some(format!("{layer}/m{i:04}.sql")),
                depends_on: RawDependsOn {
                    nodes: depends,
                    macros: vec![],
                },
                ..Default::default()
            },
        );
    }
    Dag::build(&RawManifest {
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
        macros: HashMap::new(),
    })
}

#[test]
fn large_project_splits_full_diagram_by_layer() {
    // At/above the threshold the README lineage splits into one diagram per
    // layer group, with an explanatory note — the single hairball is avoided.
    let dag = large_layered_dag(FULL_GRAPH_NODE_LIMIT + 8);
    assert!(dag.len() >= FULL_GRAPH_NODE_LIMIT);
    let out = generate_docs(&dag, "big", "src", PackageScope::Root);
    let readme = file(&out, "README.md");
    let lineage = readme
        .split("## Lineage")
        .nth(1)
        .expect("README has a Lineage section");
    assert!(
        lineage.contains("split by layer"),
        "large project carries the split note"
    );
    // One diagram per layer group (staging/intermediate/marts/utilities).
    let fences = lineage.matches("```mermaid").count();
    assert_eq!(fences, 4, "one diagram per layer group");
    for layer in LAYER_ORDER {
        assert!(
            lineage.contains(&format!("### {layer} (")),
            "layer heading for {layer}"
        );
    }
    // Still deterministic for the large/split case.
    let again = generate_docs(&dag, "big", "src", PackageScope::Root);
    assert_eq!(out, again, "split lineage is deterministic");
}

#[test]
fn split_diagrams_cover_every_node_exactly_once() {
    // Every node lands in exactly one layer group — no node dropped, none
    // duplicated across groups.
    let dag = large_layered_dag(FULL_GRAPH_NODE_LIMIT + 8);
    let groups = layer_groups(&dag);
    let mut all: Vec<String> = groups.iter().flat_map(|g| g.uids.clone()).collect();
    let total = all.len();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), total, "no node appears in two groups");
    assert_eq!(total, dag.len(), "every node lands in exactly one group");
}

#[test]
fn full_doc_set_is_byte_identical_with_all_added_pages() {
    // The end-to-end determinism guarantee covering the new surface: click
    // links, legends, the stats summary, and (via the large dag) the split.
    let small = fixture_dag();
    let a = generate_docs(
        &small,
        "jaffle_finance",
        "manifest.json",
        PackageScope::Root,
    );
    let b = generate_docs(
        &small,
        "jaffle_finance",
        "manifest.json",
        PackageScope::Root,
    );
    assert_eq!(a, b, "small project: byte-identical across runs");
    let big = large_layered_dag(FULL_GRAPH_NODE_LIMIT + 8);
    let c = generate_docs(&big, "big", "src", PackageScope::Root);
    let d = generate_docs(&big, "big", "src", PackageScope::Root);
    assert_eq!(c, d, "large project: byte-identical across runs");
}

#[test]
fn project_name_comes_from_unique_id_segment() {
    // The Dag-derived project name (the `unique_id` project segment) is what
    // the docs index should title with — robust to weird source labels.
    let dag = fixture_dag();
    assert_eq!(
        dag.project_name(),
        Some("jaffle_finance"),
        "project name read from unique_id"
    );
}

/// Extract the single fenced `\`\`\`mermaid … \`\`\`` block from a page,
/// including both fences and the trailing newline — the exact byte shape
/// [`subgraph_mermaid_linked`] / `App::lineage_mermaid` emit.
fn fenced_mermaid_block(page: &str) -> String {
    let open = "```mermaid\n";
    let start = page.find(open).expect("page has a mermaid fence");
    let after = start + open.len();
    let rel_end = page[after..].find("\n```\n").expect("closing fence");
    let end = after + rel_end + "\n```\n".len();
    page[start..end].to_string()
}

/// Drop the docs-only `click <id> "<href>"` navigation lines from a fenced
/// Mermaid block, leaving the bare diagram body (what the `m` yank emits).
fn strip_click_lines(block: &str) -> String {
    block
        .lines()
        .filter(|l| !l.trim_start().starts_with("click "))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// A page whose figure was reduced (depth dropped) or omitted can't be
/// compared to a yank at the REQUESTED depth — detect either marker.
fn figure_degraded(page: &str) -> bool {
    page.contains("reduced from") || page.contains("Diagram omitted:")
}

#[test]
fn per_node_lineage_figure_equals_the_m_key_yank() {
    // For EVERY selectable node, the per-node docs Lineage figure must render
    // the SAME diagram the interactive `m` yank produces for that node AT THE
    // SAME view — both go through the one shared `subgraph_mermaid_linked`
    // core over the SAME `subgraph_view` closure. The docs variant only adds
    // `click` navigation lines inside the fence; stripping those must leave a
    // body byte-identical to the yank. This locks the two surfaces together
    // so they can never drift.
    //
    // Compared at every depth the docs can request: `all` (the yank's default
    // view), 2 hops (the `DepthDecrease` cycle lands on it after two presses:
    // None → 3 → 2), and 1 hop (three presses). A page whose figure was
    // budget-reduced or omitted no longer matches the requested depth's yank
    // — those pages are skipped here and covered by their own assertions
    // below.
    //
    // Selection is BY UNIQUE_ID, not by name: the fixture has distinct nodes
    // that share a name (e.g. a model AND a source both named `pos_txn`), so a
    // name lookup would be ambiguous (and `nodes()` iteration order is not
    // deterministic). Walk uids in sorted order for a deterministic run.
    use dbtl_tui::{apply_action, Action, App};
    use std::path::PathBuf;

    let dag = fixture_dag();
    let files = node_files(&dag);
    let model_count = dag.count_by_resource_type("model");
    assert!(model_count > 0, "fixture has models to compare");

    // (docs depth, DepthDecrease presses to reach the matching TUI view)
    for (depth, presses) in [
        (LineageDepth::All, 0),
        (LineageDepth::Hops(2), 2),
        (LineageDepth::Hops(1), 3),
    ] {
        let out = generate_docs_with_options(
            &dag,
            "p",
            "src",
            PackageScope::Root,
            &DocsOptions {
                lineage_depth: depth,
                ..Default::default()
            },
        );
        let mut uids: Vec<&str> = dag.nodes().keys().map(String::as_str).collect();
        uids.sort_unstable();
        let mut compared = 0usize;
        for uid in uids {
            let mut app = App::new(dag.clone(), PathBuf::from(FIXTURE));
            // Only models are selectable lineage roots — the left pane lists
            // `resource_type == "model"` only, and re-rooting only ever lands
            // on a model — so the `m` yank only exists for models. Non-models
            // (their docs page still renders) have no yank to compare; skip.
            if !app.select_by_unique_id(uid) {
                continue;
            }
            for _ in 0..presses {
                apply_action(&mut app, Action::DepthDecrease);
            }
            let yank = app
                .lineage_mermaid()
                .expect("a selectable node yields an m yank");
            let page = file(&out, &files[uid]);
            if figure_degraded(page) {
                continue;
            }
            let body = strip_click_lines(&fenced_mermaid_block(page));
            assert_eq!(
                body, yank,
                "docs Lineage figure for {uid} at {depth:?} must equal the `m` \
                 yank at the same view (minus click lines)"
            );
            compared += 1;
        }
        assert!(
            compared > 0,
            "at least one model's figure was compared at {depth:?}"
        );
    }
}

// ---- the lint surface ---------------------------------------------------

/// The fixture's `run_rules` findings under the shipped defaults — the
/// shared vector `dbtl check --lint` gates on (frozen counts live in
/// `tests/manifest_fixture.rs`).
fn fixture_findings(dag: &Dag) -> Vec<Finding> {
    dbtl_core::run_rules(dbtl_core::LintInput::new(
        dag,
        &dbtl_core::LintConfig::default(),
    ))
}

#[test]
fn readme_lint_summary_counts_match_the_shared_findings() {
    // The README's Lint section is a VIEW over `run_rules` output — its
    // per-rule table must count the same numbers `check --lint` prints.
    let dag = fixture_dag();
    let findings = fixture_findings(&dag);
    let out = generate_docs_with_findings(
        &dag,
        "jaffle_finance",
        "src",
        PackageScope::Root,
        Some(&findings),
    );
    let readme = file(&out, "README.md");
    assert!(readme.contains("## Lint"), "the summary section renders");
    assert!(
        readme.contains("| Rule | Severity | Findings |"),
        "the severity column renders: {readme}"
    );
    for info in dbtl_core::lint::REGISTRY {
        let fs: Vec<&Finding> = findings.iter().filter(|f| f.rule == info.rule).collect();
        if fs.is_empty() {
            continue;
        }
        assert!(
            readme.contains(&format!(
                "| `{}` | {} | {} |",
                info.rule.name(),
                fs[0].severity.name(),
                fs.len()
            )),
            "rule {}'s severity+count matches run_rules",
            info.rule.name()
        );
    }
    assert!(
        readme.contains("27 finding(s) across 6 rule(s)."),
        "the frozen total, got README:\n{readme}"
    );
    // Every violating node is listed under its rule with a working link.
    let pos_cat = "models/model.jaffle_finance.pos_cat.md";
    assert!(
        readme.contains(&format!("- [pos_cat]({pos_cat})")),
        "violating nodes link to their pages"
    );
}

#[test]
fn node_pages_list_their_lint_violations_from_the_same_findings() {
    // A violating node's page quotes its findings (rule + message); a
    // clean node's page still labels the section (never dropped).
    let dag = fixture_dag();
    let findings = fixture_findings(&dag);
    let out = generate_docs_with_findings(
        &dag,
        "jaffle_finance",
        "src",
        PackageScope::Root,
        Some(&findings),
    );
    let pos_cat = file(&out, "models/model.jaffle_finance.pos_cat.md");
    assert!(
        pos_cat.contains("## Lint violations\n\n- `direct_join_to_source` (error): pos_cat"),
        "the finding renders with its severity on the violating node's page"
    );
    // A node with zero findings still gets a labeled section. The candidate
    // must be a type that RENDERS the section (exposures/semantic resources
    // don't), picked from a sorted list — `dag.nodes()` is a HashMap, so an
    // unsorted `find` is run-to-run nondeterministic.
    let clean = findings
        .iter()
        .map(|f| f.node_uid.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut candidates: Vec<&String> = dag
        .nodes()
        .keys()
        .filter(|u| !clean.contains(*u))
        .filter(|u| {
            matches!(
                dag.get(u).map(|n| n.resource_type.as_str()),
                Some("model" | "source" | "seed" | "snapshot")
            )
        })
        .collect();
    candidates.sort();
    let clean_uid = candidates
        .first()
        .expect("the fixture has lint-clean nodes");
    let path = node_files(&dag)[*clean_uid].clone();
    let page = file(&out, &path);
    assert!(
        page.contains("## Lint violations\n\nNone."),
        "clean nodes label the section too: {path}"
    );
}

#[test]
fn generate_docs_without_findings_emits_no_lint_output() {
    // `generate_docs` (lint never ran) must not claim anything about lint:
    // no README section, no node-page section — a committed doc set can't
    // fake a clean bill for a check it never ran.
    let dag = fixture_dag();
    let out = generate_docs(&dag, "jaffle_finance", "src", PackageScope::Root);
    let readme = file(&out, "README.md");
    assert!(!readme.contains("## Lint"), "no summary without findings");
    let pos_cat = file(&out, "models/model.jaffle_finance.pos_cat.md");
    assert!(
        !pos_cat.contains("Lint violations"),
        "no node section without findings"
    );
}

#[test]
fn a_clean_lint_run_is_stated_not_silent() {
    // `Some(&[])` (lint ran, nothing found) still documents the run — the
    // distinction from `None` (never ran) is load-bearing for committed docs.
    let dag = fixture_dag();
    let out =
        generate_docs_with_findings(&dag, "jaffle_finance", "src", PackageScope::Root, Some(&[]));
    let readme = file(&out, "README.md");
    assert!(readme.contains("## Lint"), "the section renders");
    assert!(readme.contains("No violations."), "a clean run says so");
}

#[test]
fn linted_doc_set_is_byte_identical_across_runs() {
    // The lint readout inherits the module's determinism contract.
    let dag = fixture_dag();
    let findings = fixture_findings(&dag);
    let a = generate_docs_with_findings(
        &dag,
        "jaffle_finance",
        "src",
        PackageScope::Root,
        Some(&findings),
    );
    let b = generate_docs_with_findings(
        &dag,
        "jaffle_finance",
        "src",
        PackageScope::Root,
        Some(&findings),
    );
    assert_eq!(a, b, "linted docs are byte-identical across runs");
}

// ---- synthetic-manifest helpers -------------------------------------------

use dbtl_core::{
    RawDependsOn, RawExposure, RawExposureOwner, RawManifest, RawNode, RawSemanticResource,
    RawSource,
};
use std::collections::HashMap as StdHashMap;

/// An empty `RawManifest` (no `Default` impl on the wire type — tests fill
/// the maps they exercise).
fn empty_manifest() -> RawManifest {
    RawManifest {
        metadata: Default::default(),
        nodes: StdHashMap::new(),
        sources: StdHashMap::new(),
        exposures: StdHashMap::new(),
        semantic_models: StdHashMap::new(),
        metrics: StdHashMap::new(),
        saved_queries: StdHashMap::new(),
        unit_tests: StdHashMap::new(),
        functions: StdHashMap::new(),
        parent_map: StdHashMap::new(),
        child_map: StdHashMap::new(),
        macros: StdHashMap::new(),
    }
}

/// Build a [`Dag`] from a manifest whose `depends_on` edges are folded into
/// the parent/child maps — so the synthetic manifest carries the same
/// adjacency a real `dbt`-produced one does.
fn dag_from(mut m: RawManifest) -> Dag {
    let mut edges: Vec<(String, String)> = Vec::new(); // (parent, child)
    for (uid, n) in &m.nodes {
        for p in &n.depends_on.nodes {
            edges.push((p.clone(), uid.clone()));
        }
    }
    for (uid, e) in &m.exposures {
        for p in &e.depends_on.nodes {
            edges.push((p.clone(), uid.clone()));
        }
    }
    for map in [&m.semantic_models, &m.metrics, &m.saved_queries] {
        for (uid, r) in map {
            for p in &r.depends_on.nodes {
                edges.push((p.clone(), uid.clone()));
            }
        }
    }
    for (parent, child) in edges {
        m.parent_map
            .entry(child.clone())
            .or_default()
            .push(parent.clone());
        m.child_map.entry(parent).or_default().push(child);
    }
    Dag::build(&m)
}

/// A `RawNode` with `deps` wired into `depends_on` — everything else default.
fn node(name: &str, resource_type: &str, deps: &[&str]) -> RawNode {
    RawNode {
        name: name.into(),
        resource_type: resource_type.into(),
        path: Some("models/x.sql".into()),
        depends_on: RawDependsOn {
            nodes: deps.iter().map(|d| d.to_string()).collect(),
            macros: vec![],
        },
        ..Default::default()
    }
}

// ---- per-type directories + sections -------------------------------------

/// One node of EVERY classified resource type (plus an unclassified kind)
/// linked into a small graph — the surface for directory/section checks.
fn all_kinds_dag() -> Dag {
    let mut m = empty_manifest();
    m.nodes.insert(
        "model.p.m".into(),
        node("m", "model", &["source.p.s", "seed.p.sd"]),
    );
    m.sources.insert(
        "source.p.s".into(),
        RawSource {
            name: "s".into(),
            resource_type: "source".into(),
            ..Default::default()
        },
    );
    m.nodes.insert("seed.p.sd".into(), node("sd", "seed", &[]));
    m.nodes.insert(
        "snapshot.p.sn".into(),
        node("sn", "snapshot", &["model.p.m"]),
    );
    m.exposures.insert(
        "exposure.p.e".into(),
        RawExposure {
            name: "e".into(),
            exposure_type: Some("dashboard".into()),
            owner: RawExposureOwner {
                name: Some("Ana".into()),
                email: Some("ana@x.io".into()),
            },
            url: Some("https://dash.example/e".into()),
            depends_on: RawDependsOn {
                nodes: vec!["model.p.m".into()],
                macros: vec![],
            },
            ..Default::default()
        },
    );
    m.semantic_models.insert(
        "semantic_model.p.sm".into(),
        RawSemanticResource {
            name: "sm".into(),
            depends_on: RawDependsOn {
                nodes: vec!["model.p.m".into()],
                macros: vec![],
            },
            ..Default::default()
        },
    );
    m.metrics.insert(
        "metric.p.mt".into(),
        RawSemanticResource {
            name: "mt".into(),
            depends_on: RawDependsOn {
                nodes: vec!["semantic_model.p.sm".into()],
                macros: vec![],
            },
            ..Default::default()
        },
    );
    m.saved_queries.insert(
        "saved_query.p.sq".into(),
        RawSemanticResource {
            name: "sq".into(),
            depends_on: RawDependsOn {
                nodes: vec!["model.p.m".into()],
                macros: vec![],
            },
            ..Default::default()
        },
    );
    // An unclassified resource type — retained under `other/`, never dropped.
    m.nodes.insert(
        "custom.p.cu".into(),
        node("cu", "custom_kind", &["model.p.m"]),
    );
    dag_from(m)
}

#[test]
fn every_resource_type_lands_in_its_own_directory() {
    let dag = all_kinds_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    for (uid, dir) in [
        ("model.p.m", "models"),
        ("source.p.s", "sources"),
        ("seed.p.sd", "seeds"),
        ("snapshot.p.sn", "snapshots"),
        ("exposure.p.e", "exposures"),
        ("semantic_model.p.sm", "semantic_models"),
        ("metric.p.mt", "metrics"),
        ("saved_query.p.sq", "saved_queries"),
        ("custom.p.cu", "other"),
    ] {
        let rel = files.get(uid).unwrap_or_else(|| panic!("{uid} has a page"));
        assert_eq!(
            rel,
            &format!("{dir}/{uid}.md"),
            "{uid} must live under {dir}/"
        );
        let _ = file(&out, rel); // the page was actually emitted
    }
}

#[test]
fn section_order_matches_the_resource_type() {
    let dag = all_kinds_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let headings = |uid: &str| -> Vec<String> {
        file(&out, &files[uid])
            .lines()
            .filter(|l| l.starts_with("## "))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        headings("model.p.m"),
        [
            "## Description",
            "## Columns",
            "## Tests",
            "## SQL",
            "## Upstream dependencies",
            "## Downstream dependencies",
            "## Lineage",
            "## Meta",
        ]
    );
    assert_eq!(
        headings("source.p.s"),
        [
            "## Description",
            "## Columns",
            "## Tests",
            "## Downstream dependencies",
            "## Lineage",
            "## Meta",
        ]
    );
    assert_eq!(headings("seed.p.sd"), headings("source.p.s"));
    assert_eq!(
        headings("exposure.p.e"),
        ["## Description", "## Depends on", "## Lineage",]
    );
    for uid in [
        "semantic_model.p.sm",
        "metric.p.mt",
        "saved_query.p.sq",
        "custom.p.cu",
    ] {
        assert_eq!(
            headings(uid),
            ["## Description", "## Upstream dependencies", "## Lineage",],
            "{uid} carries the consumer section set"
        );
    }
}

#[test]
fn heading_bullets_carry_the_types_metadata() {
    let dag = all_kinds_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let model = file(&out, &files["model.p.m"]);
    for bullet in [
        "- resource type: `model`",
        "- materialization:",
        "- access:",
        "- contract:",
        "- group:",
        "- version:",
        "- latest:",
        "- database:",
        "- schema:",
        "- tags:",
        "- unique_id: `model.p.m`",
        "- source file:",
    ] {
        assert!(model.contains(bullet), "model heading lacks {bullet}");
    }
    let source = file(&out, &files["source.p.s"]);
    for bullet in ["- identifier:", "- relation:"] {
        assert!(source.contains(bullet), "source heading lacks {bullet}");
    }
    let exposure = file(&out, &files["exposure.p.e"]);
    for bullet in [
        "- type: `dashboard`",
        "- owner: `Ana <ana@x.io>`",
        "- url: `https://dash.example/e`",
    ] {
        assert!(exposure.contains(bullet), "exposure heading lacks {bullet}");
    }
}

#[test]
fn cross_directory_links_are_relative_to_the_page() {
    let dag = all_kinds_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    // A model page in models/ links its source parent via `../sources/…`.
    let model = file(&out, &files["model.p.m"]);
    assert!(
        model.contains("[s](../sources/source.p.s.md)"),
        "model page links up a directory: {model}"
    );
    // A source page in sources/ links the downstream model via `../models/…`.
    let source = file(&out, &files["source.p.s"]);
    assert!(
        source.contains("[m](../models/model.p.m.md)"),
        "source page links up a directory: {source}"
    );
    // A snapshot in snapshots/ links its model parent via `../models/…`.
    let snapshot = file(&out, &files["snapshot.p.sn"]);
    assert!(
        snapshot.contains("[m](../models/model.p.m.md)"),
        "snapshot page links across directories"
    );
    // The README (root) links everything root-relative — never `../`.
    let readme = file(&out, "README.md");
    assert!(!readme.contains("](../"), "README links are root-relative");
}

// ---- the SQL section ------------------------------------------------------

/// A manifest with one model/snapshot pair per case — the SQL-section
/// surface. `compiled` goes into `compiled_code` (Manifest origin), `raw`
/// into `raw_code`.
fn sql_dag() -> Dag {
    let mut m = empty_manifest();
    // compiled preferred over raw.
    let mut with_both = node("both", "model", &[]);
    with_both.raw_code = Some("select 'raw'".into());
    with_both.compiled_code = Some("select 'compiled'".into());
    m.nodes.insert("model.p.both".into(), with_both);
    // raw only.
    let mut raw_only = node("raw", "model", &[]);
    raw_only.raw_code = Some("select 'raw only'".into());
    m.nodes.insert("model.p.raw".into(), raw_only);
    // neither → labeled.
    m.nodes
        .insert("model.p.none".into(), node("none", "model", &[]));
    // snapshot carries BOTH but must show RAW (snapshots aren't recompiled).
    let mut snap = node("snap", "snapshot", &[]);
    snap.raw_code = Some("select 'snap raw'".into());
    snap.compiled_code = Some("select 'snap compiled'".into());
    m.nodes.insert("snapshot.p.snap".into(), snap);
    // backticks inside the SQL must not close the fence early.
    let mut ticks = node("ticks", "model", &[]);
    ticks.raw_code = Some("select `a``b` from ```t```".into());
    m.nodes.insert("model.p.ticks".into(), ticks);
    // a .py model gets the python fence tag.
    let mut py = node("py", "model", &[]);
    py.raw_code = Some("def model(dbt, session): pass".into());
    py.original_file_path = Some("models/py.py".into());
    m.nodes.insert("model.p.py".into(), py);
    dag_from(m)
}

#[test]
fn sql_section_prefers_manifest_compiled_then_raw() {
    let dag = sql_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let both = file(&out, &files["model.p.both"]);
    assert!(
        both.contains("select 'compiled'") && !both.contains("select 'raw'"),
        "manifest compiled_code wins over raw_code"
    );
    let raw = file(&out, &files["model.p.raw"]);
    assert!(raw.contains("select 'raw only'"), "raw fallback renders");
    let none = file(&out, &files["model.p.none"]);
    assert!(none.contains("## SQL\n\nNo SQL."), "labeled empty section");
}

#[test]
fn snapshot_sql_section_shows_raw_not_compiled() {
    // A snapshot's manifest `compiled_code` records the historical build —
    // docs show what the author wrote (raw), like the TUI's snapshot preview.
    let dag = sql_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let snap = file(&out, &files["snapshot.p.snap"]);
    assert!(
        snap.contains("select 'snap raw'") && !snap.contains("select 'snap compiled'"),
        "snapshots always render raw SQL"
    );
}

#[test]
fn file_origin_compiled_sql_never_reaches_a_page() {
    // `target/compiled/` files are machine-local build state — ingesting them
    // must not change the generated doc set (origin `File` is ignored; only
    // `Manifest` origin renders). Write the file, ingest, and the page still
    // shows the raw SQL.
    let mut m = empty_manifest();
    let mut mm = node("m", "model", &[]);
    mm.raw_code = Some("select 'raw m'".into());
    mm.original_file_path = Some("models/m.sql".into());
    mm.package_name = Some("p".into());
    m.nodes.insert("model.p.m".into(), mm);
    let mut dag = dag_from(m);
    let dir = std::env::temp_dir().join(format!("dbtl_docs_compiled_{}", std::process::id()));
    let sql_dir = dir.join("p").join("models");
    std::fs::create_dir_all(&sql_dir).expect("temp dir");
    std::fs::write(sql_dir.join("m.sql"), "select 'from file'").expect("write compiled");
    dag.ingest_compiled(&dir, None);
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let page = file(&out, &files["model.p.m"]);
    assert!(
        page.contains("select 'raw m'") && !page.contains("select 'from file'"),
        "target/compiled content must not leak into docs"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sql_fence_grows_past_embedded_backticks() {
    let dag = sql_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let ticks = file(&out, &files["model.p.ticks"]);
    // The SQL contains a ``` run — the fence must be 4+ backticks.
    assert!(
        ticks.contains("````sql\nselect `a``b` from ```t```\n````"),
        "the fence outruns the longest embedded backtick run: {ticks}"
    );
    let py = file(&out, &files["model.p.py"]);
    assert!(
        py.contains("```python\n"),
        "a .py model is fenced as python"
    );
}

// ---- the lineage depth + Mermaid budget -----------------------------------

/// `fanout` children each depending on `hub`, each with `grandchildren`
/// children of their own — shapes for the budget-reduction tests. Display
/// NAMES are zero-padded to `name_len` (uids stay short) so the char budget
/// can be exercised independently of the edge budget.
fn fanout_dag(fanout: usize, grandchildren: usize, name_len: usize) -> Dag {
    let mut m = empty_manifest();
    m.nodes
        .insert("model.p.hub".into(), node("hub", "model", &[]));
    let name = |i: usize| format!("c{:0width$}", i, width = name_len.max(4));
    for i in 0..fanout {
        let c_uid = format!("model.p.c{i:04}");
        m.nodes
            .insert(c_uid.clone(), node(&name(i), "model", &["model.p.hub"]));
        for g in 0..grandchildren {
            m.nodes.insert(
                format!("model.p.c{i:04}_g{g}"),
                node(&format!("{}_g{g}", name(i)), "model", &[c_uid.as_str()]),
            );
        }
    }
    dag_from(m)
}

#[test]
fn mermaid_budget_is_inclusive_at_both_bounds() {
    assert!(within_mermaid_budget(MERMAID_MAX_CHARS, MERMAID_MAX_EDGES));
    assert!(within_mermaid_budget(0, 0));
    assert!(!within_mermaid_budget(MERMAID_MAX_CHARS + 1, 0));
    assert!(!within_mermaid_budget(0, MERMAID_MAX_EDGES + 1));
}

#[test]
fn over_budget_node_diagram_reduces_depth_then_omits() {
    // hub → 250 children → 1 grandchild each: the depth-2 figure carries
    // 500 edges (>400) while depth-1 carries 250 (≤400) — one reduction.
    let dag = fanout_dag(250, 1, 4);
    let out = generate_docs(&dag, "p", "src", PackageScope::Root); // default depth 2
    let files = node_files(&dag);
    let hub = file(&out, &files["model.p.hub"]);
    assert!(
        hub.contains("Lineage within 1 hops (reduced from 2 hops"),
        "the figure fell back one hop and says so: {hub}"
    );
    assert!(
        hub.contains("```mermaid\ngraph LR\n"),
        "a figure still renders at the reduced depth"
    );
    assert!(
        !hub.contains("Diagram omitted:"),
        "a reduced figure is not an omission"
    );

    // A 450-fan hub: even the depth-1 neighbourhood exceeds the edge budget —
    // the diagram AND its legend are dropped for the omission line.
    let dag = fanout_dag(450, 0, 4);
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let hub = file(&out, &files["model.p.hub"]);
    assert!(
        hub.contains("Diagram omitted: 451 nodes / 450 edges"),
        "the omission line carries the counts: {hub}"
    );
    assert!(
        !hub.contains("```mermaid"),
        "no figure survives an omission"
    );
    assert!(
        !hub.contains("Nodes in this diagram:"),
        "no legend survives an omission"
    );
}

#[test]
fn char_budget_alone_can_force_a_reduction() {
    // Long names inflate the figure's character count without adding edges:
    // 30 children × ~700-char names → depth-2 ≈ 46k chars (>40k) on 60 edges;
    // depth-1 ≈ 24k chars — the reduction fires on the char dimension alone.
    let dag = fanout_dag(30, 1, 700);
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let files = node_files(&dag);
    let hub = file(&out, &files["model.p.hub"]);
    assert!(
        hub.contains("(reduced from 2 hops"),
        "chars alone trigger the reduction"
    );
    let (_, edges) = {
        let sg = dag.subgraph_view("model.p.hub", true, true, Some(1));
        (sg.nodes.len(), sg.edges.len())
    };
    assert!(edges <= MERMAID_MAX_EDGES, "edge budget never bound here");
}

#[test]
fn all_depth_skips_depths_beyond_the_graphs_maximum() {
    // `all` on a 500-deep chain: the full figure exceeds the edge budget, so
    // the generator walks depths down from the real maximum (not a blind
    // retry at every depth below `all`). The drawn figure is the deepest
    // depth that fits — still a figure, still labeled.
    let mut m = empty_manifest();
    let n = 500usize;
    for i in 0..n {
        let deps: Vec<String> = if i == 0 {
            vec![]
        } else {
            vec![format!("model.p.c{:04}", i - 1)]
        };
        m.nodes.insert(
            format!("model.p.c{i:04}"),
            RawNode {
                name: format!("c{i:04}"),
                resource_type: "model".into(),
                path: Some("x.sql".into()),
                depends_on: RawDependsOn {
                    nodes: deps,
                    macros: vec![],
                },
                ..Default::default()
            },
        );
    }
    let dag = dag_from(m);
    let out = generate_docs_with_options(
        &dag,
        "p",
        "src",
        PackageScope::Root,
        &DocsOptions {
            lineage_depth: LineageDepth::All,
            ..Default::default()
        },
    );
    let files = node_files(&dag);
    let last = file(&out, &files["model.p.c0499"]);
    assert!(
        last.contains("reduced from the full lineage"),
        "the all-request reduced figure says so: {}",
        &last[..last.len().min(4000)]
    );
    assert!(last.contains("```mermaid"), "a figure still renders");
}

// ---- README budget behaviour -----------------------------------------------

#[test]
fn readme_splits_when_the_whole_figure_is_over_budget_below_the_node_cap() {
    // 45-node marts group with dense intra edges (~>400 in-group edges would
    // need ~29 nodes × 15 deps — 45 nodes each depending on the next 15
    // gives ~465 intra edges) — the WHOLE figure is over the edge budget
    // while the project sits far below FULL_GRAPH_NODE_LIMIT.
    let mut m = empty_manifest();
    let n = 45usize;
    for i in 0..n {
        let deps: Vec<String> = (1..=15)
            .filter_map(|k| i.checked_sub(k))
            .map(|k| format!("model.p.m{k:04}"))
            .collect();
        m.nodes.insert(
            format!("model.p.m{i:04}"),
            RawNode {
                name: format!("m{i:04}"),
                resource_type: "model".into(),
                path: Some("marts/x.sql".into()),
                depends_on: RawDependsOn {
                    nodes: deps,
                    macros: vec![],
                },
                ..Default::default()
            },
        );
    }
    // A second, light layer so the split has something to draw.
    m.nodes
        .insert("model.p.top".into(), node("top", "model", &[]));
    let dag = dag_from(m);
    assert!(dag.len() < FULL_GRAPH_NODE_LIMIT, "below the node cap");
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let readme = file(&out, "README.md");
    assert!(
        readme.contains("over the rendering limits") && readme.contains("split by layer"),
        "the split note names the budget trigger: {readme}"
    );
    // The dense group's own figure still overflows → omission + plain table;
    // the light group draws normally.
    assert!(
        readme.contains("Diagram omitted:"),
        "the oversized group degrades"
    );
    assert!(
        readme.contains("### marts (") && readme.contains("| Name | Type | Materialization |"),
        "the group table still lists every member"
    );
    assert!(readme.contains("```mermaid"), "other groups still draw");
    // Deterministic across runs — degradation changes nothing here.
    assert_eq!(
        out,
        generate_docs(&dag, "p", "src", PackageScope::Root),
        "budget-degraded README is byte-identical"
    );
}

// ---- lint severity + baselined --------------------------------------------

#[test]
fn lint_summary_marks_baselined_findings() {
    let dag = fixture_dag();
    let mut findings = fixture_findings(&dag);
    // Freeze ONE finding via the shared Baseline type — the same mark
    // `dbtl docs` applies after loading the baseline file.
    let first = vec![findings[0].clone()];
    let baseline = dbtl_core::Baseline::from_findings(&first);
    baseline.apply(&mut findings);
    let out = generate_docs_with_findings(
        &dag,
        "jaffle_finance",
        "src",
        PackageScope::Root,
        Some(&findings),
    );
    let readme = file(&out, "README.md");
    assert!(
        readme.contains("(1 baselined)"),
        "the totals line counts the frozen finding"
    );
    assert!(
        readme.contains("— ") && readme.contains("(baselined)"),
        "the baselined node entry is marked"
    );
    // The node's own page carries the mark too.
    let uid = findings[0].node_uid.clone();
    let page = file(&out, &node_files(&dag)[&uid]);
    assert!(
        page.contains("(baselined)"),
        "the node page marks the frozen finding"
    );
}
