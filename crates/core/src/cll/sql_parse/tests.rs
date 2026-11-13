// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

use super::*;
use crate::cll::ColumnLineage;
use crate::manifest::{RawManifest, RawNode};
use std::collections::HashMap;

/// A `model` node: `sql` lands in `compiled_code` (a dbt-compile product —
/// parses to Exact edges), with the given declared columns and topology
/// coming from the `parent_map`/`child_map` args of [`manifest`].
fn model(uid: &str, sql: &str, cols: &[&str]) -> (String, RawNode) {
    let columns: serde_json::Map<String, serde_json::Value> = cols
        .iter()
        .map(|c| {
            (
                c.to_string(),
                serde_json::json!({"name": c, "data_type": "int"}),
            )
        })
        .collect();
    let name = uid.rsplit('.').next().unwrap_or(uid).to_string();
    let file = format!("models/{name}.sql");
    (
        uid.to_string(),
        RawNode {
            name,
            resource_type: "model".to_string(),
            compiled_code: Some(sql.to_string()),
            columns,
            original_file_path: Some(file),
            ..Default::default()
        },
    )
}

/// Same, but the SQL is *raw* (Jinja-bearing) — the resolver renders
/// `ref`/`source`/block tags itself when no compiled text exists.
fn raw_model(uid: &str, sql: &str, cols: &[&str]) -> (String, RawNode) {
    let (k, mut n) = model(uid, sql, cols);
    n.raw_code = n.compiled_code.take();
    (k, n)
}

/// A minimal manifest: `uid -> node` models plus `parent -> child` edges.
fn manifest(nodes: Vec<(String, RawNode)>, edges: &[(&str, &str)]) -> RawManifest {
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    for (p, c) in edges {
        parent_map
            .entry(c.to_string())
            .or_default()
            .push(p.to_string());
        child_map
            .entry(p.to_string())
            .or_default()
            .push(c.to_string());
    }
    RawManifest {
        unresolved_adjacent: Default::default(),
        nodes: nodes.into_iter().collect(),
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map,
        child_map,
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        metadata: Default::default(),
        docs: Default::default(),
    }
}

/// `(from.node.from.col, to.col, kind, confidence)` — the comparable view
/// of a node's in-edges.
fn edge_set(
    dag: &Dag,
    resolver: &SqlParseResolver,
    node: &str,
) -> BTreeSet<(String, String, ColumnEdgeKind, Confidence)> {
    ColumnLineage::new(dag, resolver)
        .in_edges(node)
        .iter()
        .map(|e| {
            (
                format!("{}.{}", e.from.node, e.from.column),
                e.to.column.clone(),
                e.kind,
                e.confidence,
            )
        })
        .collect()
}

fn e(from: &str, to: &str, kind: ColumnEdgeKind) -> (String, String, ColumnEdgeKind, Confidence) {
    (from.to_string(), to.to_string(), kind, Confidence::Exact)
}

fn resolver() -> SqlParseResolver {
    SqlParseResolver::new(None, SqlDialect::Generic)
}

// -- 1. projections: rename / passthrough / transform ---------------------

