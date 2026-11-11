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
    // Every node page lives under the nodes dir.
    for (path, _) in &out {
        assert!(
            path == "README.md" || path.starts_with(&format!("{NODES_DIR}/")),
            "unexpected path {path}"
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
    // Pull every `models/...md` target out of the README markdown links and
    // confirm each names a file we actually generated (no broken links).
    let mut checked = 0usize;
    for chunk in readme.split("](") {
        if let Some(end) = chunk.find(')') {
            let target = &chunk[..end];
            if target.starts_with(&format!("{NODES_DIR}/")) && target.ends_with(".md") {
                assert!(
                    generated.contains(target),
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
        "## Upstream dependencies",
        "## Downstream dependencies",
        "## Lineage",
    ] {
        assert!(page.contains(section), "missing section {section}");
    }
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
    // A node whose name/description carry a `"`, a `|`, and a newline must
    // not break the Mermaid label, the Markdown table cell, or the fence.
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
                name: "ev\"il | model".into(),
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
    // The whole-project README must also survive the hostile name.
    let readme = file(&out, "README.md");
    assert!(
        readme.contains("ev\"il \\| model"),
        "README cell escaped the pipe"
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
    // name a file we actually generated — no broken navigation.
    let dag = fixture_dag();
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);
    let generated: HashSet<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
    let mut total_clicks = 0usize;
    for (page, contents) in &out {
        // README hrefs are root-relative (`models/…`); node-page hrefs are
        // bare basenames (`….md`), resolved against the page's own dir.
        let in_models = page.starts_with(&format!("{NODES_DIR}/"));
        for target in click_targets(contents) {
            total_clicks += 1;
            let resolved = if target.contains('/') {
                target.clone()
            } else if in_models {
                format!("{NODES_DIR}/{target}")
            } else {
                target.clone()
            };
            assert!(
                generated.contains(resolved.as_str()),
                "{page}: click links to missing file {target} (resolved {resolved})"
            );
        }
    }
    assert!(total_clicks > 0, "at least some click links were emitted");
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
        let in_models = page.starts_with(&format!("{NODES_DIR}/"));
        // Legend links are Markdown `](href)`; resolve like click targets.
        for chunk in contents.split("](") {
            if let Some(end) = chunk.find(')') {
                let target = &chunk[..end];
                if !target.ends_with(".md") {
                    continue;
                }
                let resolved = if target.contains('/') {
                    target.to_string()
                } else if in_models {
                    format!("{NODES_DIR}/{target}")
                } else {
                    target.to_string()
                };
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

#[test]
fn per_node_lineage_figure_equals_the_m_key_yank() {
    // For EVERY selectable node, the per-node docs Lineage figure must render
    // the SAME diagram the interactive `m` yank produces for that node at the
    // default view (upstream + downstream, unbounded depth) — both go through
    // the one shared `subgraph_mermaid_linked` core over the SAME full-closure
    // subgraph. The docs variant only adds `click` navigation lines inside the
    // fence; stripping those must leave a body byte-identical to the yank.
    // This locks the two surfaces together so they can never drift.
    //
    // Selection is BY UNIQUE_ID, not by name: the fixture has distinct nodes
    // that share a name (e.g. a model AND a source both named `pos_txn`), so a
    // name lookup would be ambiguous (and `nodes()` iteration order is not
    // deterministic). Walk uids in sorted order for a deterministic run.
    use dbtl_tui::App;
    use std::path::PathBuf;

    let dag = fixture_dag();
    let files = node_files(&dag);
    let out = generate_docs(&dag, "p", "src", PackageScope::Root);

    let mut uids: Vec<&str> = dag.nodes().keys().map(String::as_str).collect();
    uids.sort_unstable();
    let mut compared = 0usize;
    for uid in uids {
        let mut app = App::new(dag.clone(), PathBuf::from(FIXTURE));
        // Only models are selectable lineage roots — the left pane lists
        // `resource_type == "model"` only, and re-rooting only ever lands on
        // a model — so the `m` yank only exists for models. Non-models (their
        // docs page still renders) have no yank to compare against; skip them.
        if !app.select_by_unique_id(uid) {
            continue;
        }
        let yank = app
            .lineage_mermaid()
            .expect("a selectable node yields an m yank");
        let page = file(&out, &files[uid]);
        let body = strip_click_lines(&fenced_mermaid_block(page));
        assert_eq!(
            body, yank,
            "docs Lineage figure for {uid} must equal the `m` yank (minus click lines)"
        );
        compared += 1;
    }
    // Coverage: every model is a selectable root, and the invariant held for
    // each. (Guards against `select_by_unique_id` silently breaking and the
    // loop comparing nothing.)
    let model_count = dag.count_by_resource_type("model");
    assert!(model_count > 0, "fixture has models to compare");
    assert_eq!(compared, model_count, "every model's figure was compared");
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
    for rule in LintRule::ALL {
        let n = findings.iter().filter(|f| f.rule == rule).count();
        if n == 0 {
            continue;
        }
        assert!(
            readme.contains(&format!("| `{}` | {n} |", rule.name())),
            "rule {}'s count matches run_rules",
            rule.name()
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
        pos_cat.contains("## Lint violations\n\n- `direct_join_to_source`: pos_cat"),
        "the finding renders on the violating node's page"
    );
    // A node with zero findings still gets a labeled section.
    let clean = findings
        .iter()
        .map(|f| f.node_uid.clone())
        .collect::<std::collections::HashSet<_>>();
    let clean_uid = dag
        .nodes()
        .keys()
        .find(|u| !clean.contains(*u))
        .expect("the fixture has lint-clean nodes");
    let path = node_files(&dag)[clean_uid].clone();
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
