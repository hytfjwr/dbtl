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
        render_jinja_mapped(raw, JinjaMark::Plain).text,
        render_jinja_as(raw, JinjaMark::Plain)
    );
    assert_eq!(
        render_jinja_mapped(raw, JinjaMark::Marked).text,
        render_jinja_as(raw, JinjaMark::Marked)
    );
}

#[test]
fn render_map_maps_copies_11_and_tags_to_the_whole_tag() {
    let raw = "select a\nfrom {{ ref('up') }} u\n";
    let rendered = render_jinja_mapped(raw, JinjaMark::Plain);
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
    let rendered = render_jinja_mapped(raw, JinjaMark::Plain);
    assert_eq!(rendered.text, "select *\nfrom  a.t,b.u ");
    let line2 = rendered.text.find('\n').unwrap() + 1;
    let loc = |off: usize| Location::new(2, (off - line2 + 1) as u64);
    let comma = rendered.text.find(',').unwrap();
    assert!(rendered.only_branch_commas(Span::new(loc(comma), loc(comma + 1))));
    // A written `,` fails the check even when an arm split follows.
    let raw = "select *\nfrom a.t, {% if x %}b.u{% else %}c.v{% endif %}";
    let rendered = render_jinja_mapped(raw, JinjaMark::Plain);
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
    );
    assert!(rendered.jinja_literals.is_empty());
    // An unterminated literal is dropped — it was never a string.
    let rendered = render_jinja_mapped("select {{ f('unclosed) }}", JinjaMark::Plain);
    assert_eq!(rendered.jinja_literals, Vec::<String>::new());
}

#[test]
fn plain_positions_extract_wildcard_join_and_column_spans() {
    let raw = "select a, o.*\nfrom {{ ref('o') }} o\njoin b.u\nwhere o.id = b.id";
    let pos = plain_positions(raw, &*SqlDialect::Generic.sqlparser()).unwrap();
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
    assert!(plain_positions("select from where or", &*SqlDialect::Generic.sqlparser()).is_none());
}

#[test]
fn marked_relations_carry_name_spans() {
    let raw = "select *\nfrom analytics.orders\njoin {{ ref('u') }} u on 1 = 1";
    let rel = marked_relations(raw, &*SqlDialect::Generic.sqlparser()).unwrap();
    assert_eq!(rel.names, vec!["analytics.orders".to_string()]);
    assert_eq!(rel.spans.len(), 1);
    assert_eq!(
        rel.spans[0].1,
        raw.find("analytics.orders").unwrap()
            ..raw.find("analytics.orders").unwrap() + "analytics.orders".len(),
        "the written name underlines itself"
    );
    // An unparsable text is `None` — never a position to guess.
    assert!(marked_relations("select from where or", &*SqlDialect::Generic.sqlparser()).is_none());
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
    // The `{{`/`}}` in separate string literals never pair — the SQL
    // parses instead of being eaten, so it must NOT fall back.
    assert!(!r.is_fallback("model.t.m"));
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.s", "id", ColumnEdgeKind::FilterOrJoin),
        e("model.t.up.t", "id", ColumnEdgeKind::FilterOrJoin),
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
    // `select u.c * 10 as c … where c > 2` — the WHERE's `c` exists in
    // the FROM scope and keeps meaning the FROM column, not the alias.
    // (`u.c` already holds the value edge, so its row-shaping fan drops —
    // same attachment policy as before.)
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["id", "c"]),
            model(
                "model.t.m",
                "select u.id, u.c * 10 as c from \"up\" u where c > 2",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
    let got = edge_set(&dag, &r, "model.t.m");
    let want: BTreeSet<_> = [
        e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
        e("model.t.up.c", "c", ColumnEdgeKind::Transform),
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
    // Aliases declare left to right: item 1's `c` can't see item 2's
    // `c` — the parent provably lacks `c`, so the unresolved ref
    // records like any other missing column.
    let dag = Dag::build(&manifest(
        vec![
            model("model.t.up", "select 1", &["a"]),
            model(
                "model.t.m",
                "select u.c as x, u.a + 1 as c from \"up\" u",
                &[],
            ),
        ],
        &[("model.t.up", "model.t.m")],
    ));
    let r = resolver();
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
