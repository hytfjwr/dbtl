//! `sql` — SQL-body rules (`Requires::Sql`): wildcard projections,
//! hand-written relations, conditionless joins, provably-missing parent
//! columns, and the configured forbidden-pattern scan.
//!
//! Every rule reads the SHARED [`SqlParseResolver`] from [`LintInput`]:
//! `sql_facts`/`hard_coded_relations` are memoized per uid, so several
//! SQL rules on one model still parse it once. A model with no analyzable
//! SQL (`None`) is never judged — "can't disprove" doesn't flag.

use super::*;
use crate::SqlParseResolver;

/// `select_star`: a model's output-producing projection reads `*`/`t.*` —
/// including a UNION arm and `|> SELECT *`, never a wildcard confined to a
/// CTE (its outer select enumerates the outputs anyway). `count(*)` is a
/// function call, not a projection wildcard.
pub(super) fn select_star(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sql: &SqlParseResolver,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let Some(facts) = sql.sql_facts(dag, uid) else {
            continue;
        };
        if facts.unbounded_wildcard {
            out.push(Finding::new(
                LintRule::SelectStar,
                (*uid).clone(),
                vec![],
                format!("{}'s final SELECT uses select * / t.*", n.name),
            ));
        }
    }
}

/// `hard_coded_references`: a model names a relation literally
/// (`from analytics.orders`) instead of `ref()`/`source()`. Only
/// `raw_code` is judged — a compiled-only model's rendered names can't be
/// told apart from written ones — and anything the marked render can't
/// parse is not judged.
pub(super) fn hard_coded_references(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sql: &SqlParseResolver,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let Some(names) = sql.hard_coded_relations(dag, uid) else {
            continue;
        };
        if !names.is_empty() {
            out.push(Finding::new(
                LintRule::HardCodedReferences,
                (*uid).clone(),
                vec![],
                format!(
                    "{} reads relation(s) without ref()/source(): {}",
                    n.name,
                    names.join(", ")
                ),
            ));
        }
    }
}

/// `cartesian_join`: a JOIN arm carries no condition — `CROSS JOIN` by
/// definition, or a conditional operator (`JOIN`/`INNER`/`LEFT`/…) with no
/// ON/USING/NATURAL. Lateral applies, ASOF matches and UNNEST don't count
/// (a condition or a per-row explode, not a product).
pub(super) fn cartesian_join(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    sql: &SqlParseResolver,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let Some(facts) = sql.sql_facts(dag, uid) else {
            continue;
        };
        if !facts.cartesian.is_empty() {
            out.push(Finding::new(
                LintRule::CartesianJoin,
                (*uid).clone(),
                vec![],
                format!(
                    "{} joins without a join condition: {}",
                    n.name,
                    facts.cartesian.join(", ")
                ),
            ));
        }
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
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let Some(facts) = sql.sql_facts(dag, uid) else {
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
        out.push(Finding::new(
            LintRule::UnknownColumnReference,
            (*uid).clone(),
            related,
            format!(
                "{} names column(s) its upstream doesn't emit: {}",
                n.name,
                pairs.join(", ")
            ),
        ));
    }
}

/// `forbidden_patterns`: the model's `raw_code` (verbatim — comments stay,
/// matching is policy on what was WRITTEN) hits a configured regex. An
/// empty pattern list is a no-op; invalid patterns can't reach here — the
/// config parser rejects them at load (a programmatically-built config is
/// guarded by skipping uncompilable entries rather than panicking).
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
        if n.resource_type != "model" || !in_scope(n) {
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
            out.push(Finding::new(
                LintRule::ForbiddenPatterns,
                (*uid).clone(),
                vec![],
                format!(
                    "{}'s raw SQL matches forbidden pattern(s): {}",
                    n.name,
                    hits.join(", ")
                ),
            ));
        }
    }
}
