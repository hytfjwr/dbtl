//! dbt selector syntax over a [`Dag`]: the subset the app actually needs,
//! evaluated deterministically.
//!
//! ```text
//! spec    := term (WS term)*            // whitespace = union
//! term    := factor ("," factor)*       // comma = intersection
//! factor  := [N] "+""? atom "+"?         // hop prefixes/suffixes
//! atom    := "tag:"x | "path:"x | "config.materialized:"x | "exposure:"x
//!          | "state:"x | "meta:"k["="v] | name   // bare name; meta:k = key exists
//! ```
//!
//! `2+name` = the node plus ≤2 upstream hops; `name+` = node + all downstream;
//! `+name+` = both directions. `state:modified[+]` selects the [`DagDiff`]
//! changed set (added ∪ modified) when a diff is loaded — the in-app
//! equivalent of `dbt --select state:modified+`.
//!
//! Returned set = `unique_id`s.

use std::collections::{BTreeSet, HashMap};

use crate::{is_exposure, Dag, DagDiff};

/// Evaluate `spec` over `dag` (with optional `diff` for `state:` selectors) →
/// the matching `unique_id` set. Union across whitespace-separated terms,
/// intersection across each term's comma-separated factors.
pub fn select(dag: &Dag, diff: Option<&DagDiff>, spec: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for term in spec.split_whitespace() {
        out.extend(evaluate_term(dag, diff, term));
    }
    out
}

/// One comma-separated intersection term.
fn evaluate_term(dag: &Dag, diff: Option<&DagDiff>, term: &str) -> BTreeSet<String> {
    let mut acc: Option<BTreeSet<String>> = None;
    for factor in term.split(',') {
        let set = evaluate_factor(dag, diff, factor);
        acc = Some(match acc {
            None => set,
            Some(prev) => prev.intersection(&set).cloned().collect(),
        });
    }
    acc.unwrap_or_default()
}

/// `[@N+] atom [+]` — an atom set plus its bounded neighbourhood.
fn evaluate_factor(dag: &Dag, diff: Option<&DagDiff>, factor: &str) -> BTreeSet<String> {
    // Leading hops: "+" or "<n>+" before the atom.
    let (up_hops, rest) = match factor.split_once('+') {
        Some(("", rest)) => (None, rest), // +x
        Some((lead, rest)) if lead.chars().all(|c| c.is_ascii_digit()) => {
            (lead.parse::<usize>().ok(), rest) // 2+x
        }
        _ => (Some(0), factor),
    };
    let up_hops = up_hops.unwrap_or(usize::MAX);
    // Trailing "+": downstream, unbounded.
    let (atom, down) = match rest.strip_suffix('+') {
        Some(a) => (a, true),
        None => (rest, false),
    };
    let base = evaluate_atom(dag, diff, atom);
    if up_hops == 0 && !down {
        return base;
    }
    let mut out = base.clone();
    let parents = adjacency(dag, true);
    let children = adjacency(dag, false);
    for uid in &base {
        if up_hops > 0 {
            out.extend(bounded_walk(uid, &parents, up_hops));
        }
        if down {
            out.extend(bounded_walk(uid, &children, usize::MAX));
        }
    }
    out
}

