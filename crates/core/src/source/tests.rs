use super::*;

/// `Vars` straight from maps — `file` models vars.yml's `vars:` map (the
/// `+enabled` render table), `project` models dbt_project.yml's `vars:`
/// map (the node-render table when no vars.yml exists); `proj` is the
/// `name:` `vars: <proj>:` scopes under.
fn test_vars(file: serde_json::Value, project: serde_json::Value, proj: &str) -> Vars {
    Vars::from_maps(
        file.as_object().cloned().unwrap_or_default(),
        project.as_object().cloned().unwrap_or_default(),
        proj,
    )
}

/// No vars anywhere — every `var()` falls to its default or unresolvable.
fn no_vars() -> Vars {
    test_vars(serde_json::json!({}), serde_json::json!({}), "p")
}

/// `pat.disabled` with fresh warnings — returns the (disabled, warnings) pair.
fn disabled(sql: &str, vars: &Vars) -> (bool, Vec<String>) {
    let mut w = Vec::new();
    let d = Patterns::new().disabled(sql, vars, &mut w, "models/m.sql");
    (d, w)
}

#[test]
fn config_extractors_scope_to_config_calls_in_any_arg_order() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    // The key matches even AFTER a prior argument inside the same call.
    assert_eq!(
        pat.materialized("{{ config(enabled=true, materialized='table') }}"),
        Some("table".to_string())
    );
    assert!(pat.disabled(
        "{{ config(materialized='view', enabled=false) }}",
        &vars,
        &mut w,
        "m.sql"
    ));
    assert!(pat.disabled("{{ config(enabled=False) }}", &vars, &mut w, "m.sql"));
    // Outside a config() call the keys are inert (dbt only honours them
    // inside config()).
    assert_eq!(pat.materialized("-- materialized='ephemeral'"), None);
    assert!(!pat.disabled("where enabled = false", &vars, &mut w, "m.sql"));
    assert!(!pat.disabled("-- note: enabled=false someday", &vars, &mut w, "m.sql"));
    assert!(!pat.disabled("{% set enabled=false %}", &vars, &mut w, "m.sql"));
}

#[test]
fn doc_blocks_extract_names_and_trim_bodies() {
    let pat = Patterns::new();
    // Multiline, inline, and whitespace-controlled (`{%- -%}`) blocks;
    // bodies are trimmed but interior whitespace survives (dbt's
    // `block_contents.strip()` — verified against real dbt).
    let md = "preamble\n\
              {% docs a %}\n  Body A.\n  still A\n{% enddocs %}\n\
              mid\n\
              {% docs b %}Body B.{% enddocs %}\n\
              {%- docs c -%}\n Body C. \n{%- enddocs -%}\n";
    assert_eq!(
        pat.doc_blocks(md),
        vec![
            ("a".to_string(), "Body A.\n  still A".to_string()),
            ("b".to_string(), "Body B.".to_string()),
            ("c".to_string(), "Body C.".to_string()),
        ]
    );
    // A `{% docs %}` tag inside `{# #}` is invisible to dbt — extraction
    // runs on the Jinja-comment-stripped text, so it finds nothing.
    let hidden = strip_jinja_comments_text("{# {% docs h %}x{% enddocs %} #}");
    assert!(pat.doc_blocks(&hidden).is_empty());
    // A `{# #}` inside a body disappears entirely (text variant, like
    // dbt's Jinja render of the block contents).
    let body = strip_jinja_comments_text("{% docs j %}a{# c #}b{% enddocs %}");
    assert_eq!(
        pat.doc_blocks(&body),
        vec![("j".to_string(), "ab".to_string())]
    );
    // A digit-leading name doesn't match dbt's block-name rule.
    assert!(pat.doc_blocks("{% docs 9bad %}x{% enddocs %}").is_empty());
}

#[test]
fn render_doc_refs_substitutes_and_warns() {
    let docs = DocBlocks::new(
        "p".to_string(),
        BTreeMap::from([
            ("a".to_string(), "Body A.".to_string()),
            ("b".to_string(), "Body B.".to_string()),
        ]),
    );
    let mut w = Vec::new();
    // Pure ref, quote/space/whitespace-control variants, mixed text.
    assert_eq!(docs.render("{{ doc('a') }}", &mut w), "Body A.");
    assert_eq!(docs.render("{{doc(\"a\")}}", &mut w), "Body A.");
    assert_eq!(docs.render("{{- doc( 'a' ) -}}", &mut w), "Body A.");
    assert_eq!(
        docs.render("pre {{ doc('a') }} post", &mut w),
        "pre Body A. post"
    );
    // 2-arg with the project itself resolves locally (verified vs dbt).
    assert_eq!(docs.render("{{ doc('p', 'b') }}", &mut w), "Body B.");
    assert!(w.is_empty(), "resolved refs warn nothing: {w:?}");

    // Missing block: raw `{{ doc() }}` text is kept (dbt would error —
    // dbtl degrades), with ONE dedup'd warning per ref.
    assert_eq!(
        docs.render("pre {{ doc('ghost') }} post", &mut w),
        "pre {{ doc('ghost') }} post"
    );
    assert_eq!(
        docs.render("{{ doc('ghost') }}", &mut w),
        "{{ doc('ghost') }}"
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("ghost"), "{w:?}");

    // A foreign package never resolves — even when the name exists
    // locally — and the warning says why (package scope, not a missing
    // block).
    w.clear();
    assert_eq!(
        docs.render("{{ doc('other', 'a') }}", &mut w),
        "{{ doc('other', 'a') }}"
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("not this project"), "{w:?}");

    // Text without doc() refs passes through untouched.
    assert_eq!(docs.render("plain text", &mut w), "plain text");
    assert_eq!(docs.render("a doc( b", &mut w), "a doc( b");
}

#[test]
fn render_doc_refs_strips_jinja_comments_like_dbt() {
    let docs = DocBlocks::new(
        "p".to_string(),
        BTreeMap::from([("a".to_string(), "Body A.".to_string())]),
    );
    let mut w = Vec::new();
    // dbt renders a description as Jinja first: `{# #}` comments (and
    // the doc() inside them) disappear ENTIRELY — no warning, no space.
    assert_eq!(docs.render("{# {{ doc('a') }} #} note", &mut w), " note");
    assert_eq!(docs.render("a{# c #}b", &mut w), "ab");
    // A doc() ref hiding in a comment is never evaluated — an undefined
    // one warns NOTHING (dbt wouldn't error either).
    assert_eq!(docs.render("{# {{ doc('ghost') }} #}x", &mut w), "x");
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn render_doc_refs_honours_whitespace_control() {
    let docs = DocBlocks::new(
        "p".to_string(),
        BTreeMap::from([("a".to_string(), "Body A.".to_string())]),
    );
    let mut w = Vec::new();
    // `{{-`/` -}}` eat the whitespace adjacent to the tag (Jinja's
    // whitespace control — verified against real dbt).
    assert_eq!(docs.render("x {{- doc('a') -}} y", &mut w), "xBody A.y");
    assert_eq!(docs.render("x\n{{- doc('a') }}\ny", &mut w), "xBody A.\ny");
    assert_eq!(docs.render("x {{ doc('a') -}} y", &mut w), "x Body A.y");
    assert!(w.is_empty(), "{w:?}");
    // An UNRESOLVED ref keeps the whole raw match — adjacent whitespace
    // included — so the raw text round-trips unchanged.
    assert_eq!(
        docs.render("x {{- doc('ghost') -}} y", &mut w),
        "x {{- doc('ghost') -}} y"
    );
    assert_eq!(w.len(), 1, "{w:?}");
}

#[test]
fn render_warns_on_uninterpretable_doc_calls() {
    let docs = DocBlocks::new("p".to_string(), BTreeMap::new());
    let mut w = Vec::new();
    // A doc() call the strict pattern can't read (non-literal arg, bad
    // arg count) is a parse error for real dbt — degraded to a warning,
    // raw text kept, load unaffected.
    assert_eq!(
        docs.render("{{ doc(var('x')) }}", &mut w),
        "{{ doc(var('x')) }}"
    );
    assert_eq!(
        docs.render("{{ doc('a', 'b', 'c') }}", &mut w),
        "{{ doc('a', 'b', 'c') }}"
    );
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w.iter().all(|s| s.contains("uninterpretable")), "{w:?}");
    // A non-doc `{{ ... }}` expression is not our business — no warning.
    assert_eq!(docs.render("{{ 1 + 1 }}", &mut w), "{{ 1 + 1 }}");
    assert_eq!(w.len(), 2, "{w:?}");
}

