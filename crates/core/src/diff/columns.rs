use super::*;

// ---------------------------------------------------------------------------
// Column-level diff (the CLL refinement of `modified`)
// ---------------------------------------------------------------------------

/// The column-level half of the diff: for each node `modified` flags, compare
/// the two sides' output-column sets and per-column provenance, classify each
/// difference, and trace the dangerous ones through column lineage to the
/// downstream columns/models/exposures they reach.
///
/// Every input comes through the two resolvers, so the column source priority
/// is exactly the app's own chain — `columns()` and `column_type()` resolve
/// catalog → declared → CLL-inferred; `column_exprs()` supplies the
/// normalized per-column SELECT expressions (`None` = "no expression view",
/// never "changed").
///
/// Classification:
/// - name absent current-side → `removed`; name new current-side → `added`;
/// - a removed/added pair sharing ≥1 direct upstream column → `renamed`
///   (the one heuristic — see below);
/// - shared/paired names with differing `data_type` → `type changed`;
/// - shared/paired names with differing normalized expression → `expression
///   changed`.
///
/// Severity: a removed/renamed column that still has baseline-referenced
/// consumers (they exist current-side and remain linked to this node, or
/// resolve to nothing because the name they read is gone) is `breaking`;
/// `type`/`expression` changes are `partial`; everything else — including
/// a removed column nothing referenced and a fully-migrated rename — is
/// `non-breaking`.
///
/// RENAME heuristic: `r -> a` pairs when `upstream_baseline(r)` ∩
/// `upstream_current(a)` is non-empty (same parent uid + column name — the
/// `o.status as order_status` pattern). Each side pairs at most once;
/// candidates match greedily in (shared-source count desc, old name, new
/// name) order, so an ambiguous fan (`r` and `a` both fan-in the same
/// sources) resolves deterministically rather than by map order. A rename
/// that also changes type or expression gets additional rows on the new
/// name.
///
/// The "still referenced" test for breaking changes is deliberately strict
/// — see [`dead_columns`]: a baseline downstream column counts as dangling
/// only when the CURRENT side proves it can't be produced (a still-written
/// reference to the dead name, or starvation through a dead intermediate).
/// A consumer rewritten to draw from a sibling/foreign column — including
/// the rename's new name — is migrated, not broken.
pub(super) fn column_changes(
    base: &Dag,
    current: &Dag,
    modified: &BTreeMap<String, Vec<String>>,
    base_res: &dyn ColumnResolver,
    cur_res: &dyn ColumnResolver,
) -> (BTreeMap<String, Vec<ColumnChange>>, BTreeSet<String>) {
    if modified.is_empty() {
        return (BTreeMap::new(), BTreeSet::new());
    }
    let base_lin = ColumnLineage::new(base, base_res);
    let cur_lin = ColumnLineage::new(current, cur_res);
    let primary = current.primary_package();
    let in_scope = |n: &NodeInfo| primary.as_deref().is_none_or(|p| in_package(n, p));

    let mut out: BTreeMap<String, Vec<ColumnChange>> = BTreeMap::new();
    let mut analyzed: BTreeSet<String> = BTreeSet::new();
    for uid in modified.keys() {
        // The same exclusion `modified_reasons` applies to the column
        // surface: source mode fills seed columns from the CSV header — a
        // documented superset, not a change.
        let kind = current
            .get(uid)
            .map(|n| n.resource_type.as_str())
            .unwrap_or("");
        if kind == "seed" {
            continue;
        }
        let before: BTreeSet<String> = base_res.columns(base, uid).into_iter().collect();
        let after: BTreeSet<String> = cur_res.columns(current, uid).into_iter().collect();
        if before.is_empty() && after.is_empty() {
            continue;
        }
        // Columns were nameable on at least one side — the node is
        // "analyzed" whether or not any row below fires.
        analyzed.insert(uid.clone());
        let removed: Vec<String> = before.difference(&after).cloned().collect();
        let added: Vec<String> = after.difference(&before).cloned().collect();
        let node_name = current
            .get(uid)
            .map(|n| crate::list_name(&n.unique_id, &n.name))
            .unwrap_or_else(|| uid.clone());

        // Direct upstream provenance per output column — the rename input.
        let base_srcs = |col: &str| -> BTreeSet<(String, String)> {
            base_lin
                .upstream(&ColumnRef::new(uid.clone(), col))
                .iter()
                .map(|e| (e.from.node.clone(), e.from.column.clone()))
                .collect()
        };
        let cur_srcs = |col: &str| -> BTreeSet<(String, String)> {
            cur_lin
                .upstream(&ColumnRef::new(uid.clone(), col))
                .iter()
                .map(|e| (e.from.node.clone(), e.from.column.clone()))
                .collect()
        };
        let pairs = rename_pairs(&removed, &added, &base_srcs, &cur_srcs);
        let renamed_old: BTreeSet<&String> = pairs.iter().map(|p| &p.0).collect();
        let renamed_new: BTreeSet<&String> = pairs.iter().map(|p| &p.1).collect();

        let base_exprs = base_res.column_exprs(base, uid);
        let cur_exprs = cur_res.column_exprs(current, uid);

        // The dangling-consumer set for a baseline column: the dead-column
        // closure `dead_columns` computes — baseline consumers of `col`
        // that are provably unproducible current-side (the reference they
        // carried is still written but now names nothing, or every value
        // source they had is itself dead). A consumer rewired to a sibling
        // or foreign column keeps a live value source and stays out —
        // migrated, not broken.
        let dangling = |col: &str| {
            let dead = dead_columns(
                &base_lin,
                &cur_lin,
                cur_res,
                current,
                modified,
                &ColumnRef::new(uid.clone(), col),
            );
            affected_lists(&dead, current, uid, &in_scope, |_| true)
        };
        // The current-side spread of a column — for `type`/`expression`/
        // `added` rows.
        let spread = |col: &str| {
            let closure = cur_lin.closure_down(&ColumnRef::new(uid.clone(), col));
            affected_lists(&closure, current, uid, &in_scope, |_| true)
        };
        let push = |column: String,
                    renamed_from: Option<String>,
                    kind: ColumnChangeKind,
                    class: ColumnChangeClass,
                    type_from: Option<String>,
                    type_to: Option<String>,
                    impact: ImpactLists,
                    rows: &mut Vec<ColumnChange>| {
            rows.push(ColumnChange {
                node: uid.clone(),
                node_name: node_name.clone(),
                column,
                renamed_from,
                kind,
                class,
                type_from,
                type_to,
                affected_columns: impact.cols,
                affected_models: impact.models,
                affected_marts: impact.marts,
                affected_exposures: impact.exposures,
            });
        };
        let mut rows: Vec<ColumnChange> = Vec::new();

        for old in &removed {
            if renamed_old.contains(old) {
                continue;
            }
            let impact = dangling(old);
            let class = if impact.cols.is_empty() {
                ColumnChangeClass::NonBreaking
            } else {
                ColumnChangeClass::Breaking
            };
            push(
                old.clone(),
                None,
                ColumnChangeKind::Removed,
                class,
                None,
                None,
                impact,
                &mut rows,
            );
        }
        for (old, new) in &pairs {
            // Consumers migrated to the new name keep a live value source
            // (`uid.new`), so `dead_columns` already leaves them out — no
            // exclusion list needed.
            let impact = dangling(old);
            // A fully-migrated rename is informational, like `added`: the
            // old name is gone but nothing in the graph still reads it
            // (the gate's Breaking ⟺ dangling-nonempty invariant relies on
            // this classification mirroring `removed`'s).
            let class = if impact.cols.is_empty() {
                ColumnChangeClass::NonBreaking
            } else {
                ColumnChangeClass::Breaking
            };
            push(
                new.clone(),
                Some(old.clone()),
                ColumnChangeKind::Renamed,
                class,
                None,
                None,
                impact,
                &mut rows,
            );
        }

        // Type + expression comparison across shared names (identity pairs)
        // and detected renames (old name -> new name).
        let mut compare: Vec<(String, String)> = before
            .intersection(&after)
            .map(|c| (c.clone(), c.clone()))
            .collect();
        compare.extend(pairs.iter().cloned());
        for (bcol, ccol) in compare {
            if let (Some(b), Some(c)) = (
                base_res.column_type(base, uid, &bcol),
                cur_res.column_type(current, uid, &ccol),
            ) {
                if !type_eq(&b, &c) {
                    let impact = spread(&ccol);
                    push(
                        ccol.clone(),
                        None,
                        ColumnChangeKind::TypeChanged,
                        ColumnChangeClass::Partial,
                        Some(b),
                        Some(c),
                        impact,
                        &mut rows,
                    );
                }
            }
            if let (Some(bm), Some(cm)) = (&base_exprs, &cur_exprs) {
                if let (Some(b), Some(c)) = (bm.get(&bcol), cm.get(&ccol)) {
                    if b != c {
                        let impact = spread(&ccol);
                        push(
                            ccol.clone(),
                            None,
                            ColumnChangeKind::ExprChanged,
                            ColumnChangeClass::Partial,
                            None,
                            None,
                            impact,
                            &mut rows,
                        );
                    }
                }
            }
        }

        for new in &added {
            if renamed_new.contains(new) {
                continue;
            }
            let impact = spread(new);
            push(
                new.clone(),
                None,
                ColumnChangeKind::Added,
                ColumnChangeClass::NonBreaking,
                None,
                None,
                impact,
                &mut rows,
            );
        }

        if !rows.is_empty() {
            rows.sort_by(|a, b| (a.column.as_str(), a.kind).cmp(&(b.column.as_str(), b.kind)));
            out.insert(uid.clone(), rows);
        }
    }
    (out, analyzed)
}