#[test]
fn projection_kinds() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "status"]),
            model(
                "model.t.m",
                "select u.id as order_id, u.status, upper(u.status) as big \
                 from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "order_id", ColumnEdgeKind::Rename),
        e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
        e("model.t.up.status", "big", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- 2. qualified refs bind to the aliased parent only --------------------

#[test]
fn qualified_refs_use_the_table_alias() {
    // Both parents declare `id`/`k`: `x.id` must bind to `a` alone (v0
    // name-match would have bound `b` too). `k` in the join is FOJ.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "k"]),
            model("model.t.b", "select 1", &["id", "k"]),
            model(
                "model.t.m",
                "select x.id as aid, y.id as bid \
                 from \"a\" x join \"b\" y on x.k = y.k",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "aid", ColumnEdgeKind::Rename),
        e("model.t.b.id", "bid", ColumnEdgeKind::Rename),
        e("model.t.a.k", "aid", ColumnEdgeKind::FilterOrJoin),
        e("model.t.a.k", "bid", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.k", "aid", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.k", "bid", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- 3. wildcards ---------------------------------------------------------

#[test]
fn bare_wildcard_expands_over_declared_columns() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "status"]),
            model("model.t.m", "select * from \"up\"", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn qualified_wildcard_only_expands_its_own_table() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["x"]),
            model("model.t.b", "select 1", &["y"]),
            model(
                "model.t.m",
                "select a.*, b.y as by from \"a\" a join \"b\" b on a.x = b.y",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.x", "x", ColumnEdgeKind::Passthrough),
        e("model.t.b.y", "by", ColumnEdgeKind::Rename),
        // join keys: a.x already a value source → skipped; b.y same.
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn unknown_wildcard_invents_no_names() {
    // The parent has NO declared columns and no analyzable SQL: `*` must
    // not fabricate output columns (the declared-column probe still lands
    // the model's own schema.yml list — empty here).
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "", &[]),
            model("model.t.m", "select * from \"up\"", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    assert!(edge_set(&dag, &r, "model.t.m").is_empty());
    assert!(r.columns(&dag, "model.t.m").is_empty());
}

// -- 4. CTEs --------------------------------------------------------------

#[test]
fn cte_columns_flow_to_the_outer_select() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model(
                "model.t.m",
                "with c as (select u.id, u.id * 2 as dbl from \"up\" u) \
                 select c.id as cid, c.dbl from c",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "cid", ColumnEdgeKind::Rename),
        e("model.t.up.id", "dbl", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- 5. derived subqueries --------------------------------------------------

#[test]
fn derived_subquery_binds_through_its_alias() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model(
                "model.t.m",
                "select s.id as x from (select u.id from \"up\" u) s",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [e("model.t.up.id", "x", ColumnEdgeKind::Rename)]
        .into_iter()
        .collect();
    assert_eq!(got, want);
}

// -- 6. correlated subqueries ----------------------------------------------

#[test]
fn correlated_subquery_sees_the_outer_scope() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id"]),
            model("model.t.b", "select 1", &["k", "v"]),
            model(
                "model.t.m",
                "select a.id, (select b.v from \"b\" b where b.k = a.id) as v \
                 from \"a\" a",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    // `v`'s sources: the projected `b.v`, the predicate's `b.k`, and the
    // correlated outer `a.id` — all folded into the value's sources.
    let want: BTreeSet<_> = [
        e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.a.id", "v", ColumnEdgeKind::Transform),
        e("model.t.b.k", "v", ColumnEdgeKind::Transform),
        e("model.t.b.v", "v", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- 7. join / where-only references are FilterOrJoin -----------------------

#[test]
fn row_shaping_refs_fan_out_to_every_output() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "flag"]),
            model("model.t.b", "select 1", &["id", "cc"]),
            model(
                "model.t.m",
                "select a.id as aid from \"a\" a join \"b\" b on a.id = b.id \
                 where a.flag and b.cc = 'JP'",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "aid", ColumnEdgeKind::Rename),
        e("model.t.a.flag", "aid", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.id", "aid", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.cc", "aid", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- 8. set operations --------------------------------------------------------

#[test]
fn union_merges_positional_sources() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id"]),
            model("model.t.b", "select 1", &["id"]),
            model(
                "model.t.m",
                "select id from \"a\" union all select id from \"b\"",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.b.id", "id", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- unqualified ambiguity keeps every candidate ----------------------------

#[test]
fn ambiguous_unqualified_keeps_all_candidates() {
    // Both parents own `id`; bare `id` in the projection stays a
    // documented ambiguity → BOTH candidate edges, never a silent pick.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "k"]),
            model("model.t.b", "select 1", &["id", "k"]),
            model(
                "model.t.m",
                "select id from \"a\" a join \"b\" b on a.k = b.k",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.b.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.a.k", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.k", "id", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- fallback ----------------------------------------------------------------

#[test]
fn unparsable_sql_falls_back_and_is_counted() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model("model.t.m", "select from where (", &["id"]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let edges = r.resolve(&dag, "model.t.m");
    assert!(r.is_fallback("model.t.m"), "parse failure must fall back");
    assert_eq!(r.fallback_count(), 1);
    assert_eq!(r.fallback_nodes(), vec!["model.t.m".to_string()]);
    // The name-match resolver still answers: `id` is a declared column on
    // both sides → the approximation's Passthrough edge survives.
    assert!(edges.iter().all(|e| e.confidence == Confidence::NameMatch));
}

#[test]
fn missing_sql_uses_the_fallback_without_counting() {
    // A seed (resource_type `seed`, no SQL): same answer as v0 — [] edges —
    // but NOT a "fallback event" (nothing was ever parseable).
    let (k, mut n) = model("model.t.up", "select 1", &["id"]);
    n.resource_type = "seed".to_string();
    n.compiled_code = None;
    let dag = Dag::build(&manifest(vec![(k, n)], &[]));
    let r = resolver();
    assert!(r.resolve(&dag, "model.t.up").is_empty());
    assert!(!r.is_fallback("model.t.up"));
    assert_eq!(r.fallback_count(), 0);
}

// -- confidence ---------------------------------------------------------------

#[test]
fn parsed_edges_are_exact_raw_or_compiled() {
    // raw_code (Jinja rendered by the resolver) and compiled_code both
    // yield Confidence::Exact once they parse.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            raw_model("model.t.m", "select u.id from {{ ref('up') }} u", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let edges = r.resolve(&dag, "model.t.m");
    assert!(!r.is_fallback("model.t.m"));
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].confidence, Confidence::Exact);
    assert_eq!(edges[0].from.node, "model.t.up");
}

#[test]
fn stale_compiled_sql_marks_edges_stale() {
    let m = manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model("model.t.m", "select u.id from \"up\" u", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    );
    let mut dag = Dag::build(&m);
    dag.set_compiled_for_test("model.t.m", "select u.id from \"up\" u", true);
    let r = resolver();
    let edges = r.resolve(&dag, "model.t.m");
    assert!(edges.iter().all(|e| e.confidence == Confidence::Stale));
}

// -- Jinja rendering -----------------------------------------------------------

#[test]
fn jinja_refs_sources_and_blocks_render() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "flag"]),
            raw_model(
                "model.t.m",
                "{{ config(materialized='view') }}\n\
                 select u.id\n\
                 from {{ ref('up') }} u\n\
                 {% if true %}\n\
                 where u.flag\n\
                 {% endif %}\n\
                 {# {{ ref('ghost') }} #}",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.flag", "id", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn this_relation_produces_no_parent_edges() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            raw_model(
                "model.t.m",
                "select t.id from {{ this }} t, {{ ref('up') }} u where t.id = u.id",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let edges = r.resolve(&dag, "model.t.m");
    // `u.id` only surfaces as the join's FilterOrJoin fan — never a
    // self-edge, and `{{ this }}` never resolves to a parent.
    assert!(!r.is_fallback("model.t.m"));
    assert!(edges
        .iter()
        .all(|e| e.from.node == "model.t.up" && e.from.column == "id"));
    assert!(edges.iter().all(|e| e.kind == ColumnEdgeKind::FilterOrJoin));
}

// -- Jinja source map ----------------------------------------------------------
//
// `render_jinja_mapped` pairs the rendered text with a byte map back to
// raw — the lint findings' position backbone. Copied regions map 1:1; a
// rendered `{{ }}`/`{% %}` maps to its whole tag.

#[test]
fn render_map_is_byte_identical_to_render_jinja() {
    let raw = "-- a\r\n{{ config(materialized='table') }}\nselect {% if t %}a,{% endif %}* from {{ ref('x') }}\n";
    assert_eq!(
        render_jinja_mapped(raw, JinjaMark::Plain, SqlDialect::Generic).text,
        render_jinja_as(raw, JinjaMark::Plain, SqlDialect::Generic)
    );
    assert_eq!(
        render_jinja_mapped(raw, JinjaMark::Marked, SqlDialect::Generic).text,
        render_jinja_as(raw, JinjaMark::Marked, SqlDialect::Generic)
    );
}

#[test]
fn render_map_maps_copies_11_and_tags_to_the_whole_tag() {
    let raw = "select a\nfrom {{ ref('up') }} u\n";
    let rendered = render_jinja_mapped(raw, JinjaMark::Plain, SqlDialect::Generic);
    // A copied stretch maps byte-for-byte: `select a` → itself.
    let s = raw.find("select a").unwrap();
    assert_eq!(
        rendered.source_range(s..s + 8),
        Some(s..s + 8),
        "a verbatim copy maps 1:1"
    );
    // The rendered ref name sits inside the tag's render — every byte of
    // it maps back to the whole `{{ ref('up') }}` tag.
    let tag = raw.find("{{ ref('up') }}").unwrap();
    let r = rendered.text.find("\"up\"").unwrap();
    assert_eq!(
        rendered.source_range(r..r + 4),
        Some(tag..tag + "{{ ref('up') }}".len()),
        "a span inside a rendered tag underlines the tag, not the render"
    );
    // A range straddling copy + tag unions their sources (contiguous).
    let f = rendered.text.find("from").unwrap();
    assert_eq!(
        rendered.source_range(f..rendered.text.len()),
        Some(raw.find("from").unwrap()..raw.len()),
        "a span over copy+tag covers the contiguous union"
    );
    // Nothing past the text maps.
    assert_eq!(rendered.source_range(0..rendered.text.len() + 1), None);
    assert_eq!(rendered.source_range(5..5), None);
}

#[test]
fn only_branch_commas_tells_an_arm_split_from_a_written_comma() {
    use sqlparser::tokenizer::{Location, Span};
    // `{% else %}` between FROM items emits `,` — the render's branch
    // union (the arms never reach one render), not a written comma join.
    let raw = "select *\nfrom {% if x %}a.t{% else %}b.u{% endif %}";
    let rendered = render_jinja_mapped(raw, JinjaMark::Plain, SqlDialect::Generic);
    assert_eq!(rendered.text, "select *\nfrom  a.t,b.u ");
    let line2 = rendered.text.find('\n').unwrap() + 1;
    let loc = |off: usize| Location::new(2, (off - line2 + 1) as u64);
    let comma = rendered.text.find(',').unwrap();
    assert!(rendered.only_branch_commas(Span::new(loc(comma), loc(comma + 1))));
    // A written `,` fails the check even when an arm split follows.
    let raw = "select *\nfrom a.t, {% if x %}b.u{% else %}c.v{% endif %}";
    let rendered = render_jinja_mapped(raw, JinjaMark::Plain, SqlDialect::Generic);
    assert_eq!(rendered.text, "select *\nfrom a.t,  b.u,c.v ");
    let line2 = rendered.text.find('\n').unwrap() + 1;
    let loc = |off: usize| Location::new(2, (off - line2 + 1) as u64);
    let comma = rendered.text.find(',').unwrap();
    assert!(!rendered.only_branch_commas(Span::new(loc(comma), loc(comma + 1))));
    // …and a gap holding no comma at all is never a branch separator.
    let a_t = rendered.text.find("a.t").unwrap();
    assert!(!rendered.only_branch_commas(Span::new(loc(a_t), loc(a_t + 3))));
}

#[test]
fn opaque_jinja_expressions_carry_their_string_literals() {
    // `{{ join_customers('customers', …) }}` renders `NULL` but its args
    // still name the CTE the macro joins — `unused_cte` reads them.
    let rendered = render_jinja_mapped(
        "select * from orders {{ join_customers('customers', \"orders.id\") }}",
        JinjaMark::Plain,
        SqlDialect::Generic,
    );
    assert_eq!(rendered.text, "select * from orders  NULL ");
    assert_eq!(
        rendered.jinja_literals,
        vec!["customers".to_string(), "orders.id".to_string()]
    );
    // Classified calls and config emit none — `ref`/`source` resolve to
    // relation names, `config` to nothing.
    let rendered = render_jinja_mapped(
        "select {{ ref('x') }}, {{ source('a','b') }} {{ config(alias='y') }}",
        JinjaMark::Plain,
        SqlDialect::Generic,
    );
    assert!(rendered.jinja_literals.is_empty());
    // An unterminated literal is dropped — it was never a string.
    let rendered = render_jinja_mapped(
        "select {{ f('unclosed) }}",
        JinjaMark::Plain,
        SqlDialect::Generic,
    );
    assert_eq!(rendered.jinja_literals, Vec::<String>::new());
}

#[test]
fn plain_positions_extract_wildcard_join_and_column_spans() {
    let raw = "select a, o.*\nfrom {{ ref('o') }} o\njoin b.u\nwhere o.id = b.id";
    let pos = plain_positions(raw, SqlDialect::Generic).unwrap();
    // `o.*` — the qualified wildcard underlines itself.
    assert_eq!(
        pos.wildcards,
        vec![raw.find("o.*").unwrap()..raw.find("o.*").unwrap() + 3]
    );
    // The conditionless join underlines `b.u` — not the `join` keyword.
    assert_eq!(
        pos.cartesian,
        vec![raw.find("b.u").unwrap()..raw.find("b.u").unwrap() + 3]
    );
    // Column refs: `a` bare, plus each compound identifier's tail — the
    // Jinja-rendered relation and the `o`/`b` qualifiers contribute none.
    // (Scan order: the WHERE selection walks ahead of the projection.)
    assert_eq!(
        pos.columns,
        vec![
            (
                "id".to_string(),
                raw.find("id").unwrap()..raw.find("id").unwrap() + 2
            ),
            (
                "id".to_string(),
                raw.rfind("id").unwrap()..raw.rfind("id").unwrap() + 2
            ),
            ("a".to_string(), 7..8),
        ]
    );
    // An unparsable text is `None` — never a position to guess.
    assert!(plain_positions("select from where or", SqlDialect::Generic).is_none());
}

#[test]
fn marked_relations_carry_name_spans() {
    let raw = "select *\nfrom analytics.orders\njoin {{ ref('u') }} u on 1 = 1";
    let rel = marked_relations(raw, SqlDialect::Generic).unwrap();
    assert_eq!(rel.names, vec!["analytics.orders".to_string()]);
    assert_eq!(rel.spans.len(), 1);
    assert_eq!(
        rel.spans[0].1,
        raw.find("analytics.orders").unwrap()
            ..raw.find("analytics.orders").unwrap() + "analytics.orders".len(),
        "the written name underlines itself"
    );
    // An unparsable text is `None` — never a position to guess.
    assert!(marked_relations("select from where or", SqlDialect::Generic).is_none());
}

#[test]
fn marked_relations_skips_files_metadata_and_correlated_names() {
    // `dual` is a row constructor; `information_schema` at ANY depth —
    // including inside one quoted identifier's dots — is a metadata
    // read; a `'…'`-literal factor is a file read, not a relation; and
    // `t.arr`/`t.sub` headed by a bound FROM name reads that relation's
    // column. None land in `names`; the written `raw.orders` still does.
    for sql in [
        "select 1 from dual",
        "select * from db.information_schema.columns",
        "select * from `proj.us.INFORMATION_SCHEMA.JOBS`",
        "select * from 'data/x.parquet'",
    ] {
        assert!(
            marked_relations(sql, SqlDialect::Generic)
                .unwrap()
                .names
                .is_empty(),
            "{sql} names no relation"
        );
    }
    // Correlation at the same FROM level — `t` binds first, `t.arr`
    // reads its array column.
    let rel = marked_relations(
        "select t.id, x from raw.orders t, t.arr as x",
        SqlDialect::Generic,
    )
    .unwrap();
    assert_eq!(rel.names, vec!["raw.orders".to_string()]);
    // …and inside a nested query — the subquery's `t.sub` correlates
    // the outer `t`.
    let rel = marked_relations(
        "select * from raw.orders t where exists (select 1 from t.sub)",
        SqlDialect::Generic,
    )
    .unwrap();
    assert_eq!(rel.names, vec!["raw.orders".to_string()]);
}

#[test]
fn sql_facts_skips_cross_correlated_and_file_factors() {
    // The judged side sees the same exclusions the marked scan does: an
    // explicit `CROSS JOIN`, a factor headed by a bound name (`t.arr`),
    // a `'file'` literal, and the one-row `dual` are never products —
    // `b.u` and the qualified `sys.dual` still are.
    let dag = Dag::build(&manifest(
        vec![
            raw_model("model.t.cross", "select * from a.t cross join b.u", &[]),
            raw_model("model.t.corr", "select t.id from a.t t, t.arr", &[]),
            raw_model("model.t.file", "select * from a.t, 'f.csv'", &[]),
            raw_model("model.t.dual", "select * from a.t, dual", &[]),
            raw_model("model.t.comma", "select * from a.t, b.u", &[]),
            raw_model("model.t.qdual", "select * from a.t, sys.dual", &[]),
        ],
        &[],
    ));
    let r = resolver();
    for uid in [
        "model.t.cross",
        "model.t.corr",
        "model.t.file",
        "model.t.dual",
    ] {
        let f = r.sql_facts(&dag, uid).unwrap();
        assert!(
            f.cartesian.is_empty() && f.cartesian_count == 0,
            "{uid} carries no product: {:?}",
            f.cartesian
        );
    }
    let f = r.sql_facts(&dag, "model.t.comma").unwrap();
    assert_eq!(f.cartesian_count, 1);
    assert_eq!(f.cartesian, vec!["b.u".to_string()]);
    // A qualified `sys.dual` names a real relation — still a product.
    let f = r.sql_facts(&dag, "model.t.qdual").unwrap();
    assert_eq!(f.cartesian_count, 1);
    assert_eq!(f.cartesian, vec!["sys.dual".to_string()]);
}

#[test]
fn derived_output_names_dup_only_on_the_postgres_family() {
    // `sum(a)`/`sum(b)` → `sum`/`sum`, `a::int`/`a::text` → `a`/`a`:
    // Postgres + Redshift really assign those names (a repeat fails on
    // the warehouse); every other dialect invents a name no second
    // expression shares. Aliased repeats flag under either.
    let dag = Dag::build(&manifest(
        vec![raw_model(
            "model.t.m",
            "select sum(a), sum(b), a::int, a::text, x as c, y as c from t",
            &[],
        )],
        &[],
    ));
    let generic = resolver();
    assert_eq!(
        generic
            .sql_facts(&dag, "model.t.m")
            .unwrap()
            .duplicate_outputs,
        vec![("c".to_string(), 2)],
        "only the written alias collides under Generic"
    );
    for dialect in [SqlDialect::Postgres, SqlDialect::Redshift] {
        let r = SqlParseResolver::new(None, dialect);
        assert_eq!(
            r.sql_facts(&dag, "model.t.m").unwrap().duplicate_outputs,
            vec![
                ("sum".to_string(), 2),
                ("a".to_string(), 2),
                ("c".to_string(), 2),
            ],
            "{dialect:?}: the assigned names collide too"
        );
    }
}

#[test]
fn statementless_renders_analyze_to_empty_facts() {
    // A render producing no statements — comments only, or an
    // output-less `{{ config() }}` — is parsed-and-empty, not
    // unparsable: `sql_facts` returns Some with nothing inside, and the
    // model never counts as a fallback.
    let dag = Dag::build(&manifest(
        vec![
            raw_model("model.t.cmt", "-- just a comment\n", &[]),
            raw_model("model.t.cfg", "{{ config(materialized='view') }}\n", &[]),
            raw_model("model.t.bad", "select from where or", &[]),
        ],
        &[],
    ));
    let r = resolver();
    for uid in ["model.t.cmt", "model.t.cfg"] {
        let f = r
            .sql_facts(&dag, uid)
            .unwrap_or_else(|| panic!("{uid} is judged, not unparsable"));
        assert!(
            f.cartesian.is_empty()
                && f.duplicate_outputs.is_empty()
                && f.unused_ctes.is_empty()
                && !f.unbounded_wildcard
                && !f.unlimited_order_by,
            "{uid} is judged empty"
        );
        assert!(!r.is_fallback(uid), "{uid} never fell back");
    }
    assert!(
        r.sql_facts(&dag, "model.t.bad").is_none(),
        "real SQL that won't parse still yields None"
    );
    // …and the column path marks it a name-match fallback, where the
    // statementless models stayed analyzed-and-empty.
    let _ = r.columns(&dag, "model.t.bad");
    assert!(r.is_fallback("model.t.bad"));
    let _ = r.columns(&dag, "model.t.cmt");
    assert!(!r.is_fallback("model.t.cmt"));
}

// -- dialect ---------------------------------------------------------------

#[test]
fn dialect_names_parse() {
    for (s, d) in [
        ("auto", SqlDialect::Generic),
        ("generic", SqlDialect::Generic),
        ("postgres", SqlDialect::Postgres),
        ("bigquery", SqlDialect::BigQuery),
        ("BQ", SqlDialect::BigQuery),
        ("snowflake", SqlDialect::Snowflake),
        ("sqlite3", SqlDialect::Sqlite),
        ("redshift", SqlDialect::Redshift),
    ] {
        assert_eq!(SqlDialect::parse_name(s), Some(d), "{s}");
    }
    assert_eq!(SqlDialect::parse_name("not-a-dialect"), None);
}

#[test]
fn dialect_follows_the_adapter_metadata() {
    let mut m = manifest(vec![model("model.t.m", "select 1", &[])], &[]);
    m.metadata.adapter_type = Some("snowflake".to_string());
    let dag = Dag::build(&m);
    assert_eq!(SqlDialect::from_dag(&dag), SqlDialect::Snowflake);
    // No metadata → Generic (source mode never guesses).
    let m2 = manifest(vec![model("model.t.m", "select 1", &[])], &[]);
    assert_eq!(SqlDialect::from_dag(&Dag::build(&m2)), SqlDialect::Generic);
}

#[test]
fn for_diff_baseline_shares_the_current_dialect() {
    // The one-dialect-per-diff rule: the BASELINE side parses under the
    // CURRENT side's dialect, or `normalized_expr` differences between
    // dialects fabricate `expression changed` rows on identical SQL.
    let dag_with = |adapter: Option<&str>| {
        let mut m = manifest(vec![model("model.t.m", "select 1", &[])], &[]);
        m.metadata.adapter_type = adapter.map(str::to_string);
        Dag::build(&m)
    };
    let snow = dag_with(Some("snowflake"));
    let bq = dag_with(Some("bigquery"));
    let no_adapter = dag_with(None);
    let unknown_adapter = dag_with(Some("not_a_dialect"));

    // Current's recognized adapter wins — even when the baseline names
    // a DIFFERENT recognized one (snowflake current, bigquery baseline
    // → both sides parse as snowflake).
    assert_eq!(
        SqlDialect::for_diff_baseline(&snow, &bq),
        SqlDialect::Snowflake
    );
    // Current can't name one (source-mode load / unrecognized adapter)
    // → the BASELINE's recognized adapter fills in.
    for current in [&no_adapter, &unknown_adapter] {
        assert_eq!(
            SqlDialect::for_diff_baseline(current, &bq),
            SqlDialect::BigQuery
        );
        assert_eq!(
            SqlDialect::for_diff_baseline(current, &snow),
            SqlDialect::Snowflake
        );
    }
    // Neither side names a known dialect → Generic, deterministically.
    for current in [&no_adapter, &unknown_adapter] {
        assert_eq!(
            SqlDialect::for_diff_baseline(current, &no_adapter),
            SqlDialect::Generic
        );
        assert_eq!(
            SqlDialect::for_diff_baseline(current, &unknown_adapter),
            SqlDialect::Generic
        );
    }
}

// -- determinism --------------------------------------------------------------

#[test]
fn repeated_resolution_is_deterministic() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "k"]),
            model("model.t.b", "select 1", &["id", "k"]),
            model(
                "model.t.m",
                "select a.id as x, id as amb, count(*) as n \
                 from \"a\" a join \"b\" b on a.k = b.k",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let (ra, rb) = (resolver(), resolver());
    assert_eq!(ra.resolve(&dag, "model.t.m"), rb.resolve(&dag, "model.t.m"));
}

// -- EXCEPT/INTERSECT: the right arm shapes rows, never values ------------

#[test]
fn except_right_arm_shapes_rows_not_values() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "x"]),
            model("model.t.b", "select 1", &["id", "y"]),
            model(
                "model.t.m",
                "select id, x from \"a\" except select id, y from \"b\"",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.a.x", "x", ColumnEdgeKind::Passthrough),
        // `b`'s columns only decide which `a` rows survive — FilterOrJoin
        // on every output, never a Passthrough/Rename value edge.
        e("model.t.b.id", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.id", "x", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.y", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.y", "x", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn intersect_right_arm_shapes_rows_not_values() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id"]),
            model("model.t.b", "select 1", &["id"]),
            model(
                "model.t.m",
                "select id from \"a\" intersect select id from \"b\"",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.b.id", "id", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    // And never a value edge from the right arm.
    assert!(got
        .iter()
        .all(|e| e.0 != "model.t.b.id" || e.2 == ColumnEdgeKind::FilterOrJoin));
}

#[test]
fn union_by_name_pairs_on_names_not_positions() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "x"]),
            model("model.t.b", "select 1", &["x", "id"]),
            model(
                "model.t.m",
                "select id, x from \"a\" \
                 union all by name select x, id from \"b\"",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.b.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.a.x", "x", ColumnEdgeKind::Passthrough),
        e("model.t.b.x", "x", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- wildcard options -----------------------------------------------------

#[test]
fn wildcard_except_and_exclude_drop_columns() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "secret", "status"]),
            model(
                "model.t.m_except",
                "select * except (secret) from \"up\"",
                &[],
            ),
            model(
                "model.t.m_exclude",
                "select * exclude (secret) from \"up\"",
                &[],
            ),
        ],
        &[
            ("model.t.up", "model.t.m_except"),
            ("model.t.up", "model.t.m_exclude"),
        ],
    ));
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    let r = resolver();
    for m in ["model.t.m_except", "model.t.m_exclude"] {
        assert_eq!(edge_set(&dag, &r, m), want, "{m}");
    }
}

