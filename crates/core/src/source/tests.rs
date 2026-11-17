use super::*;

/// `Vars` straight from maps — `file` models vars.yml's `vars:` map (the
/// `+enabled` render table), `project` models dbt_project.yml's `vars:`
/// map (the node-render table when no vars.yml exists); `proj` is the
/// `name:` `vars: <proj>:` scopes under. No `--vars` (the empty CLI map).
fn test_vars(file: serde_json::Value, project: serde_json::Value, proj: &str) -> Vars {
    Vars::from_maps(
        file.as_object().cloned().unwrap_or_default(),
        project.as_object().cloned().unwrap_or_default(),
        serde_json::Map::new(),
        proj,
    )
}

/// The same with a `--vars` map — dbt's highest-precedence lookups in
/// both scopes.
fn test_vars_cli(
    file: serde_json::Value,
    project: serde_json::Value,
    cli: serde_json::Value,
    proj: &str,
) -> Vars {
    Vars::from_maps(
        file.as_object().cloned().unwrap_or_default(),
        project.as_object().cloned().unwrap_or_default(),
        cli.as_object().cloned().unwrap_or_default(),
        proj,
    )
}

/// No vars anywhere — every `var()` falls to its default or unresolvable.
fn no_vars() -> Vars {
    test_vars(serde_json::json!({}), serde_json::json!({}), "p")
}

/// An empty project-macro table for the extractor tests that don't
/// exercise macro calls — `scope_of`/`enabled`'s default.
fn no_macros() -> &'static Macros {
    static M: OnceLock<Macros> = OnceLock::new();
    M.get_or_init(Macros::default)
}

/// A [`JinjaScope`] over `sql` bound to `vars` — the extractor tests'
/// one-liner scope (`no_vars()` wherever branch conditions don't matter).
fn scope_of<'v>(sql: &str, vars: &'v Vars) -> JinjaScope<'v> {
    JinjaScope::new(sql, vars, no_macros())
}

/// The same, with a project macro table the `enabled` evaluator and
/// branch conditions resolve calls against.
fn scope_of_macros<'v>(sql: &str, vars: &'v Vars, macros: &'v Macros) -> JinjaScope<'v> {
    JinjaScope::new(sql, vars, macros)
}

/// `pat.enabled` verdict folded over an empty fallback — the
/// (verdict, warnings) pair with the eval reports already merged.
fn enabled(sql: &str, vars: &Vars) -> (Option<Enabled>, Vec<String>) {
    enabled_macros(sql, vars, no_macros())
}

/// [`enabled`] with a project macro table.
fn enabled_macros(sql: &str, vars: &Vars, macros: &Macros) -> (Option<Enabled>, Vec<String>) {
    let mut w = Vec::new();
    let mut ev = Vec::new();
    let scope = scope_of_macros(sql, vars, macros);
    let d = Patterns::new()
        .enabled(sql, &scope, &mut w, &mut ev, "models/m.sql")
        .resolve(None, &mut w, &mut ev, "models/m.sql");
    merge_eval_warnings(&mut w, &ev);
    (d, w)
}

#[test]
fn config_extractors_scope_to_config_calls_in_any_arg_order() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    let mut ev = Vec::new();
    // The key matches even AFTER a prior argument inside the same call.
    assert_eq!(
        pat.materialized(
            "{{ config(enabled=true, materialized='table') }}",
            &scope_of(
                "{{ config(enabled=true, materialized='table') }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.enabled(
            "{{ config(materialized='view', enabled=false) }}",
            &scope_of("{{ config(materialized='view', enabled=false) }}", &vars),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        Some(Enabled::Off)
    );
    assert_eq!(
        pat.enabled(
            "{{ config(enabled=False) }}",
            &scope_of("{{ config(enabled=False) }}", &vars),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        Some(Enabled::Off)
    );
    // Outside a config() call the keys are inert (dbt only honours them
    // inside config()).
    assert_eq!(
        pat.materialized(
            "-- materialized='ephemeral'",
            &scope_of("-- materialized='ephemeral'", &no_vars())
        ),
        None
    );
    assert_eq!(
        pat.enabled(
            "where enabled = false",
            &scope_of("where enabled = false", &vars),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        None
    );
    assert_eq!(
        pat.enabled(
            "-- note: enabled=false someday",
            &scope_of("-- note: enabled=false someday", &vars),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        None
    );
    assert_eq!(
        pat.enabled(
            "{% set enabled=false %}",
            &scope_of("{% set enabled=false %}", &vars),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        None
    );
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
        &scope_of(
            "{{ config(materialized='table', meta={'owner': 'alice', 'deep': {'x': 1}}) }}",
            &no_vars(),
        ),
    )
    .expect("a meta dict parses");
    assert_eq!(m["owner"], serde_json::json!("alice"));
    assert_eq!(m["deep"], serde_json::json!({"x": 1}));
    // Numbers/bools/lists/tuples and double quotes all convert.
    let m = config_meta(
        r#"{{ config(meta={'n': 2, 'f': 1.5, 'b': True, 'l': [1, "x"], 't': (3,)}) }}"#,
        &scope_of(
            r#"{{ config(meta={'n': 2, 'f': 1.5, 'b': True, 'l': [1, "x"], 't': (3,)}) }}"#,
            &no_vars(),
        ),
    )
    .unwrap();
    assert_eq!(m["n"], serde_json::json!(2));
    assert_eq!(m["f"], serde_json::json!(1.5));
    assert_eq!(m["b"], serde_json::json!(true));
    assert_eq!(m["l"], serde_json::json!([1, "x"]));
    assert_eq!(m["t"], serde_json::json!([3]));
    // Jinja's lowercase literal spellings convert the same way.
    let m = config_meta(
        r#"{{ config(meta={'b': true, 'x': none}) }}"#,
        &scope_of(r#"{{ config(meta={'b': true, 'x': none}) }}"#, &no_vars()),
    )
    .unwrap();
    assert_eq!(m["b"], serde_json::json!(true));
    assert_eq!(m["x"], serde_json::Value::Null);
    // `metadata=` is not `meta`; a `meta` inside comments/strings elsewhere
    // can't configure the node; unresolvable exprs drop rather than guess.
    assert!(config_meta(
        "{{ config(metadata={'a': 1}) }}",
        &scope_of("{{ config(metadata={'a': 1}) }}", &no_vars())
    )
    .is_none());
    assert!(config_meta(
        "{{ config(meta=var('m')) }}",
        &scope_of("{{ config(meta=var('m')) }}", &no_vars())
    )
    .is_none());
    assert!(config_meta(
        "-- no config here",
        &scope_of("-- no config here", &no_vars())
    )
    .is_none());
    assert!(config_meta(
        "{{ config(meta={'a': 1} if x else {}) }}",
        &scope_of("{{ config(meta={'a': 1} if x else {}) }}", &no_vars())
    )
    .is_none());
    assert!(config_meta(
        "{{ config(meta='not-a-dict') }}",
        &scope_of("{{ config(meta='not-a-dict') }}", &no_vars())
    )
    .is_none());
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
        &scope_of(
            "{{ config(meta={'a': 1, 'b': 'first'}) }}\n\
         select 1\n\
         {{ config(meta={'b': 'second', 'c': 3}) }}",
            &no_vars(),
        ),
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
        &scope_of(
            "{{ config(meta={'d': {'x': 1}, 'keep': 1}) }}\n\
         {{ config(meta={'d': {'y': 2}}) }}",
            &no_vars(),
        ),
    )
    .unwrap();
    assert_eq!(m["d"], serde_json::json!({"y": 2}));
    assert_eq!(m["keep"], serde_json::json!(1));
    // An unresolvable later kwarg can't clear an earlier literal — the
    // merge keeps what it could resolve (a subset of dbt's result).
    let m = config_meta(
        "{{ config(meta={'a': 1}) }}\n{{ config(meta=var('x')) }}",
        &scope_of(
            "{{ config(meta={'a': 1}) }}\n{{ config(meta=var('x')) }}",
            &no_vars(),
        ),
    )
    .unwrap();
    assert_eq!(m["a"], serde_json::json!(1));
}

#[test]
fn last_config_call_wins_for_scalar_kwargs() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    let mut ev = Vec::new();
    // dbt applies config() calls in order, each call's kwargs overriding
    // the earlier ones per key (verified):
    //   config(materialized='view')  then config(materialized='table')
    //   → materialized = 'table'
    //   config(enabled=false)        then config(enabled=true)
    //   → node stays ENABLED
    assert_eq!(
        pat.materialized(
            "{{ config(materialized='view') }}\n{{ config(materialized='table') }}",
            &scope_of(
                "{{ config(materialized='view') }}\n{{ config(materialized='table') }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.enabled(
            "{{ config(enabled=false) }}\n{{ config(enabled=true) }}",
            &scope_of(
                "{{ config(enabled=false) }}\n{{ config(enabled=true) }}",
                &vars
            ),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        Some(Enabled::On)
    );
    assert_eq!(
        pat.enabled(
            "{{ config(enabled=true) }}\n{{ config(enabled=false) }}",
            &scope_of(
                "{{ config(enabled=true) }}\n{{ config(enabled=false) }}",
                &vars
            ),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        Some(Enabled::Off)
    );
    // An unresolvable later value can't override an earlier literal one.
    assert_eq!(
        pat.materialized(
            "{{ config(materialized='view') }}\n{{ config(materialized=var('m')) }}",
            &scope_of(
                "{{ config(materialized='view') }}\n{{ config(materialized=var('m')) }}",
                &no_vars()
            )
        ),
        Some("view".to_string())
    );
}

#[test]
fn ref_regex_handles_versioned_and_extra_args() {
    let names = |sql: &str| -> Vec<String> {
        let masked = mask_jinja_comments(sql);
        call_sites_masked(&masked)
            .iter()
            .filter_map(|s| match &s.kind {
                CallKind::Ref { name, .. } => Some(masked[name.clone()].to_string()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(names("{{ ref('m', v=2) }}"), vec!["m".to_string()]);
    assert_eq!(names("{{ ref('m', version=2) }}"), vec!["m".to_string()]);
    assert_eq!(names("{{ ref('pkg', 'm', v=2) }}"), vec!["m".to_string()]);
    // No cross-call over-matching: `[^)]*` must not cross the first `)`.
    assert_eq!(
        names("{{ ref('a') }} x {{ ref('b') }}"),
        vec!["a".to_string(), "b".to_string()]
    );
    // A non-literal arg (a macro call, not a quoted string) still drops,
    // like before this change.
    assert!(names("{{ ref(var('x')) }}").is_empty());
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
        config_unique_key(
            "{{ config(unique_key='id') }}",
            &scope_of("{{ config(unique_key='id') }}", &no_vars())
        ),
        Some(serde_json::json!("id"))
    );
    assert_eq!(
        config_unique_key(
            "{{ config(materialized='i', unique_key=['a', 'b']) }}",
            &scope_of(
                "{{ config(materialized='i', unique_key=['a', 'b']) }}",
                &no_vars()
            )
        ),
        Some(serde_json::json!(["a", "b"]))
    );
    assert_eq!(
        config_unique_key(
            "{{ config(unique_key='x') }} {{ config(unique_key=['y','z']) }}",
            &scope_of(
                "{{ config(unique_key='x') }} {{ config(unique_key=['y','z']) }}",
                &no_vars()
            )
        ),
        Some(serde_json::json!(["y", "z"]))
    );
    assert_eq!(
        config_unique_key(
            "{{ config(unique_key='id') }} {{ config(unique_key=var('k')) }}",
            &scope_of(
                "{{ config(unique_key='id') }} {{ config(unique_key=var('k')) }}",
                &no_vars()
            )
        ),
        Some(serde_json::json!("id")),
        "the unresolvable expr can't shadow the earlier literal"
    );
    // Outside a config() call the key is inert.
    assert_eq!(
        config_unique_key(
            "-- unique_key='id'",
            &scope_of("-- unique_key='id'", &no_vars())
        ),
        None
    );
    assert_eq!(
        config_unique_key("select 1", &scope_of("select 1", &no_vars())),
        None
    );
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
    // Comparison/logic shapes resolve through the same project scope.
    assert_eq!(tree("\"{{ not var('flag_true') }}\"").0, Some(false));
    assert_eq!(
        tree("\"{{ var('flag_true') and var('flag_false') }}\"").0,
        Some(false)
    );
    assert_eq!(
        tree("\"{{ var('missing', 'a') == 'a' and var('flag_true') }}\"").0,
        Some(true)
    );
    // Everything dbt's enabled validation would reject — a plain 'false'
    // string, a number, another expression, an unresolvable var — keeps
    // the node enabled with the same once-per-level warning.
    for bad in [
        "'false'",
        "0",
        "\"{{ env_var('X') }}\"",
        "\"{{ target.name }}\"",
        "\"{{ var('missing') }}\"",
        "\"{{ var('s_lower') }}\"",
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
    assert_eq!(
        enabled("{{ config(enabled=var('flag')) }}", &vars).0,
        Some(Enabled::Off)
    );
    assert_eq!(
        enabled("{{ config(enabled=var('scoped', true)) }}", &vars).0,
        Some(Enabled::Off),
        "the scoped value (false) shadows the default"
    );
    assert_eq!(
        enabled("{{ config(enabled=var('missing', true)) }}", &vars).0,
        Some(Enabled::On)
    );
    assert_eq!(
        enabled("{{ config(enabled=var('flag') | as_bool) }}", &vars).0,
        Some(Enabled::Off)
    );
    // Last resolvable wins — a var-resolved value lands in the same order
    // as a literal one.
    assert_eq!(
        enabled(
            "{{ config(enabled=false) }}\n{{ config(enabled=var('missing', true)) }}",
            &vars
        )
        .0,
        Some(Enabled::On)
    );
    assert_eq!(
        enabled(
            "{{ config(enabled=true) }}\n{{ config(enabled=var('flag')) }}",
            &vars
        )
        .0,
        Some(Enabled::Off)
    );

    // With NO vars.yml the node scope is `vars:` — top-level and scoped.
    let vars = test_vars(
        serde_json::json!({}),
        serde_json::json!({"flag": false, "p": {"scoped": false}}),
        "p",
    );
    assert_eq!(
        enabled("{{ config(enabled=var('flag')) }}", &vars).0,
        Some(Enabled::Off)
    );
    assert_eq!(
        enabled("{{ config(enabled=var('scoped', true)) }}", &vars).0,
        Some(Enabled::Off)
    );

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
        let (d, w) = enabled(sql, &vars);
        assert_eq!(d, Some(Enabled::Unproven), "{sql} keeps the node enabled");
        assert_eq!(w.len(), 1, "{sql}: {w:?}");
        assert!(w[0].contains("models/m.sql"), "the file is named: {w:?}");
        assert!(w[0].contains("enabled="), "the config is named: {w:?}");
    }
    // The LAST `enabled=` decides: an unresolvable later value is a
    // boolean-validation error in dbt — it can't fall back to an earlier
    // proven `false`, so the node stays enabled with a warning.
    let (d, w) = enabled(
        "{{ config(enabled=false) }}\n{{ config(enabled=var('missing')) }}",
        &vars,
    );
    assert_eq!(d, Some(Enabled::Unproven));
    assert_eq!(w.len(), 1, "{w:?}");
    // A bare `config(` outside `{{ }}` is SQL text to dbt — never a call.
    let (d, w) = enabled("select 1 -- config(enabled=false)", &vars);
    assert_eq!(d, None);
    assert!(w.is_empty(), "{w:?}");
    // …but `{{ }}` inside a `--` comment still executes (Jinja first).
    let (d, _) = enabled("select 1 -- {{ config(enabled=false) }}", &vars);
    assert_eq!(d, Some(Enabled::Off));
}

// ---- enabled expression evaluator (comparisons, logic, membership) ----------

/// `node_enabled` outcomes for one `config(enabled=<expr>)` — the shared
/// evaluator's node-scope entry.
fn enabled_expr(expr: &str, vars: &Vars) -> Option<bool> {
    enabled_expr_macros(expr, vars, no_macros())
}

/// [`enabled_expr`] with a project macro table — the in-file context is
/// the one place a project macro call resolves.
fn enabled_expr_macros(expr: &str, vars: &Vars, macros: &Macros) -> Option<bool> {
    vars.node_enabled(expr, &BoundNames::default(), macros)
}

#[test]
fn enabled_expr_comparisons_resolve_like_python() {
    let vars = test_vars(
        serde_json::json!({}),
        serde_json::json!({
            "ver": "v1", "alt": "v2", "n": 3, "t": true, "f": false,
            "empty": "", "lst": ["a", "b"], "truthy_str": "False",
        }),
        "p",
    );
    for (expr, want) in [
        // Equality — the issue's driving shapes.
        ("var('ver') == 'v1'", Some(true)),
        ("var('ver') == var('alt')", Some(false)),
        ("var('ver') != 'v1'", Some(false)),
        ("var('missing', 'v1') == 'v1'", Some(true)),
        ("var('missing') == 'v1'", Some(false)), // None == 'v1'
        ("var('missing') != 'v1'", Some(true)),  // None != 'v1'
        ("var('missing') == var('missing2')", Some(true)), // None == None
        // Python equality: 0/1 compare equal to False/True, 'True' is not True.
        ("1 == true", Some(true)),
        ("0 == false", Some(true)),
        ("var('n') == true", Some(false)), // 3 != 1
        ("'True' == true", Some(false)),
        ("'v1' == 'v1'", Some(true)),
        // Ordering — same-type pairs only.
        ("var('n') > 2", Some(true)),
        ("var('n') >= 3 and var('n') < 4", Some(true)),
        ("'a' < 'b'", Some(true)),
        ("var('ver') < 'w'", Some(true)),
        // Chained comparisons — pairwise `and`, middle evaluated once.
        ("1 < var('n') <= 3", Some(true)),
        ("1 < var('n') < 3", Some(false)),
        ("var('n') == 3 == var('n')", Some(true)),
        // Membership — list, substring, dict key.
        ("'a' in var('lst')", Some(true)),
        ("'x' in var('lst')", Some(false)),
        ("'x' not in var('lst')", Some(true)),
        ("'v' in 'v1'", Some(true)),
        ("var('ver') in ['v1', 'v2']", Some(true)),
        ("'k' in {'k': 1}", Some(true)),
        ("'k' not in {'k': 1}", Some(false)),
        // and/or/not — Python truthiness and operand VALUES.
        ("var('f') and var('ver') == 'v1'", Some(false)),
        ("var('t') and var('ver') == 'v1'", Some(true)),
        ("var('f') or var('ver') == 'v1'", Some(true)),
        ("not var('f')", Some(true)),
        ("not var('t') and var('t')", Some(false)),
        ("not var('ver') == 'v1'", Some(false)), // not (ver == 'v1')
        ("not not var('t')", Some(true)),
        // Empty string falsy; a NON-empty string ('False' included) truthy.
        ("var('empty') and true", None), // '' and true → '' — not a bool
        ("var('empty') or false", Some(false)),
        ("var('truthy_str') and true", Some(true)),
        ("0 and true", None), // 0 and true → 0 — not a bool
        ("0 or true", Some(true)),
        // Parens regroup past precedence.
        ("(var('f') or var('t')) and var('f')", Some(false)),
        ("var('f') or (var('t') and var('f'))", Some(false)),
        ("(1 == 1) == true", Some(true)),
        // `| as_bool` — bools and 'True'/'False' only.
        ("var('truthy_str') | as_bool", Some(false)), // 'False' → False
        ("var('t') | as_bool", Some(true)),
        ("var('empty') | as_bool", None), // literal_eval fails
        ("var('n') | as_bool", None),     // literal_eval(3) → int
        ("true | as_bool", Some(true)),
        // Unprovable — a type error, an unsupported call/filter/attr.
        ("var('n') < 'x'", None),     // int < str → TypeError
        ("var('missing') < 2", None), // None < 2 → TypeError
        ("1 in 2", None),             // `in` a non-container → TypeError
        ("var('n') in 'abc'", None),  // non-str needle in a str → TypeError
        ("env_var('X')", None),
        ("var('ver').upper()", None),
        ("target.name", None),
        ("my_macro()", None),
        ("var('ver') | upper", None),
        ("var('ver')[0]", None),
        // Syntax that never reaches evaluation.
        ("", None),
        ("var('ver' ==", None),
        ("var()", None),
        ("var('a', 'b', 'c')", None),
        ("var(1)", None),
        ("1 + 1", None), // arithmetic is outside the subset
        ("var('t') ? 'a' : 'b'", None),
    ] {
        assert_eq!(enabled_expr(expr, &vars), want, "{expr}");
    }
    // and/or return OPERAND values (a non-bool result fails validation —
    // `0 or ''` resolves to `''`, not a bool).
    assert_eq!(enabled_expr("var('f') or ''", &vars), None); // '' is not a bool
    assert_eq!(enabled_expr("0 and var('t')", &vars), None); // result 0
    assert_eq!(enabled_expr("1 and var('t')", &vars), Some(true)); // result True
}

#[test]
fn enabled_expr_is_tests_resolve_like_jinja() {
    let vars = test_vars(
        serde_json::json!({}),
        serde_json::json!({
            "ver": "v1", "n": 3, "t": true, "f": false, "nothing": null,
            "lst": ["a"], "d": {"k": 1}, "s": "abc", "big": 300,
        }),
        "p",
    );
    for (expr, want) in [
        // `is defined`/`is undefined`/`is none` — the three tests that
        // split a bare missing name from Python `None` (verified on dbt
        // 1.12.6: `None is defined` is TRUE — a missing bare name is the
        // only undefined shape an enabled expression can reach, since a
        // missing `var()` yields `None` in the tolerant render).
        ("missing_name is defined", Some(false)),
        ("missing_name is not defined", Some(true)),
        ("missing_name is undefined", Some(true)),
        ("missing_name is none", Some(false)), // Undefined is not None
        (
            "missing_name is none or missing_name is undefined",
            Some(true),
        ),
        ("var('missing') is defined", Some(true)), // → None, and None IS defined
        ("var('missing') is none", Some(true)),
        ("var('nothing') is none", Some(true)),
        ("var('nothing') is defined", Some(true)),
        ("var('ver') is defined", Some(true)),
        ("var('missing', 'd') is defined", Some(true)),
        ("true is defined", Some(true)),
        // Type tests — strings, numbers (a bool IS a number in Python),
        // bools, mappings, sequences, iterables.
        ("var('ver') is string", Some(true)),
        ("var('n') is string", Some(false)),
        ("var('n') is number", Some(true)),
        ("var('t') is number", Some(true)), // isinstance(True, Number)
        ("var('t') is boolean", Some(true)),
        ("var('n') is boolean", Some(false)),
        ("var('d') is mapping", Some(true)),
        ("var('lst') is mapping", Some(false)),
        // Jinja's sequence/iterable tests duck-type — a STRING and a
        // dict pass both (verified on 1.12.6).
        ("var('lst') is sequence", Some(true)),
        ("var('d') is sequence", Some(true)),
        ("var('s') is sequence", Some(true)),
        ("var('n') is sequence", Some(false)),
        ("var('s') is iterable", Some(true)),
        ("var('t') is iterable", Some(false)),
        // `Undefined` ducks too — `missing is sequence`/`iterable`
        // render True (Jinja's Undefined defines len/getitem/iter).
        ("missing_name is sequence", Some(true)),
        ("missing_name is iterable", Some(true)),
        ("missing_name is mapping", Some(false)),
        ("var('t') is true", Some(true)),
        ("var('f') is true", Some(false)),
        ("var('f') is false", Some(true)),
        // `even`/`odd` — a non-number operand TypeErrors (unprovable).
        ("var('n') is odd", Some(true)),
        ("var('n') is even", Some(false)),
        ("var('n') is not even", Some(true)),
        ("var('s') is even", None),
        ("missing_name is even", None), // Undefined % 2 → TypeError
        // An unsupported test, or a supported one CALLED — unprovable.
        ("var('ver') is equalto('v1')", None),
        ("var('n') is divisibleby(3)", None),
        ("var('n') is defined()", None),
    ] {
        assert_eq!(enabled_expr(expr, &vars), want, "{expr}");
    }
}

#[test]
fn enabled_expr_sameas_evaluates_identity_where_safe() {
    let vars = test_vars(
        serde_json::json!({}),
        serde_json::json!({
            "n": 3, "big": 300, "ver": "v1", "t": true, "f": false,
            "nothing": null, "f1": 1.5,
        }),
        "p",
    );
    for (expr, want) in [
        // None/True/False are singletons — identity is provable even on
        // a var-drawn operand (verified on dbt 1.12.6).
        ("var('nothing') is sameas none", Some(true)),
        ("var('nothing') is sameas(None)", Some(true)),
        ("var('ver') is sameas none", Some(false)),
        ("var('t') is sameas true", Some(true)),
        ("var('f') is sameas true", Some(false)),
        ("var('f') is not sameas true", Some(true)),
        ("true is sameas true", Some(true)),
        // A bare missing name is a fresh Undefined object — never `is`
        // anything (verified: `missing_name is sameas none` → False).
        ("missing_name is sameas none", Some(false)),
        ("missing_name is not sameas none", Some(true)),
        // Small ints (-5..=256) share CPython's cache — `var('n') is
        // sameas 3` renders True under real dbt; `300` is past the cache
        // and renders False (verified).
        ("var('n') is sameas 3", Some(true)),
        ("var('n') is sameas 4", Some(false)),
        ("var('big') is sameas 300", Some(false)),
        ("var('big') is sameas(300)", Some(false)),
        ("300 is sameas 300", Some(false)),
        // A unary-signed BARE operand is a Jinja syntax error — dbt
        // surfaces a parse failure, so the expression is unprovable
        // (the `sameas(-3)` call form evaluates fine).
        ("-3 is sameas -3", None),
        ("-3 is sameas(-3)", Some(true)), // -3 IS inside the cache range
        ("-6 is sameas(-6)", Some(false)), // -6 is not (range ends -5)
        ("256 is sameas(256)", Some(true)),
        ("257 is sameas(257)", Some(false)),
        // Floats and equal STRINGS stay unprovable — identity there is
        // interning- and provenance-dependent (verified: two 'v1'
        // literals render True while `var('ver') is sameas 'v1'`
        // renders False — one operand's origin is invisible here).
        ("var('f1') is sameas 1.5", Some(false)),
        ("1.5 is sameas 1.5", Some(false)),
        ("var('ver') is sameas 'v1'", None),
        ("'v1' is sameas 'v1'", None),
        // A var() INSIDE the operand can't be judged — the operand
        // could BE the left operand's object.
        ("var('n') is sameas var('n')", None),
        ("3 is sameas var('n')", None),
        // Cross-type pairs are provably distinct objects.
        ("var('n') is sameas 'x'", Some(false)),
        ("var('n') is sameas true", Some(false)),
    ] {
        assert_eq!(enabled_expr(expr, &vars), want, "{expr}");
    }
}

#[test]
fn enabled_expr_missing_var_strictness_follows_the_render_scope() {
    // Real dbt 1.12.6 renders `var()` three ways: the node-scope render
    // (in-file `config(enabled=…)`) tolerates a missing name → `None`
    // (which then fails the bool validation → the resource stays
    // enabled); the dbt_project.yml and schema.yml renders RAISE
    // "Required var not found" — unprovable here either way.
    let vars = test_vars(serde_json::json!({}), serde_json::json!({}), "p");
    // Node scope (tolerant): missing → None → not a bool → unprovable
    // AT THE BOOL EXTRACTION (the expression itself resolves).
    assert_eq!(enabled_expr("var('missing')", &vars), None);
    // …but the resolved `None` participates in comparisons/tests.
    assert_eq!(enabled_expr("var('missing') is none", &vars), Some(true));
    assert_eq!(
        enabled_expr("var('missing') == var('missing')", &vars),
        Some(true)
    );
    assert_eq!(enabled_expr("var('missing') or true", &vars), Some(true));
    assert_eq!(enabled_expr("not var('missing')", &vars), Some(true));
    // Tree scope (strict): a missing name is a raise — unprovable.
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('missing') }}")),
        None
    );
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('missing') is none }}")),
        None
    );
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('missing') or true }}")),
        None
    );
    // Schema scope (strict — verified): same raise.
    assert_eq!(
        vars.schema_enabled(&serde_json::json!("{{ var('missing') }}")),
        None
    );
    assert_eq!(
        vars.schema_enabled(&serde_json::json!("{{ var('missing') or true }}")),
        None
    );
    // A DEFAULT still resolves in every scope — `var('missing', x)`
    // never reaches the missing-name path.
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('missing', false) }}")),
        Some(false)
    );
    assert_eq!(
        vars.schema_enabled(&serde_json::json!("{{ var('missing', true) }}")),
        Some(true)
    );
    assert_eq!(enabled_expr("var('missing', false)", &vars), Some(false));
}

