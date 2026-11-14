//! Pure `--fix` edit planning: which byte ranges of a flagged node's
//! `raw_code` a fixable rule's finding lets go — computed from the same
//! fact/position inputs the rules themselves read, so an edit can never
//! target a def the finding didn't judge. Nothing here does I/O: the
//! caller reads the file, applies [`apply_fixes`], and re-lints.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::cll::{semicolon_delete, sql_still_parses, unread_cte_deletes};
use crate::source::{call_sites_masked, mask_jinja_comments, CallKind};
use crate::{Dag, LintRule, RawManifest, SqlDialect, SqlParseResolver};

/// One byte-range deletion `--fix` may make for a finding — `rule`
/// records which rule's finding the deletion satisfies (warnings and
/// the per-node guard both read it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixEdit {
    /// Byte range into the node's `raw_code` — delete these bytes.
    pub range: Range<usize>,
    /// The rule whose finding the deletion satisfies.
    pub rule: LintRule,
}

/// The deletions `--fix` may make on `uid`'s raw SQL — `rules` are the
/// fixable `Finding.rule`s seen on the node this round (the caller
/// groups findings; only fixable rules ever produce edits). `[]` when
/// nothing is safely fixable: a `;` inside Jinja, an unread `WITH` item
/// whose def carries a marker or sits in a conditional block, a file
/// the position pass can't pin — those findings simply remain.
pub fn plan_node_edits(
    dag: &Dag,
    sql: &SqlParseResolver,
    uid: &str,
    rules: &BTreeSet<LintRule>,
) -> Vec<FixEdit> {
    let mut edits = Vec::new();
    let Some(raw) = dag.raw_code(uid) else {
        return edits;
    };
    let dialect = sql.dialect();
    if rules.contains(&LintRule::TrailingSemicolon) {
        if let Some(range) = semicolon_delete(raw, dialect) {
            edits.push(FixEdit {
                range,
                rule: LintRule::TrailingSemicolon,
            });
        }
    }
    if rules.contains(&LintRule::UnusedCte) {
        if let (Some(facts), Some(pos)) = (sql.sql_facts(dag, uid), sql.sql_positions(dag, uid)) {
            // Pair judged names with raw declaration ranges exactly like
            // the rule's own position pass: a judged name underlines the
            // raw decls that count it the same — or the lone one — so a
            // Jinja-produced CTE can't misalign a delete.
            let mut names: Vec<&String> = Vec::new();
            for name in &facts.unused_ctes {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            let mut ranges: Vec<Range<usize>> = Vec::new();
            for name in names {
                let judged = facts.unused_ctes.iter().filter(|n| n == &name).count();
                let hits: Vec<&Range<usize>> = pos
                    .unused_ctes
                    .iter()
                    .filter(|(n, _)| n == name)
                    .map(|(_, r)| r)
                    .collect();
                let paired: &[&Range<usize>] = if hits.len() == judged {
                    &hits
                } else if hits.len() == 1 {
                    &hits[..1]
                } else {
                    continue;
                };
                ranges.extend(paired.iter().map(|r| (*r).clone()));
            }
            for range in unread_cte_deletes(raw, &ranges, dialect) {
                edits.push(FixEdit {
                    range,
                    rule: LintRule::UnusedCte,
                });
            }
        }
    }
    edits
}

/// Apply `edits` to `text` and re-check the result — the caller's whole
/// per-file edit step:
///
/// - edits apply end-to-start so a later range never shifts an earlier
///   one;
/// - two edits that overlap refuse the file (`None` — the caller warns
///   and leaves it alone, per the "overlapping edits" rule);
/// - a range not on UTF-8 boundaries refuses the file (it could never
///   have come from the scans — defensive);
/// - when an `unused_cte` edit landed, the new text must still render
///   and parse — the rule's own precondition for the delete.
///
/// `Some(new)` is the file's post-fix content; `new == text` never
/// happens on a non-empty edit list (the ranges are non-empty by
/// construction) but costs the caller nothing to compare anyway.
pub fn apply_fixes(text: &str, edits: &[FixEdit], dialect: SqlDialect) -> Option<String> {
    let mut ranges: Vec<Range<usize>> = Vec::with_capacity(edits.len());
    let mut reparses = false;
    for e in edits {
        if !text.is_char_boundary(e.range.start) || !text.is_char_boundary(e.range.end) {
            return None;
        }
        reparses |= e.rule == LintRule::UnusedCte;
        ranges.push(e.range.clone());
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    if ranges.windows(2).any(|w| w[1].start < w[0].end) {
        return None;
    }
    let mut out = text.to_string();
    for r in ranges.iter().rev() {
        out.replace_range(r.clone(), "");
    }
    if reparses && !sql_still_parses(&out, dialect) {
        return None;
    }
    Some(out)
}

/// `manifest` with each `uid → new raw_code` patch applied — the
/// fixpoint loop's rebuild input — plus, per patched uid, the parents
/// the edit dropped. Two things follow the text, never the old graph:
///
/// - `compiled_code` clears on patched nodes — the stored compiled
///   text was produced from the pre-edit source; keeping it would let
///   a finding read bytes the file no longer holds.
/// - The node's adjacency is re-derived. Fixes only ever DELETE text,
///   so a patched node's parents are a subset of its original ones —
///   `parent_map[uid]`, `child_map[parent]`, and `depends_on.nodes`
///   keep exactly the parents a `ref()`/`source()` rescan of the new
///   text still names. A `{{ ref('x') }}` inside a deleted CTE loses
///   its edge: the post-fix `unused_models` sees the truth rather than
///   a parent ghost the file no longer reads — and the caller's
///   introduced-findings guard knows exactly which file orphaned it.
///
/// The second return maps `uid → parents it stopped reading` (absent
/// for a patch that dropped none).
pub fn manifest_patched(
    manifest: &RawManifest,
    patches: &BTreeMap<String, String>,
) -> (RawManifest, BTreeMap<String, Vec<String>>) {
    let mut m = manifest.clone();
    for (uid, text) in patches {
        if let Some(node) = m.nodes.get_mut(uid) {
            node.raw_code = Some(text.clone());
            node.compiled_code = None;
        }
    }
    let mut dropped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (uid, text) in patches {
        let orig: BTreeSet<String> = m
            .nodes
            .get(uid)
            .map(|n| n.depends_on.nodes.iter().cloned().collect::<BTreeSet<_>>())
            .unwrap_or_default()
            .into_iter()
            .chain(m.parent_map.get(uid).cloned().unwrap_or_default())
            .collect();
        if orig.is_empty() {
            continue;
        }
        let masked = mask_jinja_comments(text);
        let sites = call_sites_masked(&masked);
        let keeps = |p: &String| surviving_parent(p, &m, &masked, &sites);
        let kept: BTreeSet<String> = orig.iter().filter(|p| keeps(p)).cloned().collect();
        if kept.len() == orig.len() {
            continue; // nothing the node read went away
        }
        dropped.insert(
            uid.clone(),
            orig.iter()
                .filter(|p| !kept.contains(*p))
                .cloned()
                .collect(),
        );
        if let Some(row) = m.parent_map.get_mut(uid) {
            row.retain(|p| kept.contains(p));
        }
        if m.parent_map.get(uid).is_some_and(|r| r.is_empty()) {
            m.parent_map.remove(uid);
        }
        for p in &orig {
            if kept.contains(p) {
                continue;
            }
            if let Some(children) = m.child_map.get_mut(p) {
                children.retain(|c| c != uid);
            }
            if m.child_map.get(p).is_some_and(|c| c.is_empty()) {
                m.child_map.remove(p);
            }
        }
        if let Some(node) = m.nodes.get_mut(uid) {
            node.depends_on.nodes.retain(|p| kept.contains(p));
        }
    }
    (m, dropped)
}

/// Does `p` — one of `uid`'s recorded parents — still have a call
/// naming it in the patched text? A `ref()` matches on the parent's
/// declared `name` (the uid's last segment when the record carries no
/// name yet), plus the package arg when the call wrote one
/// (`ref('pkg','x')`); a `source()` matches its `<group>.<table>` uid
/// pair. Parents that are neither nodes nor sources — test uids, macro
/// adjacency — can't be proven removable, so they stay.
fn surviving_parent(
    p: &str,
    m: &RawManifest,
    masked: &str,
    sites: &[crate::source::CallSite],
) -> bool {
    let node_parent = m.nodes.contains_key(p);
    let source_parent = m.sources.contains_key(p);
    if !node_parent && !source_parent {
        return true;
    }
    let segs: Vec<&str> = p.split('.').collect();
    sites.iter().any(|s| match &s.kind {
        CallKind::Ref { name, package, .. } if node_parent => {
            if masked[name.clone()] != *m.nodes[p].name.as_str() {
                return false;
            }
            match package {
                Some(pkg) => {
                    let parent_pkg = m.nodes[p]
                        .package_name
                        .as_deref()
                        .or_else(|| segs.get(1).copied())
                        .unwrap_or_default();
                    masked[pkg.clone()] == *parent_pkg
                }
                None => true,
            }
        }
        CallKind::Source { source, table } if source_parent => {
            // `source.<pkg>.<group>.<table>` — the call's two literals.
            segs.len() == 4
                && masked[source.clone()] == *segs[2]
                && masked[table.clone()] == *m.sources[p].name.as_str()
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawDependsOn, RawNode, RawSource};
    use std::collections::HashMap;

    /// One `unused_cte`/`trailing_semicolon` edit literal.
    fn kill(rule: LintRule, s: usize, e: usize) -> FixEdit {
        FixEdit { range: s..e, rule }
    }

    #[test]
    fn apply_deletes_back_to_front() {
        // `with a as (select 1), b as (select 2)\nselect * from b;` —
        // drop `a`'s item + its `,` (5..22) and the tail `;` (53..54).
        let text = "with a as (select 1), b as (select 2)\nselect * from b;";
        let out = apply_fixes(
            text,
            &[
                kill(LintRule::TrailingSemicolon, 53, 54),
                kill(LintRule::UnusedCte, 5, 22),
            ],
            SqlDialect::Generic,
        )
        .unwrap();
        assert_eq!(out, "with b as (select 2)\nselect * from b");
    }

    #[test]
    fn apply_refuses_overlaps_and_bad_boundaries() {
        let text = "select 'é' as x;\n";
        // Overlapping ranges — the file is refused whole.
        assert_eq!(
            apply_fixes(
                text,
                &[
                    kill(LintRule::UnusedCte, 0, 5),
                    kill(LintRule::UnusedCte, 4, 9)
                ],
                SqlDialect::Generic
            ),
            None
        );
        // A range inside the `é` is not a char boundary — refused.
        let mid = text.find('é').unwrap() + 1;
        assert_eq!(
            apply_fixes(
                text,
                &[kill(LintRule::UnusedCte, mid, mid + 1)],
                SqlDialect::Generic
            ),
            None
        );
    }

    #[test]
    fn apply_reparses_after_a_cte_delete() {
        // A `;`-only edit never reparses (deleting it can't break SQL);
        // an `unused_cte` edit that would break the statement refuses.
        let text = "select 1;";
        assert_eq!(
            apply_fixes(
                text,
                &[kill(LintRule::TrailingSemicolon, 8, 9)],
                SqlDialect::Generic
            ),
            Some("select 1".to_string())
        );
        // Cutting `(` unbalances — the re-parse guard refuses the file.
        assert_eq!(
            apply_fixes(
                "with a as (select 1) select * from a",
                &[kill(LintRule::UnusedCte, 10, 11)],
                SqlDialect::Generic
            ),
            None
        );
    }

    /// A two-model manifest: `m` reads `base` (parent_map + child_map +
    /// depends_on all record the edge — the shape both loaders write).
    fn linked_manifest() -> RawManifest {
        let mut nodes = HashMap::new();
        nodes.insert(
            "model.p.base".to_string(),
            RawNode {
                name: "base".into(),
                resource_type: "model".into(),
                raw_code: Some("select 1 as id\n".into()),
                ..Default::default()
            },
        );
        nodes.insert(
            "model.p.m".to_string(),
            RawNode {
                name: "m".into(),
                resource_type: "model".into(),
                raw_code: Some("with dead as (select * from {{ ref('base') }})\nselect 2\n".into()),
                depends_on: RawDependsOn {
                    nodes: vec!["model.p.base".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        RawManifest {
            metadata: Default::default(),
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            docs: Default::default(),
            disabled: Default::default(),
            ref_scope: None,
            parent_map: HashMap::from([(
                "model.p.m".to_string(),
                vec!["model.p.base".to_string()],
            )]),
            child_map: HashMap::from([("model.p.base".to_string(), vec!["model.p.m".to_string()])]),
            unresolved_adjacent: Default::default(),
        }
    }

    #[test]
    fn patch_removing_the_last_ref_drops_the_edge() {
        let m = linked_manifest();
        let patches = BTreeMap::from([("model.p.m".to_string(), "select 2\n".to_string())]);
        let (patched, dropped) = manifest_patched(&m, &patches);
        assert_eq!(
            patched.nodes["model.p.m"].raw_code.as_deref(),
            Some("select 2\n")
        );
        assert_eq!(
            patched.parent_map.get("model.p.m"),
            None,
            "the row goes entirely"
        );
        assert_eq!(
            patched.child_map.get("model.p.base"),
            None,
            "and the reverse edge"
        );
        assert!(patched.nodes["model.p.m"].depends_on.nodes.is_empty());
        assert_eq!(
            dropped["model.p.m"],
            vec!["model.p.base".to_string()],
            "the caller sees which edge the edit cost"
        );
    }

    #[test]
    fn patch_keeping_a_ref_keeps_the_edge() {
        let m = linked_manifest();
        // The delete leaves one `ref('base')` call — the edge survives.
        let patches = BTreeMap::from([(
            "model.p.m".to_string(),
            "select * from {{ ref('base') }}\n".to_string(),
        )]);
        let (patched, dropped) = manifest_patched(&m, &patches);
        assert_eq!(
            patched.parent_map["model.p.m"],
            vec!["model.p.base".to_string()]
        );
        assert_eq!(
            patched.child_map["model.p.base"],
            vec!["model.p.m".to_string()]
        );
        assert!(dropped.is_empty());
        // A `;`-only patch changes nothing.
        let (patched, dropped) = manifest_patched(&m, &BTreeMap::new());
        assert_eq!(
            patched.parent_map["model.p.m"],
            vec!["model.p.base".to_string()]
        );
        assert!(dropped.is_empty());
    }

    #[test]
    fn patch_source_parent_matches_the_call_pair() {
        let mut m = linked_manifest();
        m.sources.insert(
            "source.p.shop.orders".to_string(),
            RawSource {
                name: "orders".into(),
                resource_type: "source".into(),
                ..Default::default()
            },
        );
        let nodes = m.nodes.get_mut("model.p.m").unwrap();
        nodes.raw_code = Some(
            "with dead as (select * from {{ source('shop','orders') }})\nselect * from {{ ref('base') }}\n".into(),
        );
        nodes.depends_on.nodes.push("source.p.shop.orders".into());
        m.parent_map
            .get_mut("model.p.m")
            .unwrap()
            .push("source.p.shop.orders".into());
        m.child_map.insert(
            "source.p.shop.orders".to_string(),
            vec!["model.p.m".to_string()],
        );
        // The patch keeps `ref('base')` but drops the source call —
        // only the source edge goes.
        let patches = BTreeMap::from([(
            "model.p.m".to_string(),
            "select * from {{ ref('base') }}\n".to_string(),
        )]);
        let (patched, dropped) = manifest_patched(&m, &patches);
        assert_eq!(
            patched.parent_map["model.p.m"],
            vec!["model.p.base".to_string()]
        );
        assert_eq!(patched.child_map.get("source.p.shop.orders"), None);
        assert_eq!(
            dropped["model.p.m"],
            vec!["source.p.shop.orders".to_string()]
        );
    }
}