#[test]
fn wildcard_rename_and_replace_reshape_columns() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "status"]),
            model(
                "model.t.m_rename",
                "select * rename status as state from \"up\"",
                &[],
            ),
            model(
                "model.t.m_replace",
                "select * replace (upper(status) as status) from \"up\"",
                &[],
            ),
        ],
        &[
            ("model.t.up", "model.t.m_rename"),
            ("model.t.up", "model.t.m_replace"),
        ],
    ));
    let r = resolver();
    // `RENAME` keeps the column raw — a renamed passthrough.
    let want_rename: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.status", "state", ColumnEdgeKind::Rename),
    ]
    .into_iter()
    .collect();
    assert_eq!(edge_set(&dag, &r, "model.t.m_rename"), want_rename);
    // `REPLACE` recomputes the value — a Transform on the same name.
    let want_replace: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.status", "status", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(edge_set(&dag, &r, "model.t.m_replace"), want_replace);
}

#[test]
fn wildcard_ilike_filters_by_name() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "secret", "status"]),
            model("model.t.m", "select * ilike 's%' from \"up\"", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.secret", "secret", ColumnEdgeKind::Passthrough),
        e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- CTE column-alias lists and sequencing ---------------------------------

#[test]
fn cte_column_alias_list_renames_outputs() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "k"]),
            model(
                "model.t.m",
                "with c(a, b) as (select u.id, u.k * 2 from \"up\" u) \
                 select c.a, c.b from c",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "a", ColumnEdgeKind::Rename),
        e("model.t.up.k", "b", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn sequential_and_shadowing_ctes() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model("model.t.up2", "select 1", &["k"]),
            model(
                "model.t.seq",
                "with a as (select u.id from \"up\" u), \
                 b as (select a.id from a) \
                 select b.id from b",
                &[],
            ),
            // The second `x` shadows the first — SQL binds the latest.
            model(
                "model.t.sh",
                "with x as (select u.id as v from \"up\" u), \
                 x as (select w.k as v from \"up2\" w) \
                 select x.v from x",
                &[],
            ),
        ],
        &[
            ("model.t.up", "model.t.seq"),
            ("model.t.up", "model.t.sh"),
            ("model.t.up2", "model.t.sh"),
        ],
    ));
    let r = resolver();
    let want_seq: BTreeSet<_> = [e("model.t.up.id", "id", ColumnEdgeKind::Passthrough)]
        .into_iter()
        .collect();
    assert_eq!(edge_set(&dag, &r, "model.t.seq"), want_seq);
    let want_sh: BTreeSet<_> = [e("model.t.up2.k", "v", ColumnEdgeKind::Rename)]
        .into_iter()
        .collect();
    assert_eq!(edge_set(&dag, &r, "model.t.sh"), want_sh);
}

// -- USING ------------------------------------------------------------------