/// The atom's direct match set (before hop expansion).
fn evaluate_atom(dag: &Dag, diff: Option<&DagDiff>, atom: &str) -> BTreeSet<String> {
    let nodes = || dag.nodes().iter();
    // `config.materialized:x` — the key itself is dotted, so handle it before
    // the generic `key:value` split (a bare split_once(':') would misread it
    // as a name containing a colon).
    if let Some(c) = atom.strip_prefix("config.") {
        return match c.split_once(':') {
            Some(("materialized", m)) => nodes()
                .filter(|(_, n)| n.materialized.as_deref() == Some(m))
                .map(|(uid, _)| uid.clone())
                .collect(),
            _ => BTreeSet::new(),
        };
    }
    // `meta:key=value` / `meta:key` — dotted like `config.` is not (dbt's own
    // selector dialect has no meta atom; this is `key[=value]` after the
    // `meta:` prefix). The value compares against the meta dict's STRINGIFIED
    // form (scalars unquoted, nested values compact JSON), so
    // `meta:owner=alice` and `meta:cfg={"a":1}` both work.
    if let Some(m) = atom.strip_prefix("meta:") {
        let (key, val) = m.split_once('=').map_or((m, None), |(k, v)| (k, Some(v)));
        return nodes()
            .filter(|(uid, _)| {
                dag.detail(uid).is_some_and(|d| match d.meta.get(key) {
                    Some(have) => val.is_none_or(|v| have == v),
                    None => false,
                })
            })
            .map(|(uid, _)| uid.clone())
            .collect();
    }
    match atom.split_once(':') {
        Some(("tag", t)) => nodes()
            .filter(|(uid, _)| {
                dag.detail(uid)
                    .is_some_and(|d| d.tags.iter().any(|x| x == t))
            })
            .map(|(uid, _)| uid.clone())
            .collect(),
        Some(("path", p)) => nodes()
            .filter(|(_, n)| n.path.as_deref().is_some_and(|np| np.contains(p)))
            .map(|(uid, _)| uid.clone())
            .collect(),
        Some(("exposure", e)) => {
            // Models feeding the named exposure = its upstream closure + the
            // exposure itself (matches `dbt --select exposure:x` semantics).
            nodes()
                .filter(|(_, n)| is_exposure(n) && n.name == e)
                .flat_map(|(uid, _)| {
                    let mut s = dag.upstream(uid);
                    s.insert(uid.clone());
                    s
                })
                .collect()
        }
        Some(("state", s)) => match (s, diff) {
            ("modified", Some(d)) => d.added.iter().chain(d.modified.keys()).cloned().collect(),
            _ => BTreeSet::new(),
        },
        _ => nodes()
            .filter(|(_, n)| n.name == atom)
            .map(|(uid, _)| uid.clone())
            .collect(),
    }
}

/// `unique_id` → neighbour `unique_id`s, from the kept edge list.
/// `up=true` gives parents (upstream walk), `false` gives children.
fn adjacency(dag: &Dag, up: bool) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for (p, c) in dag.edges() {
        let (k, v) = if up { (c, p) } else { (p, c) };
        m.entry(k).or_default().push(v);
    }
    m
}

