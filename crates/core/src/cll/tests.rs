use super::*;
use crate::manifest::{RawManifest, RawNode};
use std::collections::HashMap;

/// A `RawNode` literal with a raw SQL body and optional declared columns.
fn node(uid: &str, sql: &str, cols: &[&str]) -> (String, RawNode) {
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
            raw_code: Some(sql.to_string()),
            columns,
            original_file_path: Some(file),
            ..Default::default()
        },
    )
}

/// A minimal manifest of `uid -> (sql, declared cols)` models plus
/// `parent -> child` topology.
fn manifest(specs: &[(&str, &str, &[&str])], edges: &[(&str, &str)]) -> RawManifest {
    let mut nodes = HashMap::new();
    for (uid, sql, cols) in specs {
        let (k, v) = node(uid, sql, cols);
        nodes.insert(k, v);
    }
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
        nodes,
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
    }
}

/// `edge("model.p.stg.id", "model.p.cll.order_id", Rename)` — the uid is
/// everything before the last `.` (uids themselves contain dots).
fn edge(from: &str, to: &str, kind: ColumnEdgeKind) -> ColumnEdge {
    let (fn_, fc) = from.rsplit_once('.').unwrap();
    let (tn, tc) = to.rsplit_once('.').unwrap();
    ColumnEdge {
        from: ColumnRef::new(fn_, fc),
        to: ColumnRef::new(tn, tc),
        kind,
        confidence: Confidence::NameMatch,
    }
}

/// The acceptance scenario in miniature: `cll` reads `stg`'s `id` (rename),
/// `status` (passthrough), `cc` (transform) and the ON-clause-only
/// `agg.country` / WHERE-only `df` (filter-join); `count(*)` gets nothing.
fn fixture_dag() -> Dag {
    Dag::build(&manifest(
        &[
            (
                "model.p.stg",
                "select * from up",
                &["id", "status", "cc", "df"],
            ),
            ("model.p.agg", "select country, count(*) as n from x", &[]),
            (
                "model.p.cll",
                "select o.id as order_id, o.status, upper(o.cc) as cc, \
                 count(*) as n_orders \
                 from stg o left join agg a on o.cc = a.country \
                 where o.df = false \
                 group by o.id, o.status, o.cc",
                &[],
            ),
        ],
        &[
            ("model.p.stg", "model.p.cll"),
            ("model.p.agg", "model.p.cll"),
        ],
    ))
}

#[test]
fn resolves_all_four_edge_kinds() {
    let dag = fixture_dag();
    let r = NameMatchResolver::new(None);
    let edges: BTreeSet<ColumnEdge> = r.resolve(&dag, "model.p.cll").into_iter().collect();
    let mut want: BTreeSet<ColumnEdge> = [
        edge(
            "model.p.stg.id",
            "model.p.cll.order_id",
            ColumnEdgeKind::Rename,
        ),
        edge(
            "model.p.stg.status",
            "model.p.cll.status",
            ColumnEdgeKind::Passthrough,
        ),
        edge(
            "model.p.stg.cc",
            "model.p.cll.cc",
            ColumnEdgeKind::Transform,
        ),
    ]
    .into_iter()
    .collect();
    // FilterOrJoin fans out to every output column: `df` (WHERE-only) and
    // `country` (join-ON-only) each reach all four outputs.
    for c in ["order_id", "status", "cc", "n_orders"] {
        want.insert(edge(
            "model.p.stg.df",
            &format!("model.p.cll.{c}"),
            ColumnEdgeKind::FilterOrJoin,
        ));
        want.insert(edge(
            "model.p.agg.country",
            &format!("model.p.cll.{c}"),
            ColumnEdgeKind::FilterOrJoin,
        ));
    }
    assert_eq!(edges, want);
}