#[test]
fn using_binds_both_sides_of_the_join() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "k"]),
            model("model.t.b", "select 1", &["id", "k"]),
            model(
                "model.t.m",
                "select a.id as aid from \"a\" a join \"b\" b using (k)",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "aid", ColumnEdgeKind::Rename),
        e("model.t.a.k", "aid", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.k", "aid", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- local names: lambda params and named-argument names -------------------

#[test]
fn lambda_params_never_read_columns() {
    // DuckDB parses arrow lambdas (`x -> expr`): `x` inside the lambda is
    // a LOCAL name — even when a same-named column exists upstream.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["arr", "x", "thresh"]),
            model(
                "model.t.m",
                "select list_filter(u.arr, x -> x > 0) as f, \
                 list_filter(u.arr, x -> x > u.thresh) as g \
                 from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = SqlParseResolver::new(None, SqlDialect::DuckDb);
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.arr", "f", ColumnEdgeKind::Transform),
        e("model.t.up.arr", "g", ColumnEdgeKind::Transform),
        e("model.t.up.thresh", "g", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    // The lambda param `x` drew NO edge to the real `x` column.
    assert!(got.iter().all(|e| e.0 != "model.t.up.x"));
}

#[test]
fn expr_named_args_drop_the_name() {
    // Postgres parses `f(x => v)` with the name as an arbitrary expr —
    // `ExprNamed`. `x` is the argument's NAME, not a column read.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["x", "v"]),
            model("model.t.m", "select f(x => u.v) as s from \"up\" u", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = SqlParseResolver::new(None, SqlDialect::Postgres);
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [e("model.t.up.v", "s", ColumnEdgeKind::Transform)]
        .into_iter()
        .collect();
    assert_eq!(got, want);
    assert!(got.iter().all(|e| e.0 != "model.t.up.x"));

    // The `name := value` spelling (DuckDB) lands in `FunctionArg::Named`
    // — same rule.
    let dag2 = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["x", "v"]),
            model("model.t.m", "select f(x := u.v) as s from \"up\" u", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r2 = SqlParseResolver::new(None, SqlDialect::DuckDb);
    let got2 = edge_set(&dag2, &r2, "model.t.m");
    assert_eq!(got2, want);
}

// -- nested field access ----------------------------------------------------

#[test]
fn deep_field_access_reads_the_struct_column() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["cfg", "id"]),
            model(
                "model.t.m",
                "select u.cfg.inner.deep as d, cfg.sub as s, \
                 sch.tbl.no_such as n from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    // `u.cfg.inner.deep`: `u` is bound → its column `cfg`.
    // `cfg.sub`: unqualified struct access → column `cfg`.
    // `sch.tbl.no_such`: `sch` is neither a binding nor a column →
    // nothing (catalog-qualified struct access can't be told apart —
    // documented).
    let want: BTreeSet<_> = [
        e("model.t.up.cfg", "d", ColumnEdgeKind::Rename),
        e("model.t.up.cfg", "s", ColumnEdgeKind::Rename),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- query-level ORDER BY ---------------------------------------------------

#[test]
fn order_by_sees_unprojected_columns_and_aliases() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "ts", "f"]),
            model(
                "model.t.m",
                "select u.id as iid from \"up\" u order by u.ts, iid",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    // `u.ts` was never projected — the ORDER BY probe still resolves it
    // through the body's scope (this is the regression fix).
    let want: BTreeSet<_> = [
        e("model.t.up.id", "iid", ColumnEdgeKind::Rename),
        e("model.t.up.ts", "iid", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    // `iid` in ORDER BY binds the output alias — but `up.id` already
    // has a value edge, so it adds nothing.
    assert!(got.iter().all(|e| e.0 != "model.t.up.f"));
}

// -- Jinja rendering: literal shielding and malformed input ------------------

#[test]
fn jinja_markers_inside_literals_and_comments_do_not_pair() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "s", "t"]),
            raw_model(
                "model.t.m",
                "select u.id from {{ ref('up') }} u \
                 where u.s = '{{ not' and u.t = 'jinja }}' \
                 -- {{ also_not }}",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    // A `{{` inside a literal pairs with the NEXT `}}` — Jinja doesn't
    // know SQL literals (dbt itself syntax-errors on this input) — so the
    // render eats the stretch through `… 'jinja }}` and `u.t` contributes
    // no edge. What still parses must NOT fall back, and the marker
    // inside the `--` comment never pairs either way.
    assert!(!r.is_fallback("model.t.m"));
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.s", "id", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn malformed_unterminated_jinja_falls_back_not_truncates() {
    // An unterminated `{{` keeps the tail verbatim → the parse fails →
    // per-model fallback. (Silently eating it would fabricate a shorter
    // query with wrong edges.)
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            raw_model(
                "model.t.m",
                "select u.id from {{ ref('up') }} u where u.k = {{ unclosed",
                &["id"],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let edges = r.resolve(&dag, "model.t.m");
    assert!(r.is_fallback("model.t.m"));
    assert_eq!(r.fallback_nodes(), vec!["model.t.m".to_string()]);
    assert!(edges.iter().all(|e| e.confidence == Confidence::NameMatch));
}

// -- Jinja rendering: dialect-aware literals and position-shaped `{{ }}` ------
//
// The shapes the render rewrite targets: Jinja inside SQL string
// literals, the dialect's `\'`/`#`/prefix rules, tags glued into
// identifiers, `NULL`-placeholder alias collisions, select-item
// continuations, relation-position tags, and `ref()` names standing
// next to same-named CTEs.

#[test]
fn jinja_inside_a_string_literal_renders() {
    // `{{ var('x', 'a') }}` inside `'…'` — Jinja substitutes inside
    // strings, so the literal becomes `' NULL '` — not two literals, and
    // the tag's interior quotes never confuse the close-scan.
    assert_eq!(
        render_jinja(
            "select '{{ var('x', 'a') }}' as tag, \
             cast('{{ var('last_run') }}' as timestamp) as t",
            SqlDialect::Generic,
        ),
        "select ' NULL ' as tag, cast(' NULL ' as timestamp) as t"
    );
    // `ref`/`source` inside a literal keep their name — the literal's own
    // quote delimits it (`'"x"'` would mangle the text).
    assert_eq!(
        render_jinja("select '{{ ref('x') }}' as t", SqlDialect::Generic),
        "select 'x' as t"
    );
    // Jinja inside a quoted identifier stays a single identifier — the
    // fragment glues where a `'`/`$$` literal gets `NULL` instead.
    assert_eq!(
        render_jinja("select \"{{ c }}_sum\" as s", SqlDialect::Generic),
        "select \"__dbtl_jinja_0___sum\" as s"
    );
    // An unterminated-in-literal tag leaves the bytes verbatim — the
    // render degrades to the raw text, never a shorter fabrication.
    let raw = "select '{{ f('unclosed) }}' as t";
    assert_eq!(render_jinja(raw, SqlDialect::Generic), raw);
}

#[test]
fn dialect_decides_backslash_escapes_and_hash_comments() {
    // `\'` escapes ONLY where the dialect says so: Snowflake keeps the
    // literal open (the tag lands inside it), Generic closes the literal
    // at `\'` — matching what sqlparser itself does with the same text.
    let raw = "select 'it\\'s {{ var('x') }}' as s";
    assert_eq!(
        render_jinja(raw, SqlDialect::Snowflake),
        "select 'it\\'s  NULL ' as s"
    );
    // Under Generic the `'` after `\` really closes the literal — the
    // `{{ }}` sits at top level between `s ` and a second (unterminated)
    // `'` — a different render for a different tokenizer.
    assert_eq!(
        render_jinja(raw, SqlDialect::Generic),
        "select 'it\\'s , __dbtl_jinja_0__' as s"
    );
    // `#` opens a line comment only under hash-comment dialects —
    // Snowflake/BigQuery leave the tag inside it as comment text, while
    // Generic keeps `#` as an ordinary byte (and the `'` inside `don't`
    // still swallows the tail).
    let raw = "select id # {{ x }}\nfrom base";
    assert_eq!(render_jinja(raw, SqlDialect::Snowflake), raw);
    assert_eq!(render_jinja(raw, SqlDialect::BigQuery), raw);
    assert_eq!(
        render_jinja(raw, SqlDialect::Generic),
        "select id # __dbtl_jinja_0__\nfrom base"
    );
}

#[test]
fn jinja_inside_literals_and_after_hash_comments_parse() {
    let mk = |uid: &str, sql: &str| {
        Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                raw_model(uid, sql, &[]),
            ],
            &[("model.t.up", uid)],
        ))
    };
    // `\'` escaping and `#` comments are the dialect's call — Snowflake
    // parses what Generic can't.
    let dag = mk(
        "model.t.m",
        "select 'it\\'s' as s, u.id from {{ ref('up') }} u",
    );
    let r = resolver();
    let _ = r.columns(&dag, "model.t.m");
    assert!(r.is_fallback("model.t.m"));
    let snow = SqlParseResolver::new(None, SqlDialect::Snowflake);
    let _ = snow.columns(&dag, "model.t.m");
    assert!(!snow.is_fallback("model.t.m"));
    let dag = mk("model.t.m2", "select u.id # don't\nfrom {{ ref('up') }} u");
    let _ = r.columns(&dag, "model.t.m2");
    assert!(r.is_fallback("model.t.m2"));
    let snow = SqlParseResolver::new(None, SqlDialect::Snowflake);
    let _ = snow.columns(&dag, "model.t.m2");
    assert!(!snow.is_fallback("model.t.m2"));
    // The literal-nested tag parses in every dialect.
    let dag = mk(
        "model.t.m3",
        "select u.id, '{{ var('x', 'a') }}' as tag from {{ ref('up') }} u",
    );
    assert!(!r.is_fallback("model.t.m3"));
    assert_eq!(
        r.columns(&dag, "model.t.m3"),
        vec!["id".to_string(), "tag".to_string()]
    );
}

#[test]
fn jinja_glued_into_identifiers_stays_one_name() {
    // `{{ c }}_sum`, `total_{{ c }}`, `{{ var('s') }}_raw.tbl` — a tag
    // glued to identifier text is a name PART; the render concatenates
    // the marked fragment the way real Jinja concatenates the value.
    assert_eq!(
        render_jinja("select sum(a) as {{ c }}_sum from t", SqlDialect::Generic),
        "select sum(a) as __dbtl_jinja_0___sum from t"
    );
    assert_eq!(
        render_jinja("select a as total_{{ c }} from t", SqlDialect::Generic),
        "select a as total___dbtl_jinja_0__ from t"
    );
    assert_eq!(
        render_jinja("select a from {{ var('s') }}_raw.tbl", SqlDialect::Generic),
        "select a from __dbtl_jinja_0___raw.tbl"
    );
    let dag = Dag::build(&manifest(
        vec![
            raw_model(
                "model.t.m4",
                "select sum(a) as {{ c }}_sum from t group by a",
                &[],
            ),
            raw_model("model.t.m4b", "select a as total_{{ c }} from t", &[]),
            raw_model("model.t.m4c", "select a from {{ var('s') }}_raw.tbl", &[]),
        ],
        &[],
    ));
    let r = resolver();
    for uid in ["model.t.m4", "model.t.m4b", "model.t.m4c"] {
        let _ = r.columns(&dag, uid);
        assert!(!r.is_fallback(uid), "{uid} must parse");
    }
    assert_eq!(
        r.columns(&dag, "model.t.m4"),
        vec!["__dbtl_jinja_0___sum".to_string()]
    );
    assert_eq!(
        r.columns(&dag, "model.t.m4b"),
        vec!["total___dbtl_jinja_0__".to_string()]
    );
}

#[test]
fn jinja_aliases_and_select_items_stay_distinct_items() {
    // `a as {{ var('x1') }}, a as {{ var('x2') }}` — each alias is a
    // UNIQUE generated name; the `null`/`null` collapse that faked a
    // `duplicate_output_columns` hit can't form.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["a"]),
            raw_model(
                "model.t.m5",
                "select a as {{ var('x1') }}, a as {{ var('x2') }} from {{ ref('up') }}",
                &[],
            ),
            raw_model(
                "model.t.m6",
                "select o.id, o.a, o.id as other\n    \
                 {{ extension_columns(ref('up'), alias='o') }}\n    , o.id as third\n\
                 from {{ ref('up') }} o",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m5"), ("model.t.up", "model.t.m6")],
    ));
    let r = resolver();
    assert!(!r.is_fallback("model.t.m5"));
    assert!(!r.is_fallback("model.t.m6"));
    assert_eq!(
        r.columns(&dag, "model.t.m5"),
        vec![
            "__dbtl_jinja_0__".to_string(),
            "__dbtl_jinja_1__".to_string()
        ]
    );
    // The bare `{{ macro(...) }}` line is a NEW item — `other , x , o.id
    // as third` — never `other AS x` swallowing the macro's columns.
    assert_eq!(
        r.columns(&dag, "model.t.m6"),
        vec![
            "id".to_string(),
            "a".to_string(),
            "other".to_string(),
            "__dbtl_jinja_0__".to_string(),
            "third".to_string(),
        ]
    );
    // `sql_facts` sees no duplicate outputs on either model.
    assert!(r
        .sql_facts(&dag, "model.t.m5")
        .is_some_and(|f| f.duplicate_outputs.is_empty()));
    assert!(r
        .sql_facts(&dag, "model.t.m6")
        .is_some_and(|f| f.duplicate_outputs.is_empty()));
}