#[test]
fn tree_enabled_deepest_declaration_wins_proven_or_unproven() {
    let vars = no_vars();
    let mut w = Vec::new();
    // A deeper UNPROVABLE `+enabled` beats a proven shallower one —
    // dbt would hard-error on the deeper level, never reaching the
    // shallower's verdict.
    let t = resolve_tree_config(
        &serde_json::json!({
            "p": {"+enabled": false, "marts": {"+enabled": "{{ var('missing') }}"}},
        }),
        &["p".to_string(), "marts".to_string()],
        "models".to_string(),
        &mut w,
        Some(&vars),
    );
    assert!(t.enabled_unproven, "{t:?}");
    assert_eq!(t.enabled_verdict(), Some(Enabled::Unproven));
    assert_eq!(w.len(), 1, "{w:?}");
    // The same walk with the levels flipped — the deeper proven value
    // wins over a shallower unproven one.
    let mut w = Vec::new();
    let t = resolve_tree_config(
        &serde_json::json!({
            "p": {"+enabled": "{{ var('missing') }}", "marts": {"+enabled": false}},
        }),
        &["p".to_string(), "marts".to_string()],
        "models".to_string(),
        &mut w,
        Some(&vars),
    );
    assert!(!t.enabled_unproven, "{t:?}");
    assert_eq!(t.enabled_verdict(), Some(Enabled::Off));
    // A proven false at the leaf decides; a missing segment descends
    // past nothing.
    let mut w = Vec::new();
    let t = resolve_tree_config(
        &serde_json::json!({"p": {"+enabled": "{{ var('missing') }}"}}),
        &["p".to_string(), "deeper".to_string()],
        "models".to_string(),
        &mut w,
        Some(&vars),
    );
    assert_eq!(t.enabled_verdict(), Some(Enabled::Unproven));
    // And the still-bool-only `sources:` walk (vars: None) warns
    // nothing and reads a literal false.
    let mut w = Vec::new();
    let t = resolve_tree_config(
        &serde_json::json!({"p": {"+enabled": false}}),
        &["p".to_string()],
        "sources".to_string(),
        &mut w,
        None,
    );
    assert_eq!(t.enabled_verdict(), Some(Enabled::Off));
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn cli_vars_outrank_every_var_source_in_both_scopes() {
    // `--vars` beats vars.yml in the project-file scope AND vars.yml,
    // `vars: <p>:` and top-level `vars:` in the node scope — verified
    // against real dbt 1.12.6.
    let vars = test_vars_cli(
        serde_json::json!({
            "file_flag": true, "shared": "file", "nested": "top",
            "p": {"scoped": false, "nested": "scoped_file"},
        }),
        serde_json::json!({
            "proj_flag": false, "shared": "proj", "nested": "proj_top",
            "p": {"scoped": true, "nested": "scoped_proj"},
        }),
        serde_json::json!({"cli_flag": true, "shared": "cli", "file_flag": false}),
        "p",
    );
    // Project scope (`+enabled`): cli > file — `vars:` stays invisible.
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('file_flag') }}")),
        Some(false),
        "cli overrides vars.yml"
    );
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('cli_flag', false) }}")),
        Some(true),
        "a cli-only key resolves in the project scope"
    );
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('shared') == 'cli' }}")),
        Some(true),
        "cli overrides vars.yml in the project scope too"
    );
    assert_eq!(
        vars.tree_enabled(&serde_json::json!("{{ var('shared') }}")),
        None,
        "'cli' is not a bool → enabled validation fails"
    );
    // Node scope (`config()`/schema.yml): cli > scoped > top-level — the
    // whole chain inside the effective table (vars.yml here, so `vars:` —
    // `proj_flag` and ITS scoped entries — stays invisible).
    assert_eq!(enabled_expr("var('shared')", &vars), None); // 'cli' is not a bool
    assert_eq!(enabled_expr("var('shared') == 'cli'", &vars), Some(true));
    assert_eq!(enabled_expr("var('scoped')", &vars), Some(false));
    assert_eq!(
        enabled_expr("var('nested') == 'scoped_file'", &vars),
        Some(true),
        "the scoped entry beats the table's top level"
    );
    assert_eq!(
        enabled_expr("var('cli_flag')", &vars),
        Some(true),
        "cli wins over every node-scope channel"
    );
    assert_eq!(enabled_expr("var('file_flag')", &vars), Some(false));
    assert_eq!(enabled_expr("var('proj_flag')", &vars), None);
    // A scoped SECTION inside --vars does NOT reach var() (verified on
    // 1.12.6) — only top-level keys count.
    let vars = test_vars_cli(
        serde_json::json!({}),
        serde_json::json!({}),
        serde_json::json!({"p": {"scoped": false}}),
        "p",
    );
    assert_eq!(enabled_expr("var('scoped', true)", &vars), Some(true));
}