#[test]
fn star_select_expands_to_parent_columns() {
    // stg has no declared columns: `select *` outputs the parent's whole
    // column list, each passthrough'd from the same-named parent column.
    let dag = Dag::build(&manifest(
        &[
            ("model.p.up", "select 1", &["a", "b"]),
            ("model.p.stg", "select * from up", &[]),
        ],
        &[("model.p.up", "model.p.stg")],
    ));
    let r = NameMatchResolver::new(None);
    assert_eq!(r.columns(&dag, "model.p.stg"), vec!["a", "b"]);
    let edges = r.resolve(&dag, "model.p.stg");
    assert_eq!(
        edges,
        vec![
            edge("model.p.up.a", "model.p.stg.a", ColumnEdgeKind::Passthrough),
            edge("model.p.up.b", "model.p.stg.b", ColumnEdgeKind::Passthrough),
        ]
    );
}

#[test]
fn closures_walk_column_edges_transitively() {
    let dag = fixture_dag();
    let r = NameMatchResolver::new(None);
    let lin = ColumnLineage::new(&dag, &r);
    let stg_status = ColumnRef::new("model.p.stg", "status");
    let down = lin.closure_down(&stg_status);
    assert_eq!(
        down.iter()
            .map(|c| format!("{}.{}", c.node, c.column))
            .collect::<Vec<_>>(),
        vec!["model.p.cll.status".to_string()]
    );
    // `stg.df` is WHERE-only in cll: its downstream closure covers every
    // output column of cll.
    let df_down = lin.closure_down(&ColumnRef::new("model.p.stg", "df"));
    assert_eq!(df_down.len(), 4, "filter column reaches all outputs");
    // And the reverse closure answers the inverse question.
    let up = lin.closure_up(&ColumnRef::new("model.p.cll", "order_id"));
    assert_eq!(
        up.iter()
            .map(|c| format!("{}.{}", c.node, c.column))
            .collect::<Vec<_>>(),
        vec![
            "model.p.agg.country".to_string(),
            "model.p.stg.df".to_string(),
            "model.p.stg.id".to_string()
        ]
    );
    assert!(!up.contains(&ColumnRef::new("model.p.cll", "order_id")));
}

#[test]
fn repeated_queries_are_deterministic_and_lazy() {
    let dag = fixture_dag();
    let r = NameMatchResolver::new(None);
    let lin = ColumnLineage::new(&dag, &r);
    let a = lin.downstream(&ColumnRef::new("model.p.stg", "cc"));
    let b = lin.downstream(&ColumnRef::new("model.p.stg", "cc"));
    assert_eq!(a, b);
    let all = lin.resolve_all();
    let mut sorted = all.clone();
    sorted.sort();
    assert_eq!(all, sorted, "resolve_all emits sorted edges");
}

#[test]
fn normalization_rules_are_v0_canonical() {
    assert_eq!(normalize_column("  Status "), "status");
    assert_eq!(normalize_column("\"Quoted\""), "Quoted");
    assert_eq!(normalize_column("`Ticked`"), "Ticked");
    assert_eq!(normalize_column("[Bracketed]"), "Bracketed");
    // The same spelling unquoted vs quoted must NOT unify in v0.
    assert_ne!(normalize_column("Mixed"), normalize_column("\"Mixed\""));
}

/// The 08-swappability contract: a resolver returning hand-written edges
/// works through the same engine untouched.
struct StubResolver;
impl ColumnResolver for StubResolver {
    fn resolve(&self, _dag: &Dag, _node: &str) -> Vec<ColumnEdge> {
        vec![edge(
            "model.p.up.a",
            "model.p.stg.a",
            ColumnEdgeKind::Passthrough,
        )]
    }
}

#[test]
fn engine_runs_on_any_column_resolver() {
    let dag = fixture_dag();
    let stub = StubResolver;
    let lin = ColumnLineage::new(&dag, &stub);
    let up = lin.upstream(&ColumnRef::new("model.p.stg", "a"));
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].kind, ColumnEdgeKind::Passthrough);
}

