use super::*;

#[test]
fn config_extractors_scope_to_config_calls_in_any_arg_order() {
    let pat = Patterns::new();
    // The key matches even AFTER a prior argument inside the same call.
    assert_eq!(
        pat.materialized("{{ config(enabled=true, materialized='table') }}"),
        Some("table".to_string())
    );
    assert!(pat.disabled("{{ config(materialized='view', enabled=false) }}"));
    assert!(pat.disabled("{{ config(enabled=False) }}"));
    // Outside a config() call the keys are inert (dbt only honours them
    // inside config()).
    assert_eq!(pat.materialized("-- materialized='ephemeral'"), None);
    assert!(!pat.disabled("where enabled = false"));
    assert!(!pat.disabled("-- note: enabled=false someday"));
    assert!(!pat.disabled("{% set enabled=false %}"));
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
    assert!(!pat.disabled("{{ config(enabled=false) }}\n{{ config(enabled=true) }}"));
    assert!(pat.disabled("{{ config(enabled=true) }}\n{{ config(enabled=false) }}"));
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

#[test]
fn effective_freshness_merges_per_threshold() {
    let t = |count: i64| RawFreshnessThreshold {
        count: Some(count),
        period: Some("hour".to_string()),
    };
    let src = Some(Some(RawFreshness {
        warn_after: Some(t(12)),
        error_after: Some(t(24)),
    }));
    // A table declaring only `error_after` keeps the inherited warn_after —
    // the per-threshold merge real dbt does (verified on 1.12.6).
    let tbl = Some(Some(RawFreshness {
        warn_after: None,
        error_after: Some(t(6)),
    }));
    let out = effective_freshness(&tbl, &src, &None).expect("merged");
    assert_eq!(out.warn_after.and_then(|x| x.count), Some(12));
    assert_eq!(out.error_after.and_then(|x| x.count), Some(6));
    // An absent table level inherits the source wholesale.
    let out = effective_freshness(&None, &src, &None).expect("inherited");
    assert_eq!(out.warn_after.and_then(|x| x.count), Some(12));
    assert_eq!(out.error_after.and_then(|x| x.count), Some(24));
    // `freshness: null` on the table clears everything — the walk STOPS at
    // the explicit null (the tree `+freshness` never lands either).
    let tree = Some(RawFreshness {
        warn_after: Some(t(48)),
        error_after: None,
    });
    assert!(effective_freshness(&Some(None), &src, &tree).is_none());
    // But a source-level `freshness: null` only stops the levels BELOW the
    // table: the table's own declared thresholds still count.
    let out = effective_freshness(&tbl, &Some(None), &tree).expect("table wins");
    assert_eq!(out.error_after.and_then(|x| x.count), Some(6));
    assert!(out.warn_after.is_none(), "source null stops its warn");
    // The tree is the lowest level: absent schema levels pass through to it.
    let out = effective_freshness(&None, &None, &tree).expect("tree lands");
    assert_eq!(out.warn_after.and_then(|x| x.count), Some(48));
    // Nothing declared anywhere → no freshness object at all.
    assert!(effective_freshness(&None, &None, &None).is_none());
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
