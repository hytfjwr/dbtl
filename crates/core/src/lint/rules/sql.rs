//! `sql` — SQL-body rules (`Requires::Sql`): wildcard projections,
//! hand-written relations, conditionless joins, provably-missing parent
//! columns, and the configured forbidden-pattern scan.
//!
//! Every rule reads the SHARED [`SqlParseResolver`] from [`LintInput`]:
//! `sql_facts`/`hard_coded_relations` are memoized per uid, so several
//! SQL rules on one model still parse it once. A model with no analyzable
//! SQL (`None`) is never judged — "can't disprove" doesn't flag; a model
//! whose SQL EXISTS but wouldn't parse lands on `unparsed` instead, so
//! the summary can say the rules skipped it rather than silently passing
//! it. Python models (`language == "python"`) are excluded entirely —
//! their `raw_code` is Python, not SQL.

use std::ops::Range;

use super::*;
use crate::SqlParseResolver;

/// One raw-SQL occurrence a finding can point at: the byte range inside
/// the model's `raw_code` and the note the position carries (the primary
/// position's `label`, a secondary `labels` entry).
type SpanNote = (Range<usize>, String);

/// `uid` asked for parse facts and got `None` though the model HAS SQL
/// text (raw or compiled — [`crate::resolution_sql`]'s either-source
/// read): it belongs on the outcome's `unparsed` set, so the summary can
/// report "skipped by SQL rules" instead of silently passing it.
fn record_unparsed(dag: &Dag, uid: &str, unparsed: &mut BTreeSet<String>) {
    if crate::resolution_sql(dag, uid).is_some() {
        unparsed.insert(uid.to_string());
    }
}

/// The `(file, raw SQL)` a SQL finding's positions anchor into — the
/// `.sql` path plus the raw text byte ranges index into. `None` when
/// either is unknown — or when the file is a `.py` model (its `raw_code`
/// is Python: a coincidental regex hit or parsed token would underline
/// Python, never SQL). The finding still reports, it just can't point.
fn sql_anchor<'a>(dag: &'a Dag, uid: &str) -> Option<(String, &'a str)> {
    let file = dag.detail(uid)?.original_file_path.clone()?;
    if file.ends_with(".py") {
        return None;
    }
    let raw = dag.raw_code(uid)?;
    Some((file, raw))
}

/// Attach positions to `f`: every entry in `spans` is one annotation —
/// sorted by source position, the first convertible range becomes the
/// primary position (its note the `label`), the rest `labels`. Ranges
/// index into `raw`; `byte_span_position` does the line/column math — a
/// range it can't pin is dropped, never guessed.
fn with_positions(f: Finding, file: &str, raw: &str, spans: &mut [SpanNote]) -> Finding {
    spans.sort_by_key(|s| (s.0.start, s.0.end));
    let mut it = spans
        .iter()
        .filter_map(|(r, msg)| byte_span_position(raw, r.start, r.end).map(|p| (p, msg.as_str())));
    let Some(((line, column, end_line, end_column), msg)) = it.next() else {
        return f;
    };
    f.with_span(file, line, column, end_line, end_column)
        .with_label(msg)
        .with_labels(
            it.map(|((line, column, end_line, end_column), msg)| Label {
                line,
                column: Some(column),
                end_line: Some(end_line),
                end_column: Some(end_column),
                message: msg.to_string(),
            })
            .collect(),
        )
}

/// `select_star`: a model's output-producing projection reads `*`/`t.*` —
/// including a UNION arm and `|> SELECT *`, never a wildcard confined to a
/// CTE (its outer select enumerates the outputs anyway). `count(*)` is a
/// function call, not a projection wildcard.
pub(super) fn select_star(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sql: &SqlParseResolver,
    unparsed: &mut BTreeSet<String>,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_sql_model(dag, n) || !in_scope(n) {
            continue;
        }
        let Some(facts) = sql.sql_facts(dag, uid) else {
            record_unparsed(dag, uid, unparsed);
            continue;
        };
        if !facts.unbounded_wildcard {
            continue;
        }
        let mut f = Finding::new(
            LintRule::SelectStar,
            (*uid).clone(),
            vec![],
            format!("{}'s final SELECT uses select * / t.*", n.name),
        );
        if let Some((file, raw)) = sql_anchor(dag, uid) {
            if let Some(pos) = sql.sql_positions(dag, uid) {
                // Positional pairing by wildcard count: when the judged
                // text (compiled-preferred) and the raw render see the
                // same number of output wildcards, each raw one is
                // locatable — otherwise a Jinja-dropped UNION arm could
                // shift the underline onto a different arm's `*`, so the
                // finding stays unlocated. The first wildcard is the
                // primary position; further ones ride as labels.
                if pos.wildcards.len() == facts.wildcard_count {
                    let mut spans: Vec<SpanNote> = pos
                        .wildcards
                        .iter()
                        .map(|r| (r.clone(), "wildcard".to_string()))
                        .collect();
                    f = with_positions(f, &file, raw, &mut spans);
                }
            }
        }
        out.push(f);
    }
}