#[test]
fn config_meta_parses_literal_dicts() {
    let m = config_meta(
        "{{ config(materialized='table', meta={'owner': 'alice', 'deep': {'x': 1}}) }}",
    )
    .expect("a meta dict parses");
    assert_eq!(m["owner"], serde_json::json!("alice"));
    assert_eq!(m["deep"], serde_json::json!({"x": 1}));
    // Numbers/bools/lists/tuples and double quotes all convert.
    let m = config_meta(
        r#"{{ config(meta={'n': 2, 'f': 1.5, 'b': True, 'l': [1, "x"], 't': (3,)}) }}"#,
    )
    .unwrap();
    assert_eq!(m["n"], serde_json::json!(2));
    assert_eq!(m["f"], serde_json::json!(1.5));
    assert_eq!(m["b"], serde_json::json!(true));
    assert_eq!(m["l"], serde_json::json!([1, "x"]));
    assert_eq!(m["t"], serde_json::json!([3]));
    // Jinja's lowercase literal spellings convert the same way.
    let m = config_meta(r#"{{ config(meta={'b': true, 'x': none}) }}"#).unwrap();
    assert_eq!(m["b"], serde_json::json!(true));
    assert_eq!(m["x"], serde_json::Value::Null);
    // `metadata=` is not `meta`; a `meta` inside comments/strings elsewhere
    // can't configure the node; unresolvable exprs drop rather than guess.
    assert!(config_meta("{{ config(metadata={'a': 1}) }}").is_none());
    assert!(config_meta("{{ config(meta=var('m')) }}").is_none());
    assert!(config_meta("-- no config here").is_none());
    assert!(config_meta("{{ config(meta={'a': 1} if x else {}) }}").is_none());
    assert!(config_meta("{{ config(meta='not-a-dict') }}").is_none());
}

#[test]
fn config_meta_merges_every_config_call_per_key() {
    // dbt applies config() calls in order and merges each meta dict per
    // top-level key — a later call replaces only the keys it names
    // (verified: {'a':1,'b':1} then {'b':2} → {'a':1,'b':2}).
    let m = config_meta(
        "{{ config(meta={'a': 1, 'b': 'first'}) }}\n\
         select 1\n\
         {{ config(meta={'b': 'second', 'c': 3}) }}",
    )
    .expect("both dicts merge");
    assert_eq!(m["a"], serde_json::json!(1));
    assert_eq!(m["b"], serde_json::json!("second"));
    assert_eq!(m["c"], serde_json::json!(3));
    // A NESTED dict is replaced wholesale, not deep-merged (dbt's shallow
    // per-key rule applies across calls the same as across intakes).
    let m = config_meta(
        "{{ config(meta={'d': {'x': 1}, 'keep': 1}) }}\n\
         {{ config(meta={'d': {'y': 2}}) }}",
    )
    .unwrap();
    assert_eq!(m["d"], serde_json::json!({"y": 2}));
    assert_eq!(m["keep"], serde_json::json!(1));
    // An unresolvable later kwarg can't clear an earlier literal — the
    // merge keeps what it could resolve (a subset of dbt's result).
    let m = config_meta("{{ config(meta={'a': 1}) }}\n{{ config(meta=var('x')) }}").unwrap();
    assert_eq!(m["a"], serde_json::json!(1));
}

#[test]
fn last_config_call_wins_for_scalar_kwargs() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    // dbt applies config() calls in order, each call's kwargs overriding
    // the earlier ones per key (verified):
    //   config(materialized='view')  then config(materialized='table')
    //   → materialized = 'table'
    //   config(enabled=false)        then config(enabled=true)
    //   → node stays ENABLED
    assert_eq!(
        pat.materialized("{{ config(materialized='view') }}\n{{ config(materialized='table') }}",),
        Some("table".to_string())
    );
    assert!(!pat.disabled(
        "{{ config(enabled=false) }}\n{{ config(enabled=true) }}",
        &vars,
        &mut w,
        "m.sql"
    ));
    assert!(pat.disabled(
        "{{ config(enabled=true) }}\n{{ config(enabled=false) }}",
        &vars,
        &mut w,
        "m.sql"
    ));
    // An unresolvable later value can't override an earlier literal one.
    assert_eq!(
        pat.materialized("{{ config(materialized='view') }}\n{{ config(materialized=var('m')) }}"),
        Some("view".to_string())
    );
}

#[test]
fn ref_regex_handles_versioned_and_extra_args() {
    let pat = Patterns::new();
    assert_eq!(pat.refs("{{ ref('m', v=2) }}"), vec!["m".to_string()]);
    assert_eq!(pat.refs("{{ ref('m', version=2) }}"), vec!["m".to_string()]);
    assert_eq!(
        pat.refs("{{ ref('pkg', 'm', v=2) }}"),
        vec!["m".to_string()]
    );
    // No cross-call over-matching: `[^)]*` must not cross the first `)`.
    assert_eq!(
        pat.refs("{{ ref('a') }} x {{ ref('b') }}"),
        vec!["a".to_string(), "b".to_string()]
    );
    // A non-literal arg (a macro call, not a quoted string) still drops,
    // like before this change.
    assert!(pat.refs("{{ ref(var('x')) }}").is_empty());
}

#[test]
fn yaml_scalars_resolve_like_pyyaml() {
    // REAL dbt loads every .yml with PyYAML (YAML 1.1): a PLAIN
    // `yes`/`no`/`on`/`off` scalar (any casing) is a bool, `017` is
    // octal, `1:30` sexagesimal — while `y`/`n`, `0o17`, `08`, `1e5`
    // (sign-less exponent!) stay strings.
    let yaml = |s: &str| {
        let mut w = Vec::new();
        yaml_to_json(s, &mut w).expect("yaml parses")
    };
    for s in [
        "yes", "Yes", "YES", "on", "On", "ON", "true", "True", "TRUE",
    ] {
        assert_eq!(yaml(s), serde_json::json!(true), "{s}");
    }
    for s in [
        "no", "No", "NO", "off", "Off", "OFF", "false", "False", "FALSE",
    ] {
        assert_eq!(yaml(s), serde_json::json!(false), "{s}");
    }
    for s in ["y", "n", "maybe", "0o17", "08", "1e5", "1.0e5"] {
        assert_eq!(yaml(s), serde_json::json!(s), "{s}");
    }
    // The PyYAML number forms the old 1.2-schema parser got WRONG.
    assert_eq!(yaml("017"), serde_json::json!(15), "octal");
    assert_eq!(yaml("1:30"), serde_json::json!(90), "sexagesimal");
    assert_eq!(yaml("0x1F"), serde_json::json!(31));
    assert_eq!(yaml("0b101"), serde_json::json!(5));
    assert_eq!(yaml("1_000"), serde_json::json!(1000));
    assert_eq!(yaml(".5"), serde_json::json!(0.5));
    assert_eq!(yaml("5."), serde_json::json!(5.0));
    assert_eq!(yaml("1.0e+5"), serde_json::json!(100000.0));
    for s in ["~", "null", "Null", "NULL"] {
        assert_eq!(yaml(s), serde_json::Value::Null, "{s}");
    }
    // An empty plain scalar is null too (`key:` → `{"key": null}`).
    assert_eq!(yaml("key:"), serde_json::json!({"key": null}));
    // A bool/num spelling nested in a mapping/sequence resolves too, and
    // a `yes:` KEY resolves like a `yes` value (PyYAML `True` → "true").
    assert_eq!(
        yaml("{flag: yes, list: [on, off], yes: v, 5: five}"),
        serde_json::json!({"flag": true, "list": [true, false], "true": "v", "5": "five"})
    );
}