#[test]
fn jinja_in_relation_position_parses_as_a_relation() {
    // `from {{ var('t') }}` / `from {{ target.schema }}.tbl` — a relation
    // name, never `NULL` (`FROM NULL` fails on Snowflake); a glued
    // qualifier stays one qualified name.
    let dag = Dag::build(&manifest(
        vec![
            raw_model("model.t.m7", "select id from {{ var('t') }}", &[]),
            raw_model("model.t.m7b", "select id from {{ target.schema }}.tbl", &[]),
        ],
        &[],
    ));
    let r = resolver();
    assert_eq!(
        render_jinja("select id from {{ var('t') }}", SqlDialect::Generic),
        "select id from __dbtl_jinja_0__"
    );
    assert_eq!(
        render_jinja(
            "select id from {{ target.schema }}.tbl",
            SqlDialect::Generic
        ),
        "select id from __dbtl_jinja_0__.tbl"
    );
    let _ = r.columns(&dag, "model.t.m7");
    let _ = r.columns(&dag, "model.t.m7b");
    assert!(!r.is_fallback("model.t.m7"));
    assert!(!r.is_fallback("model.t.m7b"));
    // The marked render parses the same shapes — and the generated names
    // still aren't relations (hard_coded_relations stays empty).
    let rel = marked_relations("select id from {{ var('t') }}", SqlDialect::Generic).unwrap();
    assert!(rel.names.is_empty());
    // A glued qualifier's mark is buried mid-name — `record_name`'s
    // `contains` check still catches it.
    let rel = marked_relations(
        "select id from {{ target.schema }}.tbl",
        SqlDialect::Generic,
    )
    .unwrap();
    assert!(rel.names.is_empty());
    // `{{ target.col }}` marks the column — the provable-miss bookkeeping
    // never records a generated name.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            raw_model(
                "model.t.m",
                "select {{ target.col }} from {{ ref('up') }} u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .is_some_and(|f| { !f.unresolved.iter().any(|(_, col)| col.contains("__dbtl_")) }));
}

#[test]
fn ref_rendered_next_to_same_named_cte_does_not_read_it() {
    // `with stg_x as (…), j as (select * from {{ ref('stg_x') }})` — the
    // `ref` resolves to the MODEL `stg_x`, not the in-file CTE: the
    // generated render sits in a non-copy segment, so `record_name`
    // never counts it as a CTE read and `stg_x` stays unused.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.stg_x", "select 1", &["id"]),
            raw_model(
                "model.t.m",
                "with stg_x as (select 1 as id), \
                 j as (select * from {{ ref('stg_x') }}) \
                 select id from j",
                &[],
            ),
            // …but a WRITTEN `from stg_x` still reads it.
            raw_model(
                "model.t.m8b",
                "with stg_x as (select 1 as id), \
                 j as (select * from {{ ref('stg_x') }}) \
                 select id from j union all select id from stg_x",
                &[],
            ),
        ],
        &[
            ("model.t.stg_x", "model.t.m"),
            ("model.t.stg_x", "model.t.m8b"),
        ],
    ));
    let r = resolver();
    let facts = r.sql_facts(&dag, "model.t.m").unwrap();
    assert_eq!(facts.unused_ctes, vec!["stg_x".to_string()]);
    let facts = r.sql_facts(&dag, "model.t.m8b").unwrap();
    assert!(facts.unused_ctes.is_empty());
}

#[test]
fn jinja_comment_never_treats_its_quotes_as_strings() {
    // `{# don't #}` — a `'` inside a Jinja comment is comment text, not
    // a string opener: the comment still closes at `#}`. (Skipping a
    // quoted span once hunted a closing quote to EOF — `tag_close`
    // returned `None`, the `{#` leaked into the render, and the model
    // fell back unparsable.)
    assert_eq!(
        render_jinja("select a {# don't #} from t", SqlDialect::Generic),
        "select a   from t"
    );
    // A second comment's `#}` is the SECOND comment's close — the SQL
    // between them isn't eaten as comment text.
    assert_eq!(
        render_jinja(
            "select a {# don't #} , b {# isn't #} from t",
            SqlDialect::Generic
        ),
        "select a   , b   from t"
    );
    // And the file still parses.
    let dag = Dag::build(&manifest(
        vec![raw_model(
            "model.t.m",
            "select a {# don't run this #} , b from t",
            &[],
        )],
        &[],
    ));
    let r = resolver();
    let _ = r.columns(&dag, "model.t.m");
    assert!(!r.is_fallback("model.t.m"));
    // `{{`/`{%` DO hold Jinja strings — a quoted `}}`/`%}` still can't
    // fake their close.
    assert_eq!(
        render_jinja("select {{ f('}') }} from t", SqlDialect::Generic),
        "select __dbtl_jinja_0__ from t"
    );
}

#[test]
fn inner_subquery_keywords_do_not_leak_into_the_outer_context() {
    // `select a, (select y from t) {{ m() }}` — the inner `from` is the
    // SUBQUERY's: it must not close the outer item list and render the
    // tag as ` NULL ` (the `)` restores `select`, and `)` itself ends an
    // expression → `, <name>`).
    assert_eq!(
        render_jinja("select a, (select y from t) {{ m() }}", SqlDialect::Generic),
        "select a, (select y from t) , __dbtl_jinja_0__"
    );
    // `where exists (select 1) {{ m() }}` — the inner `select` is the
    // subquery's: it must not flip the outer context into a select list
    // and drop `, __dbtl_…` into a WHERE expression — a value render
    // (` NULL `) is what the position means.
    assert_eq!(
        render_jinja(
            "select a from t where exists (select 1) {{ m() }}",
            SqlDialect::Generic
        ),
        "select a from t where exists (select 1)  NULL "
    );
    // The select-item case parses to a real item.
    let dag = Dag::build(&manifest(
        vec![raw_model(
            "model.t.m",
            "select a, (select y from t) {{ m() }} from t",
            &[],
        )],
        &[],
    ));
    let r = resolver();
    assert!(!r.is_fallback("model.t.m"));
    // `(select …)` is an unnamed (synthetic) output — `columns()` never
    // invents it — leaving the written `a` and the generated item.
    assert_eq!(
        r.columns(&dag, "model.t.m"),
        vec!["a".to_string(), "__dbtl_jinja_0__".to_string()]
    );
}

#[test]
fn glued_ref_source_this_render_as_generated_names() {
    // `{{ ref('events') }}_2024` — the real-world partitioned-table
    // idiom: the glued identifier can't hold `"events"` (doesn't parse)
    // or the bare `events` (would bind a same-named CTE/relation the tag
    // never read) — a marked generated name stays one identifier AND
    // keeps the generated-name protections.
    assert_eq!(
        render_jinja(
            "select * from {{ ref('events') }}_2024",
            SqlDialect::Generic
        ),
        "select * from __dbtl_jinja_0___2024"
    );
    assert_eq!(
        render_jinja(
            "select * from {{ source('raw', 'events') }}_v2",
            SqlDialect::Generic
        ),
        "select * from __dbtl_jinja_0___v2"
    );
    assert_eq!(
        render_jinja("select * from {{ this }}_stg", SqlDialect::Generic),
        "select * from __dbtl_jinja_0___stg"
    );
    // A qualified-in tag (`o.{{ ref('x') }}`) glues the same way —
    // `o."x"` can't form inside a dotted name.
    assert_eq!(
        render_jinja("select o.{{ ref('x') }} from t o", SqlDialect::Generic),
        "select o.__dbtl_jinja_0__ from t o"
    );
    // A NON-glued ref still renders its quoted relation name.
    assert_eq!(
        render_jinja("select * from {{ ref('events') }}", SqlDialect::Generic),
        "select * from \"events\""
    );
    // The glued shapes parse — and never surface as written relations.
    let dag = Dag::build(&manifest(
        vec![
            raw_model("model.t.m", "select * from {{ ref('events') }}_2024", &[]),
            raw_model(
                "model.t.m2",
                "select * from {{ source('raw', 'events') }}_v2",
                &[],
            ),
        ],
        &[],
    ));
    let r = resolver();
    let _ = r.columns(&dag, "model.t.m");
    let _ = r.columns(&dag, "model.t.m2");
    assert!(!r.is_fallback("model.t.m"));
    assert!(!r.is_fallback("model.t.m2"));
    assert!(marked_relations(
        "select * from {{ ref('events') }}_2024",
        SqlDialect::Generic
    )
    .unwrap()
    .names
    .is_empty());
}