/// `hard_coded_references`: a model names a relation literally
/// (`from analytics.orders`) instead of `ref()`/`source()`. Only
/// `raw_code` is judged — a compiled-only model's rendered names can't be
/// told apart from written ones — and anything the marked render can't
/// parse is not judged (it counts on `unparsed` instead, when raw SQL
/// exists). `information_schema` reads never reach the scan; the `ignore`
/// parameter's globs drop additional named relations — a pattern matches
/// when it covers the recorded name OR any trailing suffix of it, so
/// `orders` ignores `analytics.orders` too.
pub(super) fn hard_coded_references(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    sql: &SqlParseResolver,
    unparsed: &mut BTreeSet<String>,
    out: &mut Vec<Finding>,
) {
    let ignore: Vec<regex::Regex> = config
        .str_list_param(LintRule::HardCodedReferences, "ignore")
        .expect("the registry declares hard_coded_references.ignore")
        .iter()
        .map(|p| crate::select::glob_regex(p))
        .collect();
    let ignored = |name: &str| {
        ignore.iter().any(|re| {
            // Try the full dotted name, then each shorter suffix, so a
            // pattern can match `db.sch.t`, `sch.t`, or `t` alike.
            let mut rest = name;
            loop {
                if re.is_match(rest) {
                    return true;
                }
                match rest.split_once('.') {
                    Some((_, tail)) => rest = tail,
                    None => return false,
                }
            }
        })
    };
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_sql_model(dag, n) || !in_scope(n) {
            continue;
        }
        let Some(scan) = sql.hard_coded_relations(dag, uid) else {
            // `hard_coded_relations` judges `raw_code` only — a compiled-
            // only model isn't unparsed, it's deliberately unjudged.
            if dag.raw_code(uid).is_some_and(|r| !r.trim().is_empty()) {
                unparsed.insert((*uid).clone());
            }
            continue;
        };
        let names: Vec<&String> = scan.names.iter().filter(|n| !ignored(n)).collect();
        if !names.is_empty() {
            let mut f = Finding::new(
                LintRule::HardCodedReferences,
                (*uid).clone(),
                vec![],
                format!(
                    "{} reads {} without ref()/source(): {}",
                    n.name,
                    pluralize(names.len(), "relation"),
                    names
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            if let Some((file, raw)) = sql_anchor(dag, uid) {
                let mut spans: Vec<SpanNote> = scan
                    .spans
                    .iter()
                    .filter(|(name, _)| !ignored(name))
                    .map(|(_, r)| (r.clone(), "not a ref() or source()".to_string()))
                    .collect();
                f = with_positions(f, &file, raw, &mut spans);
            }
            out.push(f);
        }
    }
}

/// `cartesian_join`: a JOIN arm carries no condition — `CROSS JOIN` with
/// no constraint (MySQL lets one carry `ON` — a real condition there is a
/// join, not a product), a conditional operator (`JOIN`/`INNER`/`LEFT`/…)
/// with no ON/USING/NATURAL, an `ON` that can never reject a row
/// (`ON TRUE`, `ON 1 = 1`), or a comma-separated `FROM a, b` with no
/// `WHERE`. Lateral applies, ASOF matches and UNNEST don't count (a
/// condition or a per-row explode, not a product).
pub(super) fn cartesian_join(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sql: &SqlParseResolver,
    unparsed: &mut BTreeSet<String>,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_sql_model(dag, n) || !in_scope(n) {
            continue;
        }
        let Some(facts) = sql.sql_facts(dag, uid) else {
            record_unparsed(dag, uid, unparsed);
            continue;
        };
        if facts.cartesian.is_empty() {
            continue;
        }
        let mut f = Finding::new(
            LintRule::CartesianJoin,
            (*uid).clone(),
            vec![],
            format!(
                "{} joins without a join condition: {}",
                n.name,
                facts.cartesian.join(", ")
            ),
        );
        if let Some((file, raw)) = sql_anchor(dag, uid) {
            if let Some(pos) = sql.sql_positions(dag, uid) {
                // Positional pairing: attach only when the raw render sees
                // the same number of conditionless joins the facts were
                // computed on — a macro-added arm can't misalign i↔i.
                if pos.cartesian.len() == facts.cartesian_count {
                    let mut spans: Vec<SpanNote> = pos
                        .cartesian
                        .iter()
                        .map(|r| (r.clone(), "no join condition".to_string()))
                        .collect();
                    f = with_positions(f, &file, raw, &mut spans);
                }
            }
        }
        out.push(f);
    }
}