#[test]
fn yaml_quoted_scalars_stay_strings() {
    // The whole point of the event-level parse: serde `Value`s erase
    // scalar style, but dbt's `"yes"`/`'no'`/`"017"` are STRINGS — only
    // the PLAIN spelling resolves. (The sample fixture's `shared: "yes"`
    // exercises this through the full loader.)
    let yaml = |s: &str| {
        let mut w = Vec::new();
        yaml_to_json(s, &mut w).expect("yaml parses")
    };
    for s in ["\"yes\"", "'no'", "\"on\"", "'017'", "\"1:30\"", "\"true\""] {
        assert_eq!(
            yaml(s),
            serde_json::json!(s.trim_matches(|c| c == '"' || c == '\'')),
            "{s}"
        );
    }
    // A block scalar keeps its literal text (and never resolves).
    assert_eq!(yaml("|\n  yes\n"), serde_json::json!("yes\n"));
    // A QUOTED mapping key stays a string where the plain one boolifies.
    assert_eq!(
        yaml("{ \"yes\": a, yes: b }"),
        serde_json::json!({"yes": "a", "true": "b"})
    );
}

#[test]
fn yaml_anchors_aliases_and_merge_keys() {
    let mut w = Vec::new();
    let v = yaml_to_json(
        "base: &b\n  a: 1\n  shared: yes\nleft:\n  <<: *b\n  a: 9\nright:\n  <<: [*b, {extra: e}]\n",
        &mut w,
    )
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "base": {"a": 1, "shared": true},
            "left": {"a": 9, "shared": true},      // explicit key beats merged
            "right": {"a": 1, "shared": true, "extra": "e"},
        })
    );
}

#[test]
fn meta_map_warns_on_non_mapping_meta() {
    // `meta: <scalar>` / `meta: [list]` is a dbt error — degraded to a
    // warning + empty map (the load still succeeds).
    let mut w = Vec::new();
    for bad in ["meta: 'a string'", "meta: [1, 2]", "meta: 7"] {
        let v = yaml_to_json(bad, &mut w).unwrap();
        let got = meta_map(&v["meta"], &mut w, "model 'x'");
        assert!(got.is_empty(), "{bad} yields no meta entries");
    }
    assert_eq!(w.len(), 3, "one warning per non-mapping meta: {w:?}");
    assert!(w[0].contains("model 'x'"), "warning names the entry: {w:?}");
    // Absent/Null meta is normal — no warning.
    let mut w = Vec::new();
    let v = yaml_to_json("meta:", &mut w).unwrap();
    assert!(meta_map(&v["meta"], &mut w, "model 'x'").is_empty());
    assert!(w.is_empty(), "null meta warns nothing: {w:?}");
}

#[test]
fn tree_config_meta_stringifies_keys_and_warns() {
    // `+meta` keys stringify like schema.yml meta keys (a `yes:` key
    // boolifies); a non-mapping `+meta` warns with its tree path.
    let mut w = Vec::new();
    let tree = yaml_to_json(
        "sample:\n  +meta:\n    yes: v\n    5: five\n  staging:\n    +meta: not-a-map\n",
        &mut w,
    )
    .unwrap();
    let cfg = resolve_tree_config(
        &tree,
        &["sample".to_string(), "staging".to_string()],
        "models".to_string(),
        &mut w,
        Some(&no_vars()),
    );
    assert_eq!(cfg.meta["true"], serde_json::json!("v"));
    assert_eq!(cfg.meta["5"], serde_json::json!("five"));
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("models.sample.staging"),
        "warning names the bad level: {w:?}"
    );
}

#[test]
fn config_unique_key_keeps_the_declared_shape() {
    // A bare string stays a string, a list stays a list — `Dag::build`
    // normalizes both to `Vec<String>`; here the raw `Value` is asserted.
    // The LAST resolvable `unique_key=` wins (ordered config() application,
    // same as `materialized`); an unresolvable expression can't override an
    // earlier literal.
    assert_eq!(
        config_unique_key("{{ config(unique_key='id') }}"),
        Some(serde_json::json!("id"))
    );
    assert_eq!(
        config_unique_key("{{ config(materialized='i', unique_key=['a', 'b']) }}"),
        Some(serde_json::json!(["a", "b"]))
    );
    assert_eq!(
        config_unique_key("{{ config(unique_key='x') }} {{ config(unique_key=['y','z']) }}"),
        Some(serde_json::json!(["y", "z"]))
    );
    assert_eq!(
        config_unique_key("{{ config(unique_key='id') }} {{ config(unique_key=var('k')) }}"),
        Some(serde_json::json!("id")),
        "the unresolvable expr can't shadow the earlier literal"
    );
    // Outside a config() call the key is inert.
    assert_eq!(config_unique_key("-- unique_key='id'"), None);
    assert_eq!(config_unique_key("select 1"), None);
}