#[test]
fn generated_columns_never_invent_parent_columns() {
    // `u.{{ m() }}` → `u.__dbtl_jinja_0__` — against a parent whose
    // column list is UNKNOWN (an unparsable model declares none), the
    // qualified ref must not be trusted into `(up, __dbtl_jinja_0__)`:
    // the generated name exists in no column list, so crediting it would
    // fabricate an edge. A written `u.id` still trusts the unknown list.
    let dag = Dag::build(&manifest(
        vec![
            raw_model("model.t.up", "select from", &[]),
            raw_model(
                "model.t.m",
                "select u.id, u.{{ m() }}, {{ m2() }} from {{ ref('up') }} u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [e("model.t.up.id", "id", ColumnEdgeKind::Passthrough)]
        .into_iter()
        .collect();
    assert_eq!(got, want);
    // The generated outputs exist (positions keep them) but carry no
    // invented sources.
    assert_eq!(
        r.columns(&dag, "model.t.m"),
        vec![
            "id".to_string(),
            "__dbtl_jinja_0__".to_string(),
            "__dbtl_jinja_1__".to_string()
        ]
    );
}

#[test]
fn dialect_decides_line_comments_nested_blocks_and_string_prefixes() {
    // `//` is a line comment only under Snowflake — elsewhere the `{{`
    // after it is live code.
    let raw = "select a // {{ x }}\nfrom t";
    assert_eq!(render_jinja(raw, SqlDialect::Snowflake), raw);
    assert_eq!(
        render_jinja(raw, SqlDialect::Generic),
        "select a // __dbtl_jinja_0__\nfrom t"
    );
    // `--` needs whitespace only where the dialect asks (MySQL): `--x`
    // is an expression, so the `{{` on the line stays live; `-- x` is a
    // comment there too.
    let raw = "select a --x {{ y }}\nfrom t";
    assert_eq!(render_jinja(raw, SqlDialect::Generic), raw);
    assert_eq!(
        render_jinja(raw, SqlDialect::MySql),
        "select a --x , __dbtl_jinja_0__\nfrom t"
    );
    let raw = "select a -- {{ y }}\nfrom t";
    assert_eq!(render_jinja(raw, SqlDialect::MySql), raw);
    // `/*` nests only where the dialect says — Postgres closes the
    // comment at the OUTER `*/` (the `{{` stays comment text); MySQL's
    // flat scan ends at the first `*/`, leaving the `{{` live.
    let raw = "select a /* x /* y */ {{ z }} */ from t";
    assert_eq!(render_jinja(raw, SqlDialect::Postgres), raw);
    assert_eq!(
        render_jinja(raw, SqlDialect::MySql),
        "select a /* x /* y */ , __dbtl_jinja_0__ */ from t"
    );
    // `$$` is a dollar-quoted string everywhere except SQLite, where
    // `$…$` is a placeholder — the `{{` between the `$$`s is a literal
    // interior under Postgres but live code under SQLite.
    let raw = "select $$ {{ x }} $$";
    assert_eq!(
        render_jinja(raw, SqlDialect::Postgres),
        "select $$  NULL  $$"
    );
    assert_eq!(
        render_jinja(raw, SqlDialect::Sqlite),
        "select $$ , __dbtl_jinja_0__ $$"
    );
    // `q'[x]'`/`nq'[x]'` — Oracle/Generic — hold an interior `'` inside
    // the literal; elsewhere `q` is an identifier and `'` opens a plain
    // literal that `don't`'s apostrophe closes.
    let raw = "select q'[don't {{ x }}]' from t";
    assert_eq!(
        render_jinja(raw, SqlDialect::Oracle),
        "select q'[don't  NULL ]' from t"
    );
    assert_eq!(
        render_jinja(raw, SqlDialect::Postgres),
        "select q'[don't , __dbtl_jinja_0__]' from t"
    );
    // `u&'…'` — Postgres/Generic — `\` begins a codepoint escape (a
    // `\'` can't close); elsewhere `u` `&` are ordinary and the `'`
    // literal follows the dialect's own escape rule.
    let raw = "select u&'a\\' {{ x }} b'";
    assert_eq!(
        render_jinja(raw, SqlDialect::Postgres),
        "select u&'a\\'  NULL  b'"
    );
    assert_eq!(
        render_jinja(raw, SqlDialect::Sqlite),
        "select u&'a\\' , __dbtl_jinja_0__ b'"
    );
}

// -- synthetic `__colN` outputs never leak -----------------------------------

#[test]
fn synthetic_unnamed_outputs_never_leak() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "k", "f", "__col1"]),
            model(
                "model.t.m",
                "select u.id, u.k + 1 from \"up\" u where u.f",
                &[],
            ),
            // A REAL parent column named `__col1` resolves normally.
            model("model.t.real", "select u.__col1 from \"up\" u", &[]),
        ],
        &[("model.t.up", "model.t.m"), ("model.t.up", "model.t.real")],
    ));
    let r = resolver();
    // The unnamed `u.k + 1` output is `__col1` positionally — invisible:
    // `columns()` never invents it, and the WHERE fan doesn't target it.
    assert_eq!(r.columns(&dag, "model.t.m"), vec!["id".to_string()]);
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.k", "__col1", ColumnEdgeKind::Transform),
        // `u.f` fans ONLY to the real output — not the placeholder.
        e("model.t.up.f", "id", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);

    let got_real = edge_set(&dag, &r, "model.t.real");
    let want_real: BTreeSet<_> = [e(
        "model.t.up.__col1",
        "__col1",
        ColumnEdgeKind::Passthrough,
    )]
    .into_iter()
    .collect();
    assert_eq!(got_real, want_real);
    assert_eq!(r.columns(&dag, "model.t.real"), vec!["__col1".to_string()]);
}

// -- correlated unknown bindings vs. provable outer hits --------------------