/// BFS from `start` through `adj`, collecting nodes within `max_hops`
/// (`usize::MAX` = transitive closure, same set `Dag::upstream` yields).
fn bounded_walk(
    start: &str,
    adj: &HashMap<String, Vec<String>>,
    max_hops: usize,
) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut frontier = vec![start.to_string()];
    for _ in 0..max_hops {
        let mut next = Vec::new();
        for uid in frontier.drain(..) {
            for nb in adj.get(&uid).into_iter().flatten() {
                if seen.insert(nb.clone()) {
                    next.push(nb.clone());
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RawDependsOn, RawManifest, RawNode};
    use crate::Dag;

    /// stg -> int -> fct (+ a lone `other`), with tags/materializations/paths
    /// parameterizable by test.
    pub(super) fn dag() -> Dag {
        let mk = |uid: &str, name: &str, path: &str, mat: &str, deps: &[&str], tags: &[&str]| {
            (
                uid.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    path: Some(path.into()),
                    raw_code: Some("select 1".into()),
                    config: crate::RawConfig {
                        materialized: Some(mat.into()),
                    },
                    tags: tags.iter().map(|t| t.to_string()).collect(),
                    depends_on: RawDependsOn {
                        nodes: deps.iter().map(|d| d.to_string()).collect(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
        };
        let nodes = HashMap::from([
            mk(
                "model.p.stg",
                "stg",
                "staging/stg.sql",
                "view",
                &[],
                &["nightly"],
            ),
            mk(
                "model.p.int",
                "int",
                "intermediate/int.sql",
                "ephemeral",
                &["model.p.stg"],
                &["nightly"],
            ),
            mk(
                "model.p.fct",
                "fct",
                "marts/fct.sql",
                "table",
                &["model.p.int"],
                &["daily"],
            ),
            mk(
                "model.p.other",
                "other",
                "marts/other.sql",
                "table",
                &[],
                &[],
            ),
        ]);
        Dag::build(&RawManifest {
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

            parent_map: HashMap::from([
                ("model.p.int".into(), vec!["model.p.stg".into()]),
                ("model.p.fct".into(), vec!["model.p.int".into()]),
            ]),
            child_map: HashMap::from([
                ("model.p.stg".into(), vec!["model.p.int".into()]),
                ("model.p.int".into(), vec!["model.p.fct".into()]),
            ]),
        })
    }

    fn names(set: &BTreeSet<String>) -> Vec<String> {
        set.iter()
            .map(|u| u.rsplit('.').next().unwrap().to_string())
            .collect()
    }

    #[test]
    fn bare_name_and_attr_selectors() {
        let d = dag();
        assert_eq!(names(&select(&d, None, "fct")), vec!["fct"]);
        assert_eq!(names(&select(&d, None, "tag:nightly")), vec!["int", "stg"]);
        assert_eq!(names(&select(&d, None, "path:staging")), vec!["stg"]);
        assert_eq!(
            names(&select(&d, None, "config.materialized:table")),
            vec!["fct", "other"]
        );
    }

    #[test]
    fn hop_expansion_both_directions() {
        let d = dag();
        // stg+ = stg and everything downstream of it.
        assert_eq!(names(&select(&d, None, "stg+")), vec!["fct", "int", "stg"]);
        // +fct = fct and its whole upstream.
        assert_eq!(names(&select(&d, None, "+fct")), vec!["fct", "int", "stg"]);
        // 1+fct = fct + exactly one hop up.
        assert_eq!(names(&select(&d, None, "1+fct")), vec!["fct", "int"]);
    }

    #[test]
    fn space_is_union_comma_is_intersection() {
        let d = dag();
        // union: two names
        assert_eq!(names(&select(&d, None, "stg fct")), vec!["fct", "stg"]);
        // intersection: nightly AND a table → nothing (nightlys aren't tables)
        assert!(select(&d, None, "tag:nightly,config.materialized:table").is_empty());
        // intersection that does match: nightly AND ephemeral
        assert_eq!(
            names(&select(
                &d,
                None,
                "tag:nightly,config.materialized:ephemeral"
            )),
            vec!["int"]
        );
    }

    /// Two models carrying `meta` dicts (incl. a nested object value) + one
    /// without — the meta selector testbed.
    fn meta_dag() -> Dag {
        let mk = |uid: &str, name: &str, meta: serde_json::Value| {
            (
                uid.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    meta: meta.as_object().cloned().unwrap_or_default(),
                    ..Default::default()
                },
            )
        };
        let nodes = HashMap::from([
            mk(
                "model.p.orders",
                "orders",
                serde_json::json!({"owner": "alice", "domain": "sales"}),
            ),
            mk(
                "model.p.custs",
                "custs",
                serde_json::json!({"owner": "bob", "cfg": {"tier": 1}}),
            ),
            mk("model.plain", "plain", serde_json::json!({})),
        ]);
        Dag::build(&RawManifest {
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
            parent_map: HashMap::new(),
            child_map: HashMap::new(),
        })
    }

    #[test]
    fn meta_selectors() {
        let d = meta_dag();
        // key=value
        assert_eq!(names(&select(&d, None, "meta:owner=alice")), vec!["orders"]);
        // missing key matches nothing, even for existing nodes
        assert!(select(&d, None, "meta:nosuch=v").is_empty());
        // wrong value matches nothing
        assert!(select(&d, None, "meta:owner=carol").is_empty());
        // key existence alone
        assert_eq!(names(&select(&d, None, "meta:domain")), vec!["orders"]);
        // a nested dict compares as its compact-JSON string form
        assert_eq!(
            names(&select(&d, None, r#"meta:cfg={"tier":1}"#)),
            vec!["custs"]
        );
        // meta composes with hops/intersection like every other atom
        assert_eq!(
            names(&select(&d, None, "meta:owner=bob,meta:cfg")),
            vec!["custs"]
        );
    }

    #[test]
    fn state_modified_uses_the_diff() {
        // current: fct's SQL changed → modified.
        let mut changed = RawManifest {
            metadata: Default::default(),
            nodes: HashMap::new(),
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),

            parent_map: HashMap::new(),
            child_map: HashMap::new(),
        };
        let mut base_nodes = HashMap::new();
        let fct = RawNode {
            name: "fct".into(),
            resource_type: "model".into(),
            path: Some("marts/fct.sql".into()),
            raw_code: Some("select 1".into()),
            ..Default::default()
        };
        let mut fct2 = fct.clone();
        fct2.raw_code = Some("select 2".into());
        base_nodes.insert("model.p.fct".to_string(), fct);
        let mut cur_nodes = HashMap::new();
        cur_nodes.insert("model.p.fct".to_string(), fct2);
        let base = Dag::build(&RawManifest {
            nodes: base_nodes,
            ..changed.clone()
        });
        changed.nodes = cur_nodes;
        let cur = Dag::build(&changed);
        let diff = crate::compute_diff(&base, &cur);
        // state:modified matches the changed uid even in the FULL dag's space
        // only when evaluated against the diff's uid space — here the diff dag
        // is `cur` itself.
        assert_eq!(
            select(&cur, Some(&diff), "state:modified"),
            BTreeSet::from(["model.p.fct".to_string()])
        );
        // Without a diff, state: matches nothing.
        assert!(select(&cur, None, "state:modified").is_empty());
    }
}