/// A `SchemaSourceTable`/`SchemaSource` with just the freshness fields
/// populated — the [`effective_freshness`] inputs.
fn freshness_table(
    freshness: Option<Option<RawFreshness>>,
    config_freshness: Option<Option<RawFreshness>>,
) -> SchemaSourceTable {
    SchemaSourceTable {
        freshness,
        config: SchemaSourceConfig {
            freshness: config_freshness,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn freshness_source(
    freshness: Option<Option<RawFreshness>>,
    config_freshness: Option<Option<RawFreshness>>,
) -> SchemaSource {
    SchemaSource {
        freshness,
        config: SchemaSourceConfig {
            freshness: config_freshness,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn effective_freshness_merges_per_threshold() {
    let t = |count: i64| RawFreshnessThreshold {
        count: Some(count),
        period: Some("hour".to_string()),
    };
    let src = freshness_source(
        Some(Some(RawFreshness {
            warn_after: Some(t(12)),
            error_after: Some(t(24)),
        })),
        None,
    );
    // A table declaring only `error_after` keeps the inherited warn_after —
    // the per-threshold merge real dbt does (verified on 1.12.6).
    let tbl = freshness_table(
        Some(Some(RawFreshness {
            warn_after: None,
            error_after: Some(t(6)),
        })),
        None,
    );
    let out = effective_freshness(&tbl, &src, &None).expect("merged");
    assert_eq!(out.warn_after.and_then(|x| x.count), Some(12));
    assert_eq!(out.error_after.and_then(|x| x.count), Some(6));
    // An absent table level inherits the source wholesale.
    let out = effective_freshness(&freshness_table(None, None), &src, &None).expect("inherited");
    assert_eq!(out.warn_after.and_then(|x| x.count), Some(12));
    assert_eq!(out.error_after.and_then(|x| x.count), Some(24));
    // `freshness: null` on the table clears everything — the walk STOPS at
    // the explicit null (the tree `+freshness` never lands either).
    let tree = Some(RawFreshness {
        warn_after: Some(t(48)),
        error_after: None,
    });
    assert!(effective_freshness(&freshness_table(Some(None), None), &src, &tree).is_none());
    // But a source-level `freshness: null` only stops the levels BELOW the
    // table: the table's own declared thresholds still count.
    let out =
        effective_freshness(&tbl, &freshness_source(Some(None), None), &tree).expect("table wins");
    assert_eq!(out.error_after.and_then(|x| x.count), Some(6));
    assert!(out.warn_after.is_none(), "source null stops its warn");
    // The tree is the lowest level: absent schema levels pass through to it.
    let out = effective_freshness(
        &freshness_table(None, None),
        &freshness_source(None, None),
        &tree,
    )
    .expect("tree lands");
    assert_eq!(out.warn_after.and_then(|x| x.count), Some(48));
    // Nothing declared anywhere → no freshness object at all.
    assert!(effective_freshness(
        &freshness_table(None, None),
        &freshness_source(None, None),
        &None
    )
    .is_none());
}

#[test]
fn config_blocks_join_the_freshness_chain_in_dbt_order() {
    let t = |count: i64| RawFreshnessThreshold {
        count: Some(count),
        period: Some("hour".to_string()),
    };
    let f = |warn: Option<i64>, error: Option<i64>| {
        Some(Some(RawFreshness {
            warn_after: warn.map(t),
            error_after: error.map(t),
        }))
    };
    // Precedence high → low: table config > table props > source config >
    // source props > `+freshness` (verified: a table's top-level
    // `freshness:` beats the source's `config.freshness`).
    let src = freshness_source(f(Some(1), Some(1)), f(Some(2), Some(2)));
    let tbl = freshness_table(f(Some(3), Some(3)), f(Some(4), Some(4)));
    let tree = Some(RawFreshness {
        warn_after: Some(t(5)),
        error_after: Some(t(5)),
    });
    let out = effective_freshness(&tbl, &src, &tree).expect("merged");
    assert_eq!(out.warn_after.and_then(|x| x.count), Some(4));
    assert_eq!(out.error_after.and_then(|x| x.count), Some(4));
    // Drop the table config: the table's top-level props win next.
    let tbl = freshness_table(f(Some(3), Some(3)), None);
    let out = effective_freshness(&tbl, &src, &tree).expect("merged");
    assert_eq!(out.error_after.and_then(|x| x.count), Some(3));
    // Then the source config, then the source props.
    let out = effective_freshness(&freshness_table(None, None), &src, &tree).expect("merged");
    assert_eq!(out.error_after.and_then(|x| x.count), Some(2));
    let src = freshness_source(f(Some(1), Some(1)), None);
    let out = effective_freshness(&freshness_table(None, None), &src, &tree).expect("merged");
    assert_eq!(out.error_after.and_then(|x| x.count), Some(1));
    // A source-config `freshness: null` stops the walk below it — but the
    // table's declared level still counts.
    let src = freshness_source(f(Some(1), Some(1)), Some(None));
    let tbl = freshness_table(f(None, Some(3)), None); // declares error only
    let out = effective_freshness(&tbl, &src, &tree).expect("table wins");
    assert_eq!(out.error_after.and_then(|x| x.count), Some(3));
    assert!(out.warn_after.is_none(), "the null stops source-level warn");
}

#[test]
fn source_table_three_state_fields_deserialize() {
    // `loaded_at_field:`/`freshness:` are `Option<Option<_>>`: ABSENT means
    // "inherit the higher level", an explicit `null` means "clear it".
    let mut w = Vec::new();
    let v = yaml_to_json(
        "tables:\n  - name: a\n    loaded_at_field: null\n    freshness: null\n  - name: b\n    loaded_at_field: ts\n",
        &mut w,
    )
    .unwrap();
    let tables: Vec<SchemaSourceTable> = serde_json::from_value(v["tables"].clone()).unwrap();
    assert!(matches!(tables[0].loaded_at_field, Some(None)));
    assert!(matches!(tables[0].freshness, Some(None)));
    assert_eq!(
        tables[1].loaded_at_field.clone().flatten(),
        Some("ts".to_string()),
        "a declared value overrides"
    );
    assert!(tables[1].freshness.is_none(), "absent means inherit");
}

#[test]
fn tree_config_reads_unique_key_and_freshness() {
    // `+unique_key` keeps its declared shape (string OR list — `Dag::build`
    // normalizes); `+freshness` parses into the wire freshness type; a
    // deeper level wins over a shallower one.
    let mut w = Vec::new();
    let tree = yaml_to_json(
        "sample:\n  +unique_key: base_uk\n  staging:\n    +unique_key: [a, b]\n    +freshness:\n      warn_after: {count: 48, period: hour}\n",
        &mut w,
    )
    .unwrap();
    let cfg = resolve_tree_config(
        &tree,
        &["sample".to_string(), "staging".to_string()],
        "sources".to_string(),
        &mut w,
        Some(&no_vars()),
    );
    assert_eq!(cfg.unique_key, Some(serde_json::json!(["a", "b"])));
    assert_eq!(
        cfg.freshness
            .as_ref()
            .and_then(|f| f.warn_after.as_ref())
            .and_then(|t| t.count),
        Some(48)
    );
    // A malformed `+freshness` warns instead of silently losing the block.
    let mut w = Vec::new();
    let tree = yaml_to_json("sample:\n  +freshness: 7\n", &mut w).unwrap();
    let cfg = resolve_tree_config(
        &tree,
        &["sample".to_string()],
        "sources".to_string(),
        &mut w,
        Some(&no_vars()),
    );
    assert!(cfg.freshness.is_none());
    assert_eq!(w.len(), 1, "{w:?}");
}

// ---- call_sites: the span-carrying extractors -------------------------------

#[test]
fn mask_jinja_comments_preserves_byte_length_and_lines() {
    // Every comment byte (markers included) becomes a space — except
    // newlines, so line numbers never shift. Length matches the input, so
    // an offset in the masked text indexes the original.
    let text = "a{# c1\n c2 #}b\nselect é -- keep me\n{# open";
    let masked = mask_jinja_comments(text);
    assert_eq!(masked.len(), text.len());
    assert_eq!(
        masked, "a     \n      b\nselect é -- keep me\n{# open",
        "the comment vanishes, newlines and the unclosed `{{#` stay"
    );
    // SQL comments are NOT masked — dbt sees through them.
    assert_eq!(
        mask_jinja_comments("-- {{ ref('x') }}"),
        "-- {{ ref('x') }}"
    );
    assert_eq!(mask_jinja_comments("/* ref('x') */"), "/* ref('x') */");
}

/// The full mask+scan path on raw SQL — what `call_sites` composes.
fn sites_of(sql: &str) -> Vec<CallSite> {
    call_sites_masked(&mask_jinja_comments(sql))
}

/// The `(name, package, version)` of a `Ref` site, sliced back out of
/// `sql` — kind fields are byte ranges into the (masked) text.
fn ref_args<'a>(sql: &'a str, site: &CallSite) -> (&'a str, Option<&'a str>, Option<&'a str>) {
    let CallKind::Ref {
        name,
        package,
        version,
    } = &site.kind
    else {
        panic!("expected a ref site, got {:?}", site.kind);
    };
    (
        &sql[name.clone()],
        package.as_ref().map(|r| &sql[r.clone()]),
        version.as_ref().map(|r| &sql[r.clone()]),
    )
}

/// The `(source, table)` of a `Source` site, sliced back out of `sql`.
fn source_args<'a>(sql: &'a str, site: &CallSite) -> (&'a str, &'a str) {
    let CallKind::Source { source, table } = &site.kind else {
        panic!("expected a source site, got {:?}", site.kind);
    };
    (&sql[source.clone()], &sql[table.clone()])
}

#[test]
fn call_sites_span_excludes_the_jinja_wrapper() {
    let sql = "select * from {{ ref('a') }} t\njoin {{ source('s', 'b') }} u on true\n";
    let sites = sites_of(sql);
    assert_eq!(sites.len(), 2);
    // `ref('a')` — the call, not the `{{ }}` around it.
    let r = &sites[0];
    assert_eq!(ref_args(sql, r), ("a", None, None));
    assert_eq!(&sql[r.span.clone()], "ref('a')");
    assert_eq!(&sql[r.args.clone().unwrap()], "'a'");
    let s = &sites[1];
    assert_eq!(source_args(sql, s), ("s", "b"));
    assert_eq!(&sql[s.span.clone()], "source('s', 'b')");
}

#[test]
fn call_sites_index_the_original_text_through_comments() {
    // The `{# #}` comment masks `ref('ghost')` out entirely AND keeps the
    // following `ref('b')` span pointing at the same bytes of `sql`.
    let sql = "{# {{ ref('ghost') }} #}\nselect * from {{ ref('b') }}\n";
    let sites = sites_of(sql);
    assert_eq!(sites.len(), 1);
    assert_eq!(&sql[sites[0].span.clone()], "ref('b')");
    assert_eq!(sites[0].span.start, sql.find("ref('b')").unwrap());
    // `--`/`/* */` comments do NOT hide calls — dbt renders Jinja before
    // SQL comments exist.
    let commented = "-- {{ ref('a') }}\n/* {{ source('s', 't') }} */";
    let sites = sites_of(commented);
    assert_eq!(sites.len(), 2);
    assert_eq!(ref_args(commented, &sites[0]), ("a", None, None));
    assert_eq!(source_args(commented, &sites[1]), ("s", "t"));
}

#[test]
fn call_sites_skip_embedded_identifiers_and_capture_packages_versions() {
    // `my_ref`/`other_source`/`refactor` embed the callee inside a longer
    // identifier — `\b` keeps them out (the dim_customers fixture case).
    assert!(
        sites_of("select my_ref('a'), other_source('s','t'), xrefactor('c') from d").is_empty()
    );
    // `ref('pkg','x')` resolves to `x` and records the package; `v=` and
    // `version=` kwargs both land in `version` as literal text.
    let sql = "{{ ref('pkg', 'x') }} {{ ref('y', v=2) }} {{ ref('z', version='v3') }}";
    let sites = sites_of(sql);
    assert_eq!(
        sites.iter().map(|c| ref_args(sql, c)).collect::<Vec<_>>(),
        vec![
            ("x", Some("pkg"), None),
            ("y", None, Some("2")),
            ("z", None, Some("v3")),
        ]
    );
}

#[test]
fn call_sites_rescan_balanced_args_and_report_duplicates() {
    // A `)` inside an argument can't truncate the call: the regex match
    // ends early but the balanced rescan lands the TRUE close.
    let sql = "{{ ref('a', other=func(1)) }}\n{{ ref('a') }}\n";
    let sites = sites_of(sql);
    assert_eq!(sites.len(), 2);
    assert_eq!(&sql[sites[0].span.clone()], "ref('a', other=func(1))");
    assert_eq!(&sql[sites[1].span.clone()], "ref('a')");
    // Every call is its own site — duplicates are the positions lint
    // labels individually.
    assert_eq!(
        sites_of(sql)
            .iter()
            .map(|c| c.span.start)
            .collect::<Vec<_>>(),
        sites_of(sql)
            .iter()
            .map(|c| c.span.start)
            .collect::<Vec<_>>(),
        "extraction is deterministic"
    );
    // A malformed call the regex still recognized keeps its match-range
    // span and reports no args (`ref('x', 'unclosed)`).
    let broken = sites_of("{{ ref('x', 'unclosed) }}");
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].args, None);
}

#[test]
fn call_sites_find_config_calls_for_metadata_positions() {
    // `config()` is a call site too — `args` carries the kwarg list for
    // readers like `materialized_arg`; identifier-embedded `my_config` is
    // not a call and nested parens can't truncate the arg scan.
    let sql = "{{ config(materialized='incremental', meta={'a': 'b)'}) }}\n{{ my_config(materialized='view') }}\nselect 1\n";
    let sites = sites_of(sql);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].kind, CallKind::Config);
    assert_eq!(
        &sql[sites[0].args.clone().unwrap()],
        "materialized='incremental', meta={'a': 'b)'}"
    );
    // A config() inside `{# #}` is invisible like any other call.
    assert!(sites_of("{# {{ config(materialized='x') }} #}").is_empty());
    // An unclosed config swallows the rest of the file — nothing after it
    // can still be a call.
    assert_eq!(
        sites_of("{{ config(materialized='x' {{ ref('a') }}").len(),
        1
    );
}