#[test]
fn cli_vars_parse_accepts_dbt_style_yaml_and_json() {
    // YAML flow and block dicts (dbt's spelling) and JSON (a YAML subset).
    let v = CliVars::parse("{a: 1, b: 'x'}").unwrap();
    assert_eq!(v.vars()["a"], serde_json::json!(1));
    assert_eq!(v.vars()["b"], serde_json::json!("x"));
    assert_eq!(v.raw(), "{a: 1, b: 'x'}");
    let v = CliVars::parse("a: 1\nb: true\n").unwrap();
    assert_eq!(v.vars()["b"], serde_json::json!(true));
    let v = CliVars::parse(r#"{"a": {"nested": [1, 2]}}"#).unwrap();
    assert_eq!(v.vars()["a"], serde_json::json!({"nested": [1, 2]}));
    // dbt rejects a non-dict --vars — so do we.
    for bad in ["[1, 2]", "'a string'", "1", "{unclosed: ["] {
        assert!(CliVars::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn schema_enabled_uses_the_node_scope_like_dbt() {
    // A `config.enabled:`/`enabled:` yml scalar is rendered inside the
    // node's context — cli > `vars: <p>:` > top-level (vars.yml here) —
    // and must be ONE `{{ }}` block resolving to a bool.
    let vars = test_vars(
        serde_json::json!({"flag": true, "p": {"scoped": false}}),
        serde_json::json!({}),
        "p",
    );
    assert_eq!(
        vars.schema_enabled(&serde_json::json!("{{ var('scoped', true) }}")),
        Some(false)
    );
    assert_eq!(
        vars.schema_enabled(&serde_json::json!("{{ var('flag') and false }}")),
        Some(false)
    );
    assert_eq!(vars.schema_enabled(&serde_json::json!(false)), Some(false));
    // Non-rendered scalars keep their type → not a bool.
    for v in [
        serde_json::json!("false"),
        serde_json::json!("True"),
        serde_json::json!(0),
        serde_json::json!(null),
        serde_json::json!("{{ var('flag') }} and then some"),
    ] {
        assert_eq!(vars.schema_enabled(&v), None, "{v}");
    }
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
        None,
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
        vars.node_enabled("var('flag')", &BoundNames::default(), no_macros()),
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
        None,
        &mut w,
    );
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(
        vars.node_enabled("var('flag')", &BoundNames::default(), no_macros()),
        Some(true)
    );
}

#[test]
fn vars_load_warns_on_malformed_vars_sources() {
    let dir = tempfile::tempdir().unwrap();
    // A non-mapping `vars:` in dbt_project.yml → warning, vars ignored.
    let mut w = Vec::new();
    let vars = Vars::load(
        dir.path(),
        &test_project(serde_json::json!([1, 2])),
        None,
        &mut w,
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("vars:") && w[0].contains("not a mapping"),
        "{w:?}"
    );
    assert_eq!(
        vars.node_enabled("var('flag', true)", &BoundNames::default(), no_macros()),
        Some(true)
    );
    // An unparsable vars.yml → warning, contributes nothing.
    std::fs::write(dir.path().join("vars.yml"), "vars: {a: [unclosed\n").unwrap();
    let mut w = Vec::new();
    let vars = Vars::load(
        dir.path(),
        &test_project(serde_json::json!({"flag": false})),
        None,
        &mut w,
    );
    assert!(
        w.iter()
            .any(|s| s.contains("vars.yml") && s.contains("parse")),
        "{w:?}"
    );
    assert_eq!(
        vars.node_enabled("var('flag')", &BoundNames::default(), no_macros()),
        Some(false),
        "vars: still backs the node scope"
    );
    // A non-mapping `vars:` inside vars.yml → the same ignored-vars warning.
    std::fs::write(dir.path().join("vars.yml"), "vars: not-a-map\n").unwrap();
    let mut w = Vec::new();
    Vars::load(
        dir.path(),
        &test_project(serde_json::json!({})),
        None,
        &mut w,
    );
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
    let (manifest, w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
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
fn sources_tree_enabled_disables_like_the_other_trees() {
    // `sources:` `+enabled` is applied (dbt disables `source.p.s.t` by
    // fqn) — the same var-scoped evaluation as `models:`/`seeds:`.
    let vars = test_vars(
        serde_json::json!({"flag": true}),
        serde_json::json!({}),
        "p",
    );
    let mut w = Vec::new();
    let cfg = resolve_tree_config(
        &serde_json::json!({"p": {"+enabled": "{{ var('flag') }}"}}),
        &["p".to_string()],
        "sources".to_string(),
        &mut w,
        Some(&vars),
    );
    assert_eq!(cfg.enabled, Some(true));
    assert!(w.is_empty(), "{w:?}");
    let cfg = resolve_tree_config(
        &serde_json::json!({"p": {"+enabled": false}}),
        &["p".to_string()],
        "sources".to_string(),
        &mut w,
        Some(&vars),
    );
    assert_eq!(cfg.enabled, Some(false));
    assert!(w.is_empty(), "{w:?}");
    // Unresolvable → warn once, stays enabled (never a silent disable).
    let cfg = resolve_tree_config(
        &serde_json::json!({"p": {"+enabled": "{{ var('missing') }}"}}),
        &["p".to_string()],
        "sources".to_string(),
        &mut w,
        Some(&vars),
    );
    assert_eq!(cfg.enabled, None);
    assert_eq!(w.len(), 1, "{w:?}");
}

#[test]
fn malformed_enabled_tails_warn_instead_of_disabling() {
    // A `and`/`or` rhs that fails to parse is a syntax error in dbt — the
    // expression is unprovable, so the node stays enabled with a warning
    // rather than silently dropping.
    let vars = test_vars(serde_json::json!({"x": false}), serde_json::json!({}), "p");
    for expr in [
        "var('x') and",
        "var('x') and var('y'",
        "var('x') or",
        "var('x') not",
    ] {
        assert_eq!(enabled_expr(expr, &vars), None, "{expr}");
    }
    // `()` is a legal empty tuple — `False and ()` short-circuits to
    // `False` in Python too.
    assert_eq!(enabled_expr("var('x') and ()", &vars), Some(false));
    // `is` tests in the tolerant node render: a missing `var()` yields
    // Python `None` (not Jinja's Undefined) — `None is defined` is TRUE
    // while `is none` also answers True; a bare missing NAME is the
    // only Undefined the scope reaches.
    assert_eq!(enabled_expr("var('missing') is defined", &vars), Some(true));
    assert_eq!(
        enabled_expr("var('missing') is not defined", &vars),
        Some(false)
    );
    assert_eq!(
        enabled_expr("a_bare_missing_name is defined", &vars),
        Some(false)
    );
    assert_eq!(enabled_expr("var('missing') is none", &vars), Some(true));
    assert_eq!(enabled_expr("var('x') is defined", &vars), Some(true));
    assert_eq!(enabled_expr("var('x') is none", &vars), Some(false));
    assert_eq!(enabled_expr("var('x') is boolean", &vars), Some(true));
    // `False is number` renders True — a Python bool IS a Number
    // (`isinstance(False, int)`), verified on Jinja 3.1.
    assert_eq!(enabled_expr("var('x') is number", &vars), Some(true));
    assert_eq!(enabled_expr("not var('x') is defined", &vars), Some(false));
    // A rhs that can't be resolved to a value (`env_var`/`target` calls)
    // makes the whole expression unprovable — enabled + warning, never a
    // silent disable.
    assert_eq!(
        enabled_expr("var('x') and env_var('Y') == '1'", &vars),
        None
    );
    assert_eq!(enabled_expr("var('x') and target.name == 'p'", &vars), None);
}

// -- Jinja branch structure --------------------------------------------------

/// The arm path `text[offset]` sits inside — `jinja_branches` on the
/// masked text, like [`CallSiteResolver`](crate::lint) reads it.
fn path_at(text: &str, offset: usize) -> Vec<ArmStep> {
    jinja_branches(&mask_jinja_comments(text))
        .arm_path(offset)
        .to_vec()
}

/// An `{% if %}`-chain step literal — `ArmKind::For` only ever comes
/// from `{% for %}` blocks.
fn if_arm(chain: u32, arm: u32) -> ArmStep {
    ArmStep {
        chain,
        arm,
        kind: ArmKind::If,
    }
}

/// A `{% for %}`-chain step literal — arm 0 the body, arm 1 its
/// `{% else %}` fallback.
fn for_arm(chain: u32, arm: u32) -> ArmStep {
    ArmStep {
        chain,
        arm,
        kind: ArmKind::For,
    }
}

#[test]
fn jinja_branches_index_if_elif_else_arms() {
    let sql = "a{% if x %}b{% elif y %}c{% else %}d{% endif %}e";
    let arm = |c: &str| path_at(sql, sql.find(c).unwrap());
    assert_eq!(arm("a"), vec![]);
    assert_eq!(arm("b"), vec![if_arm(0, 0)]);
    assert_eq!(arm("c"), vec![if_arm(0, 1)]);
    assert_eq!(arm("d"), vec![if_arm(0, 2)]);
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
    assert_eq!(x, vec![if_arm(0, 0), if_arm(1, 0)]);
    assert_eq!(y, vec![if_arm(0, 0), if_arm(1, 1)]);
    assert_eq!(z, vec![if_arm(0, 1)]);
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
    assert_eq!(arm("a"), vec![for_arm(0, 0)]);
    assert_eq!(arm("b"), vec![for_arm(0, 1)]);
    assert_eq!(arm("z"), vec![if_arm(1, 0)]);
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

// ---- branch-aware config() verdicts ------------------------------------------

#[test]
fn enabled_in_unselectable_arms_never_disables() {
    let vars = no_vars();
    // A proven-true arm's `enabled=false` parks the node; a proven-false
    // arm's never applies; an unprovable arm's keeps the node and marks
    // the verdict unproven, naming the unknown condition.
    assert_eq!(
        enabled("{% if true %}{{ config(enabled=false) }}{% endif %}", &vars).0,
        Some(Enabled::Off)
    );
    assert_eq!(
        enabled(
            "{% if false %}{{ config(enabled=false) }}{% endif %}",
            &vars
        )
        .0,
        None
    );
    for (sql, gate) in [
        (
            "{% if target.name == 'prod' %}{{ config(enabled=false) }}{% endif %}",
            "target.name == 'prod'",
        ),
        (
            "{% set flag = target.name %}{% if flag %}{{ config(enabled=false) }}{% endif %}",
            "flag",
        ),
        (
            "{% for x in items %}{{ config(enabled=false) }}{% endfor %}",
            "x in items",
        ),
    ] {
        let (verdict, warnings) = enabled(sql, &vars);
        assert_eq!(verdict, Some(Enabled::Unproven), "{sql}");
        assert!(
            warnings.iter().any(|w| w.contains(gate)),
            "{sql} should warn naming {gate:?}: {warnings:?}"
        );
    }
    // `execute` is parse-time false — an `enabled=false` parked behind it
    // is dead code, not a disable.
    assert_eq!(
        enabled(
            "{% if execute %}{{ config(enabled=false) }}{% endif %}",
            &vars
        )
        .0,
        None
    );
    // A bare undefined name is Jinja-falsy — provably NOT taken, not
    // merely unprovable.
    assert_eq!(
        enabled("{% if flag %}{{ config(enabled=false) }}{% endif %}", &vars).0,
        None
    );
    // An unconditional `enabled=false` plus a gated conflicting call is
    // still unproven — dbt's outcome leans on the unknown arm either way.
    let (verdict, warnings) = enabled(
        "{{ config(enabled=false) }}\n{% if target.name %}{{ config(enabled=true) }}{% endif %}",
        &vars,
    );
    assert_eq!(verdict, Some(Enabled::Unproven));
    assert!(!warnings.is_empty());
    // Both arms agreeing on `enabled=false` still isn't PROVEN off — no
    // call executes unconditionally, so the node stays and is marked
    // rather than silently parked.
    let (verdict, warnings) = enabled(
        "{% if target.name %}{{ config(enabled=false) }}{% else %}{{ config(enabled=false) }}{% endif %}",
        &vars,
    );
    assert_eq!(verdict, Some(Enabled::Unproven));
    assert!(!warnings.is_empty());
}

#[test]
fn enabled_arms_evaluate_resolvable_conditions() {
    // `{% if var('flag', false) %}` — the default keeps the node; a vars
    // map flipping it drops it.
    let sql = "{% if var('flag', false) %}{{ config(enabled=false) }}{% endif %}";
    assert_eq!(enabled(sql, &no_vars()).0, None);
    let vars_on = test_vars(
        serde_json::json!({"flag": true}),
        serde_json::json!({}),
        "p",
    );
    assert_eq!(enabled(sql, &vars_on).0, Some(Enabled::Off));
    // `{% if var('x', 1) == 1 %}…{% else %}…{% endif %}` — the true arm
    // runs, the else arm's `enabled=false` never applies.
    assert_eq!(
        enabled(
            "{% if var('x', 1) == 1 %}a{% else %}{{ config(enabled=false) }}{% endif %}",
            &no_vars()
        )
        .0,
        None
    );
    // `{% if not execute %}` — `execute` is parse-time false, so `not
    // execute` is a PROVEN arm: its `enabled=false` drops the node.
    assert_eq!(
        enabled(
            "{% if not execute %}{{ config(enabled=false) }}{% endif %}",
            &no_vars()
        )
        .0,
        Some(Enabled::Off)
    );
    // `{% if execute %}` alone leaves the verdict untouched AND unmarked —
    // a provable arm warns nothing.
    assert_eq!(
        enabled(
            "{% if execute %}{{ config(enabled=false) }}{% endif %}",
            &no_vars()
        ),
        (None, vec![])
    );
    // `is defined` on a never-bound name is provably false → drops.
    assert_eq!(
        enabled(
            "{{ config(enabled=nonexistent_flag is defined) }}",
            &no_vars()
        )
        .0,
        Some(Enabled::Off)
    );
    // An unprovable arm carrying only `enabled=true` changes nothing —
    // no mark, no warning.
    assert_eq!(
        enabled(
            "{% if target.name %}{{ config(enabled=true) }}{% endif %}",
            &no_vars()
        ),
        (Some(Enabled::On), vec![])
    );
    // Two unprovable arms DISAGREEING — `enabled=false` then
    // `enabled=true`: the renders that take only the first arm leave the
    // node disabled, so the verdict can't settle at On — Unproven, and
    // the warning names the diverging call's condition.
    let (verdict, warnings) = enabled(
        "{% if target.name == 'a' %}{{ config(enabled=false) }}{% endif %}\n\
         {% if target.name == 'b' %}{{ config(enabled=true) }}{% endif %}",
        &no_vars(),
    );
    assert_eq!(verdict, Some(Enabled::Unproven));
    assert!(warnings.iter().any(|w| w.contains("target.name == 'a'")));
    // An unconditional call AFTER the gated ones overrides them all —
    // the gated calls can never be the effective last.
    assert_eq!(
        enabled(
            "{% if target.name %}{{ config(enabled=false) }}{% endif %}\n{{ config(enabled=true) }}",
            &no_vars()
        ),
        (Some(Enabled::On), vec![])
    );
    // `{% set flag = var('x', true) %}{% if not flag %}…` — a bound name
    // is unprovable even though the var has a default: the node stays
    // and is marked.
    let (verdict, warnings) = enabled(
        "{% set flag = var('x', true) %}{% if not flag %}{{ config(enabled=false) }}{% endif %}",
        &no_vars(),
    );
    assert_eq!(verdict, Some(Enabled::Unproven));
    assert!(warnings.iter().any(|w| w.contains("not flag")));
    // A bound name inside `enabled=` itself is unprovable too.
    let (verdict, _) = enabled("{{ config(enabled=flag == true) }}", &no_vars());
    // `flag` unbound entirely → Undefined → `Undefined == true` is false
    // → provably OFF, matching Jinja. (Bound only via {% set %} it would
    // be unprovable — see the arm case above.)
    assert_eq!(verdict, Some(Enabled::Off));
    // A bound `execute` shadows the parse-time `false` — Jinja resolves
    // the `{% set %}` value, which can't be read from source, so the
    // call stays unproven rather than a guessed disable.
    let (verdict, warnings) = enabled(
        "{% set execute = 1 %}{{ config(enabled=execute) }}",
        &no_vars(),
    );
    assert_eq!(verdict, Some(Enabled::Unproven));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

#[test]
fn enabled_in_elif_conditions_follows_the_chain() {
    let vars = no_vars();
    // `{% elif %}`'s own expressions evaluate when the EARLIER arms were
    // falsy — a proven-true `{% if %}` parks the elif's config entirely,
    // a proven-false one lets it run, an unprovable one gates it.
    assert_eq!(
        enabled(
            "{% if true %}a{% elif config(enabled=false) %}b{% endif %}",
            &vars
        )
        .0,
        None
    );
    assert_eq!(
        enabled(
            "{% if false %}a{% elif config(enabled=false) %}b{% endif %}",
            &vars
        )
        .0,
        Some(Enabled::Off)
    );
    let (verdict, warnings) = enabled(
        "{% if target.name %}a{% elif config(enabled=false) %}b{% endif %}",
        &vars,
    );
    assert_eq!(verdict, Some(Enabled::Unproven));
    assert!(
        warnings.iter().any(|w| w.contains("target.name")),
        "{warnings:?}"
    );
}

#[test]
fn enabled_in_statement_tags_and_definition_bodies() {
    let vars = no_vars();
    // `{% do %}`/`{% set _ = %}` invoke config() exactly like `{{ }}`.
    for sql in [
        "{% do config(enabled=false) %}",
        "{% set _ = config(enabled=false) %}",
    ] {
        assert_eq!(enabled(sql, &vars).0, Some(Enabled::Off), "{sql}");
    }
    // …and obey arm gates the same way.
    let (verdict, _) = enabled(
        "{% if target.name %}{% do config(enabled=false) %}{% endif %}",
        &vars,
    );
    assert_eq!(verdict, Some(Enabled::Unproven));
    // Deferred definition bodies never run top-level — a `config()` inside
    // a macro/test/docs/materialization/verification block applies (at
    // most) at an invocation the parse can't prove.
    for (open, close) in [
        ("{% macro m() %}", "{% endmacro %}"),
        ("{% docs d %}", "{% enddocs %}"),
        (
            "{% materialization m, default %}",
            "{% endmaterialization %}",
        ),
        ("{% test t() %}", "{% endtest %}"),
        ("{% verification v() %}", "{% endverification %}"),
    ] {
        let sql = format!("{open}{{{{ config(enabled=false) }}}}{close}");
        assert_eq!(
            enabled(&sql, &vars).0,
            None,
            "config inside {open} must not disable the model"
        );
    }
}

#[test]
fn non_enabled_keys_in_unselectable_arms_contribute_conservatively() {
    let pat = Patterns::new();
    let vars = no_vars();
    // Non-`enabled` keys can't drop a node — both arms may contribute, so
    // the gated call's literal still lands (last-resolvable-literal order).
    let sql =
        "{% if target.name %}{{ config(materialized='table') }}{% else %}{{ config(materialized='view') }}{% endif %}";
    let scope = scope_of(sql, &vars);
    assert_eq!(pat.materialized(sql, &scope), Some("view".to_string()));
    // A gated call DOES override an earlier unconditional literal — the
    // conservative direction keeps the later value, never fabricates.
    let sql = "{{ config(materialized='view') }}\n{% if target.name %}{{ config(materialized='table') }}{% endif %}";
    let scope = scope_of(sql, &vars);
    assert_eq!(pat.materialized(sql, &scope), Some("table".to_string()));
    // A proven-dead arm contributes nothing, even to non-enabled keys.
    let sql = "{{ config(materialized='view') }}\n{% if false %}{{ config(materialized='table') }}{% endif %}";
    let scope = scope_of(sql, &vars);
    assert_eq!(pat.materialized(sql, &scope), Some("view".to_string()));
    // A deferred body contributes nothing either.
    let sql = "{{ config(materialized='view') }}\n{% macro m() %}{{ config(materialized='table') }}{% endmacro %}";
    let scope = scope_of(sql, &vars);
    assert_eq!(pat.materialized(sql, &scope), Some("view".to_string()));
    // A resolvable arm picks its own literal — `var()` resolves the
    // condition, so the dead arm's `table` never lands.
    let sql = "{% if var('m', 'view') == 'view' %}{{ config(materialized='view') }}{% else %}{{ config(materialized='table') }}{% endif %}";
    let scope = scope_of(sql, &vars);
    assert_eq!(pat.materialized(sql, &scope), Some("view".to_string()));
    // A bare `config(…)` inside a `--` comment is SQL text to dbt, not a
    // Jinja call — it applies nothing.
    let sql = "-- note: config(materialized='table') someday\nselect 1";
    let scope = scope_of(sql, &vars);
    assert_eq!(pat.materialized(sql, &scope), None);
    // …while `{% set _x = config(…) %}` IS a call and applies.
    let sql = "{% set _x = config(materialized='table') %}\nselect 1";
    let scope = scope_of(sql, &vars);
    assert_eq!(pat.materialized(sql, &scope), Some("table".to_string()));
}

// ---- config({...}) positional-dict spelling ---------------------------------

#[test]
fn config_dict_arg_reads_keys_like_kwargs() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    let mut ev = Vec::new();
    // The dict spelling is the same call's kwargs (verified: real dbt
    // materializes `config({"materialized": "table"})` identically to the
    // kwarg form).
    assert_eq!(
        pat.materialized(
            r#"{{ config({"materialized": "table"}) }}"#,
            &scope_of(r#"{{ config({"materialized": "table"}) }}"#, &no_vars())
        ),
        Some("table".to_string())
    );
    // Single-quoted keys and whitespace work the same.
    assert_eq!(
        pat.materialized(
            "{{ config({ 'materialized' : 'ephemeral' }) }}",
            &scope_of("{{ config({ 'materialized' : 'ephemeral' }) }}", &no_vars())
        ),
        Some("ephemeral".to_string())
    );
    // `enabled`/`alias`/`unique_key`/`meta` and the governance readers all
    // route through the same lookup.
    assert_eq!(
        pat.enabled(
            r#"{{ config({"enabled": false}) }}"#,
            &scope_of(r#"{{ config({"enabled": false}) }}"#, &vars),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        Some(Enabled::Off)
    );
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(
        config_alias(
            r#"{{ config({"alias": "renamed"}) }}"#,
            &scope_of(r#"{{ config({"alias": "renamed"}) }}"#, &no_vars())
        ),
        Some("renamed".to_string())
    );
    assert_eq!(
        config_unique_key(
            r#"{{ config({"unique_key": ["a", "b"]}) }}"#,
            &scope_of(r#"{{ config({"unique_key": ["a", "b"]}) }}"#, &no_vars())
        ),
        Some(serde_json::json!(["a", "b"]))
    );
    let m = config_meta(
        r#"{{ config({"meta": {"owner": "alice"}}) }}"#,
        &scope_of(r#"{{ config({"meta": {"owner": "alice"}}) }}"#, &no_vars()),
    )
    .unwrap();
    assert_eq!(m["owner"], serde_json::json!("alice"));
    let gov = config_governance(
        r#"{{ config({"group": "g1", "access": "private"}) }}"#,
        &scope_of(
            r#"{{ config({"group": "g1", "access": "private"}) }}"#,
            &no_vars(),
        ),
    );
    assert_eq!(gov.group.as_deref(), Some("g1"));
    assert_eq!(gov.access.as_deref(), Some("private"));
}

#[test]
fn config_dict_arg_joins_the_ordered_call_sequence() {
    let pat = Patterns::new();
    let vars = no_vars();
    let mut w = Vec::new();
    let mut ev = Vec::new();
    // The dict call is one step of the same last-wins chain the kwarg calls
    // follow (verified: real dbt applies them in order).
    assert_eq!(
        pat.materialized(
            "{{ config(materialized='view') }}\n{{ config({\"materialized\": \"table\"}) }}",
            &scope_of(
                "{{ config(materialized='view') }}\n{{ config({\"materialized\": \"table\"}) }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.materialized(
            "{{ config({\"materialized\": \"table\"}) }}\n{{ config(materialized='view') }}",
            &scope_of(
                "{{ config({\"materialized\": \"table\"}) }}\n{{ config(materialized='view') }}",
                &no_vars()
            )
        ),
        Some("view".to_string())
    );
    assert_eq!(
        pat.enabled(
            "{{ config(enabled=true) }}\n{{ config({\"enabled\": false}) }}",
            &scope_of(
                "{{ config(enabled=true) }}\n{{ config({\"enabled\": false}) }}",
                &vars
            ),
            &mut w,
            &mut ev,
            "m.sql"
        )
        .resolve(None, &mut w, &mut ev, "m.sql"),
        Some(Enabled::Off)
    );
    // A later LITERAL entry overrides; an unresolvable VALUE inside a dict
    // can't clear an earlier literal — the same stance unresolvable kwargs
    // take. Literal keys around it still land.
    assert_eq!(
        pat.materialized(
            "{{ config(materialized='view') }}\n{{ config({\"materialized\": var('m'), \"group\": \"g\"}) }}", &scope_of(
            "{{ config(materialized='view') }}\n{{ config({\"materialized\": var('m'), \"group\": \"g\"}) }}", &no_vars())),
        Some("view".to_string())
    );
    let gov = config_governance(
        "{{ config({\"materialized\": var('m'), \"group\": \"g\"}) }}",
        &scope_of(
            "{{ config({\"materialized\": var('m'), \"group\": \"g\"}) }}",
            &no_vars(),
        ),
    );
    assert_eq!(gov.group.as_deref(), Some("g"));
    // Meta entries merge per key across the dict and kwarg spellings.
    let m = config_meta(
        "{{ config(meta={'a': 1}) }}\n{{ config({\"meta\": {\"b\": 2}}) }}",
        &scope_of(
            "{{ config(meta={'a': 1}) }}\n{{ config({\"meta\": {\"b\": 2}}) }}",
            &no_vars(),
        ),
    )
    .unwrap();
    assert_eq!(m["a"], serde_json::json!(1));
    assert_eq!(m["b"], serde_json::json!(2));
}

#[test]
fn config_dict_arg_shape_edges() {
    let pat = Patterns::new();
    // Only the FIRST positional arg is the dict spelling — a dict bound to
    // a kwarg is a value, not a config dict.
    assert_eq!(
        pat.materialized(
            "{{ config(meta={'materialized': 'table'}) }}",
            &scope_of("{{ config(meta={'materialized': 'table'}) }}", &no_vars())
        ),
        None,
        "the kwarg's dict value is not the config dict"
    );
    // A non-string key can't name a config; a duplicate key yields the
    // last entry (the Python-dict rule); non-literal keys are skipped.
    assert_eq!(
        pat.materialized(
            "{{ config({1: 'x', 'materialized': 'table'}) }}",
            &scope_of(
                "{{ config({1: 'x', 'materialized': 'table'}) }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.materialized(
            "{{ config({'materialized': 'view', 'materialized': 'table'}) }}",
            &scope_of(
                "{{ config({'materialized': 'view', 'materialized': 'table'}) }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    // A bare `config()` and a first arg that isn't a dict contribute nothing.
    assert_eq!(
        pat.materialized("{{ config() }}", &scope_of("{{ config() }}", &no_vars())),
        None
    );
    assert_eq!(
        pat.materialized(
            "{{ config('materialized') }}",
            &scope_of("{{ config('materialized') }}", &no_vars())
        ),
        None
    );
}

// ---- config(**{…}) / `*` spreads — the argument-shape rule -------------------

#[test]
fn config_spread_args_bind_like_literal_kwargs() {
    let pat = Patterns::new();
    // `**{…}` appended after the explicit kwargs is dbt-valid (verified
    // on 1.12.6): its entries bind like kwargs written in place.
    assert_eq!(
        pat.materialized(
            "{{ config(**{'materialized': 'table'}) }}",
            &scope_of("{{ config(**{'materialized': 'table'}) }}", &no_vars())
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.materialized(
            "{{ config(alias='a', **{'materialized': 'table'}) }}",
            &scope_of(
                "{{ config(alias='a', **{'materialized': 'table'}) }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    assert_eq!(
        config_alias(
            "{{ config(alias='a', **{'materialized': 'table'}) }}",
            &scope_of(
                "{{ config(alias='a', **{'materialized': 'table'}) }}",
                &no_vars()
            )
        ),
        Some("a".to_string())
    );
    // `*[{…}]` / `*({…},)` unpack into the one-dict positional spelling.
    assert_eq!(
        pat.materialized(
            "{{ config(*[{'materialized': 'table'}]) }}",
            &scope_of("{{ config(*[{'materialized': 'table'}]) }}", &no_vars())
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.materialized(
            "{{ config(*({'materialized': 'table'},)) }}",
            &scope_of("{{ config(*({'materialized': 'table'},)) }}", &no_vars())
        ),
        Some("table".to_string())
    );
    // A `**` operand that isn't a literal resolves at render — nothing a
    // static read can bind from the operand itself (dbt reaches it
    // through its own render; explicit kwargs beside it still bind).
    assert_eq!(
        pat.materialized(
            "{{ config(**var('cfg')) }}",
            &scope_of("{{ config(**var('cfg')) }}", &no_vars())
        ),
        None
    );
    // A trailing comma is still a valid call.
    assert_eq!(
        pat.materialized(
            "{{ config(**{'materialized': 'table'},) }}",
            &scope_of("{{ config(**{'materialized': 'table'},) }}", &no_vars())
        ),
        Some("table".to_string())
    );
}

#[test]
fn config_render_spread_binds_kwargs_and_warns() {
    let pat = Patterns::new();
    // `config(materialized='table', **var('cfg'))` — dbt RENDERS the
    // operand, so a resolvable var lands the whole call (verified on
    // 1.12.6); unresolvable fails at render. The static read keeps the
    // explicit kwargs and warns instead of dropping the call.
    assert_eq!(
        pat.materialized(
            "{{ config(materialized='table', **var('cfg')) }}",
            &scope_of(
                "{{ config(materialized='table', **var('cfg')) }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    assert_eq!(
        pat.materialized(
            "{{ config(*var('cfg'), materialized='table') }}",
            &scope_of(
                "{{ config(*var('cfg'), materialized='table') }}",
                &no_vars()
            )
        ),
        Some("table".to_string())
    );
    let mut w = Vec::new();
    config_shape_warnings(
        "{{ config(materialized='table', **var('cfg')) }}",
        &scope_of(
            "{{ config(materialized='table', **var('cfg')) }}",
            &no_vars(),
        ),
        &mut w,
        "models/m.sql",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("not a literal"), "{w:?}");
    // `**{1: 'x'}` — a non-string literal key is Python's
    // "keywords must be strings" TypeError: the call shape dbt rejects.
    assert_eq!(
        pat.materialized(
            "{{ config(**{1: 'x', 'materialized': 'table'}) }}",
            &scope_of(
                "{{ config(**{1: 'x', 'materialized': 'table'}) }}",
                &no_vars()
            )
        ),
        None
    );
    let mut w = Vec::new();
    config_shape_warnings(
        "{{ config(**{1: 'x'}) }}",
        &scope_of("{{ config(**{1: 'x'}) }}", &no_vars()),
        &mut w,
        "models/m.sql",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("not a shape dbt accepts"), "{w:?}");
    // Shape warnings only read `{{ config(…) }}` calls — `config(…)` in
    // ordinary SQL text is not a config call; a call inside a SQL `--`
    // comment still is (Jinja renders the whole file, comment or not).
    let mut w = Vec::new();
    config_shape_warnings(
        "select config(1, 2)\n-- {{ config({'a': 1}, alias='x') }}",
        &scope_of(
            "select config(1, 2)\n-- {{ config({'a': 1}, alias='x') }}",
            &no_vars(),
        ),
        &mut w,
        "models/m.sql",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    // A bare `{{ config() }}` carries no arguments — a valid no-op call
    // that warns on nothing.
    let mut w = Vec::new();
    config_shape_warnings(
        "{{ config() }}",
        &scope_of("{{ config() }}", &no_vars()),
        &mut w,
        "models/m.sql",
    );
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn config_shape_warns_once_per_call() {
    let mut w = Vec::new();
    config_shape_warnings(
        "{{ config({'a': 1}, alias='x') }}\n{{ config({'b': 2}, alias='y') }}",
        &scope_of(
            "{{ config({'a': 1}, alias='x') }}\n{{ config({'b': 2}, alias='y') }}",
            &no_vars(),
        ),
        &mut w,
        "models/m.sql",
    );
    assert_eq!(w.len(), 2, "{w:?}");
}

#[test]
fn config_arg_shapes_dbt_rejects_apply_nothing() {
    let pat = Patterns::new();
    for (sql, why) in [
        // A dict mixed with kwargs — "Invalid inline model config" in dbt.
        (
            "{{ config({'materialized': 'table'}, alias='x') }}",
            "dict then kwarg",
        ),
        (
            "{{ config(alias='x', {'materialized': 'table'}) }}",
            "kwarg then dict",
        ),
        // Two positional dicts — `config({'a': 1}, {'b': 2})` is the same
        // invalid shape (a call can't carry two positional dicts).
        (
            "{{ config({'a': 1}, {'materialized': 'table'}) }}",
            "two dicts",
        ),
        // `*{…}` unpacks the dict's KEYS into positional strings.
        ("{{ config(*{'materialized': 'table'}) }}", "star dict"),
        // `**` must be the LAST argument and appear at most once.
        (
            "{{ config(**{'materialized': 'table'}, alias='x') }}",
            "spread mid-args",
        ),
        (
            "{{ config(**{'materialized': 'table'}, **{'x': 1}) }}",
            "two spreads",
        ),
        (
            "{{ config({'a': 1}, **{'materialized': 'table'}) }}",
            "dict then spread",
        ),
        // A `**` key colliding with an explicit kwarg is Python's
        // "multiple values" error.
        (
            "{{ config(materialized='view', **{'materialized': 'table'}) }}",
            "spread key collides",
        ),
        // `f(k=1, k=2)` — a repeated kwarg name — is a Python error.
        (
            "{{ config(materialized='view', materialized='table') }}",
            "repeated kwarg",
        ),
        // Positional non-dict args — `config('x')`, `config(None)`, a
        // positional after a kwarg.
        ("{{ config(None) }}", "positional none"),
        (
            "{{ config(materialized='table', 'x') }}",
            "kwarg then positional",
        ),
        (
            "{{ config(*[{'a': 1}, {'materialized': 'table'}]) }}",
            "star two dicts",
        ),
        ("{{ config(*['x']) }}", "star string"),
        // A render-time spread mixed with a non-dict positional —
        // `*var(…)` could only unpack positional args, and `'y'` is not
        // a dict: invalid outright (verified on 1.12.6).
        ("{{ config(*var('cfg'), 'y') }}", "star then non-dict"),
    ] {
        assert_eq!(
            pat.materialized(sql, &scope_of(sql, &no_vars())),
            None,
            "{why}: {sql}"
        );
    }
}

#[test]
fn config_tags_accumulate_each_call_in_order() {
    assert_eq!(
        config_tags(
            "{{ config(tags=['a', 'b']) }}\n{{ config(tags=['c']) }}",
            &scope_of(
                "{{ config(tags=['a', 'b']) }}\n{{ config(tags=['c']) }}",
                &no_vars()
            )
        ),
        vec!["a", "b", "c"]
    );
    // A bare scalar wraps to a one-element list; numbers stringify —
    // dbt's StringOrArrayOfStrings coercion (verified on 1.12.6).
    assert_eq!(
        config_tags(
            "{{ config(tags='solo') }}\n{{ config(tags=[7, 'x']) }}",
            &scope_of(
                "{{ config(tags='solo') }}\n{{ config(tags=[7, 'x']) }}",
                &no_vars()
            )
        ),
        vec!["solo", "7", "x"]
    );
    // The dict and `**` spellings bind the same; a shape dbt rejects
    // contributes nothing.
    assert_eq!(
        config_tags(
            "{{ config({'tags': ['d']}) }}\n{{ config(**{'tags': ['s']}) }}\n{{ config(tags=['a'], {'tags': ['b']}) }}"
        , &scope_of(
            "{{ config({'tags': ['d']}) }}\n{{ config(**{'tags': ['s']}) }}\n{{ config(tags=['a'], {'tags': ['b']}) }}"
        , &no_vars())),
        vec!["d", "s"]
    );
    // A non-literal `tags` yields nothing for that call — an earlier
    // literal still stands (the last-resolvable stance).
    assert_eq!(
        config_tags(
            "{{ config(tags=['kept']) }}\n{{ config(tags=var('t')) }}",
            &scope_of(
                "{{ config(tags=['kept']) }}\n{{ config(tags=var('t')) }}",
                &no_vars()
            )
        ),
        vec!["kept"]
    );
}

#[test]
fn tree_config_tags_accumulate_outer_first() {
    // `+tags` is the one tree key that ACCUMULATES down the levels —
    // shallower first (verified on 1.12.6). A bare scalar wraps and
    // elements stringify (`7` → `"7"`); a non-scalar member warns once.
    let mut w = Vec::new();
    let tree = yaml_to_json("p:\n  +tags: [a]\n  staging:\n    +tags: [b, 7]\n", &mut w).unwrap();
    let cfg = resolve_tree_config(
        &tree,
        &["p".to_string(), "staging".to_string()],
        "models".to_string(),
        &mut w,
        Some(&no_vars()),
    );
    assert_eq!(cfg.tags, vec!["a", "b", "7"]);
    assert!(w.is_empty(), "{w:?}");
    let mut w = Vec::new();
    let tree = yaml_to_json("p:\n  +tags: 'solo'\n", &mut w).unwrap();
    let cfg = resolve_tree_config(
        &tree,
        &["p".to_string()],
        "models".to_string(),
        &mut w,
        Some(&no_vars()),
    );
    assert_eq!(cfg.tags, vec!["solo"]);
    let mut w = Vec::new();
    let tree = yaml_to_json("p:\n  +tags: [{k: 1}, 'ok']\n", &mut w).unwrap();
    let cfg = resolve_tree_config(
        &tree,
        &["p".to_string()],
        "models".to_string(),
        &mut w,
        Some(&no_vars()),
    );
    assert_eq!(cfg.tags, vec!["ok"]);
    assert_eq!(w.len(), 1, "{w:?}");
}

#[test]
fn source_tree_ignores_tags_entirely() {
    // dbt ignores a `+tags` rung on the `sources:` tree — so completely
    // that even a non-scalar member there earns no warning (verified on
    // 1.12.6: only the model/seed/snapshot/exposures trees read `+tags`).
    let mut w = Vec::new();
    let tree = yaml_to_json("p:\n  raw:\n    +tags: [{k: 1}]\n", &mut w).unwrap();
    let cfg = resolve_source_tree_config(
        &tree,
        &["p".to_string(), "raw".to_string()],
        "sources".to_string(),
        &mut w,
        Some(&no_vars()),
    );
    assert!(cfg.tags.is_empty(), "{:?}", cfg.tags);
    assert!(w.is_empty(), "{w:?}");
}

// ---- node tags: tree `+tags` → in-file `config(tags=…)` → `config.tags:` ----

/// A scratch project exercising every node-tag channel: two `+tags`
/// levels, in-file `config(tags=…)`, schema `config.tags:` on ordinary
/// and `versions:`-declared models, seeds, `{% snapshot %}` blocks, the
/// ignored top-level `tags:` properties — plus the exposure tag/group
/// rules and the `config.loaded_at_field: null` no-op.
fn write_tags_project(dir: &std::path::Path) {
    for sub in [
        "models/nested",
        "models/grp",
        "models/extra",
        "models/dup",
        "seeds",
        "snapshots",
    ] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::write(
        dir.join("dbt_project.yml"),
        r#"name: p
model-paths: [models]
seed-paths: [seeds]
snapshot-paths: [snapshots]
models:
  p:
    +tags: [m_tree]
    nested:
      +tags: [m_nested]
    dup:
      +tags: [m_dup, m_tree, m_dup]
seeds:
  p:
    +tags: [seed_tree]
snapshots:
  p:
    +tags: [snap_tree]
exposures:
  p:
    +tags: [exp_tree]
    grp:
      +group: exp_tree_grp
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("models/m.sql"),
        "{{ config(tags=['m_in', 'dup']) }}\nselect 1\n",
    )
    .unwrap();
    std::fs::write(dir.join("models/nested/n.sql"), "select 1\n").unwrap();
    // The dup rung — `+tags` repeats the outer rung's tag AND its own
    // element; node.tags still dedups the whole union (verified on
    // 1.12.6) even though the wire's config.tags keeps every occurrence.
    std::fs::write(dir.join("models/dup/d.sql"), "select 1\n").unwrap();
    // A `**{…}` config call — the dict the spread carries binds like
    // kwargs written in place (verified on 1.12.6).
    std::fs::write(
        dir.join("models/spread.sql"),
        "{{ config(**{'tags': ['spread_in'], 'materialized': 'table'}) }}\nselect 1\n",
    )
    .unwrap();
    // The versioned member — `yml_tags` records `ver ++ model`.
    std::fs::write(
        dir.join("models/ord_v1.sql"),
        "{{ config(tags=['ord_in']) }}\nselect 1\n",
    )
    .unwrap();
    std::fs::write(dir.join("seeds/orders.csv"), "id\n1\n").unwrap();
    std::fs::write(
        dir.join("snapshots/snap.sql"),
        "{% snapshot s %}{{ config(tags=['snap_in']) }} select 1 {% endsnapshot %}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("models/schema.yml"),
        r#"version: 2
models:
  - name: m
    tags: [m_ignored]
    config:
      tags: [dup, m_yml]
  - name: n
    tags: [n_ignored]
    config:
      tags: [n_yml]
  - name: ord
    config: {tags: [ord_model_cfg]}
    versions:
      - v: 1
        config: {tags: [ord_ver_cfg]}
seeds:
  - name: orders
    tags: [seed_ignored]
    config:
      tags: [seed_yml]
snapshots:
  - name: s
    tags: [snap_ignored]
    config:
      tags: [snap_yml]
groups:
  - name: grp
    owner: {name: ops}
  - name: exp_tree_grp
    owner: {name: ops}
sources:
  - name: nulled_src
    loaded_at_field: null
    config: {loaded_at_field: nulled_cfg_ts}
    tables:
      - name: t1
      - name: cfg_null
        config: {loaded_at_field: null}
      - name: prop_null
        loaded_at_field: null
"#,
    )
    .unwrap();
    // Exposures under `models/grp/` — fqn `[p, grp, <name>]` picks up
    // the `exposures: p:` `+tags` AND the `grp:` level's `+group`.
    std::fs::write(
        dir.join("models/grp/e.yml"),
        r#"version: 2
exposures:
  - name: exp_a
    type: dashboard
    owner: {name: ops}
    tags: [top_a]
    config:
      tags: [z_cfg, a_cfg]
      group: grp
    depends_on:
      - ref('m')
  - name: exp_b
    type: analysis
    owner: {name: ops}
    tags: [z_top]
    depends_on:
      - ref('m')
  - name: exp_c
    type: notebook
    owner: {name: ops}
    tags: [c_top]
    group: top_group_ignored
    config:
      tags: []
    depends_on:
      - ref('m')
"#,
    )
    .unwrap();
    // `exp_d` under `models/extra/` — inside `exposures: p:`'s `+tags`
    // but OUTSIDE the `grp:` level's `+group`. Declared `group:` only at
    // the top level — the property dbt ignores — so `config.group`
    // stays absent.
    std::fs::write(
        dir.join("models/extra/e.yml"),
        "version: 2\n\
         exposures:\n  - name: exp_d\n    type: ml\n    owner: {name: ops}\n    tags: [d_top]\n    group: top_only\n    depends_on:\n      - ref('m')\n",
    )
    .unwrap();
}

#[test]
fn node_tags_merge_tree_infile_yml_in_dbt_order() {
    let dir = tempfile::tempdir().unwrap();
    write_tags_project(dir.path());
    let m = manifest_from_source(dir.path()).unwrap();
    // `m` — every channel: tree `+tags` (outer then inner level when the
    // fqn descends), the file's `config(tags=…)`, the entry's
    // `config.tags:` — dedup'd, first occurrence keeping its slot
    // (verified on 1.12.6). The top-level `tags:` is a no-op.
    assert_eq!(
        m.nodes["model.p.m"].tags,
        vec!["m_tree", "m_in", "dup", "m_yml"]
    );
    // `n` — two tree levels accumulate outer-first; no in-file config.
    assert_eq!(
        m.nodes["model.p.n"].tags,
        vec!["m_tree", "m_nested", "n_yml"]
    );
    // `d` — the union dedups ACROSS tree rungs and inside one list:
    // `+tags: [m_dup, m_tree, m_dup]` under `+tags: [m_tree]` still yields
    // each tag once (verified on 1.12.6 — only the wire's config.tags
    // keeps every occurrence).
    assert_eq!(m.nodes["model.p.d"].tags, vec!["m_tree", "m_dup"]);
    // The `**{…}` call's tags land in the in-file channel — and its
    // materialized binds too.
    assert_eq!(m.nodes["model.p.spread"].tags, vec!["m_tree", "spread_in"]);
    assert_eq!(
        m.nodes["model.p.spread"].config.materialized.as_deref(),
        Some("table")
    );
    // The versioned member: tree → in-file → (ver cfg then model cfg).
    assert_eq!(
        m.nodes["model.p.ord.v1"].tags,
        vec!["m_tree", "ord_in", "ord_ver_cfg", "ord_model_cfg"]
    );
    // Seeds and `{% snapshot %}` blocks share the same channels.
    assert_eq!(m.nodes["seed.p.orders"].tags, vec!["seed_tree", "seed_yml"]);
    assert_eq!(
        m.nodes["snapshot.p.s"].tags,
        vec!["snap_tree", "snap_in", "snap_yml"]
    );
    for uid in m.nodes.keys() {
        for ignored in ["m_ignored", "n_ignored", "seed_ignored", "snap_ignored"] {
            assert!(
                !m.nodes[uid].tags.contains(&ignored.to_string()),
                "{uid} must not carry the ignored top-level property"
            );
        }
    }
}

#[test]
fn exposure_tags_and_group_follow_the_wire_rules() {
    let dir = tempfile::tempdir().unwrap();
    write_tags_project(dir.path());
    let (m, warnings) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    // `exp_a` — `config.tags` declared: `config.tags ++ tags:` wins over
    // the tree's `+tags` outright, the union sorted + dedup'd the way the
    // wire keeps it (verified on 1.12.6). `config.group` lands in
    // `config` — the wire's only group slot.
    let a = &m.exposures["exposure.p.exp_a"];
    assert_eq!(a.tags, vec!["a_cfg", "top_a", "z_cfg"]);
    assert_eq!(a.config.tags, a.tags);
    assert_eq!(a.config.group.as_deref(), Some("grp"));
    // `exp_b` — no `config.tags`: `tags: ++ +tags`, still sorted.
    let b = &m.exposures["exposure.p.exp_b"];
    assert_eq!(b.tags, vec!["exp_tree", "z_top"]);
    assert_eq!(b.config.tags, b.tags);
    assert_eq!(b.config.group.as_deref(), Some("exp_tree_grp"));
    // `exp_c` — `config: {tags: []}`: declared-and-empty STILL suppresses
    // the tree's `+tags` (verified) — and the top-level `group:` property
    // never lands.
    let c = &m.exposures["exposure.p.exp_c"];
    assert_eq!(c.tags, vec!["c_top"]);
    assert_eq!(c.config.tags, c.tags);
    assert_eq!(c.config.group.as_deref(), Some("exp_tree_grp"));
    // `exp_d` — outside the `+group` path: `+tags` still reaches it (`p`
    // is its first fqn level) but no `+group` level does — and the
    // ignored `group:` property stays off the wire.
    let d = &m.exposures["exposure.p.exp_d"];
    assert_eq!(d.tags, vec!["d_top", "exp_tree"]);
    assert!(d.config.group.is_none(), "top-level group: must not land");
    // The top-level `group:` is a dbt parse error — the loader ignores it
    // but warns, once per misdeclared exposure (exp_c and exp_d here).
    let group_warnings = warnings
        .iter()
        .filter(|w| w.contains("top-level 'group:'"))
        .count();
    assert_eq!(group_warnings, 2, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("exp_c") && w.contains("top-level 'group:'")),
        "exp_c's group: warns: {warnings:?}"
    );
}

#[test]
fn source_loaded_at_field_null_semantics() {
    let dir = tempfile::tempdir().unwrap();
    write_tags_project(dir.path());
    let m = manifest_from_source(dir.path()).unwrap();
    // `nulled_src` — the source's own `loaded_at_field: null` reads as
    // ABSENT (verified on 1.12.6), so its `config.loaded_at_field` fills.
    assert_eq!(
        m.sources["source.p.nulled_src.t1"]
            .loaded_at_field
            .as_deref(),
        Some("nulled_cfg_ts")
    );
    // `config.loaded_at_field: null` on the TABLE is a no-op — the
    // inherited source-config value flows through (verified).
    assert_eq!(
        m.sources["source.p.nulled_src.cfg_null"]
            .loaded_at_field
            .as_deref(),
        Some("nulled_cfg_ts")
    );
    // …while the table's TOP-LEVEL `loaded_at_field: null` clears
    // everything — the only clearing spelling (verified).
    assert_eq!(
        m.sources["source.p.nulled_src.prop_null"].loaded_at_field,
        None
    );
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
fn source_config_enabled_parks_tables_like_dbt_enabled() {
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
fn unverifiable_enabled_keeps_the_node_and_marks_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::create_dir_all(dir.path().join("seeds")).unwrap();
    // `+enabled: "{{ var('missing') }}"` — the project-file render's
    // strict `var()` raises on the missing name (verified on 1.12.6):
    // a dbt parse error, so the verdict is unprovable, not enabled.
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\n\
         seeds:\n  p:\n    uncertain_seed:\n      +enabled: \"{{ var('missing') }}\"\n\
         models:\n  p:\n    tree_uncertain:\n      +enabled: \"{{ var('missing') }}\"\n",
    )
    .unwrap();
    // In-file `config(enabled=var('missing'))` — the tolerant node
    // render yields `None`, which fails dbt's bool validation → the
    // node stays enabled but unverifiable (real dbt may have parked it).
    std::fs::write(
        dir.path().join("models/uncertain.sql"),
        "{{ config(enabled=var('missing')) }}\nselect 1\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("models/tree_uncertain.sql"), "select 1\n").unwrap();
    // A proven-off model stays parked — the mark is NOT for it.
    std::fs::write(
        dir.path().join("models/off.sql"),
        "{{ config(enabled=false) }}\nselect 1\n",
    )
    .unwrap();
    // schema.yml `config.enabled:` — the schema render is strict like
    // the project file's (verified): missing var → raise → unprovable.
    // A source table reads the same channel, as does an exposure.
    std::fs::write(dir.path().join("models/schema_uncertain.sql"), "select 1\n").unwrap();
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\
         models:\n  - name: schema_uncertain\n    config:\n      enabled: \"{{ var('missing') }}\"\n\
         sources:\n  - name: raw\n    tables:\n      - name: t\n        config:\n          enabled: \"{{ var('missing') }}\"\n\
         exposures:\n  - name: dash\n    type: dashboard\n    depends_on:\n      - ref('uncertain')\n    config:\n      enabled: \"{{ var('missing') }}\"\n",
    )
    .unwrap();
    // Python `dbt.config(enabled=…)` — a non-bool literal is a
    // validation error in dbt → unprovable.
    std::fs::write(
        dir.path().join("models/uncertain_py.py"),
        "def model(dbt, session):\n    dbt.config(enabled='maybe')\n    return None\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("seeds/uncertain_seed.csv"), "id\n1\n").unwrap();

    let (m, warnings) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    // Every channel's unprovable resource stays IN the graph and is
    // marked — the set `Dag` folds into pruned adjacency + the lint
    // runner's existence filter.
    for uid in [
        "model.p.uncertain",
        "model.p.tree_uncertain",
        "model.p.schema_uncertain",
        "model.p.uncertain_py",
        "seed.p.uncertain_seed",
    ] {
        assert!(m.nodes.contains_key(uid), "{uid} stays in the graph");
        assert!(m.enabled_unverifiable.contains(uid), "{uid} is marked");
    }
    assert!(m.sources.contains_key("source.p.raw.t"));
    assert!(m.enabled_unverifiable.contains("source.p.raw.t"));
    assert!(m.exposures.contains_key("exposure.p.dash"));
    assert!(m.enabled_unverifiable.contains("exposure.p.dash"));
    // A proven-off node parks under `disabled` like always — absent
    // from the graph AND unmarked.
    assert!(!m.nodes.contains_key("model.p.off"));
    assert!(!m.enabled_unverifiable.contains("model.p.off"));
    // Every channel warned once, naming its site — the dedup in
    // `finish` keeps the yaml-snapshot-style double read to one line.
    for needle in [
        "config(enabled=var('missing'))",
        "+enabled",
        "config.enabled",
    ] {
        assert!(
            warnings.iter().any(|w| w.contains(needle)),
            "a warning names {needle}: {warnings:?}"
        );
    }
}

#[test]
fn snapshots_read_enabled_with_the_same_branch_semantics() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::create_dir_all(dir.path().join("snapshots")).unwrap();
    std::fs::write(dir.path().join("dbt_project.yml"), "name: p\n").unwrap();
    std::fs::write(dir.path().join("models/m.sql"), "select 1\n").unwrap();
    // Gated on `target.name` → stays + marked; the dead-arm one stays
    // untouched; the true-arm one parks under `disabled`.
    std::fs::write(
        dir.path().join("snapshots/gated.sql"),
        "{% snapshot gated %}{% if target.name == 'prod' %}{{ config(enabled=false) }}{% endif %}{{ config(target_schema='s', unique_key='id', strategy='check', check_cols='all') }} select 1 {% endsnapshot %}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("snapshots/dead.sql"),
        "{% snapshot dead %}{% if false %}{{ config(enabled=false) }}{% endif %}{{ config(target_schema='s', unique_key='id', strategy='check', check_cols='all') }} select 1 {% endsnapshot %}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("snapshots/live_off.sql"),
        "{% snapshot live_off %}{% if true %}{{ config(enabled=false) }}{% endif %}{{ config(target_schema='s', unique_key='id', strategy='check', check_cols='all') }} select 1 {% endsnapshot %}\n",
    )
    .unwrap();
    let (m, warnings) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    assert!(m.nodes.contains_key("snapshot.p.gated"));
    assert!(m.enabled_unverifiable.contains("snapshot.p.gated"));
    assert!(m.nodes.contains_key("snapshot.p.dead"));
    assert!(!m.enabled_unverifiable.contains("snapshot.p.dead"));
    assert!(!m.nodes.contains_key("snapshot.p.live_off"));
    assert!(
        warnings.iter().any(|w| w.contains("target.name == 'prod'")),
        "the warning names the unprovable condition: {warnings:?}"
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
          - not_null: {config: {severity: warn}, enabled: true, where: "id > 0"}
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
    assert_eq!(kinds, ["not_null", "not_null", "unique"], "{kinds:?}");
    // An enabled test's `config:`/`enabled:` keys never leak into kwargs —
    // the real manifest records them under `config`, not test args. The
    // map form proves the strip is real (a bare test has no keys to strip).
    let t = m
        .nodes
        .values()
        .find(|n| {
            n.test_metadata.as_ref().and_then(|t| t.name.as_deref()) == Some("not_null")
                && n.test_metadata
                    .as_ref()
                    .and_then(|t| t.kwargs.as_ref())
                    .is_some_and(|k| !k.is_empty())
        })
        .unwrap();
    let kwargs = t.test_metadata.as_ref().unwrap().kwargs.as_ref().unwrap();
    assert!(!kwargs.contains_key("config") && !kwargs.contains_key("enabled"));
    assert_eq!(kwargs.len(), 1, "only the real arg survives: {kwargs:?}");
    assert_eq!(kwargs["where"], serde_json::json!("id > 0"));
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
    let (m, w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    assert!(
        m.nodes.contains_key("seed.p.grains"),
        "the data-paths alias must locate the seed"
    );
    // …and dbt's rename notice lands in warnings (verified wording).
    assert!(
        w.iter()
            .any(|s| s.contains("The data-paths config has been renamed to seed-paths")),
        "{w:?}"
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
    let (m, w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
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

// ---- macro-derived edges (`{% macro %}` bodies' ref/source calls) ----------

/// Write a minimal project: `dbt_project.yml` (name `p`), `models/` SQL
/// bodies, `macros/` SQL bodies, and an optional `models/schema.yml`.
fn macro_project(
    dir: &std::path::Path,
    models: &[(&str, &str)],
    macros: &[(&str, &str)],
    schema: Option<&str>,
) {
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::create_dir_all(dir.join("macros")).unwrap();
    std::fs::write(dir.join("dbt_project.yml"), "name: p\n").unwrap();
    if let Some(schema) = schema {
        std::fs::write(dir.join("models/schema.yml"), schema).unwrap();
    }
    for (name, body) in models {
        std::fs::write(dir.join(format!("models/{name}.sql")), body).unwrap();
    }
    for (name, body) in macros {
        std::fs::write(dir.join(format!("macros/{name}.sql")), body).unwrap();
    }
}

const SHOP_SCHEMA: &str = "version: 2\nsources:\n  - name: shop\n    schema: raw\n    tables:\n      - name: orders\n      - name: customers\n      - name: unused_tbl\n";

#[test]
fn jinja_calls_finds_invocations_in_expression_and_statement_tags() {
    let sql = concat!(
        "{{ m1('a') }}",
        "{% set x = m2('b') %}",
        "{% call(y) m3() %}z{% endcall %}",
        "{% if m4() %}q{% elif m5() %}r{% else %}s{% endif %}",
        "{% for i in m6() %}w{% endfor %}",
        "{% do m7() %}",
        "{%- if m8() -%}",
        "{% macro decl(a) %}{% endmacro %}",
        "{% filter upper %}t{% endfilter %}",
        "{# comment m9() #}",
        "plain m10() text",
    );
    let masked = mask_jinja_comments(sql);
    let names: Vec<&str> = jinja_calls(&masked).iter().map(|c| c.name).collect();
    assert_eq!(names, ["m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8"]);
}

#[test]
fn jinja_calls_captures_namespaced_callees_and_skips_unterminated() {
    let masked = mask_jinja_comments("{{ shipkit.audited_select('t') }}{{ unclosed(");
    let calls = jinja_calls(&masked);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shipkit.audited_select");
    assert_eq!(calls[0].args, "'t'");
}

#[test]
fn macro_invocations_add_ref_and_source_edges() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // Literal positional arg.
            ("m1", "{{ union_raw('orders') }}"),
            // Project-name-qualified invocation.
            ("m2", "{{ p.union_raw('customers') }}"),
            // Nested project macro — the dep one level down resolves too.
            ("m3", "{{ chain('orders') }}"),
            // Keyword-bound arg.
            ("m4", "{{ union_raw(table_name='orders') }}"),
            // Call inside `{% %}`-wrapped shapes.
            (
                "m5",
                "{% set t = 1 %}{{ union_raw('orders') }}{% if true %}select 2{% endif %}",
            ),
            ("m6", "{% call union_raw('orders') %}{% endcall %}"),
            // `ref` inside a macro body → a model edge.
            ("m7", "{{ ref_wrap('m1') }}"),
            // A model that also refs directly.
            ("m8", "select * from {{ ref('m7') }}"),
        ],
        &[(
            "macros",
            concat!(
                "{% macro union_raw(table_name) %}",
                "select * from {{ source('shop', table_name) }}",
                "{% endmacro %}",
                "{% macro chain(table_name) %}{{ union_raw(table_name) }}{% endmacro %}",
                "{% macro ref_wrap(name) %}select * from {{ ref(name) }}{% endmacro %}",
            ),
        )],
        Some(SHOP_SCHEMA),
    );
    let (m, _w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    let parents = |uid: &str| m.parent_map.get(uid).cloned().unwrap_or_default();
    assert_eq!(parents("model.p.m1"), vec!["source.p.shop.orders"]);
    assert_eq!(parents("model.p.m2"), vec!["source.p.shop.customers"]);
    assert_eq!(parents("model.p.m3"), vec!["source.p.shop.orders"]);
    assert_eq!(parents("model.p.m4"), vec!["source.p.shop.orders"]);
    assert_eq!(parents("model.p.m5"), vec!["source.p.shop.orders"]);
    assert_eq!(parents("model.p.m6"), vec!["source.p.shop.orders"]);
    assert_eq!(parents("model.p.m7"), vec!["model.p.m1"]);
    assert_eq!(parents("model.p.m8"), vec!["model.p.m7"]);
    // `depends_on.macros` records only the DIRECTLY invoked macro — dbt
    // writes the nested callee on the callee's own `depends_on.macros`
    // (verified against the logistics fixture's real-dbt manifest:
    // `stg_sensor_pings` carries `feeder_chain`, never `carrier_feed`).
    assert_eq!(
        m.nodes["model.p.m1"].depends_on.macros,
        vec!["macro.p.union_raw"]
    );
    assert_eq!(
        m.nodes["model.p.m3"].depends_on.macros,
        vec!["macro.p.chain"]
    );
    // The manifest's `macros` map carries the project table and the
    // macro→macro call graph.
    assert!(m.macros.contains_key("macro.p.union_raw"));
    assert_eq!(
        m.macros["macro.p.chain"].depends_on.macros,
        vec!["macro.p.union_raw"]
    );
    // Nothing here was unresolvable — the side table stays empty.
    assert!(
        m.unresolved_adjacent.is_empty(),
        "{:?}",
        m.unresolved_adjacent
    );
}

#[test]
fn macro_defaults_and_branch_bodies_resolve() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // The default fills the unbound parameter.
            ("m1", "{{ union_raw() }}"),
            // `{% if %}` arms all resolve (over-cover — the source parser
            // can't evaluate the guard, and a missing edge is worse).
            ("m2", "{{ branchy('orders') }}"),
        ],
        &[(
            "macros",
            concat!(
                "{% macro union_raw(table_name='orders') %}",
                "select * from {{ source('shop', table_name) }}",
                "{% endmacro %}",
                "{% macro branchy(t) %}",
                "{% if t == 'x' %}select * from {{ source('shop', 'orders') }}",
                "{% else %}select * from {{ ref('m1') }}{% endif %}",
                "{% endmacro %}",
            ),
        )],
        Some(SHOP_SCHEMA),
    );
    let m = manifest_from_source(dir.path()).unwrap();
    let parents = |uid: &str| m.parent_map.get(uid).cloned().unwrap_or_default();
    assert_eq!(parents("model.p.m1"), vec!["source.p.shop.orders"]);
    assert_eq!(
        parents("model.p.m2"),
        vec!["model.p.m1", "source.p.shop.orders"]
    );
    assert!(m.unresolved_adjacent.is_empty());
}

#[test]
fn macro_param_defaults_bind_in_the_callee_frame() {
    // `inner(a, b=a)`: the `b` default indexes inner's OWN parameters, so
    // it must evaluate against inner's partially bound slots — not the
    // caller's frame. `outer('customers')` calls `inner('orders')`: the
    // caller's slot 0 holds 'customers', but `b` must land on 'orders'
    // (evaluating the default in the caller frame would fabricate a
    // shop.customers edge).
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[("m1", "{{ outer('customers') }}")],
        &[(
            "macros",
            concat!(
                "{% macro inner(a, b=a) %}",
                "select * from {{ source('shop', a) }} join {{ source('shop', b) }} on true",
                "{% endmacro %}",
                "{% macro outer(t) %}{{ inner('orders') }}{% endmacro %}",
            ),
        )],
        Some(SHOP_SCHEMA),
    );
    let (m, _w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    let parents = |uid: &str| m.parent_map.get(uid).cloned().unwrap_or_default();
    assert_eq!(parents("model.p.m1"), vec!["source.p.shop.orders"]);
    assert!(m.unresolved_adjacent.is_empty());
}

#[test]
fn unresolvable_macro_calls_count_as_unprovable_adjacency() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // A package-macro call: the callee's reach can't be proven —
            // counted as a possible parent, its literal arg kept as a
            // source-table candidate.
            ("m1", "{{ pkg_thing.wrap('orders') }}"),
            // Builtins and a no-dep project macro are NOT counted.
            (
                "m2",
                "{{ is_incremental() }}{{ env_var('X') }}{{ log('x') }}{{ local_noop() }}{{ adapter.dispatch('local_noop') }}",
            ),
            // A project macro whose bound arg stayed non-literal →
            // `incomplete` — counted, no literal candidates.
            ("m3", "{{ union_raw(var('t')) }}"),
            // Fully resolved — not counted.
            ("m4", "{{ union_raw('customers') }}"),
        ],
        &[(
            "macros",
            concat!(
                "{% macro union_raw(table_name) %}select * from {{ source('shop', table_name) }}{% endmacro %}",
                "{% macro local_noop() %}select 1{% endmacro %}",
            ),
        )],
        Some(SHOP_SCHEMA),
    );
    let m = manifest_from_source(dir.path()).unwrap();
    let ua = &m.unresolved_adjacent;
    assert_eq!(ua["model.p.m1"].parents, 1);
    assert_eq!(ua["model.p.m3"].parents, 1);
    assert!(
        !ua.contains_key("model.p.m2"),
        "builtins and no-dep macros must not count: {ua:?}"
    );
    assert!(!ua.contains_key("model.p.m4"), "{ua:?}");
    // 'orders' (the counted call's literal arg) names a declared source
    // table → a possible hidden child. 'customers' came through a
    // RESOLVED call — no unprovable side to record.
    assert_eq!(ua["source.p.shop.orders"].children, 1);
    assert!(!ua.contains_key("source.p.shop.customers"), "{ua:?}");
    // The resolvable call still emitted its edge.
    assert_eq!(m.parent_map["model.p.m4"], vec!["source.p.shop.customers"]);
}

#[test]
fn unresolved_macro_calls_suppress_root_models_and_unused_sources() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // Only "parent" is a package macro's hidden source() — without
            // the unprovable-adjacency record, root_models would flag it.
            ("m1", "{{ pkg_thing.wrap('orders') }}"),
            // An unresolved bound-arg call — same suppression shape.
            ("m3", "{{ union_raw(var('t')) }}"),
            // The control: a genuinely parentless model MUST still flag.
            ("ctl", "select 1"),
        ],
        &[(
            "macros",
            "{% macro union_raw(table_name) %}select * from {{ source('shop', table_name) }}{% endmacro %}",
        )],
        Some(SHOP_SCHEMA),
    );
    let m = manifest_from_source(dir.path()).unwrap();
    let dag = Dag::build(&m);
    let findings =
        crate::run_rules(crate::LintInput::new(&dag, &crate::LintConfig::default())).findings;
    let flagged: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule == crate::LintRule::RootModels)
        .map(|f| f.node_uid.as_str())
        .collect();
    assert!(flagged.contains(&"model.p.ctl"), "{flagged:?}");
    assert!(
        !flagged.contains(&"model.p.m1") && !flagged.contains(&"model.p.m3"),
        "a call whose reach can't be proven must not read as 'no parents': {flagged:?}"
    );
    let unused: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule == crate::LintRule::UnusedSources)
        .map(|f| f.node_uid.as_str())
        .collect();
    assert!(unused.contains(&"source.p.shop.unused_tbl"), "{unused:?}");
    assert!(
        unused.contains(&"source.p.shop.customers"),
        "a source no call even NAMES still flags: {unused:?}"
    );
    assert!(
        !unused.contains(&"source.p.shop.orders"),
        "a counted call's literal arg suppresses its source only: {unused:?}"
    );
}

#[test]
fn macro_calls_in_jinja_comments_are_inert() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[("m1", "{# {{ union_raw('orders') }} #}select 1")],
        &[(
            "macros",
            "{% macro union_raw(t) %}select * from {{ source('shop', t) }}{% endmacro %}",
        )],
        Some(SHOP_SCHEMA),
    );
    let m = manifest_from_source(dir.path()).unwrap();
    assert!(!m.parent_map.contains_key("model.p.m1"));
    // A commented-out call is still a call-shape we can classify — it is
    // masked before the scan, so it neither edges nor counts.
    assert!(m.unresolved_adjacent.is_empty());
}

#[test]
fn comment_and_raw_blocks_never_reach_the_call_scan() {
    // `{% comment %}…{% endcomment %}` is dead text Jinja never parses,
    // `{% raw %}…{% endraw %}` emits its body literally — calls inside
    // either can't be edges (dbt only runs the live `{{ }}`).
    let sql = "{% comment %} {{ ref('ghost') }} {% endcomment %}\
             \n{% raw %} {{ source('s','t') }} {% endraw %}\
             \n{{ ref('live') }}";
    let sites = sites_of(sql);
    assert_eq!(sites.len(), 1);
    assert_eq!(&sql[sites[0].span.clone()], "ref('live')");
    // Unclosed blocks mask to the end — dbt errors on them, and nothing
    // they wrap can phantom-edge.
    let sites = sites_of("{{ ref('live') }} {% comment %} {{ ref('ghost') }}");
    assert_eq!(sites.len(), 1);
}

#[test]
fn comment_and_raw_calls_produce_no_edges() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[(
            "m1",
            "{% comment %} {{ union_raw('orders') }} {% endcomment %}select 1",
        )],
        &[(
            "macros",
            "{% macro union_raw(t) %}select * from {{ source('shop', t) }}{% endmacro %}",
        )],
        Some(SHOP_SCHEMA),
    );
    let m = manifest_from_source(dir.path()).unwrap();
    assert!(!m.parent_map.contains_key("model.p.m1"));
    assert!(m.unresolved_adjacent.is_empty());
}

#[test]
fn macro_sql_stores_the_full_definition_block() {
    // `RawMacro.macro_sql` must byte-match what a real manifest records:
    // from the `{%` of `{% macro %}` through the `%}` of `{% endmacro %}`
    // — a `{# #}` comment BEFORE the def stays out, one INSIDE stays in,
    // whitespace control pulls the whitespace it eats into the slice, and
    // the file's stripped bounds fence it (dbt strips before scanning).
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[],
        &[(
            "macros",
            concat!(
                "{# a comment BEFORE the def stays out #}\n",
                "{% macro plain() %}\n",
                "{# inside the body, it stays #}\n",
                "{{ 1 }}\n",
                "{% endmacro %}\n",
                "   {%- macro padded() -%}\n",
                "{{ 2 }}\n",
                "{%- endmacro -%}   \n",
                "trailing text keeps {%- macro after() %}x{% endmacro %}",
            ),
        )],
        None,
    );
    let m = manifest_from_source(dir.path()).unwrap();
    let sql = |name: &str| {
        m.macros[&format!("macro.p.{name}")]
            .macro_sql
            .clone()
            .unwrap()
    };
    assert_eq!(
        sql("plain"),
        "{% macro plain() %}\n{# inside the body, it stays #}\n{{ 1 }}\n{% endmacro %}"
    );
    // `{%-` absorbs every whitespace byte before the tag (`\n   `); `-%}`
    // absorbs the run after it (`   \n`) — both stops inside the text.
    assert_eq!(
        sql("padded"),
        "\n   {%- macro padded() -%}\n{{ 2 }}\n{%- endmacro -%}   \n"
    );
    assert_eq!(sql("after"), " {%- macro after() %}x{% endmacro %}");
}

#[test]
fn macro_sql_spans_multiple_defs_in_one_file() {
    // Each definition keeps only its own block — shared-file siblings
    // never bleed in, and a def unterminated at EOF (dbt errors; the
    // load stays lenient) runs to the stripped end. `{% test %}` /
    // `{% materialization %}` blocks never register at all: only the
    // literal `macro` keyword collects, the way dbt's own registrations
    // under `macro.*` stay out of the comparable set.
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[],
        &[(
            "macros",
            concat!(
                "{% macro a() %}first{% endmacro %}",
                "{% test pos() %}t{% endtest %}",
                "{% materialization view %}m{% endmaterialization %}",
                "{% macro b() %}second{% endmacro %}  \n",
                "{% macro tail() %}unclosed body",
            ),
        )],
        None,
    );
    let m = manifest_from_source(dir.path()).unwrap();
    let sql = |name: &str| {
        m.macros[&format!("macro.p.{name}")]
            .macro_sql
            .clone()
            .unwrap()
    };
    assert_eq!(sql("a"), "{% macro a() %}first{% endmacro %}");
    assert_eq!(sql("b"), "{% macro b() %}second{% endmacro %}");
    assert_eq!(sql("tail"), "{% macro tail() %}unclosed body");
    assert!(
        !m.macros.contains_key("macro.p.test_pos")
            && !m
                .macros
                .contains_key("macro.p.materialization_view_default"),
        "non-macro blocks never collect: {:?}",
        m.macros.keys().collect::<Vec<_>>()
    );
}

#[test]
fn model_local_definitions_do_not_edge_or_configure() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // `ref`/`source`/`config` inside a model-local `{% macro %}`
            // body run only when the macro is invoked — an un-invoked
            // definition is not the model's call (an invoked PROJECT
            // macro's calls still arrive through `Macros`).
            (
                "m1",
                concat!(
                    "{% macro helper() %}",
                    "select * from {{ ref('other') }} join {{ source('shop', 'orders') }} on true",
                    "{% endmacro %}",
                    "{% macro cfg() %}{{ config(enabled=false, materialized='table') }}{% endmacro %}",
                    "{{ config(materialized='view') }}",
                    "select 1",
                ),
            ),
            ("other", "select 1"),
        ],
        &[(
            "macros",
            "{% macro union_raw(t) %}select * from {{ source('shop', t) }}{% endmacro %}",
        )],
        Some(SHOP_SCHEMA),
    );
    let (m, _w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    // The macro body's `enabled=false` did not park the model, and its
    // `materialized='table'` did not beat the model's own `view`.
    assert!(m.nodes.contains_key("model.p.m1"));
    assert_eq!(
        m.nodes["model.p.m1"].config.materialized.as_deref(),
        Some("view")
    );
    let parents = m.parent_map.get("model.p.m1").cloned().unwrap_or_default();
    assert!(
        !parents
            .iter()
            .any(|p| p.contains("other") || p.contains("shop")),
        "definition bodies must not edge: {parents:?}"
    );
    assert!(
        !m.unresolved_adjacent.contains_key("model.p.m1"),
        "an un-invoked definition isn't unprovable adjacency: {:?}",
        m.unresolved_adjacent
    );
}

#[test]
fn call_bodies_configure_and_edge_like_dbt() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // `{% call f() %}…{% endcall %}` is NOT a deferred definition:
            // dbt's static extractor treats the body as toplevel — its
            // `config()` applies (an `enabled=false` parks the node) and
            // its `ref` edges, even though the body only RENDERS when the
            // callee invokes `caller()`. Verified against the generated
            // manifest; deferring it would diverge from dbt.
            (
                "cfg",
                "{% call wrap() %}{{ config(materialized='table') }}{{ ref('other') }}{% endcall %}select 1",
            ),
            (
                "off",
                "{% call wrap() %}{{ config(enabled=false) }}{% endcall %}select 1",
            ),
            ("other", "select 1"),
        ],
        &[(
            "macros",
            "{% macro wrap() %}select {{ caller() }}{% endmacro %}",
        )],
        Some(SHOP_SCHEMA),
    );
    let (m, _w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    assert!(m.nodes.contains_key("model.p.cfg"));
    assert_eq!(
        m.nodes["model.p.cfg"].config.materialized.as_deref(),
        Some("table"),
        "the call body's config applies unconditionally, like dbt"
    );
    assert_eq!(
        m.parent_map.get("model.p.cfg").cloned().unwrap_or_default(),
        vec!["model.p.other"],
        "the call body's ref edges, like dbt"
    );
    assert!(
        !m.nodes.contains_key("model.p.off") && !m.enabled_unverifiable.contains("model.p.off"),
        "the call body's enabled=false parks the node, like dbt"
    );
}

#[test]
fn branch_arms_edge_only_when_they_can_run() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // An UNPROVABLE arm's calls edge either way — `ref` in the
            // gated arm AND `source` in its else both land, matching
            // dbt's static extractor.
            (
                "gated",
                "{% if target.name %}{{ ref('other') }}{% else %}{{ source('shop', 'orders') }}{% endif %}select 1",
            ),
            // A statically-false arm's calls are dead — dbt's extractor
            // drops them too (verified: only the live arm's ref lands in
            // the real manifest).
            (
                "dead",
                "{% if false %}{{ ref('other') }}{% else %}{{ ref('live') }}{% endif %}select 1",
            ),
            ("other", "select 1"),
            ("live", "select 1"),
        ],
        &[],
        Some(SHOP_SCHEMA),
    );
    let m = manifest_from_source(dir.path()).unwrap();
    let parents = |uid: &str| m.parent_map.get(uid).cloned().unwrap_or_default();
    assert_eq!(
        parents("model.p.gated"),
        vec!["model.p.other", "source.p.shop.orders"]
    );
    assert_eq!(parents("model.p.dead"), vec!["model.p.live"]);
}

// ---- unit_tests: YAML → RawManifest → TestInfo ------------------------------

/// A scratch project with `models/` SQL files and one schema.yml — returns
/// the synthesized manifest plus the loader warnings.
fn unit_test_project(
    files: &[(&str, &str)],
    schema: &str,
) -> (crate::manifest::RawManifest, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nmodel-paths: [models]\n",
    )
    .unwrap();
    for (name, sql) in files {
        std::fs::write(dir.path().join(format!("models/{name}.sql")), sql).unwrap();
    }
    std::fs::write(dir.path().join("models/schema.yml"), schema).unwrap();
    manifest_from_source_with_warnings(dir.path(), None).unwrap()
}

#[test]
fn unit_tests_synthesize_wire_records_and_attach() {
    // The full intake: `unit_tests:` → a `RawUnitTest` keyed like a real
    // manifest (`unit_test.<proj>.<model>.<name>`), `depends_on.nodes`
    // carrying the tested model's uid, `given`/`expect` verbatim — and
    // `Dag::build` attaches it as `kind: "unit"` metadata with the
    // UnitTestShape counts.
    let (m, w) = unit_test_project(
        &[("stg_a", "select 1"), ("fct_a", "select 1")],
        r#"version: 2
unit_tests:
  - name: doubles_amount
    model: fct_a
    given:
      - input: ref('stg_a')
        rows:
          - {id: 1, amount: 2}
          - {id: 2, amount: 3}
    expect:
      rows:
        - {id: 1, doubled: 4}
"#,
    );
    assert!(w.is_empty(), "{w:?}");
    let ut = m
        .unit_tests
        .get("unit_test.p.fct_a.doubles_amount")
        .expect("the wire record is keyed like a manifest unit test");
    assert_eq!(ut.model.as_deref(), Some("fct_a"));
    assert_eq!(ut.depends_on.nodes, vec!["model.p.fct_a".to_string()]);
    let dag = crate::Dag::build(&m);
    let t = dag
        .tests("model.p.fct_a")
        .iter()
        .find(|t| t.kind == "unit")
        .expect("the unit test attaches to its model");
    let shape = t.unit.as_ref().expect("kind unit carries its shape");
    assert_eq!(
        (shape.inputs, shape.given_rows, shape.expect_rows),
        (1, Some(2), Some(1))
    );
}

#[test]
fn unit_tests_expand_versioned_families_by_member_files() {
    // A `versions:`-declared family expands one entry per bound member:
    // an unfiltered entry emits one record per member, `include: [2]`
    // selects just v2, `exclude: ['beta']` drops it. The selection
    // matches the declared `v:` VERBATIM (dbt rejects `v2` for a `v: 2`
    // member — the same `v{arg}`-suffix lookup `ref(m, v=…)` performs),
    // and the uid/version segments match what a real manifest writes.
    let files = [
        ("fct_orders_v1", "select 1"),
        ("fct_orders_v2", "select 1"),
        ("fct_orders_vbeta", "select 1"),
    ];
    let (m, w) = unit_test_project(
        &files,
        r#"version: 2
models:
  - name: fct_orders
    latest_version: 2
    versions:
      - v: 1
      - v: 2
      - v: beta
unit_tests:
  - name: all_versions
    model: fct_orders
    given: []
  - name: scoped_v2
    model: fct_orders
    versions: {include: [2]}
    given: []
  - name: excluding_beta
    model: fct_orders
    versions: {exclude: ['beta']}
    given: []
"#,
    );
    assert!(w.is_empty(), "{w:?}");
    let mut ids: Vec<&str> = m.unit_tests.keys().map(String::as_str).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![
            "unit_test.p.fct_orders.all_versions_v1",
            "unit_test.p.fct_orders.all_versions_v2",
            "unit_test.p.fct_orders.all_versions_vbeta",
            "unit_test.p.fct_orders.excluding_beta_v1",
            "unit_test.p.fct_orders.excluding_beta_v2",
            "unit_test.p.fct_orders.scoped_v2_v2",
        ]
    );
    let v2 = &m.unit_tests["unit_test.p.fct_orders.scoped_v2_v2"];
    assert_eq!(v2.version.as_deref(), Some("2"));
    assert_eq!(
        v2.depends_on.nodes,
        vec!["model.p.fct_orders.v2".to_string()]
    );
    let beta = &m.unit_tests["unit_test.p.fct_orders.all_versions_vbeta"];
    assert_eq!(beta.version.as_deref(), Some("beta"));
    // …and they attach to the right graph nodes only.
    let dag = crate::Dag::build(&m);
    let kinds = |uid: &str| -> Vec<&str> {
        let mut v: Vec<&str> = dag.tests(uid).iter().map(|t| t.name.as_str()).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        kinds("model.p.fct_orders.v2"),
        vec!["all_versions", "excluding_beta", "scoped_v2"]
    );
    assert_eq!(kinds("model.p.fct_orders.vbeta"), vec!["all_versions"]);
}

#[test]
fn unit_tests_csv_rows_count_the_records_dbt_parses() {
    // `format: csv` rows stay a string in YAML, but real dbt parses them
    // into row dicts — source mode reproduces the COUNT (header excluded)
    // so `expect_rows` matches the manifest shape.
    let (m, w) = unit_test_project(
        &[("fct_a", "select 1")],
        r#"version: 2
unit_tests:
  - name: csv_expect
    model: fct_a
    given:
      - input: ref('fct_a')
        format: csv
        rows: |
          id,amount
          1,2
          2,3
    expect:
      format: csv
      rows: |
        id,doubled
        1,4
"#,
    );
    assert!(w.is_empty(), "{w:?}");
    let dag = crate::Dag::build(&m);
    let t = dag
        .tests("model.p.fct_a")
        .iter()
        .find(|t| t.kind == "unit")
        .expect("the unit test attaches");
    let shape = t.unit.as_ref().unwrap();
    assert_eq!(
        (shape.inputs, shape.given_rows, shape.expect_rows),
        (1, Some(2), Some(1))
    );
}

#[test]
fn unit_tests_config_enabled_and_missing_targets() {
    // `config.enabled: false` parks the test (dbt's `disabled` map);
    // a missing `model:` warns and skips; an undeclared target warns
    // and drops the record rather than synthesize a `depends_on` that
    // resolves to nothing (dbt hard-errors there).
    let (m, w) = unit_test_project(
        &[("fct_a", "select 1")],
        r#"version: 2
unit_tests:
  - name: off_test
    model: fct_a
    config: {enabled: false}
    given: []
  - name: no_model
    given: []
  - name: ghost_target
    model: ghost
    given: []
"#,
    );
    assert!(
        w.iter().any(|s| s.contains("'no_model'")),
        "the missing-model warning names the test: {w:?}"
    );
    assert!(
        w.iter()
            .any(|s| s.contains("'ghost_target'") && s.contains("unknown model 'ghost'")),
        "the ghost-target warning names the test and model: {w:?}"
    );
    assert!(
        m.unit_tests.is_empty(),
        "disabled and unresolvable unit tests synthesize nothing: {:?}",
        m.unit_tests.keys().collect::<Vec<_>>()
    );
    let dag = crate::Dag::build(&m);
    assert!(
        dag.tests("model.p.fct_a").iter().all(|t| t.kind != "unit"),
        "a disabled unit test never attaches"
    );
}

// ---- project macro calls in `config(enabled=…)` ------------------------

/// A `Macros` table built from in-memory bodies — one file under the
/// dir's `macros/` per entry, run through the real loader (masking,
/// shape analysis, the works).
fn macros_of(dir: &std::path::Path, bodies: &[&str]) -> Macros {
    std::fs::create_dir_all(dir.join("macros")).unwrap();
    for (i, body) in bodies.iter().enumerate() {
        std::fs::write(dir.join(format!("macros/m{i}.sql")), body).unwrap();
    }
    let mut w = Vec::new();
    Macros::load(dir, &["macros".to_string()], &mut w)
}

/// The background shape — straight-line `set`, a guard `{% if %}` whose
/// body only raises, one `do return`.
const BOOL_VAR: &str = concat!(
    "{% macro bool_var(name, default) %}",
    "{% set value = var(name, default) %}",
    "{% if value is not sameas true and value is not sameas false %}",
    "{% do exceptions.raise_compiler_error('not a boolean') %}",
    "{% endif %}",
    "{% do return(value) %}",
    "{% endmacro %}",
);

#[test]
fn enabled_evaluates_value_shaped_project_macros() {
    let dir = tempfile::tempdir().unwrap();
    let macros = macros_of(
        dir.path(),
        &[
            BOOL_VAR,
            "{% macro ret_off() %}{% do return(false) %}{% endmacro %}",
            "{% macro ret_on() %}{{ return(true) }}{% endmacro %}",
            "{% macro kw(flag, other=flag) %}{% do return(other) %}{% endmacro %}",
            "{% macro with_default(name, flag=var('inner_flag', false)) %}{% do return(flag) %}{% endmacro %}",
            "{% macro inner(name, default) %}{% do return(var(name, default)) %}{% endmacro %}",
            "{% macro outer(name, default) %}{% set v = p.inner(name, default) %}{% do return(v) %}{% endmacro %}",
        ],
    );
    let vars = test_vars(
        serde_json::json!({}),
        serde_json::json!({"flag_on": true}),
        "p",
    );
    // Project-qualified and bare spellings resolve alike (dbt lets both
    // reach the root project's table).
    assert_eq!(
        enabled_expr_macros("p.bool_var('flag_on', false)", &vars, &macros),
        Some(true)
    );
    assert_eq!(
        enabled_expr_macros("bool_var('missing', false)", &vars, &macros),
        Some(false)
    );
    // `var()` reads the NODE's table inside the body — `flag_on` is
    // true, everything missing falls to the declared default.
    assert_eq!(
        enabled_expr_macros("p.bool_var('missing', true)", &vars, &macros),
        Some(true)
    );
    // Return-only bodies — `{% do %}` and `{{ }}` spellings alike.
    assert_eq!(
        enabled_expr_macros("p.ret_off()", &vars, &macros),
        Some(false)
    );
    assert_eq!(enabled_expr_macros("ret_on()", &vars, &macros), Some(true));
    // Keyword arguments bind by name; a default sees the params bound
    // before it (`other=flag` reads the arg `flag` took).
    assert_eq!(
        enabled_expr_macros("p.kw(flag=false)", &vars, &macros),
        Some(false)
    );
    assert_eq!(
        enabled_expr_macros("p.kw(flag=true)", &vars, &macros),
        Some(true)
    );
    assert_eq!(
        enabled_expr_macros("p.kw(true, other=false)", &vars, &macros),
        Some(false)
    );
    // A default's own expression evaluates in the callee frame — here it
    // calls `var()` against the caller's (node) table.
    assert_eq!(
        enabled_expr_macros("p.with_default('x')", &vars, &macros),
        Some(false)
    );
    let vars = test_vars(
        serde_json::json!({}),
        serde_json::json!({"inner_flag": true}),
        "p",
    );
    assert_eq!(
        enabled_expr_macros("p.with_default('x')", &vars, &macros),
        Some(true)
    );
    // A macro calling a macro — qualified inside the body — follows the
    // same binding rules down the chain.
    assert_eq!(
        enabled_expr_macros("p.outer('inner_flag', false)", &vars, &macros),
        Some(true)
    );
    assert_eq!(
        enabled_expr_macros("outer('absent', false)", &vars, &macros),
        Some(false)
    );
}

#[test]
fn enabled_leaves_unsupported_callees_and_shapes_unproven() {
    let dir = tempfile::tempdir().unwrap();
    let macros = macros_of(
        dir.path(),
        &[
            // `{% for %}` — control flow outside the value shape.
            "{% macro loopy(name, default) %}{% set out = default %}{% for k in ['a'] %}{% set out = var(name, default) %}{% endfor %}{% do return(out) %}{% endmacro %}",
            // An `{% if %}` whose body does more than raise/log.
            "{% macro branchy(flag) %}{% if flag %}{% do return(true) %}{% else %}{% do return(false) %}{% endif %}{% endmacro %}",
            // A body that ends in rendered output, not `return(…)`.
            "{% macro prints() %}true{% endmacro %}",
            // Two returns — the shape wants exactly one final action.
            "{% macro twice() %}{% do return(true) %}{% do return(false) %}{% endmacro %}",
            // Recursion — the depth cap refuses it.
            "{% macro rec() %}{% do return(rec()) %}{% endmacro %}",
            // No default for `name` — a bare call can't bind it.
            "{% macro needs(name, default) %}{% do return(var(name, default)) %}{% endmacro %}",
        ],
    );
    let vars = no_vars();
    for expr in [
        // Bodies outside the value shape.
        "p.loopy('x', false)",
        "p.branchy(true)",
        "p.prints()",
        "p.twice()",
        // Recursion past the depth cap.
        "p.rec()",
        // A package macro and an adapter dispatch — never read.
        "pkg.macro_fn('x')",
        "adapter.dispatch('needs')",
        // A name the project table lacks — qualified or bare.
        "p.unknown_macro()",
        "unknown_macro()",
        // Call shapes dbt itself rejects.
        "p.needs()",              // missing a required argument
        "p.needs('a','b','c')",   // a positional past the arity
        "p.needs(name='a', 'b')", // positional after kwarg → slot collision
        "p.needs('a', name='b')", // kwarg re-naming a filled slot
        // A builtin — `env_var` can't be read from source.
        "env_var('X')",
    ] {
        assert_eq!(
            enabled_expr_macros(expr, &vars, &macros),
            None,
            "{expr} should stay unproven"
        );
    }
}

#[test]
fn enabled_macro_calls_follow_the_bound_name_rules() {
    let dir = tempfile::tempdir().unwrap();
    let macros = macros_of(
        dir.path(),
        &[
            BOOL_VAR,
            "{% macro gate() %}{% do return(false) %}{% endmacro %}",
        ],
    );
    let vars = no_vars();
    // A name the template binds shadows the macro table — `gate` is a
    // `{% set %}`-bound value here, not callable; the verdict stays
    // unproven rather than guessing.
    let (d, w) = enabled_macros(
        "{% set gate = 1 %}{{ config(enabled=gate()) }}",
        &vars,
        &macros,
    );
    assert_eq!(d, Some(Enabled::Unproven));
    assert_eq!(w.len(), 1, "{w:?}");
    // A bound name as an ARGUMENT is likewise unprovable — its value
    // can't be read from source.
    let (d, w) = enabled_macros(
        "{% set key = 'x' %}{{ config(enabled=bool_var(key, false)) }}",
        &vars,
        &macros,
    );
    assert_eq!(d, Some(Enabled::Unproven));
    assert_eq!(w.len(), 1, "{w:?}");
    // A branch condition evaluates the same calls — a proven-false
    // macro result parks the arm's config like a literal `false`.
    let (d, _) = enabled_macros(
        "{% if p.gate() %}{{ config(enabled=false) }}{% endif %}",
        &vars,
        &macros,
    );
    assert_eq!(d, None);
    // …and a proven-true one applies it.
    let (d, _) = enabled_macros(
        "{% if bool_var('missing', true) %}{{ config(enabled=false) }}{% endif %}",
        &vars,
        &macros,
    );
    assert_eq!(d, Some(Enabled::Off));
    // `var` follows the same name resolution as any other callee: a name
    // the template binds shadows the builtin — a bound `var` is a value,
    // not provably callable.
    let (d, w) = enabled_macros(
        "{% set var = 'x' %}{{ config(enabled=var('missing', false)) }}",
        &vars,
        &macros,
    );
    assert_eq!(d, Some(Enabled::Unproven));
    assert_eq!(w.len(), 1, "{w:?}");
    // …and a project macro named `var` shadows the builtin too (the
    // table's own order puts project defs first) — bare and qualified
    // spellings alike.
    let var_macros = macros_of(
        dir.path(),
        &["{% macro var(name, fallback) %}{% do return(fallback) %}{% endmacro %}"],
    );
    assert_eq!(
        enabled_expr_macros("var('missing', true)", &vars, &var_macros),
        Some(true)
    );
    assert_eq!(
        enabled_expr_macros("p.var('missing', true)", &vars, &var_macros),
        Some(true)
    );
}

#[test]
fn enabled_macro_warnings_fold_by_normalized_expression() {
    let dir = tempfile::tempdir().unwrap();
    let loopy =
        "{% macro loopy(name, default) %}{% set out = default %}{% for k in ['a'] %}{% set out = var(name, default) %}{% endfor %}{% do return(out) %}{% endmacro %}";
    macro_project(
        dir.path(),
        &[
            ("u1", "{{ config(enabled=p.loopy('gate', false)) }}select 1"),
            // The same expression spelled across two lines — the fold
            // key normalizes the wrap away.
            (
                "u2",
                "{{ config(enabled=p.loopy('gate',\n    false)) }}select 1",
            ),
            ("u3", "{{ config(enabled=p.loopy('gate', false)) }}select 1"),
            // A different expression keeps its own line.
            (
                "u4",
                "{{ config(enabled=p.loopy('other', false)) }}select 1",
            ),
            ("u5", "{{ config(enabled=p.loopy('other', true)) }}select 1"),
        ],
        &[("macros", loopy)],
        None,
    );
    let (m, w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    // Every gated node stays enabled and marked — nothing is silently
    // disabled on an unproven expression.
    for uid in [
        "model.p.u1",
        "model.p.u2",
        "model.p.u3",
        "model.p.u4",
        "model.p.u5",
    ] {
        assert!(m.nodes.contains_key(uid), "{uid} stays");
        assert!(m.enabled_unverifiable.contains(uid), "{uid} is marked");
    }
    // The folded group lands where its first report appeared — before
    // the lone warnings.
    let eval_lines: Vec<&str> = w
        .iter()
        .filter(|s| s.contains("could not be evaluated"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        eval_lines,
        vec![
            "config(enabled=p.loopy('gate', false)) could not be evaluated as a boolean in 3 files (first: models/u1.sql); those resources stay enabled",
            "models/u4.sql: config(enabled=p.loopy('other', false)) could not be evaluated as a boolean; the resource stays enabled",
            "models/u5.sql: config(enabled=p.loopy('other', true)) could not be evaluated as a boolean; the resource stays enabled",
        ],
        "{w:?}"
    );
}

#[test]
fn eval_warnings_print_the_normalized_expression() {
    // The lone-file line folds whitespace like the grouped spelling —
    // the `{% if %}` tag keeps the padding around its condition in the
    // raw source, and the `enabled=` expression keeps the line wraps
    // inside the call.
    let (d, w) = enabled(
        "{% if  target.name == 'prod'\n %}{{ config(enabled=false) }}{% endif %}select 1",
        &no_vars(),
    );
    assert_eq!(d, Some(Enabled::Unproven));
    assert_eq!(
        w,
        vec![
            "models/m.sql: config(enabled=…) sits in a Jinja branch whose condition cannot be evaluated (target.name == 'prod'); the resource stays enabled"
                .to_string()
        ],
        "{w:?}"
    );
    let (_, w) = enabled(
        "{{ config(enabled=  target.name\n   ==\t'prod' ) }}select 1",
        &no_vars(),
    );
    assert_eq!(
        w,
        vec![
            "models/m.sql: config(enabled=target.name == 'prod') could not be evaluated as a boolean; the resource stays enabled"
                .to_string()
        ],
        "{w:?}"
    );
}

#[test]
fn enabled_macro_calls_drive_the_source_disabled_set() {
    let dir = tempfile::tempdir().unwrap();
    macro_project(
        dir.path(),
        &[
            // The background shape — `inputs_enabled` absent ⇒ the
            // declared default decides.
            ("a", "{{ config(enabled=p.bool_var('inputs_enabled', false)) }}select 1"),
            ("on", "{{ config(enabled=p.bool_var('inputs_enabled', true)) }}select 1"),
            // return-only / kwargs / default / nested / bare — all provable.
            ("ret", "{{ config(enabled=p.ret_off()) }}select 1"),
            ("kw", "{{ config(enabled=p.kw_flag(flag=false)) }}select 1"),
            ("def", "{{ config(enabled=p.with_default('anything')) }}select 1"),
            ("nest", "{{ config(enabled=p.outer('deep_flag', false)) }}select 1"),
            ("unq", "{{ config(enabled=ret_off()) }}select 1"),
            // An arm gated on a macro call — `gate_flag` defaults true.
            (
                "arm",
                "{% if p.bool_var('gate_flag', true) %}{{ config(enabled=false) }}{% endif %}select 1",
            ),
        ],
        &[
            ("macros", BOOL_VAR),
            (
                "more_macros",
                concat!(
                    "{% macro ret_off() %}{% do return(false) %}{% endmacro %}",
                    "{% macro kw_flag(flag) %}{% do return(flag) %}{% endmacro %}",
                    "{% macro with_default(name, flag=var('inner_flag', false)) %}{% do return(flag) %}{% endmacro %}",
                    "{% macro inner(name, default) %}{% do return(var(name, default)) %}{% endmacro %}",
                    "{% macro outer(name, default) %}{% set v = inner(name, default) %}{% do return(v) %}{% endmacro %}",
                ),
            ),
        ],
        None,
    );
    // No vars → defaults decide: `on` alone survives (its default is
    // true; `arm`'s gate proves true and its `enabled=false` applies).
    let (m, w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    assert_eq!(
        m.nodes.keys().collect::<Vec<_>>(),
        vec!["model.p.on"],
        "{:?}",
        m.nodes.keys().collect::<Vec<_>>()
    );
    assert!(
        !w.iter().any(|s| s.contains("could not be evaluated")),
        "{w:?}"
    );
    // `--vars` flips the var-backed calls — the literal-false bodies
    // (`ret`, `kw`) stay parked regardless.
    let vars = CliVars::parse(
        "{inputs_enabled: true, gate_flag: false, inner_flag: true, deep_flag: true}",
    )
    .unwrap();
    let (m, _) = manifest_from_source_with_warnings(dir.path(), Some(&vars)).unwrap();
    for uid in [
        "model.p.a",
        "model.p.on",
        "model.p.def",
        "model.p.nest",
        "model.p.arm",
    ] {
        assert!(m.nodes.contains_key(uid), "{uid} survives the --vars flip");
    }
    for uid in ["model.p.ret", "model.p.kw", "model.p.unq"] {
        assert!(!m.nodes.contains_key(uid), "{uid} stays parked");
    }
}

#[test]
fn enabled_project_tree_and_schema_stay_macro_blind() {
    let dir = tempfile::tempdir().unwrap();
    // `+enabled`/`enabled:` render in the YAML pass — dbt itself can't
    // call a project macro there (`'ret_off' is undefined` — verified
    // on 1.12.7, a parse error), so the expression stays unprovable:
    // the node keeps its enabled+marked stance either way.
    macro_project(
        dir.path(),
        &[("m2", "select 1")],
        &[(
            "macros",
            "{% macro ret_off() %}{% do return(false) %}{% endmacro %}",
        )],
        Some(
            "version: 2\nmodels:\n  - name: m2\n    config:\n      enabled: \"{{ ret_off() }}\"\n",
        ),
    );
    // The tree `+enabled` for m1 only (a subdirectory's tree).
    std::fs::create_dir_all(dir.path().join("models/sub")).unwrap();
    std::fs::write(dir.path().join("models/sub/m1.sql"), "select 1").unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nmodels:\n  p:\n    sub:\n      +enabled: \"{{ ret_off() }}\"\n",
    )
    .unwrap();
    let (m, w) = manifest_from_source_with_warnings(dir.path(), None).unwrap();
    // Both channels' macro calls stay unproven — each node survives,
    // marked, with its own warning.
    for uid in ["model.p.m1", "model.p.m2"] {
        assert!(m.nodes.contains_key(uid), "{uid} stays");
        assert!(m.enabled_unverifiable.contains(uid), "{uid} is marked");
    }
    let eval_lines: Vec<&str> = w
        .iter()
        .filter(|s| s.contains("could not be evaluated"))
        .map(String::as_str)
        .collect();
    assert_eq!(eval_lines.len(), 2, "{w:?}");
}