/// The four display lists a [`ColumnChange`] carries — folded out of a
/// column-closure by [`affected_lists`].
pub(super) struct ImpactLists {
    /// `"model.col"` strings of the kept closure members, sorted.
    pub(super) cols: Vec<String>,
    /// `model` nodes holding kept columns, by name, sorted.
    pub(super) models: Vec<String>,
    /// The `models` subset in the `marts` layer, by name, sorted.
    pub(super) marts: Vec<String>,
    /// Exposures downstream of the affected models + the changed node, as
    /// display strings, sorted.
    pub(super) exposures: Vec<String>,
}

/// Fold a `closure` of reached [`ColumnRef`]s into the display lists a
/// [`ColumnChange`] carries, resolved against the CURRENT dag:
/// `keep`-passing columns as `"model.col"`, the `model` nodes holding them
/// (with the `marts` subset), and the exposures downstream of those models
/// plus the changed node `home` itself (a dashboard depending on the edited
/// model directly is just as exposed to the column's move).
pub(super) fn affected_lists(
    closure: &BTreeSet<ColumnRef>,
    current: &Dag,
    home: &str,
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    mut keep: impl FnMut(&ColumnRef) -> bool,
) -> ImpactLists {
    let mut cols: BTreeSet<String> = BTreeSet::new();
    let mut model_uids: BTreeSet<String> = BTreeSet::new();
    for cr in closure {
        let Some(n) = current.get(&cr.node) else {
            continue;
        };
        if !keep(cr) {
            continue;
        }
        cols.insert(format!(
            "{}.{}",
            crate::list_name(&n.unique_id, &n.name),
            cr.column
        ));
        if n.resource_type == "model" && in_scope(n) {
            model_uids.insert(cr.node.clone());
        }
    }
    let mut roots: BTreeSet<String> = model_uids.clone();
    roots.insert(home.to_string());
    let mut exposure_uids: BTreeSet<String> = BTreeSet::new();
    for from in &roots {
        for u in current.downstream(from) {
            if current
                .get(&u)
                .is_some_and(|n| is_exposure(n) && in_scope(n))
            {
                exposure_uids.insert(u);
            }
        }
    }
    let name_of = |uid: &String| {
        current
            .get(uid)
            .map(|n| crate::list_name(&n.unique_id, &n.name))
            .unwrap_or_else(|| uid.clone())
    };
    let mut models: Vec<String> = model_uids.iter().map(&name_of).collect();
    models.sort();
    models.dedup();
    let mut marts: Vec<String> = model_uids
        .iter()
        .filter(|uid| {
            current
                .get(uid)
                .and_then(|n| first_dir(n))
                .is_some_and(|d| d == "marts")
        })
        .map(&name_of)
        .collect();
    marts.sort();
    marts.dedup();
    let mut exposures: Vec<String> = exposure_uids
        .iter()
        .map(|u| format!("{}{}", name_of(u), exposure_note(current.exposure(u))))
        .collect();
    exposures.sort();
    ImpactLists {
        cols: cols.into_iter().collect(),
        models,
        marts,
        exposures,
    }
}