// ---- var()-driven `enabled` (the shared vars.rs evaluator) ------------------

#[test]
fn tree_enabled_resolves_var_forms_like_dbt() {
    // `+enabled` evaluates in dbt's project-render scope: vars.yml's
    // top-level `vars:` ONLY — never `vars:` (the project file renders
    // before it loads), never a `vars: <pkg>:` scope.
    let vars = test_vars(
        serde_json::json!({
            "flag_true": true, "flag_false": false,
            "s_True": "True", "s_False": "False", "s_lower": "false",
            "p": {"scoped": false},
        }),
        serde_json::json!({"proj_flag": true}),
        "p",
    );
    // `+enabled: <raw>` under `p:` → (resolved enabled, warnings).
    let tree = |raw: &str| {
        let mut w = Vec::new();
        let tree = yaml_to_json(&format!("p:\n  +enabled: {raw}\n"), &mut w).unwrap();
        let cfg = resolve_tree_config(
            &tree,
            &["p".to_string(), "m".to_string()],
            "models".to_string(),
            &mut w,
            Some(&vars),
        );
        (cfg.enabled, w)
    };
    // Native bools and var-resolved bools.
    assert_eq!(tree("true").0, Some(true));
    assert_eq!(tree("false").0, Some(false));
    assert_eq!(tree("\"{{ var('flag_true') }}\"").0, Some(true));
    assert_eq!(tree("\"{{ var('flag_false') }}\"").0, Some(false));
    // Missing vars take the literal default.
    assert_eq!(tree("\"{{ var('missing', false) }}\"").0, Some(false));
    assert_eq!(tree("\"{{ var('missing', true) }}\"").0, Some(true));
    // `vars:` and `vars: <p>:` are INVISIBLE at this pass (verified: dbt
    // renders dbt_project.yml before `vars:` loads, and scoped keys aren't
    // in the flat table) — `proj_flag` (vars:) and `scoped` (under `p:`)
    // both fall through to their defaults.
    assert_eq!(tree("\"{{ var('proj_flag', false) }}\"").0, Some(false));
    assert_eq!(tree("\"{{ var('scoped', true) }}\"").0, Some(true));
    // Whitespace control and double quotes.
    assert_eq!(tree("\"  {{- var('flag_false') -}}  \"").0, Some(false));
    assert_eq!(tree("'{{ var(\"flag_false\") }}'").0, Some(false));
    // Outer whitespace WITHOUT the matching `-` dash renders literally
    // (" False ") and fails dbt's enabled validation → unresolvable.
    for (enabled, w) in [
        tree("\" {{ var('flag_false') }} \""),
        tree("\" {{ var('flag_false') }}\""),
        tree("\"{{ var('flag_false') }} \""),
        tree("\" {{ var('flag_false') -}}\""),
        tree("\"{{- var('flag_false') }} \""),
    ] {
        assert_eq!(enabled, None, "literal outer whitespace: {w:?}");
        assert_eq!(w.len(), 1, "{w:?}");
    }
    // `| as_bool`: 'True'/'False' and bools resolve; 'false' does not
    // (dbt literal_evals and demands a bool) → unresolvable → enabled +
    // one warning naming the tree level.
    assert_eq!(tree("\"{{ var('s_True') | as_bool }}\"").0, Some(true));
    assert_eq!(tree("\"{{ var('s_False') | as_bool }}\"").0, Some(false));
    assert_eq!(tree("\"{{ var('flag_false') | as_bool }}\"").0, Some(false));
    let (enabled, w) = tree("\"{{ var('s_lower') | as_bool }}\"");
    assert_eq!(enabled, None, "'false' fails as_bool → unresolvable");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("models.p"), "the level is named: {w:?}");
    // Everything dbt's enabled validation would reject — a plain 'false'
    // string, a number, another expression, an unresolvable var — keeps
    // the node enabled with the same once-per-level warning.
    for bad in [
        "'false'",
        "0",
        "\"{{ env_var('X') }}\"",
        "\"{{ not var('flag_true') }}\"",
        "\"{{ var('missing') }}\"",
    ] {
        let (enabled, w) = tree(bad);
        assert_eq!(enabled, None, "{bad}");
        assert_eq!(w.len(), 1, "{bad}: {w:?}");
    }
    // An absent `+enabled` warns nothing.
    let mut w = Vec::new();
    resolve_tree_config(
        &serde_json::json!({"p": {}}),
        &["p".to_string()],
        "models".to_string(),
        &mut w,
        Some(&vars),
    );
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn disabled_resolves_var_forms_like_dbt() {
    // `config(enabled=var(...))` evaluates in the NODE scope — vars.yml
    // when declared (here), `vars: <proj>:` entries shadowing the top
    // level (the project's own `vars:` is shadowed by the file).
    let vars = test_vars(
        serde_json::json!({"flag": false, "p": {"scoped": false}}),
        serde_json::json!({"flag": true, "p": {"scoped": true}}),
        "p",
    );
    assert!(disabled("{{ config(enabled=var('flag')) }}", &vars).0);
    assert!(
        disabled("{{ config(enabled=var('scoped', true)) }}", &vars).0,
        "the scoped value (false) shadows the default"
    );
    assert!(!disabled("{{ config(enabled=var('missing', true)) }}", &vars).0);
    assert!(disabled("{{ config(enabled=var('flag') | as_bool) }}", &vars).0);
    // Last resolvable wins — a var-resolved value lands in the same order
    // as a literal one.
    assert!(
        !disabled(
            "{{ config(enabled=false) }}\n{{ config(enabled=var('missing', true)) }}",
            &vars
        )
        .0
    );
    assert!(
        disabled(
            "{{ config(enabled=true) }}\n{{ config(enabled=var('flag')) }}",
            &vars
        )
        .0
    );

    // With NO vars.yml the node scope is `vars:` — top-level and scoped.
    let vars = test_vars(
        serde_json::json!({}),
        serde_json::json!({"flag": false, "p": {"scoped": false}}),
        "p",
    );
    assert!(disabled("{{ config(enabled=var('flag')) }}", &vars).0);
    assert!(disabled("{{ config(enabled=var('scoped', true)) }}", &vars).0);

    // Unresolvable forms warn once (file + config named) and keep the
    // node enabled — `var('missing')` no-default, a non-var expression,
    // and the literal strings/numbers dbt's boolean validation rejects.
    let vars = no_vars();
    for sql in [
        "{{ config(enabled=var('missing')) }}",
        "{{ config(enabled=env_var('X')) }}",
        "{{ config(enabled='false') }}",
        "{{ config(enabled=0) }}",
    ] {
        let (d, w) = disabled(sql, &vars);
        assert!(!d, "{sql} keeps the node enabled");
        assert_eq!(w.len(), 1, "{sql}: {w:?}");
        assert!(w[0].contains("models/m.sql"), "the file is named: {w:?}");
        assert!(w[0].contains("enabled="), "the config is named: {w:?}");
    }
    // An unresolvable later value can't override an earlier `false`.
    let (d, w) = disabled(
        "{{ config(enabled=false) }}\n{{ config(enabled=var('missing')) }}",
        &vars,
    );
    assert!(d);
    assert_eq!(w.len(), 1, "{w:?}");
}