#[test]
fn stale_compiled_sql_marks_edges_stale() {
    // A node whose resolution text is a stale compiled file reports
    // Confidence::Stale on every edge — the column-level "you're looking
    // at old SQL" flag.
    let m = manifest(
        &[
            ("model.p.up", "select 1", &["a"]),
            ("model.p.stg", "select a from up", &[]),
        ],
        &[("model.p.up", "model.p.stg")],
    );
    let mut dag = Dag::build(&m);
    dag.set_compiled_for_test("model.p.stg", "select a from up", true);
    let r = NameMatchResolver::new(None);
    let edges = r.resolve(&dag, "model.p.stg");
    assert!(!edges.is_empty());
    assert!(
        edges.iter().all(|e| e.confidence == Confidence::Stale),
        "stale compiled text marks every edge Stale"
    );
}

#[test]
fn masking_keeps_comments_and_literals_out_of_resolution() {
    // `--`-commented and string-embedded column names must not produce
    // edges — Jinja-visibility rules do NOT apply to column identifiers.
    let dag = Dag::build(&manifest(
        &[
            ("model.p.up", "select 1", &["a", "b"]),
            (
                "model.p.stg",
                "select a from up -- also b\nwhere 'b' = a",
                &[],
            ),
        ],
        &[("model.p.up", "model.p.stg")],
    ));
    let r = NameMatchResolver::new(None);
    let edges = r.resolve(&dag, "model.p.stg");
    assert!(
        edges.iter().all(|e| e.from.column != "b"),
        "comment/literal columns invisible: {edges:?}"
    );
    assert!(edges.iter().any(|e| e.from.column == "a"));
}

// ---- scanner regressions: quoted / qualified identifiers + the word
// stoplist. `parse_select`/`idents`/`keyword_at` are unit-tested directly;
// `resolve` tests pin the phantom-edge prevention end to end. ----

#[test]
fn sql_words_stay_sorted_for_binary_search() {
    assert!(
        SQL_WORDS.windows(2).all(|p| p[0] < p[1]),
        "SQL_WORDS must stay sorted"
    );
    assert!(is_sql_keyword("order") && is_sql_keyword("timestamp"));
    assert!(!is_sql_keyword("status") && !is_sql_keyword("n_orders"));
}

#[test]
fn keyword_at_rejects_qualified_segments() {
    assert!(!keyword_at(b"t.from", 2, b"from"), "t.from is a column");
    assert!(keyword_at(b"a from b", 2, b"from"));
    assert!(!keyword_at(b"myselect", 0, b"select"));
    assert!(!keyword_at(b"selection", 0, b"select"));
}

#[test]
fn quoted_and_qualified_keywords_do_not_corrupt_the_parse() {
    // `t.from` must not end the select list; `"union"` / `` `except` ``
    // must not act as set-operator boundaries; a comma inside quotes must
    // not split items. All three were column-extraction killers.
    let sql = "select t.id, t.from, x as \"union\", \"a,b\" as pair \
               from stg t join o on t.y = o.`except`";
    let p = parse_select(&mask_sql(sql));
    let aliases: Vec<Option<&str>> = p.items.iter().map(|i| i.alias.as_deref()).collect();
    assert_eq!(
        aliases,
        vec![Some("id"), Some("from"), Some("union"), Some("pair")],
        "every item survives: {:?}",
        p.items
    );
    assert!(
        p.filter_idents.contains("except"),
        "a quoted keyword is still an identifier to name-match: {:?}",
        p.filter_idents
    );
    assert!(
        !p.filter_idents.contains("join") && !p.filter_idents.contains("on"),
        "bare join keywords are not idents: {:?}",
        p.filter_idents
    );
}