#[test]
fn inner_unknowns_do_not_steal_provable_outer_hits() {
    // `u`'s column list is unknown (nothing declared, unparsable SQL);
    // `t`'s is known. Bare `z` inside the subquery resolves in the OUTER
    // scope — the inner unknown binding must NOT also claim it.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.t", "select 1", &["id", "z"]),
            model("model.t.u", "", &[]),
            model(
                "model.t.m",
                "select t.id as tid, \
                 (select z from \"u\" where u.w = t.id) as q \
                 from \"t\" t",
                &[],
            ),
        ],
        &[("model.t.t", "model.t.m"), ("model.t.u", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.t.id", "tid", ColumnEdgeKind::Rename),
        e("model.t.t.id", "q", ColumnEdgeKind::Transform),
        e("model.t.t.z", "q", ColumnEdgeKind::Transform),
        // `u.w` stays a candidate — no provable hit exists for `w`.
        e("model.t.u.w", "q", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(got.iter().all(|e| e.0 != "model.t.u.z"));

    // Same rule within one level: a KNOWN hit in one binding doesn't get
    // joined by the sibling binding's unknown list.
    let dag2 = Dag::build(&manifest(
        vec![
            model("model.t.t", "select 1", &["id", "x"]),
            model("model.t.u", "", &[]),
            model(
                "model.t.m",
                "select x from \"t\" t join \"u\" u on t.id = u.id",
                &[],
            ),
        ],
        &[("model.t.t", "model.t.m"), ("model.t.u", "model.t.m")],
    ));
    let got2 = edge_set(&dag2, &resolver(), "model.t.m");
    let want2: BTreeSet<_> = [
        e("model.t.t.x", "x", ColumnEdgeKind::Passthrough),
        e("model.t.t.id", "x", ColumnEdgeKind::FilterOrJoin),
        e("model.t.u.id", "x", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got2, want2);
    assert!(got2.iter().all(|e| e.0 != "model.t.u.x"));
}

// -- `TABLE name` bodies -----------------------------------------------------

#[test]
fn table_statement_reads_the_whole_relation() {
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "k"]),
            model(
                "model.t.m",
                "select x.id, x.k as kk from (table \"up\") x",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.k", "kk", ColumnEdgeKind::Rename),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- declared-column probe gating (schema.yml drift can't fabricate) ---------

#[test]
fn declared_but_unselected_columns_do_not_fabricate_edges() {
    // `region` is documented on `m` (schema drift) but the projection
    // provably never produced it — there is no `*` it could hide
    // behind. The old probe credited `up.region` anyway and emitted an
    // Exact Passthrough edge for a column that was never selected.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "name", "region"]),
            model(
                "model.t.m",
                "select u.id as order_id, u.name as customer_name from \"up\" u",
                &["order_id", "customer_name", "region"],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "order_id", ColumnEdgeKind::Rename),
        e("model.t.up.name", "customer_name", ColumnEdgeKind::Rename),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn wildcard_probe_still_covers_columns_behind_an_unknown_star() {
    // The legitimate probe case: `select *` CAN carry a declared
    // column the analyzer couldn't enumerate when the parent's column
    // list is unknowable — the gate admits exactly that binding.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "", &[]),
            model("model.t.m", "select * from \"up\"", &["id", "region"]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.region", "region", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn quoted_alias_case_matches_the_declared_name() {
    // `AS "User_ID"` produces `User_ID`; documented `user_id` is the
    // same column in the normalized space — it must count as produced
    // (a raw compare would probe a phantom `up.user_id → user_id`
    // edge). `region` stays a real `*`-carried probe hit.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "", &[]),
            model(
                "model.t.m",
                "select u.x as \"User_ID\", u.* from \"up\" u",
                &["user_id", "region"],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        // Edge targets fold through `ColumnRef`'s normalization too —
        // the declared `user_id` spelling is the one that lands.
        e("model.t.up.x", "user_id", ColumnEdgeKind::Rename),
        e("model.t.up.region", "region", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- qualified names never bind CTEs -----------------------------------------

#[test]
fn qualified_relation_name_never_binds_a_cte() {
    // `sch.c` is a real relation — compiled `ref('c')` prints
    // `analytics.c` — and can never address the CTE `c`. The parent's
    // `z2` resolves; the CTE's `z` stays unreferenced.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model("model.t.c", "select 1", &["z2"]),
            model(
                "model.t.m",
                "with c as (select u.id as z from \"up\" u) \
                 select sch.c.z2 from sch.c",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m"), ("model.t.c", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [e("model.t.c.z2", "z2", ColumnEdgeKind::Passthrough)]
        .into_iter()
        .collect();
    assert_eq!(got, want);
}

// -- Jinja branch splits ------------------------------------------------------

#[test]
fn jinja_else_and_elif_split_select_items_into_the_branch_union() {
    // `{% else %}`/`{% elif %}` render as `,` so every branch stays a
    // separate select item — union coverage — instead of fusing into
    // `a AS b` (the false branch's name stealing the true branch's
    // value). Whitespace-control dashes parse the same.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "name", "status"]),
            raw_model(
                "model.t.m",
                "select {% if x %}a.id{%- elif y -%}a.name{% else %}a.status{% endif %} \
                 from {{ ref('up') }} a",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.name", "name", ColumnEdgeKind::Passthrough),
        e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

#[test]
fn jinja_else_preserves_the_conditional_comma_idiom() {
    // `{% if %}, b{% else %}, c{% endif %}` — the else sits BETWEEN two
    // literal commas; another `,` there would render `, ,` and kill
    // the parse into a whole-model fallback.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["a", "b", "c"]),
            raw_model(
                "model.t.m",
                "select a {% if x %}, b{% else %}, c{% endif %} from {{ ref('up') }}",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.a", "a", ColumnEdgeKind::Passthrough),
        e("model.t.up.b", "b", ColumnEdgeKind::Passthrough),
        e("model.t.up.c", "c", ColumnEdgeKind::Passthrough),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- template block bodies -----------------------------------------------------

#[test]
fn jinja_template_block_bodies_render_as_whitespace() {
    // `{% set x %}…{% endset %}`/`{% macro %}…{% endmacro %}`/
    // `{% call %}…{% endcall %}` bodies are template-side text — dbt
    // evaluates them into a variable, macro, or call result and never
    // emits their SQL at that position — so the WHOLE paired span
    // renders whitespace, not two statements' worth of body text.
    for (raw, rendered) in [
        (
            "{% set q %}select 9 as bogus{% endset %}select 1 as id",
            " select 1 as id",
        ),
        (
            "{% macro m() %}select 9 as bogus{% endmacro %}select 2 as id",
            " select 2 as id",
        ),
        (
            "{% call m() %}select 9 as bogus{% endcall %}select 3 as id",
            " select 3 as id",
        ),
        // `{% set x = … %}` is the assignment statement — no body —
        // so only the tag drops and the tail stays live.
        ("{% set x = 1 %}select 4 as id", " select 4 as id"),
        // Same-word blocks nest — the span ends at the OUTER
        // `{% endset %}`.
        (
            "{% set a %}{% set b %}x{% endset %}y{% endset %}select 5 as id",
            " select 5 as id",
        ),
        // A `{% raw %}` body inside a block is inert text — its
        // `{% endset %}` can't close the outer block early.
        (
            "{% set a %}{% raw %}{% endset %}{% endraw %}y{% endset %}select 6 as id",
            " select 6 as id",
        ),
        // An unmatched opener falls back to tag-only dropping — the
        // body text stays live rather than eating the tail.
        ("{% set q %}select 7 as id", " select 7 as id"),
    ] {
        assert_eq!(render_jinja(raw, SqlDialect::Generic), rendered, "{raw:?}");
    }
    // Whitespace-control dashes parse the same.
    assert_eq!(
        render_jinja(
            "{%- set q -%}select 9 as bogus{%- endset -%}select 8 as id",
            SqlDialect::Generic
        ),
        " select 8 as id"
    );
}

#[test]
fn jinja_arm_start_picks_the_else_render() {
    // An arm opening with `from` keeps the `,` and blanks the keyword —
    // `from a ,  b` unions the relations as FROM items. An arm opening
    // with any other clause keyword (`limit`/`where`/`select`…) can't
    // follow `,` without the keyword posing as a name — it emits
    // whitespace and the [`jinja_parse`] retry keeps one arm instead.
    assert_eq!(
        render_jinja(
            "select * {% if x %} from a {% else %} from b {% endif %}",
            SqlDialect::Generic
        ),
        "select *   from a ,   b  "
    );
    assert_eq!(
        render_jinja(
            "select id from t {% if x %} limit 10 {% else %} limit 20 {% endif %}",
            SqlDialect::Generic
        ),
        "select id from t   limit 10   limit 20  "
    );
    // A `{% for %}`'s `{% else %}` is its empty-iteration fallback —
    // never a union member — so it renders a space, not `,`.
    assert_eq!(
        render_jinja(
            "select {% for i in xs %}a{% else %}b{% endfor %} from t",
            SqlDialect::Generic
        ),
        "select  a b  from t"
    );
}

#[test]
fn jinja_parse_recovers_loop_separators_and_exclusive_arms() {
    // The `{% if not loop.last %}` idiom leaves `union all` dangling —
    // `loop.last` can't be evaluated statically — so the strip blanks
    // it and the single statement parses.
    let sql = "{% for t in ['a','b'] %}\nselect id from {{ t }}\n\
               {% if not loop.last %} union all {% endif %}\n{% endfor %}\n";
    let (r, stmts) = jinja_parse(sql, JinjaMark::Plain, SqlDialect::Generic).unwrap();
    assert_eq!(stmts.len(), 1);
    assert!(!r.text.contains("union"), "the dangling separator blanked");
    // The same idiom on select items — a trailing `,` before `from`.
    let sql =
        "select {% for c in ['a','b'] %} c{% if not loop.last %},{% endif %} {% endfor %} from t";
    let (_, stmts) = jinja_parse(sql, JinjaMark::Plain, SqlDialect::Generic).unwrap();
    assert_eq!(stmts.len(), 1);
    // Positions the union can't combine — a `,` between arm bodies is
    // no clause separator — collapse to one arm's render.
    for sql in [
        "select id from t where {% if x %} a > 1 {% else %} a > 2 {% endif %}",
        "select a.id from a join b on {% if x %} a.x = b.x {% else %} a.y = b.y {% endif %}",
        "select id from t {% if x %} limit 10 {% else %} limit 20 {% endif %}",
        "{% if x %}select 1 as id{% else %}select 2 as id{% endif %}",
        "select * {% if x %} from a.t {% else %} from b.u {% endif %}",
    ] {
        assert!(
            jinja_parse(sql, JinjaMark::Plain, SqlDialect::Generic).is_some(),
            "{sql:?} parses"
        );
    }
    // Genuinely broken SQL still fails every retry.
    assert!(jinja_parse(
        "select from where or",
        JinjaMark::Plain,
        SqlDialect::Generic
    )
    .is_none());
}

#[test]
fn jinja_arm_splits_where_a_comma_cannot_parse_render_a_space() {
    // `,` needs a completed item or expression before it — after an
    // operand-expecting keyword, an operator, `(`, or nothing at all it
    // is guaranteed-invalid, so the split emits whitespace and the
    // arms' text concatenates instead.
    for (sql, rendered) in [
        // Empty arm 0 — `select ,a`/`from ,t` fail where the space
        // render parses.
        (
            "select {% if x %}{% else %}a{% endif %} from t",
            "select   a  from t",
        ),
        (
            "select * from {% if x %}{% else %}t{% endif %}",
            "select * from   t ",
        ),
        // `a +` can't precede `,` — `a +  b` reads as one expression.
        (
            "select {% if x %}a +{% else %}b{% endif %} from t",
            "select  a + b  from t",
        ),
        // `and`/`limit` need their operand — `and ,b`/`limit ,10` fail
        // where `and  b`/`limit  10` parse.
        (
            "select id from t where a > 0 and {% if x %}{% else %}b > 0{% endif %}",
            "select id from t where a > 0 and   b > 0 ",
        ),
        (
            "select id from t limit {% if x %}{% else %}10{% endif %}",
            "select id from t limit   10 ",
        ),
        // `end` completes a CASE — `end , b` IS a valid list split.
        (
            "select case when p then 1 end {% if x %}{% else %}b{% endif %} from t",
            "select case when p then 1 end  ,b  from t",
        ),
    ] {
        assert_eq!(render_jinja(sql, SqlDialect::Generic), rendered, "{sql:?}");
        assert!(
            jinja_parse(sql, JinjaMark::Plain, SqlDialect::Generic).is_some(),
            "{sql:?} parses"
        );
    }
}

#[test]
fn jinja_comment_commas_never_read_as_written_separators() {
    // A `{# … #}` inside the `{% else %}from` gap can't leak into the
    // render — Jinja drops it.
    assert_eq!(
        render_jinja(
            "select * {% if x %} from a.t {% else %}{# c #} from b.u {% endif %}",
            SqlDialect::Generic
        ),
        "select *   from a.t ,   b.u  "
    );
    // A `,` inside a comment in an arm body is text, not a written
    // comma join — the branch comma alone separates the arms.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id"]),
            model("model.t.b", "select 1", &["id"]),
            raw_model(
                "model.t.m",
                "select t.id from {% if x %}{{ ref('a') }} t{% else %}/* , */ {{ ref('b') }} t{% endif %}",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let r = resolver();
    assert!(!r.is_fallback("model.t.m"));
    assert!(r.sql_facts(&dag, "model.t.m").unwrap().cartesian.is_empty());
}

#[test]
fn jinja_block_body_models_resolve_the_surrounding_sql() {
    // The block body never reaches the render — the model resolves
    // against the SQL around it, not a phantom first statement.
    for (uid, sql) in [
        (
            "model.t.set",
            "{% set q %}select 9 as bogus{% endset %}select u.id from {{ ref('up') }} u",
        ),
        (
            "model.t.macro",
            "{% macro m() %}select 9 as bogus{% endmacro %}select u.id from {{ ref('up') }} u",
        ),
        (
            "model.t.call",
            "{% call m() %}select 9 as bogus{% endcall %}select u.id from {{ ref('up') }} u",
        ),
    ] {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                raw_model(uid, sql, &[]),
            ],
            &[("model.t.up", uid)],
        ));
        let r = resolver();
        assert!(!r.is_fallback(uid), "{uid} parses");
        let got = edge_set(&dag, &r, uid);
        let want: BTreeSet<_> = [e("model.t.up.id", "id", ColumnEdgeKind::Passthrough)]
            .into_iter()
            .collect();
        assert_eq!(got, want, "{uid}");
    }
}

#[test]
fn exclusive_arms_do_not_fabricate_duplicate_output_columns() {
    // The same output name once per `{% if %}`/`{% else %}` arm is ONE
    // output column on every real render — not a duplicate. A repeat
    // inside ONE arm still flags.
    let dag = Dag::build(&manifest(
        vec![
            raw_model(
                "model.t.dup",
                "select {% if x %}r.refund_total as refund_total\
                 {% else %}cast(null as integer) as refund_total{% endif %} from r",
                &[],
            ),
            raw_model(
                "model.t.real",
                "select {% if x %}a as n, a as n{% else %}b as n{% endif %} from t",
                &[],
            ),
        ],
        &[],
    ));
    let r = resolver();
    assert!(r
        .sql_facts(&dag, "model.t.dup")
        .unwrap()
        .duplicate_outputs
        .is_empty());
    assert_eq!(
        r.sql_facts(&dag, "model.t.real").unwrap().duplicate_outputs,
        vec![("n".to_string(), 2)]
    );
}

#[test]
fn exclusive_arms_do_not_fabricate_an_unread_cte() {
    // `final` declared once per arm and read unconditionally: whichever
    // render ran, a `from final` bound SOME `final` — no render leaves
    // a same-named sibling unread. A genuinely unread CTE still flags.
    let dag = Dag::build(&manifest(
        vec![
            raw_model(
                "model.t.dup",
                "with {% if x %}final as (select 1 as a)\
                 {% else %}final as (select 2 as a){% endif %} select * from final",
                &[],
            ),
            raw_model(
                "model.t.real",
                "with dead as (select 1 as a), live as (select 2 as a) select * from live",
                &[],
            ),
        ],
        &[],
    ));
    let r = resolver();
    assert!(r
        .sql_facts(&dag, "model.t.dup")
        .unwrap()
        .unused_ctes
        .is_empty());
    assert_eq!(
        r.sql_facts(&dag, "model.t.real").unwrap().unused_ctes,
        vec!["dead".to_string()]
    );
}

#[test]
fn from_keyword_arms_union_relations_without_a_phantom_from() {
    // `{% else %}from …` renders `,` + a blanked keyword: the FROM-item
    // union keeps both written relations, `from` never poses as one,
    // and the branch comma isn't a written comma join.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id"]),
            model("model.t.b", "select 1", &["id"]),
            raw_model(
                "model.t.m",
                "select t.id {% if x %}from {{ ref('a') }} t{% else %}from {{ ref('b') }} t{% endif %}",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let r = resolver();
    assert!(!r.is_fallback("model.t.m"));
    assert!(r.sql_facts(&dag, "model.t.m").unwrap().cartesian.is_empty());
    // The Marked scan sees the written names around the refs — and no
    // `from` among them.
    let m = marked_relations(
        "select * {% if x %} from a.t {% else %} from b.u {% endif %}",
        SqlDialect::Generic,
    )
    .unwrap();
    assert_eq!(m.names, vec!["a.t".to_string(), "b.u".to_string()]);
}

// -- table alias column lists -------------------------------------------------

#[test]
fn parent_table_alias_columns_rename_positionally() {
    // `FROM up t(a, b)` renames the parent's outputs positionally —
    // `t.b` reads the parent's SECOND column (`status`), like a CTE
    // alias list.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "status"]),
            model("model.t.m", "select t.b from \"up\" t(a, b)", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [e("model.t.up.status", "b", ColumnEdgeKind::Rename)]
        .into_iter()
        .collect();
    assert_eq!(got, want);
}

// -- NATURAL JOIN ----------------------------------------------------------------

#[test]
fn natural_join_fans_out_the_shared_columns() {
    // `NATURAL JOIN` keys on every same-named column of both sides:
    // shared {id, k} is row-shaping — `a.id` is already a value source
    // so only the other shared names fan out.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["id", "k"]),
            model("model.t.b", "select 1", &["id", "k"]),
            model(
                "model.t.m",
                "select a.id from \"a\" a natural join \"b\" b",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.b.id", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.a.k", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.b.k", "id", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- duplicate binding names -----------------------------------------------------

#[test]
fn a_later_binding_shadows_an_earlier_same_name() {
    // `FROM a t, b t`: the rightmost `t` wins — `t.x` must not reach
    // the earlier binding's `x` column.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.a", "select 1", &["x"]),
            model("model.t.b", "select 1", &["y"]),
            model(
                "model.t.m",
                "select t.x as ox, t.y as oy from \"a\" t, \"b\" t",
                &[],
            ),
        ],
        &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [e("model.t.b.y", "oy", ColumnEdgeKind::Rename)]
        .into_iter()
        .collect();
    assert_eq!(got, want);
}

// -- adapter dialect reporting ---------------------------------------------------

#[test]
fn unknown_adapter_maps_to_generic_and_is_reported() {
    let mut m = manifest(vec![model("model.t.m", "select 1", &[])], &[]);
    m.metadata.adapter_type = Some("not_a_dialect".to_string());
    let dag = Dag::build(&m);
    assert_eq!(SqlDialect::from_dag(&dag), SqlDialect::Generic);
    assert_eq!(
        SqlDialect::unrecognized_adapter(&dag),
        Some("not_a_dialect")
    );
    m.metadata.adapter_type = Some("postgres".to_string());
    let dag = Dag::build(&m);
    assert_eq!(SqlDialect::from_dag(&dag), SqlDialect::Postgres);
    assert_eq!(SqlDialect::unrecognized_adapter(&dag), None);
}

#[test]
fn every_advertised_dialect_name_parses() {
    // `NAMES` feeds `--help` and the `--sql-dialect` error message — it
    // must be a subset of what `parse_name` actually accepts.
    for n in SqlDialect::NAMES {
        assert!(
            SqlDialect::parse_name(n).is_some(),
            "NAMES lists '{n}' but parse_name rejects it"
        );
    }
}

// -- columns()-only fallback flagging --------------------------------------------

#[test]
fn columns_only_fallback_still_marks_the_node() {
    // A caller that only asks `columns()` (never `resolve()`) must see
    // the same `[name-match]` marking — the flag isn't resolve-only.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model("model.t.m", "select from where (", &[]),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let _ = r.columns(&dag, "model.t.m");
    assert!(r.is_fallback("model.t.m"));
    assert_eq!(r.fallback_nodes(), vec!["model.t.m".to_string()]);
}

// -- date/time parts are keywords, never columns ------------------------------

#[test]
fn date_part_words_never_read_columns() {
    // `dateadd(day, …)`'s `day` — a bare unit word at a documented part
    // slot — is the keyword, not a column. Only the real inputs feed
    // the outputs.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "d"]),
            model(
                "model.t.m",
                "select u.id, dateadd(day, 1, u.d) as d1, \
                 datediff(day, u.d, u.d) as n, \
                 datepart(hour, u.d) as h, \
                 timestampadd(minute, 1, u.d) as d2, \
                 timeadd(hour, 1, u.d) as t, \
                 dateadd((day), 1, u.d) as p, \
                 last_day(u.d, month) as ld \
                 from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.d", "d1", ColumnEdgeKind::Transform),
        e("model.t.up.d", "n", ColumnEdgeKind::Transform),
        e("model.t.up.d", "h", ColumnEdgeKind::Transform),
        e("model.t.up.d", "d2", ColumnEdgeKind::Transform),
        e("model.t.up.d", "t", ColumnEdgeKind::Transform),
        e("model.t.up.d", "p", ColumnEdgeKind::Transform),
        e("model.t.up.d", "ld", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn bigquery_date_fns_take_the_part_last() {
    // BigQuery's `_trunc`/`_diff` family ends with the part —
    // `date_trunc(d, month)`, `date_diff(a, b, day)`. The same slot
    // leads elsewhere; each dialect reads its own documented shape.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["d", "d2"]),
            model(
                "model.t.m",
                "select date_trunc(u.d, month) as m, \
                 date_diff(u.d, u.d2, day) as n, \
                 timestamp_diff(u.d, u.d2, hour) as h \
                 from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = SqlParseResolver::new(None, SqlDialect::BigQuery);
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.d", "m", ColumnEdgeKind::Transform),
        e("model.t.up.d", "n", ColumnEdgeKind::Transform),
        e("model.t.up.d2", "n", ColumnEdgeKind::Transform),
        e("model.t.up.d", "h", ColumnEdgeKind::Transform),
        e("model.t.up.d2", "h", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn a_part_word_at_a_non_part_slot_stays_a_column_ref() {
    // Dialect precision cuts both ways: under Snowflake `date_trunc`
    // leads with the part, so `date_trunc(d, month)`'s `month` at arg 1
    // IS a value position — it stays a column reference (an unresolved
    // one here, since `up` provably lacks it).
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["d"]),
            model(
                "model.t.m",
                "select date_trunc(u.d, month) as m from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = SqlParseResolver::new(None, SqlDialect::Snowflake);
    assert_eq!(
        r.sql_facts(&dag, "model.t.m").unwrap().unresolved,
        vec![("model.t.up".to_string(), "month".to_string())]
    );
}

#[test]
fn generic_suppresses_the_part_word_in_either_position() {
    // With no pinned dialect the signature's part slot is unknowable —
    // a documented part word in EITHER spelling's slot stays uncounted
    // rather than guessed.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["d"]),
            model(
                "model.t.m",
                "select date_trunc(month, u.d) as a, \
                 date_trunc(u.d, month) as b, \
                 date_diff(day, u.d, u.d) as c, \
                 date_diff(u.d, u.d, day) as e \
                 from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn a_non_part_word_at_a_part_slot_stays_a_column_ref() {
    // `dateadd(status, 1, d)` — `status` isn't a documented part word,
    // so it keeps resolving as a column (and lands in `unresolved`
    // when the parent provably lacks it).
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["d"]),
            model(
                "model.t.m",
                "select dateadd(status, 1, u.d) as x from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    assert_eq!(
        r.sql_facts(&dag, "model.t.m").unwrap().unresolved,
        vec![("model.t.up".to_string(), "status".to_string())]
    );
}

#[test]
fn a_real_column_at_a_part_slot_still_resolves() {
    // Suppression is word-list-bounded: `parts` isn't a unit word, so
    // `dateadd(parts, …)` still reads the parent column `parts`.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["parts", "d"]),
            model(
                "model.t.m",
                "select dateadd(parts, 1, u.d) as x from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let got = edge_set(&dag, &resolver(), "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.parts", "x", ColumnEdgeKind::Transform),
        e("model.t.up.d", "x", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
}

// -- same-SELECT aliases -------------------------------------------------------

#[test]
fn select_alias_resolves_in_where_group_by_and_having() {
    // `where c`/`group by c`/`having sum(c)` read the `a + b as c`
    // alias — never an upstream column `c`. The clause's real FROM
    // column `f` still binds through the FROM scope (its FilterOrJoin
    // fan reaches every output); the alias's own sources shape the
    // rows too, but already hold `c`'s value edge so no fan shows.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "a", "b", "f"]),
            model(
                "model.t.m",
                "select u.id, u.a + u.b as c from \"up\" u \
                 where c > u.f group by c having sum(c) > 0",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.a", "c", ColumnEdgeKind::Transform),
        e("model.t.up.b", "c", ColumnEdgeKind::Transform),
        e("model.t.up.f", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.up.f", "c", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn select_alias_resolves_in_qualify() {
    // `qualify rn = 1` reads the window function's alias — QUALIFY's
    // whole job is reaching select aliases. (`id` already holds the
    // value edges, so its row-shaping fan drops — the fix's observable
    // is that `rn` never records as an unresolved reference.)
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id"]),
            model(
                "model.t.m",
                "select u.id, row_number() over (order by u.id) as rn \
                 from \"up\" u qualify rn = 1",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = SqlParseResolver::new(None, SqlDialect::Snowflake);
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.id", "rn", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn later_select_items_read_earlier_aliases() {
    // `select a + 1 as c, c * 2 as d` — `d`'s `c` is the alias, so `d`
    // traces to `a` rather than a phantom upstream `c`.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["a"]),
            model(
                "model.t.m",
                "select u.a + 1 as c, c * 2 as d from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.a", "c", ColumnEdgeKind::Transform),
        e("model.t.up.a", "d", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn a_from_column_beats_a_same_named_select_alias() {
    // `u.a as c` declares the alias `c`, but `where c` keeps reading the
    // FROM column `u.c` — the FROM scope sits inner to the alias level.
    // The read is provable: `up.c` is never projected, so it can only
    // reach the edges through the FilterOrJoin fan the WHERE feeds. An
    // alias-first read would put `up.a` there instead — and `up.a`
    // already holds `c`'s value edge, so nothing would show at all.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["a", "c"]),
            model(
                "model.t.m",
                "select u.a as c from \"up\" u where c = 1",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.a", "c", ColumnEdgeKind::Rename),
        e("model.t.up.c", "c", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn a_later_alias_does_not_leak_backward() {
    // Aliases declare left to right: item 1's bare `c` can't see item
    // 2's `c`. A leak wouldn't flag anything — it would silently read
    // `a` through the alias — so the check is that `c` still records
    // unresolved against the parent that lacks it, and `x` carries no
    // source at all.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["a"]),
            model(
                "model.t.m",
                "select c as x, u.a + 1 as c from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [e("model.t.up.a", "c", ColumnEdgeKind::Transform)]
        .into_iter()
        .collect();
    assert_eq!(got, want);
    assert_eq!(
        r.sql_facts(&dag, "model.t.m").unwrap().unresolved,
        vec![("model.t.up".to_string(), "c".to_string())]
    );
}

#[test]
fn order_by_expression_reads_the_select_alias() {
    // `order by c + f` — a compound ORDER BY expression sees the
    // select's declared names too (a bare `order by c` already bound
    // via the output-name match). `c`'s own source `a` already holds a
    // value edge; `f`'s row-shaping fan reaches every output.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "a", "f"]),
            model(
                "model.t.m",
                "select u.id, u.a + 1 as c from \"up\" u order by c + f limit 5",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.a", "c", ColumnEdgeKind::Transform),
        e("model.t.up.f", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.up.f", "c", ColumnEdgeKind::FilterOrJoin),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

// -- UNNEST output names -------------------------------------------------------

#[test]
fn unnest_table_alias_names_its_value_column() {
    // `unnest(u.payload) as x`: `x` names the exploded value column —
    // `select x` reads it and never an upstream column `x`. (`payload`
    // already holds `x`'s value edge, so its row-shaping fan drops.)
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "payload"]),
            model(
                "model.t.m",
                "select b.id, x from \"up\" b cross join unnest(b.payload) as x",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.payload", "x", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn unnest_table_alias_names_its_value_column_in_table_syntax() {
    // Dialects without the UNNEST table factor parse `unnest(x) t` as
    // a function-like table — the same value-column binding results.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "payload"]),
            model(
                "model.t.m",
                "select b.id, x from \"up\" b cross join unnest(b.payload) as x",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = SqlParseResolver::new(None, SqlDialect::Snowflake);
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.payload", "x", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}

#[test]
fn unnest_column_alias_list_still_names_the_value() {
    // `unnest(u.payload) as t(v)` — an explicit column alias keeps
    // naming the value column, unchanged.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "payload"]),
            model(
                "model.t.m",
                "select b.id, t.v from \"up\" b cross join unnest(b.payload) as t(v)",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.payload", "v", ColumnEdgeKind::Transform),
    ]
    .into_iter()
    .collect();
    assert_eq!(got, want);
    assert!(r
        .sql_facts(&dag, "model.t.m")
        .unwrap()
        .unresolved
        .is_empty());
}