// ---- Vars::load + warning-path coverage -------------------------------------

/// A `DbtProject` with just `name`/`vars` populated (the only fields
/// [`Vars::load`] reads).
fn test_project(vars: serde_json::Value) -> DbtProject {
    serde_json::from_value(serde_json::json!({"name": "p", "vars": vars})).unwrap()
}

#[test]
fn vars_load_warns_once_and_vars_yml_wins_when_both_declared() {
    // vars.yml + a non-empty `vars:` is a combination dbt rejects —
    // one warning, and the file's table wins in BOTH scopes.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("vars.yml"), "vars:\n  flag: false\n").unwrap();
    let mut w = Vec::new();
    let vars = Vars::load(
        dir.path(),
        &test_project(serde_json::json!({"flag": true})),
        &mut w,
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("both"), "{w:?}");
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('flag') }}")),
        Some(false),
        "vars.yml wins the project-file scope"
    );
    assert_eq!(
        vars.node_enabled("var('flag')"),
        Some(false),
        "vars.yml wins the node scope"
    );
    // An EMPTY vars.yml vars: map isn't a "declaration" — no warning,
    // and the node scope falls back to `vars:` like dbt.
    std::fs::write(dir.path().join("vars.yml"), "vars: {}\n").unwrap();
    let mut w = Vec::new();
    let vars = Vars::load(
        dir.path(),
        &test_project(serde_json::json!({"flag": true})),
        &mut w,
    );
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(vars.node_enabled("var('flag')"), Some(true));
}

#[test]
fn vars_load_warns_on_malformed_vars_sources() {
    let dir = tempfile::tempdir().unwrap();
    // A non-mapping `vars:` in dbt_project.yml → warning, vars ignored.
    let mut w = Vec::new();
    let vars = Vars::load(dir.path(), &test_project(serde_json::json!([1, 2])), &mut w);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("vars:") && w[0].contains("not a mapping"),
        "{w:?}"
    );
    assert_eq!(vars.node_enabled("var('flag', true)"), Some(true));
    // An unparsable vars.yml → warning, contributes nothing.
    std::fs::write(dir.path().join("vars.yml"), "vars: {a: [unclosed\n").unwrap();
    let mut w = Vec::new();
    let vars = Vars::load(
        dir.path(),
        &test_project(serde_json::json!({"flag": false})),
        &mut w,
    );
    assert!(
        w.iter()
            .any(|s| s.contains("vars.yml") && s.contains("parse")),
        "{w:?}"
    );
    assert_eq!(
        vars.node_enabled("var('flag')"),
        Some(false),
        "vars: still backs the node scope"
    );
    // A non-mapping `vars:` inside vars.yml → the same ignored-vars warning.
    std::fs::write(dir.path().join("vars.yml"), "vars: not-a-map\n").unwrap();
    let mut w = Vec::new();
    Vars::load(dir.path(), &test_project(serde_json::json!({})), &mut w);
    assert!(
        w.iter()
            .any(|s| s.contains("vars.yml") && s.contains("not a mapping")),
        "{w:?}"
    );
}

#[test]
fn manifest_dedups_a_shared_unresolvable_tree_enabled_warning() {
    // Two models under one config level → the level's unresolvable
    // `+enabled` warns ONCE (the identical strings collapse in the
    // final dedup) and both nodes stay enabled.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models/grp")).unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nmodel-paths: [models]\nmodels:\n  p:\n    grp:\n      +enabled: \"{{ var('missing') }}\"\n",
    )
    .unwrap();
    for m in ["a", "b"] {
        std::fs::write(dir.path().join(format!("models/grp/{m}.sql")), "select 1\n").unwrap();
    }
    let (manifest, w) = manifest_from_source_with_warnings(dir.path()).unwrap();
    let enabled_warns: Vec<_> = w.iter().filter(|s| s.contains("'+enabled'")).collect();
    assert_eq!(enabled_warns.len(), 1, "{w:?}");
    assert!(enabled_warns[0].contains("models.p.grp"), "{w:?}");
    for m in ["a", "b"] {
        assert!(
            manifest.nodes.keys().any(|k| k.ends_with(&format!(".{m}"))),
            "{m} stays enabled"
        );
    }
}

#[test]
fn sources_tree_walk_does_not_warn_on_enabled() {
    // `+enabled` is not applied on `sources:` (meta-only walk) — passing
    // no var scope keeps it a silent read: no warning noise on a key
    // dbtl ignores, unresolvable or not.
    let mut w = Vec::new();
    let cfg = resolve_tree_config(
        &serde_json::json!({"p": {"+enabled": "{{ var('flag') }}"}}),
        &["p".to_string()],
        "sources".to_string(),
        &mut w,
        None,
    );
    assert_eq!(cfg.enabled, None);
    assert!(w.is_empty(), "{w:?}");
    // A native bool still lands on the (unapplied) field — same as
    // before the var evaluator existed.
    let cfg = resolve_tree_config(
        &serde_json::json!({"p": {"+enabled": false}}),
        &["p".to_string()],
        "sources".to_string(),
        &mut w,
        None,
    );
    assert_eq!(cfg.enabled, Some(false));
    assert!(w.is_empty(), "{w:?}");
}

// -- Jinja branch structure --------------------------------------------------

/// The arm path `text[offset]` sits inside — `jinja_branches` on the
/// masked text, like [`CallSiteResolver`](crate::lint) reads it.
fn path_at(text: &str, offset: usize) -> Vec<ArmStep> {
    jinja_branches(&mask_jinja_comments(text))
        .arm_path(offset)
        .to_vec()
}