#[test]
fn set_operators_still_bound_the_first_arm() {
    let p = parse_select(&mask_sql(
        "select a as \"except\" from t union select b from u",
    ));
    assert_eq!(p.items.len(), 1, "one first-arm item: {:?}", p.items);
    assert_eq!(p.items[0].alias.as_deref(), Some("except"));
    assert!(p.filter_idents.contains("t"));
    assert!(
        !p.filter_idents.contains("u") && !p.filter_idents.contains("b"),
        "second union arm invisible: {:?}",
        p.filter_idents
    );
}

#[test]
fn idents_skip_brackets_and_bare_keywords_but_keep_marked_uses() {
    let ids = idents(b"arr[i] t.order \"left\" order by timestamp(1)");
    assert!(ids.contains("arr"), "arr outside brackets: {ids:?}");
    assert!(!ids.contains("i"), "[i] is array indexing: {ids:?}");
    assert!(
        ids.contains("order"),
        "t.order is qualified — keywords can't be: {ids:?}"
    );
    assert!(
        ids.contains("left"),
        "\"left\" quoted marks identifier intent: {ids:?}"
    );
    for kw in ["by", "timestamp"] {
        assert!(!ids.contains(kw), "bare {kw} dropped: {ids:?}");
    }
}

#[test]
fn keywords_and_types_cannot_draw_phantom_edges() {
    // The parent literally carries `date`/`order`/`from` columns:
    // `cast(x as date)`'s type name and a bare `order by` must not
    // name-match them, while the qualified `t.date`/`t.from` still do.
    let dag = Dag::build(&manifest(
        &[
            ("model.p.up", "select 1", &["a", "date", "order", "from"]),
            (
                "model.p.stg",
                "select t.a, cast(t.a as date) as d, t.date as dt, t.from as f \
                 from up t order by t.a",
                &[],
            ),
        ],
        &[("model.p.up", "model.p.stg")],
    ));
    let r = NameMatchResolver::new(None);
    let edges = r.resolve(&dag, "model.p.stg");
    assert!(
        edges
            .iter()
            .all(|e| !(e.to.column == "d" && e.from.column == "date")),
        "`cast(.. as date)` is a type, not a source: {edges:?}"
    );
    assert!(
        edges.iter().all(|e| e.from.column != "order"),
        "bare `order by` keyword draws no edge: {edges:?}"
    );
    assert!(edges.iter().any(|e| e.from.column == "date"
        && e.to.column == "dt"
        && e.kind == ColumnEdgeKind::Rename));
    assert!(edges.iter().any(|e| e.from.column == "from"
        && e.to.column == "f"
        && e.kind == ColumnEdgeKind::Rename));
}

#[test]
fn report_row_confidence_is_the_weakest_links() {
    // Row `p.x` feeds two in-scope columns whose edges were resolved from
    // different bases: a fresh name-match (a) and a STALE compiled file
    // (b). The row must report `stale` — the same most-conservative rule
    // as the report header, never fresher than the worst link.
    let m = manifest(
        &[
            ("model.p.p", "select 1", &["x"]),
            ("model.p.a", "select x from p", &[]),
            ("model.p.b", "select x from p", &[]),
            (
                "model.p.t",
                "select a.x as x1, b.x as x2 from a, b",
                &["x1"],
            ),
        ],
        &[
            ("model.p.p", "model.p.a"),
            ("model.p.p", "model.p.b"),
            ("model.p.a", "model.p.t"),
            ("model.p.b", "model.p.t"),
        ],
    );
    let mut dag = Dag::build(&m);
    dag.set_compiled_for_test("model.p.b", "select x from p", true);
    let r = NameMatchResolver::new(None);
    let report = closure_report(
        &dag,
        &r,
        &ColumnRef::new("model.p.t", "x1"),
        ClosureDirection::Upstream,
    );
    assert_eq!(report.confidence, Confidence::Stale);
    let row = report
        .columns
        .iter()
        .find(|c| c.column == "p.x")
        .expect("p.x is reached");
    assert_eq!(
        row.confidence,
        Confidence::Stale,
        "one stale link marks the whole row stale"
    );
}