/// The dead-column closure of a removed/renamed-away baseline column
/// `seed`: the baseline consumers of `seed` that provably cannot be
/// produced by the CURRENT Dag — the dangling references the `Breaking`
/// class exists to name.
///
/// The scan is bounded by the BASELINE downstream closure (only a column
/// that depended on `seed` can be orphaned by its removal) but judged on
/// the CURRENT side, so a consumer rewritten in the same PR reads as
/// migrated rather than broken. Two signals mark a column dead:
///
/// - its model still NAMES a dead column — [`ColumnResolver::unresolved_refs`]
///   reports a `(parent, col)` ref the parent's known column list doesn't
///   contain (a `select m.gone`, `join ... on m.gone = ...`, or
///   `where m.gone` left behind). The query can't compile, so EVERY column
///   the model still emits dies — a dead WHERE/JOIN ref sinks the whole
///   model, not just one projection.
/// - a current value edge into the column draws from a dead source (the
///   dead column's name still emits — e.g. a `select *` passthrough — but
///   the model that would produce it is dead). `FilterOrJoin` edges are
///   excluded from this check: they shape the row set without carrying the
///   value, so a surviving join edge alone can't keep a column alive. (A
///   dead FOJ *reference* is already caught by the unresolved check —
///   against a known column list it records a miss instead of an edge.)
/// - the column is a GHOST: the consumer's own definition is unchanged
///   (absent from `modified` — nobody rewrote it on purpose) yet nothing
///   current feeds it a value — the `select o.*` that carried it baseline
///   now expands past the dead name, leaving a declared column the model
///   silently stops producing. A consumer that DID change — `select m.id`
///   after dropping its `where m.status`, or `select 1 as a` — escapes
///   this rule entirely: a rewritten model's edgeless columns are
///   deliberate, migrated rather than broken.
///
/// Starvation propagates to a fixpoint: a dead intermediate column starves
/// the columns downstream of it in turn. Conversely a consumer whose
/// current in-edges reach only LIVE sources — a sibling column, another
/// node — is migrated and never enters the set. Deterministic: `BTree`
/// sets throughout, iteration to a bounded fixpoint over a finite domain.
pub(super) fn dead_columns(
    base_lin: &ColumnLineage<'_, dyn ColumnResolver + '_>,
    cur_lin: &ColumnLineage<'_, dyn ColumnResolver + '_>,
    cur_res: &dyn ColumnResolver,
    current: &Dag,
    modified: &BTreeMap<String, Vec<String>>,
    seed: &ColumnRef,
) -> BTreeSet<ColumnRef> {
    // Baseline consumers of the seed — the only places a stale reference
    // can live — and the nodes that hold them.
    let candidates = base_lin.closure_down(seed);
    let candidate_nodes: BTreeSet<String> = candidates.iter().map(|c| c.node.clone()).collect();

    // Per candidate node: the refs it still writes that resolve to nothing
    // (the dangling-reference signal) and the columns it still emits (the
    // resolver's list plus the `to` side of its in-edges — a
    // wildcard-carried column never declared still materializes).
    let unresolved: BTreeMap<String, BTreeSet<ColumnRef>> = candidate_nodes
        .iter()
        .map(|n| {
            (
                n.clone(),
                cur_res
                    .unresolved_refs(current, n)
                    .into_iter()
                    .map(|(u, c)| ColumnRef::new(u, &c))
                    .collect(),
            )
        })
        .collect();
    let emitted: BTreeMap<String, BTreeSet<String>> = candidate_nodes
        .iter()
        .map(|n| {
            let mut s: BTreeSet<String> = cur_res.columns(current, n).into_iter().collect();
            s.extend(cur_lin.in_edges(n).iter().map(|e| e.to.column.clone()));
            (n.clone(), s)
        })
        .collect();

    let mut dead: BTreeSet<ColumnRef> = BTreeSet::from([seed.clone()]);
    loop {
        let mut grew = false;
        for n in &candidate_nodes {
            // The model names a dead column → its query can't compile →
            // every still-emitted column dies.
            let named_dead = unresolved[n].iter().any(|r| dead.contains(r));
            // An UNCHANGED node's definitions are all deliberate: a
            // still-emitted column no current edge feeds is a ghost — the
            // wildcard that produced it shrank past its name.
            let node_unchanged = !modified.contains_key(n);
            for col in &emitted[n] {
                let cr = ColumnRef::new(n.clone(), col);
                if dead.contains(&cr) {
                    continue;
                }
                let mut fed_by_dead = false;
                let mut has_value_source = false;
                for e in cur_lin.in_edges(n).iter() {
                    if e.to.column != *col || e.kind == ColumnEdgeKind::FilterOrJoin {
                        continue;
                    }
                    has_value_source = true;
                    if dead.contains(&e.from) {
                        fed_by_dead = true;
                    }
                }
                // The ghost rule needs baseline proof the seed fed this
                // column — an always-sourceless col (`select 1 as k`) in an
                // unchanged consumer was never the seed's to kill.
                let ghost = node_unchanged && !has_value_source && candidates.contains(&cr);
                if named_dead || fed_by_dead || ghost {
                    dead.insert(cr);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    // The seed itself is gone current-side, not "affected" — drop it.
    dead.remove(seed);
    dead
}

/// Pair removed output names with added ones as `old -> new` renames: a
/// candidate needs ≥1 shared direct upstream column (`upstream_baseline(old)`
/// ∩ `upstream_current(new)` over `(parent uid, column)`), scored by the
/// shared count, matched greedily in (score desc, old, new) order with each
/// side used at most once — deterministic under any fan shape. Ties are
/// broken ALPHABETICALLY on both names: when several removed/added columns
/// share the same provenance (a `select *` fan, sibling renames off one
/// source), the higher-scored pairings win first and equal-scored
/// candidates pair in name order — so `a,c removed + b,d added` pairs
/// `a->b, c->d` rather than by manifest or SQL order. The alphabetical
/// rule is documented, not semantic: either pairing is a defensible
/// rename guess, the value is that it never varies.
pub(super) fn rename_pairs(
    removed: &[String],
    added: &[String],
    base_srcs: &dyn Fn(&str) -> BTreeSet<(String, String)>,
    cur_srcs: &dyn Fn(&str) -> BTreeSet<(String, String)>,
) -> Vec<(String, String)> {
    let mut cand: Vec<(usize, String, String)> = Vec::new();
    for old in removed {
        let so = base_srcs(old);
        if so.is_empty() {
            continue;
        }
        for new in added {
            let sn = cur_srcs(new);
            if sn.is_empty() {
                continue;
            }
            let score = so.intersection(&sn).count();
            if score > 0 {
                cand.push((score, old.clone(), new.clone()));
            }
        }
    }
    cand.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let mut used_old: BTreeSet<String> = BTreeSet::new();
    let mut used_new: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for (_, old, new) in cand {
        if used_old.insert(old.clone()) && used_new.insert(new.clone()) {
            out.push((old, new));
        }
    }
    out.sort();
    out
}

/// Warehouse-type comparison: case- and whitespace-blind (`VARCHAR(10)` and
/// `varchar( 10 )` are the same type change-wise).
pub(super) fn type_eq(a: &str, b: &str) -> bool {
    a.split_whitespace().collect::<String>().to_lowercase()
        == b.split_whitespace().collect::<String>().to_lowercase()
}

/// The change reasons for a node present in BOTH Dags, in one fixed order
/// (materialization, upstream deps, columns, tests, SQL) so the listing is
/// deterministic by construction. Empty = unchanged.
///
/// Compared surfaces are the ones a dbt change actually moves:
/// - `materialized` (the detail's, same source as the lineage box tag),
/// - the direct parent set (a `ref()`/`source()` edit),
/// - the column list as a sorted `(name, data_type)` multiset (order-blind:
///   reordering schema.yml column docs is a tidy, not a model change),
/// - the tests as a sorted `(kind, column)` multiset,
/// - the raw SQL — MODELS only, and only when BOTH sides carry code, trimmed.
///
/// On top of those per-node surfaces, [`compute_diff`] appends `macro:<name>`
/// reasons for nodes that transitively call a changed manifest macro (see
/// [`macro_change_reasons`]) — the one change raw SQL can never show.
///
/// A `--diff` pair routinely spans LOAD MODES (a manifest baseline vs a
/// source-parsed current, or vice versa), so the surfaces the manifest↔source
/// consistency gate (`tests/consistency.rs`) deliberately excludes are
/// excluded here too — otherwise every cross-mode diff would report phantom
/// changes:
/// - seed `columns` (source mode reads the CSV header, dbt records only the
///   documented ones) → the column comparison skips seeds;
/// - snapshot `raw_code` (source mode stores the whole file incl. the
///   `{% snapshot %}` wrapper, dbt stores only the block body) → the SQL
///   comparison runs for `model` nodes only (and a one-sided absence — e.g. a
///   manifest that omits `raw_code` — is a loader artifact, not a change).
pub(super) fn modified_reasons(base: &Dag, current: &Dag, uid: &str) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    let kind = current
        .get(uid)
        .map(|n| n.resource_type.as_str())
        .unwrap_or("");
    let empty = NodeDetail::default();
    let bd = base.detail(uid).unwrap_or(&empty);
    let cd = current.detail(uid).unwrap_or(&empty);

    if bd.materialized != cd.materialized {
        let show = |m: &Option<String>| m.clone().unwrap_or_else(|| "(none)".to_string());
        reasons.push(format!(
            "materialized: {} -> {}",
            show(&bd.materialized),
            show(&cd.materialized)
        ));
    }

    let parents = |dag: &Dag| -> BTreeSet<String> {
        dag.subgraph_view(uid, true, false, Some(1))
            .edges
            .into_iter()
            .map(|e| e.parent)
            .collect()
    };
    if parents(base) != parents(current) {
        reasons.push("upstream deps changed".to_string());
    }

    let cols = |d: &NodeDetail| -> Vec<(String, Option<String>)> {
        let mut v: Vec<(String, Option<String>)> = d
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.data_type.clone()))
            .collect();
        v.sort();
        v
    };
    if kind != "seed" && cols(bd) != cols(cd) {
        reasons.push("columns changed".to_string());
    }

    let tests = |dag: &Dag| -> Vec<(String, Option<String>)> {
        let mut v: Vec<(String, Option<String>)> = dag
            .tests(uid)
            .iter()
            .map(|t| (t.kind.clone(), t.column_name.clone()))
            .collect();
        v.sort();
        v
    };
    let (bt, ct) = (tests(base), tests(current));
    if bt != ct {
        // Equal counts can still differ (a test moved column): say "changed",
        // never a no-op-looking "2 -> 2".
        if bt.len() == ct.len() {
            reasons.push("tests changed".to_string());
        } else {
            reasons.push(format!("tests: {} -> {}", bt.len(), ct.len()));
        }
    }

    if kind == "model" {
        if let (Some(b), Some(c)) = (base.raw_code(uid), current.raw_code(uid)) {
            if b.trim() != c.trim() {
                reasons.push("SQL changed".to_string());
            }
        }
    }

    reasons
}

/// The shared-macro impact map: for every node present in the CURRENT Dag, the
/// sorted names of CHANGED macros the node transitively calls — the fix for
/// "an edit inside a shared macro is invisible in raw SQL".
///
/// A macro is *changed* when its `macro_sql` differs across the pair or it
/// exists on only one side (added or removed). A node is *impacted* when the
/// changed macro is reachable from the node's `depends_on.macros` through the
/// CURRENT macro→macro call graph (`depends_on.macros` on macros), so a change
/// two calls deep still lands on the model. The walk is `seen`-guarded, so a
/// macro cycle terminates instead of looping.
///
/// Empty when either Dag lacks macro data — synthesized source-mode manifests
/// carry no macros, so a manifest↔source diff reports no phantom `macro:`
/// reasons (the same cross-mode exclusion the other surfaces get).
pub(super) fn macro_change_reasons(
    base: &Dag,
    current: &Dag,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if base.macros().is_empty() && current.macros().is_empty() {
        return out;
    }

    let mut changed: BTreeSet<String> = BTreeSet::new();
    for uid in base.macros().keys().chain(current.macros().keys()) {
        let changed_here = match (base.macros().get(uid), current.macros().get(uid)) {
            (Some(b), Some(c)) => b.sql != c.sql,
            // Present on one side only: added or removed — either way the
            // compiled surface moved for anything still calling it.
            _ => true,
        };
        if changed_here {
            changed.insert(uid.clone());
        }
    }
    if changed.is_empty() {
        return out;
    }

    // A changed macro's display name: prefer the current side's record, fall
    // back to the baseline's (a removed macro still names the reason), then to
    // the uid's last segment.
    let name_of = |uid: &str| -> String {
        current
            .macros()
            .get(uid)
            .or_else(|| base.macros().get(uid))
            .map(|m| m.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| uid.rsplit('.').next().unwrap_or(uid).to_string())
    };

    for uid in current.nodes().keys() {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = current.node_macros(uid).to_vec();
        let mut hits: BTreeSet<String> = BTreeSet::new();
        while let Some(m) = queue.pop() {
            if !seen.insert(m.clone()) {
                continue;
            }
            if changed.contains(&m) {
                hits.insert(name_of(&m));
            }
            if let Some(info) = current.macros().get(&m) {
                queue.extend(info.depends_on.iter().cloned());
            }
        }
        if !hits.is_empty() {
            out.insert(uid.clone(), hits);
        }
    }
    out
}