#[test]
fn jinja_branches_index_if_elif_else_arms() {
    let sql = "a{% if x %}b{% elif y %}c{% else %}d{% endif %}e";
    let arm = |c: &str| path_at(sql, sql.find(c).unwrap());
    assert_eq!(arm("a"), vec![]);
    assert_eq!(arm("b"), vec![ArmStep { chain: 0, arm: 0 }]);
    assert_eq!(arm("c"), vec![ArmStep { chain: 0, arm: 1 }]);
    assert_eq!(arm("d"), vec![ArmStep { chain: 0, arm: 2 }]);
    assert_eq!(path_at(sql, sql.rfind('e').unwrap()), vec![]);
    // `{% if %}`/`{% elif %}`/`{% else %}` arms are mutually exclusive.
    assert!(arms_exclusive(&arm("b"), &arm("c")));
    assert!(arms_exclusive(&arm("b"), &arm("d")));
    assert!(arms_exclusive(&arm("c"), &arm("d")));
    // …an unconditional position and the same arm never are.
    assert!(!arms_exclusive(&arm("a"), &arm("b")));
    assert!(!arms_exclusive(&arm("b"), &arm("b")));
}

#[test]
fn jinja_branches_nest_and_compose() {
    // Chain 0's arms each hold their own chain — nesting composes, only
    // same-chain arm disagreements are exclusive.
    let sql = "{% if a %}{% if b %}x{% else %}y{% endif %}{% else %}z{% endif %}";
    let arm = |c: &str| path_at(sql, sql.find(c).unwrap());
    let (x, y, z) = (arm("x"), arm("y"), arm("z"));
    assert_eq!(
        x,
        vec![ArmStep { chain: 0, arm: 0 }, ArmStep { chain: 1, arm: 0 }]
    );
    assert_eq!(
        y,
        vec![ArmStep { chain: 0, arm: 0 }, ArmStep { chain: 1, arm: 1 }]
    );
    assert_eq!(z, vec![ArmStep { chain: 0, arm: 1 }]);
    // `x`/`y` share the outer arm but disagree inside it → exclusive;
    // `x`/`z` disagree on the outer chain → exclusive too.
    assert!(arms_exclusive(&x, &y));
    assert!(arms_exclusive(&x, &z));
}

#[test]
fn jinja_branches_for_else_and_macro_blocks() {
    // A `{% for %}`'s `{% else %}` arm is exclusive with its body (the
    // empty-iteration fallback dbt renders instead of it); a
    // `{% macro %}` block nests transparently — the arms inside it still
    // index under their own chain.
    let sql =
        "{% for i in xs %}a{% else %}b{% endfor %}{% macro m() %}{% if c %}z{% endif %}{% endmacro %}";
    let arm = |c: &str| path_at(sql, sql.find(c).unwrap());
    assert!(arms_exclusive(&arm("a"), &arm("b")));
    assert_eq!(arm("z"), vec![ArmStep { chain: 1, arm: 0 }]);
    // The `{% macro %}` block itself isn't an arm — `a` and `z` compose
    // freely (different chains, no disagreement).
    assert!(!arms_exclusive(&arm("a"), &arm("z")));
}

#[test]
fn jinja_branches_malformed_structure_is_unconditional() {
    for sql in [
        "{% if %}x",                                  // unclosed
        "{% if %}x{% endif %}{% else %}y",            // stray else
        "{% if %}x{% else %}y{% elif %}z{% endif %}", // elif after else
        "{% if %}x{% endfor %}",                      // mismatched end
        "{% raw %}x",                                 // unclosed raw
    ] {
        let b = jinja_branches(&mask_jinja_comments(sql));
        for off in [0, sql.len().saturating_sub(1)] {
            assert_eq!(
                b.arm_path(off),
                &[],
                "{sql:?} should read unconditionally at offset {off}"
            );
        }
    }
    // An unterminated `{% if %}` mid-file marks the REST text — nothing
    // after it carries a path either.
    let b = jinja_branches("ok\n{% if x %}never closed");
    assert_eq!(b.arm_path(4), &[]);
    // `{% raw %}` bodies are inert — a `{% if %}` inside opens nothing.
    let sql = "{% raw %}{% if %}x{% endif %}{% endraw %}y";
    assert_eq!(path_at(sql, sql.find('x').unwrap()), vec![]);
    assert_eq!(path_at(sql, sql.find('y').unwrap()), vec![]);
}

// ---- config({...}) positional-dict spelling ---------------------------------