/// `unknown_column_reference`: the model names a column its parent
/// provably doesn't emit — restricted to TRUSTED parent column lists
/// ([`SqlParseResolver::trusted_parent_columns`]: the catalog knows the
/// parent's columns, or the parent enforces a column contract, so its
/// declared `columns:` are authoritative). Declared-but-unenforced
/// parents are advisory (yml can lag reality) and never flag.
pub(super) fn unknown_column_reference(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sql: &SqlParseResolver,
    unparsed: &mut BTreeSet<String>,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_sql_model(dag, n) || !in_scope(n) {
            continue;
        }
        let Some(facts) = sql.sql_facts(dag, uid) else {
            record_unparsed(dag, uid, unparsed);
            continue;
        };
        let mut missed: Vec<&(String, String)> = facts
            .unresolved
            .iter()
            .filter(|(parent, _)| sql.trusted_parent_columns(dag, parent))
            .collect();
        if missed.is_empty() {
            continue;
        }
        missed.sort();
        missed.dedup();
        let related: Vec<String> = {
            let mut v: Vec<&String> = missed.iter().map(|(p, _)| p).collect();
            v.sort();
            v.dedup();
            v.into_iter().cloned().collect()
        };
        let pairs: Vec<String> = missed.iter().map(|(p, c)| format!("{p}.{c}")).collect();
        let mut f = Finding::new(
            LintRule::UnknownColumnReference,
            (*uid).clone(),
            related,
            format!(
                "{} names {} its upstream doesn't emit: {}",
                n.name,
                pluralize(pairs.len(), "column"),
                pairs.join(", ")
            ),
        );
        if let Some((file, raw)) = sql_anchor(dag, uid) {
            if let Some(pos) = sql.sql_positions(dag, uid) {
                // A missed column pins a position only when its name
                // occurs EXACTLY once in the raw text — zero or several
                // can't point at the culprit, so they leave no position.
                let mut spans: Vec<SpanNote> = Vec::new();
                for (parent, col) in &missed {
                    let hits: Vec<Range<usize>> = pos
                        .columns
                        .iter()
                        .filter(|(name, _)| name.eq_ignore_ascii_case(col))
                        .map(|(_, r)| r.clone())
                        .collect();
                    if let [r] = hits.as_slice() {
                        spans.push((r.clone(), format!("{parent} does not output this column")));
                    }
                }
                f = with_positions(f, &file, raw, &mut spans);
            }
        }
        out.push(f);
    }
}

/// `forbidden_patterns`: the model's `raw_code` (verbatim — comments stay,
/// matching is policy on what was WRITTEN) hits a configured regex. An
/// empty pattern list is a no-op; invalid patterns can't reach here — the
/// config parser rejects them at load (a programmatically-built config is
/// guarded by skipping uncompilable entries rather than panicking). A
/// raw-text rule by design, so it never feeds `unparsed`.
pub(super) fn forbidden_patterns(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let patterns = config
        .str_list_param(LintRule::ForbiddenPatterns, "patterns")
        .expect("the registry declares forbidden_patterns.patterns");
    let compiled: Vec<(String, regex::Regex)> = patterns
        .into_iter()
        .filter_map(|p| regex::Regex::new(&p).ok().map(|r| (p, r)))
        .collect();
    if compiled.is_empty() {
        return;
    }
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_sql_model(dag, n) || !in_scope(n) {
            continue;
        }
        let Some(raw) = dag.raw_code(uid) else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let hits: Vec<&str> = compiled
            .iter()
            .filter(|(_, re)| re.is_match(raw))
            .map(|(p, _)| p.as_str())
            .collect();
        if !hits.is_empty() {
            let mut f = Finding::new(
                LintRule::ForbiddenPatterns,
                (*uid).clone(),
                vec![],
                format!(
                    "{}'s raw SQL matches forbidden {}: {}",
                    n.name,
                    pluralize(hits.len(), "pattern"),
                    hits.join(", ")
                ),
            );
            if let Some((file, raw)) = sql_anchor(dag, uid) {
                // Every match is an annotation: the first becomes the
                // primary position, the rest secondary labels — same raw
                // text the rule judged, no render needed.
                let mut spans: Vec<SpanNote> = compiled
                    .iter()
                    .flat_map(|(p, re)| {
                        re.find_iter(raw)
                            .map(move |m| (m.start()..m.end(), format!("matches {p}")))
                    })
                    .collect();
                f = with_positions(f, &file, raw, &mut spans);
            }
            out.push(f);
        }
    }
}