#[test]
fn config_dict_arg_reads_keys_like_kwargs() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    // The dict spelling is the same call's kwargs (verified: real dbt
    // materializes `config({"materialized": "table"})` identically to the
    // kwarg form).
    assert_eq!(
        pat.materialized(r#"{{ config({"materialized": "table"}) }}"#),
        Some("table".to_string())
    );
    // Single-quoted keys and whitespace work the same.
    assert_eq!(
        pat.materialized("{{ config({ 'materialized' : 'ephemeral' }) }}"),
        Some("ephemeral".to_string())
    );
    // `enabled`/`alias`/`unique_key`/`meta` and the governance readers all
    // route through the same lookup.
    assert!(pat.disabled(
        r#"{{ config({"enabled": false}) }}"#,
        &vars,
        &mut w,
        "m.sql"
    ));
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(
        config_alias(r#"{{ config({"alias": "renamed"}) }}"#),
        Some("renamed".to_string())
    );
    assert_eq!(
        config_unique_key(r#"{{ config({"unique_key": ["a", "b"]}) }}"#),
        Some(serde_json::json!(["a", "b"]))
    );
    let m = config_meta(r#"{{ config({"meta": {"owner": "alice"}}) }}"#).unwrap();
    assert_eq!(m["owner"], serde_json::json!("alice"));
    let gov = config_governance(r#"{{ config({"group": "g1", "access": "private"}) }}"#);
    assert_eq!(gov.group.as_deref(), Some("g1"));
    assert_eq!(gov.access.as_deref(), Some("private"));
}

#[test]
fn config_dict_arg_joins_the_ordered_call_sequence() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    // The dict call is one step of the same last-wins chain the kwarg calls
    // follow (verified: real dbt applies them in order).
    assert_eq!(
        pat.materialized(
            "{{ config(materialized='view') }}\n{{ config({\"materialized\": \"table\"}) }}",
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.materialized(
            "{{ config({\"materialized\": \"table\"}) }}\n{{ config(materialized='view') }}",
        ),
        Some("view".to_string())
    );
    assert!(pat.disabled(
        "{{ config(enabled=true) }}\n{{ config({\"enabled\": false}) }}",
        &vars,
        &mut w,
        "m.sql"
    ));
    // A later LITERAL entry overrides; an unresolvable VALUE inside a dict
    // can't clear an earlier literal — the same stance unresolvable kwargs
    // take. Literal keys around it still land.
    assert_eq!(
        pat.materialized(
            "{{ config(materialized='view') }}\n{{ config({\"materialized\": var('m'), \"group\": \"g\"}) }}",
        ),
        Some("view".to_string())
    );
    let gov = config_governance("{{ config({\"materialized\": var('m'), \"group\": \"g\"}) }}");
    assert_eq!(gov.group.as_deref(), Some("g"));
    // Meta entries merge per key across the dict and kwarg spellings.
    let m =
        config_meta("{{ config(meta={'a': 1}) }}\n{{ config({\"meta\": {\"b\": 2}}) }}").unwrap();
    assert_eq!(m["a"], serde_json::json!(1));
    assert_eq!(m["b"], serde_json::json!(2));
}

#[test]
fn config_dict_arg_shape_edges() {
    let pat = Patterns::new();
    // Only the FIRST positional arg is the dict spelling — a dict bound to
    // a kwarg is a value, not a config dict.
    assert_eq!(
        pat.materialized("{{ config(meta={'materialized': 'table'}) }}"),
        None,
        "the kwarg's dict value is not the config dict"
    );
    // A non-string key can't name a config; a duplicate key yields the
    // last entry (the Python-dict rule); non-literal keys are skipped.
    assert_eq!(
        pat.materialized("{{ config({1: 'x', 'materialized': 'table'}) }}"),
        Some("table".to_string())
    );
    assert_eq!(
        pat.materialized("{{ config({'materialized': 'view', 'materialized': 'table'}) }}",),
        Some("table".to_string())
    );
    // A bare `config()` and a first arg that isn't a dict contribute nothing.
    assert_eq!(pat.materialized("{{ config() }}"), None);
    assert_eq!(pat.materialized("{{ config('materialized') }}"), None);
}

// ---- source/table `config:` blocks ------------------------------------------

/// A scratch project with `config:` blocks at every level of a source —
/// the shapes real dbt resolves into the wire source.
fn write_source_config_project(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(
        dir.join("dbt_project.yml"),
        "name: p\nmodel-paths: [models]\n",
    )
    .unwrap();
    std::fs::write(dir.join("models/used.sql"), "select 1\n").unwrap();
    std::fs::write(
        dir.join("models/schema.yml"),
        r#"version: 2
sources:
  - name: raw
    meta: {src_meta: prop, share: src_prop}
    loaded_at_field: src_prop_field
    freshness: {warn_after: {count: 1, period: hour}, error_after: {count: 2, period: hour}}
    tags: [src_prop_tag]
    config:
      meta: {src_meta: cfg, share: src_cfg}
      loaded_at_field: src_cfg_field
      freshness: {error_after: {count: 3, period: hour}}
      tags: [src_cfg_tag]
    tables:
      - name: plain
      - name: overridden
        meta: {tbl_meta: prop, share: tbl_prop}
        loaded_at_field: tbl_prop_field
        freshness: {warn_after: {count: 4, period: hour}}
        tags: [tbl_prop_tag]
        config:
          meta: {share: tbl_cfg}
          loaded_at_field: tbl_cfg_field
          freshness: {error_after: {count: 5, period: hour}}
          tags: [tbl_cfg_tag]
      - name: dropped
        config: {enabled: false}
      - name: top_enabled_ignored
        enabled: false
  - name: disabled_src
    config: {enabled: false}
    tables:
      - name: under_disabled
      - name: re_enabled
        config: {enabled: true}
"#,
    )
    .unwrap();
}

#[test]
fn source_config_blocks_merge_in_dbt_order() {
    let dir = tempfile::tempdir().unwrap();
    write_source_config_project(dir.path());
    let m = manifest_from_source(dir.path()).unwrap();
    let src = |t: &str| {
        m.sources
            .get(&format!("source.p.raw.{t}"))
            .unwrap_or_else(|| panic!("source.p.raw.{t} exists"))
    };
    // `plain`: everything inherits from the source levels — `meta`/
    // `freshness` resolve config-first per level, but `loaded_at_field`
    // resolves property-first (verified against real dbt).
    let plain = src("plain");
    assert_eq!(plain.loaded_at_field.as_deref(), Some("src_prop_field"));
    let fr = plain.freshness.as_ref().expect("freshness");
    assert_eq!(fr.warn_after.as_ref().and_then(|t| t.count), Some(1));
    assert_eq!(fr.error_after.as_ref().and_then(|t| t.count), Some(3));
    assert_eq!(plain.meta["share"], serde_json::json!("src_cfg"));
    assert_eq!(plain.meta["src_meta"], serde_json::json!("cfg"));
    assert_eq!(plain.tags, vec!["src_cfg_tag", "src_prop_tag"]);
    // `overridden`: for `meta`/`freshness` the table's config wins
    // outright and its top-level props still beat the source's config —
    // the full chain order. For `loaded_at_field` the table's own
    // top-level property is the highest rung.
    let over = src("overridden");
    assert_eq!(over.loaded_at_field.as_deref(), Some("tbl_prop_field"));
    let fr = over.freshness.as_ref().expect("freshness");
    assert_eq!(fr.warn_after.as_ref().and_then(|t| t.count), Some(4));
    assert_eq!(fr.error_after.as_ref().and_then(|t| t.count), Some(5));
    assert_eq!(over.meta["share"], serde_json::json!("tbl_cfg"));
    assert_eq!(over.meta["tbl_meta"], serde_json::json!("prop"));
    assert_eq!(over.meta["src_meta"], serde_json::json!("cfg"));
    // Wire order: config channel (src→tbl) then property channel
    // (tbl→src — the resource's own tags precede the inherited source's).
    assert_eq!(
        over.tags,
        vec!["src_cfg_tag", "tbl_cfg_tag", "tbl_prop_tag", "src_prop_tag"]
    );
}

#[test]
fn source_config_enabled_parks_tables_like_dbt_disabled() {
    let dir = tempfile::tempdir().unwrap();
    write_source_config_project(dir.path());
    let m = manifest_from_source(dir.path()).unwrap();
    // `config: {enabled: false}` on the table drops it; on the source it
    // drops every table that doesn't re-enable itself.
    for uid in [
        "source.p.raw.dropped",
        "source.p.disabled_src.under_disabled",
    ] {
        assert!(!m.sources.contains_key(uid), "{uid} must be parked");
    }
    // A table's `config.enabled: true` re-enables under a disabled source.
    assert!(
        m.sources.contains_key("source.p.disabled_src.re_enabled"),
        "table config.enabled: true wins over the source's false"
    );
    // A table's top-level `enabled:` is ignored by dbt — the table stays.
    assert!(
        m.sources.contains_key("source.p.raw.top_enabled_ignored"),
        "top-level enabled: is not a source-table property"
    );
}

#[test]
fn disabled_generic_tests_are_never_synthesized() {
    let dir = tempfile::tempdir().unwrap();
    write_source_config_project(dir.path());
    std::fs::write(
        dir.path().join("models/more.yml"),
        r#"version: 2
models:
  - name: used
    columns:
      - name: id
        tests:
          - unique
          - unique: {config: {enabled: false}}
          - not_null: {enabled: false}
          - not_null
"#,
    )
    .unwrap();
    let m = manifest_from_source(dir.path()).unwrap();
    // One enabled unique + one enabled not_null synthesize; the two
    // `enabled: false` spellings park like dbt's `disabled`.
    let mut kinds: Vec<&str> = m
        .nodes
        .keys()
        .filter(|k| k.starts_with("test."))
        .map(|k| {
            m.nodes[k]
                .test_metadata
                .as_ref()
                .and_then(|t| t.name.as_deref())
                .unwrap_or("?")
        })
        .collect();
    kinds.sort();
    assert_eq!(kinds, ["not_null", "unique"], "{kinds:?}");
    // An enabled test's `config:`/`enabled:` keys never leak into kwargs —
    // the real manifest records them under `config`, not test args.
    let t = m
        .nodes
        .values()
        .find(|n| n.test_metadata.as_ref().and_then(|t| t.name.as_deref()) == Some("not_null"))
        .unwrap();
    let kwargs = t.test_metadata.as_ref().unwrap().kwargs.as_ref().unwrap();
    assert!(!kwargs.contains_key("config") && !kwargs.contains_key("enabled"));
}

// ---- data-paths (the pre-1.0 seed-paths alias) -------------------------------

#[test]
fn data_paths_alias_feeds_seed_discovery() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\ndata-paths: [data]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("data/grains.csv"), "id\n1\n").unwrap();
    let m = manifest_from_source(dir.path()).unwrap();
    assert!(
        m.nodes.contains_key("seed.p.grains"),
        "the data-paths alias must locate the seed"
    );
    // The default `seeds/` dir does NOT kick in when data-paths redirects:
    // a stray csv under seeds/ is outside the configured resource paths.
    std::fs::create_dir_all(dir.path().join("seeds")).unwrap();
    std::fs::write(dir.path().join("seeds/stray.csv"), "id\n1\n").unwrap();
    let m = manifest_from_source(dir.path()).unwrap();
    assert!(!m.nodes.contains_key("seed.p.stray"));
}

#[test]
fn data_paths_and_seed_paths_both_declared_warns_and_prefers_seed_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("seeds")).unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nseed-paths: [seeds]\ndata-paths: [data]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("seeds/a.csv"), "id\n1\n").unwrap();
    std::fs::write(dir.path().join("data/b.csv"), "id\n1\n").unwrap();
    let (m, w) = manifest_from_source_with_warnings(dir.path()).unwrap();
    // Declaring both is a dbt error — degraded to a warning here, with
    // `seed-paths` winning.
    assert!(
        w.iter()
            .any(|s| s.contains("data-paths") && s.contains("seed-paths")),
        "{w:?}"
    );
    assert!(m.nodes.contains_key("seed.p.a"));
    assert!(!m.nodes.contains_key("seed.p.b"));
}
