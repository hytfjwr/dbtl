//! The `ref()`/`source()`/`config()` call sites behind graph findings, plus
//! the shared display-name and wording helpers the rule messages use.
//!
//! [`CallSites`] runs the same extraction the source loader uses
//! ([`crate::source::call_sites`]) on a node's `raw_code` and maps each call
//! to the graph parent it produced, so a finding can point at the call that
//! created the edge instead of just naming the file. Spans index the raw
//! text — extraction masks `{# #}` comments length-preserving internally —
//! and become line/column positions through [`byte_span_position`].

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;

use crate::dag::{normalize_version, uid_package, versioned_model_segs, Dag, NodeInfo};
use crate::source::{
    arms_exclusive, call_sites_masked, call_version, config_arg, jinja_branches, kwarg, lit_value,
    mask_jinja_comments, scan_python, ArmKind, ArmStep, CallKind, CallSite, JinjaBranches,
    PyCallKind,
};

use super::{Finding, Label};

/// The name a message shows for `uid` — `source.p.raw.orders` reads
/// `raw.orders`, a versioned `model.p.dim.v2` reads `dim.v2`, everything
/// else the node name (the uid's last segment, which `NodeInfo.name`
/// agrees with).
pub fn display_name(dag: &Dag, uid: &str) -> String {
    if let Some((name, vseg)) = versioned_model_segs(uid) {
        return format!("{name}.{vseg}");
    }
    // `it.next()` x5 — no Vec: uids have ≤4 segments.
    let mut it = uid.split('.');
    match (it.next(), it.next(), it.next(), it.next(), it.next()) {
        (Some("source"), Some(_), Some(source), Some(table), None) => {
            format!("{source}.{table}")
        }
        (Some(_), Some(_), Some(name), None, None) => name.to_string(),
        _ => dag
            .get(uid)
            .map_or_else(|| uid.to_string(), |n| n.name.clone()),
    }
}

/// [`display_name`] over a uid list, in the same order.
pub fn display_names(dag: &Dag, uids: &[String]) -> Vec<String> {
    uids.iter().map(|u| display_name(dag, u)).collect()
}

/// `a, b, c` — names past the fifth collapse to `and N more`
/// (`a, b, c, d, e and 2 more`), the shared enumeration cap.
pub fn name_list(names: &[String]) -> String {
    const CAP: usize = 5;
    if names.len() <= CAP {
        names.join(", ")
    } else {
        format!("{} and {} more", names[..CAP].join(", "), names.len() - CAP)
    }
}

/// `word` for one item, the English plural for several (`pluralize(2,
/// "model")` → `models`, `pluralize(3, "prefix")` → `prefixes`) — the
/// `(s)`-free phrasing every message shares.
pub(crate) fn pluralize(n: usize, word: &str) -> String {
    if n == 1 {
        return word.to_string();
    }
    if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|e| word.ends_with(e))
    {
        format!("{word}es")
    } else {
        format!("{word}s")
    }
}

/// The `(package, name)` a model uid folds into — `model.<pkg>.<name>` and
/// `model.<pkg>.<name>.vN` are the same logical model for counting (the
/// version spellings share one `ref('name')` family). A non-model uid is
/// its own key — only models carry versions.
fn model_family_key(uid: &str) -> (&str, &str) {
    let mut it = uid.split('.');
    match (it.next(), it.next(), it.next()) {
        (Some("model"), Some(pkg), Some(name)) => (pkg, name),
        _ => (uid, ""),
    }
}

/// Group `uids` into version families (see [`model_family_key`]): one
/// entry per distinct `(package, name)`, so the `v1`/`v2`/`vbeta`
/// spellings of one model count ONCE for fanout/join thresholds.
/// Families order by first appearance in `uids` — callers pass the
/// uid-sorted neighbour list, so the output is deterministic; members
/// keep that input order.
pub(crate) fn version_families(uids: &[String]) -> Vec<Vec<&String>> {
    let mut families: Vec<Vec<&String>> = Vec::new();
    let mut index: HashMap<(&str, &str), usize> = HashMap::new();
    for uid in uids {
        let key = model_family_key(uid);
        if let Some(&i) = index.get(&key) {
            families[i].push(uid);
        } else {
            index.insert(key, families.len());
            families.push(vec![uid]);
        }
    }
    families
}

/// The name one version family carries in a message: a lone member keeps
/// its display name (`fct_orders.v1` names the one version participating);
/// several spellings share the model's bare name — the family IS the
/// model, and it was counted once.
pub(crate) fn family_name(dag: &Dag, members: &[&String]) -> String {
    if let [only] = members {
        display_name(dag, only)
    } else {
        members[0]
            .split('.')
            .nth(2)
            .unwrap_or(members[0])
            .to_string()
    }
}

/// The numeric version a `model.<pkg>.<name>.vN` uid carries — `None`
/// for unversioned models, non-model uids, and non-numeric spellings
/// (`vbeta`).
fn version_number(uid: &str) -> Option<u64> {
    let (_, vseg) = versioned_model_segs(uid)?;
    normalize_version(vseg).parse().ok()
}

/// The masked-SQL + call-site pair [`CallSiteResolver`] extracts once per
/// node and shares across rules — along with the node's file path, raw
/// text, and direct-parent uids so the per-rule view construction is a
/// struct copy, not more `Dag` lookups.
struct Sites<'a> {
    file: &'a str,
    text: &'a str,
    parents: &'a [String],
    masked: Cow<'a, str>,
    sites: Vec<CallSite>,
    /// A `.py` file's "calls `dbt.ref`/`dbt.source` at all" fact —
    /// [`CallSites::has_dependency_calls`] reads it; `sites` stays empty
    /// because call spans index SQL text, not Python.
    py_dep_calls: bool,
    /// The `{% if %}`/`{% for %}` arm structure — which `sites` can never
    /// execute in one render (`multiple_sources_joined` reads it through
    /// [`CallSites::simultaneous_parents`]).
    branches: JinjaBranches,
    /// Byte offset of each line start (index 0 = offset 0) — built on the
    /// first positioned label so `position` is a binary search + one
    /// line's char count instead of a full-text scan per label.
    lines: std::cell::OnceCell<Vec<u32>>,
    /// The literal `ref()`/`source()` calls in this node — computed on the
    /// first [`CallSites::dep_calls`] ask and shared with every later ask
    /// (`unresolved_references` and the possible-edge index hit the same
    /// node's calls, so the argument pass runs once).
    dep_calls: std::cell::OnceCell<Vec<DepCall>>,
}

/// The hasher the memo uses — every graph rule calls [`CallSiteResolver::for_node`]
/// per candidate node, so the per-lookup hash is on the hot path; a
/// multiply-xor fold over 8-byte chunks hashes a uid in a handful of
/// instructions where SipHash's rounds cost more than the lookup itself.
/// (uid strings come from the trusted manifest — no HashDoS exposure.)
#[derive(Default)]
struct UidHasher(u64);

impl std::hash::Hasher for UidHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(buf)).wrapping_mul(K);
        }
    }
}

type UidMap<V> = HashMap<String, V, std::hash::BuildHasherDefault<UidHasher>>;

/// A `K -> &[&'a str]` lookup over dag-borrowed strings, hashed by
/// [`UidHasher`] — the same fast hasher the node memo uses.
type FastMap<'a, K> = HashMap<K, Vec<&'a str>, std::hash::BuildHasherDefault<UidHasher>>;

/// The per-`run_rules` call-site cache every graph rule shares —
/// [`SqlParseResolver`](crate::SqlParseResolver)'s memo shape. Without it
/// each rule re-parses the same `raw_code`: the extraction (mask + regex
/// scan) dominates lint runtime, so `for_node` memoizes by uid — a node
/// with no positionable calls caches `None` the same way.
pub(crate) struct CallSiteResolver<'a> {
    dag: &'a Dag,
    memo: RefCell<UidMap<Option<Rc<Sites<'a>>>>>,
    /// The possible-edge index — the `target -> callers` map
    /// [`CallSiteResolver::possible_children`] serves — built on the first
    /// ask, after which the per-node [`Sites`] memo already holds every
    /// extraction it needs (the scan each graph rule shares is reused,
    /// never re-run).
    possible: std::cell::OnceCell<BTreeMap<String, BTreeSet<String>>>,
}

impl<'a> CallSiteResolver<'a> {
    pub(crate) fn new(dag: &'a Dag) -> Self {
        Self {
            dag,
            memo: RefCell::new(UidMap::default()),
            possible: std::cell::OnceCell::new(),
        }
    }

    /// The calls `uid`'s raw SQL makes — `None` for a node without raw
    /// code (sources/seeds/exposures), without a declared file, or whose
    /// file is neither SQL nor Python. A Python model's `dbt.*` calls get
    /// no position — only the dep-call fact `has_dependency_calls` reads
    /// is extracted. YAML-defined snapshots carry the `.yml` file, so
    /// their synthesized `select * from {{ ref(…) }}` never reaches here
    /// either — call spans index `raw_code`, which wouldn't name real
    /// `.yml` offsets.
    pub(crate) fn for_node(&self, uid: &str) -> Option<CallSites<'a>> {
        if let Some(entry) = self.memo.borrow().get(uid) {
            return entry.as_ref().map(|sites| self.make(sites.clone()));
        }
        let entry = self.extract(uid).map(Rc::new);
        self.memo
            .borrow_mut()
            .insert(uid.to_string(), entry.clone());
        entry.map(|sites| self.make(sites))
    }

    /// The one extraction the memo stores — the file/raw-code gates plus
    /// the masked text and the calls in it. `parents` is captured too:
    /// `is_latest`'s version-family fallback would otherwise re-read the
    /// edge set per call/parent pair.
    fn extract(&self, uid: &str) -> Option<Sites<'a>> {
        let file = self.dag.detail(uid)?.original_file_path.as_deref()?;
        let text = self.dag.raw_code(uid)?;
        if file.ends_with(".py") {
            // A `.py` model's `dbt.*` calls carry no SQL spans — only the
            // dep-call fact `has_dependency_calls` reports is extracted.
            let py_dep_calls = scan_python(text)
                .calls
                .iter()
                .any(|c| matches!(c.kind, PyCallKind::Ref | PyCallKind::Source));
            return Some(Sites {
                file,
                text,
                parents: self.dag.direct_parents(uid),
                masked: Cow::Borrowed(text),
                sites: Vec::new(),
                py_dep_calls,
                branches: JinjaBranches::default(),
                lines: std::cell::OnceCell::new(),
                dep_calls: std::cell::OnceCell::new(),
            });
        }
        if !file.ends_with(".sql") {
            return None;
        }
        // One mask pass feeds both the call scan and the arg parsing —
        // free when the file carries no `{#` comments.
        let masked = mask_jinja_comments(text);
        let sites = call_sites_masked(&masked);
        let branches = jinja_branches(&masked);
        Some(Sites {
            file,
            text,
            parents: self.dag.direct_parents(uid), // &[String] — no clone
            masked,
            sites,
            py_dep_calls: false,
            branches,
            lines: std::cell::OnceCell::new(),
            dep_calls: std::cell::OnceCell::new(),
        })
    }

    /// The possible consumers of `uid` — graph members whose raw SQL
    /// carries a literal `ref()`/`source()` call resolving to `uid`
    /// **without** the matching recorded edge. The index is a lint-only
    /// view: it never mutates `direct_children`/`downstream`, so lineage,
    /// `ls`, and docs stay dbt-faithful. Rules that check "could anything
    /// read this" read the set the way they read `direct_children` — a
    /// possible child means under-reporting is the safe direction only.
    /// On a source-mode `Dag` the set is small but real: the loader drops
    /// calls in provably-dead arms and deferred definition bodies (as
    /// dbt's extractor does) and counts them as edges nowhere — lint
    /// still weighs them as possible reads exactly like manifest mode.
    ///
    /// Resolution shares [`CallSites::targets`]' rules through
    /// [`version_pin_matches`]/[`family_latest`] — the pin-vs-segment
    /// comparison and the bare-ref latest pick the edge match makes —
    /// and adds the package disambiguation `targets` never needs (its
    /// calls only match recorded parents): an explicit `package` arg
    /// scopes the family to that package outright, while a bare name
    /// prefers members in the caller's own package and falls back to
    /// foreign members only when exactly one package owns the name —
    /// dbt's same-project-first name lookup. A literal `v=`/`version=`
    /// pin must hit a member's `.vN` segment verbatim (`v='v3'` names
    /// `.vv3`, never `.v3`); a pin that isn't literal resolves the way
    /// a bare ref does — the family's latest. A missing name or a hit
    /// the `depends_on` list already carries resolves to no possible
    /// edge.
    pub(crate) fn possible_children(&self, uid: &str) -> &BTreeSet<String> {
        static EMPTY: BTreeSet<String> = BTreeSet::new();
        self.possible
            .get_or_init(|| self.build_possible())
            .get(uid)
            .unwrap_or(&EMPTY)
    }

    /// Build the `target -> callers` possible-edge index — one pass over
    /// the kept graph's call sites, resolving each literal call to a
    /// member uid through [`resolve_call`]. A call whose resolution
    /// matches an already-recorded parent contributes nothing (the real
    /// edge answers for it).
    fn build_possible(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut refs: FastMap<'a, &'a str> = FastMap::default();
        let mut sources: FastMap<'a, (&'a str, &'a str)> = FastMap::default();
        for (uid, n) in self.dag.nodes() {
            match n.resource_type.as_str() {
                "model" | "seed" | "snapshot" => {
                    refs.entry(n.name.as_str()).or_default().push(uid.as_str())
                }
                "source" => {
                    // `source.<pkg>.<src>.<tbl>[.<gen>]` — the call's two
                    // args name the trailing segments.
                    let mut segs = uid.split('.');
                    if let (Some(s), Some(t)) = (segs.nth(2), segs.next()) {
                        sources.entry((s, t)).or_default().push(uid.as_str());
                    }
                }
                _ => {}
            }
        }
        // `dag.nodes()` iterates in HashMap order — sort the member vecs
        // so pool picks (first unversioned, declared-latest, single
        // foreign) never depend on the map's layout.
        for members in refs.values_mut().chain(sources.values_mut()) {
            members.sort_unstable();
        }
        let mut callers: Vec<&String> = self.dag.nodes().keys().collect();
        callers.sort();
        for caller in callers {
            let Some(cs) = self.for_node(caller) else {
                continue;
            };
            for call in cs.dep_calls() {
                let Some(target) = resolve_call(self.dag, &refs, &sources, caller, call) else {
                    continue;
                };
                if target == caller.as_str()
                    || self
                        .dag
                        .direct_parents(caller)
                        .iter()
                        .any(|p| p.as_str() == target)
                {
                    continue;
                }
                map.entry(target.to_string())
                    .or_default()
                    .insert(caller.clone());
            }
        }
        map
    }

    /// Could `uid` have a CHILD the recorded graph can't show — a package
    /// child the `PackageScope::Root` prune severed
    /// ([`Dag::pruned_adjacent`]) or a kept node whose raw code calls
    /// `uid` in a branch dbt never rendered ([`possible_children`])? The
    /// ONE "is anything possibly reading this" question connectedness
    /// rules ask: `direct_children` alone proves nothing recorded reads
    /// the node, never that nothing reads it. `kind` filters the
    /// possible callers the way the rule filters `direct_children` —
    /// severed package children can't be type-checked, so their
    /// presence always answers `true`; pass `|_| true` for "any
    /// consumer".
    pub(crate) fn unrecorded_children(&self, uid: &str, kind: impl Fn(&NodeInfo) -> bool) -> bool {
        self.dag
            .pruned_adjacent(uid)
            .is_some_and(|a| a.children > 0)
            || self
                .possible_children(uid)
                .iter()
                .any(|c| self.dag.get(c).is_some_and(&kind))
    }

    /// Could `uid` have a PARENT the recorded graph can't show — a
    /// package parent the scope prune severed
    /// ([`Dag::pruned_adjacent`]), or a `ref()`/`source()` call in the
    /// node's own `raw_code` ([`CallSites::has_dependency_calls`]) that
    /// produced no edge? `root_models`'s under-connection guard: a node
    /// making dep calls at all is under-connected, not a root —
    /// conservative on a node whose calls DID land (every recorded
    /// parent still reads "more parents possible"), so ask it where the
    /// recorded-parent check already ran.
    pub(crate) fn unrecorded_parents(&self, uid: &str) -> bool {
        self.dag.pruned_adjacent(uid).is_some_and(|a| a.parents > 0)
            || self
                .for_node(uid)
                .is_some_and(|cs| cs.has_dependency_calls())
    }

    fn make(&self, sites: Rc<Sites<'a>>) -> CallSites<'a> {
        CallSites {
            dag: self.dag,
            sites,
        }
    }
}

/// The member pool a `ref()`/`source()` call picks from — the caller's own
/// package first (dbt's name lookup resolves same-project names before
/// cross-project ones), else the foreign members when the family lives
/// under exactly one package. A name owned by multiple foreign packages
/// can't pick a render deterministically — the pool comes back empty and
/// the call resolves to nothing.
fn caller_pool<'m>(members: &[&'m str], caller_pkg: &str) -> Vec<&'m str> {
    let home: Vec<&str> = members
        .iter()
        .copied()
        .filter(|u| uid_package(u) == Some(caller_pkg))
        .collect();
    if !home.is_empty() {
        return home;
    }
    let foreign: Vec<&str> = members
        .iter()
        .copied()
        .filter(|u| uid_package(u).is_some_and(|p| p != caller_pkg))
        .collect();
    let single_pkg = foreign
        .first()
        .and_then(|u| uid_package(u))
        .is_some_and(|p| foreign.iter().all(|u| uid_package(u) == Some(p)));
    if single_pkg {
        foreign
    } else {
        Vec::new()
    }
}

/// Does the literal `v=`/`version=` pin `pin` — rendered the way dbt
/// spells the version — name the member whose uid segment is `vseg`?
/// The pin compares VERBATIM against the normalized segment: `v='v3'`
/// names `.vv3` (the `version: 'v3'` member), never `.v3`; `v=1.10`
/// names `.v1.1`; `v='02'` names nothing. `normalize_version` strips
/// exactly one `v`, so `pin == normalize(vseg)` is the `v{pin} == vseg`
/// check dbt's lookup makes — the ONE comparison [`CallSites::targets`]
/// and the possible-edge resolver share.
fn version_pin_matches(pin: &str, vseg: &str) -> bool {
    normalize_version(vseg) == pin
}

/// The "latest" member of an all-versioned family among `candidates` —
/// a member whose own `version` equals its declared `latest_version`
/// wins (dbt's own latest), else the highest `.vN` segment. `None` when
/// no member's version can be read. The ONE latest pick
/// [`CallSites::is_latest`] and the possible-edge resolver share —
/// `is_latest` asks whether one recorded parent is it, the index asks
/// which member it lands on.
fn family_latest<'m>(dag: &Dag, candidates: &[&'m str]) -> Option<&'m str> {
    if let Some(u) = candidates.iter().find(|u| {
        dag.detail(u).is_some_and(|d| {
            matches!(
                (d.version.as_deref(), d.latest_version.as_deref()),
                (Some(v), Some(l)) if normalize_version(v) == normalize_version(l)
            )
        })
    }) {
        return Some(u);
    }
    candidates
        .iter()
        .filter_map(|u| version_number(u).map(|n| (n, *u)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, u)| u)
}

/// Resolve one literal [`DepCall`] to the member uid it names — the
/// possible-edge counterpart of [`CallSites::targets`]'s match, sharing
/// its pin/latest rules through [`version_pin_matches`]/
/// [`family_latest`] and layering the package scoping on top (a
/// possible edge has no recorded parent to anchor the guess). `None`
/// when the call can't name a target: a name no member owns, a literal
/// `v=` pin hitting no version segment, or a name the member family
/// can't disambiguate.
fn resolve_call<'m>(
    dag: &Dag,
    refs: &HashMap<&'m str, Vec<&'m str>, std::hash::BuildHasherDefault<UidHasher>>,
    sources: &HashMap<(&'m str, &'m str), Vec<&'m str>, std::hash::BuildHasherDefault<UidHasher>>,
    caller_uid: &str,
    call: &DepCall,
) -> Option<&'m str> {
    let caller_pkg = uid_package(caller_uid).unwrap_or("");
    match &call.kind {
        DepCallKind::Ref {
            name,
            package,
            versioned: _,
            version,
        } => {
            let members = refs.get(name.as_str())?;
            let pool: Vec<&'m str> = match package.as_deref() {
                // An explicit package arg scopes the family outright
                // (dbt cross-project semantics) — a miss resolves nothing.
                Some(p) => members
                    .iter()
                    .copied()
                    .filter(|u| uid_package(u) == Some(p))
                    .collect(),
                None => caller_pool(members, caller_pkg),
            };
            if let Some(pin) = version.as_deref() {
                // A literal pin names ONE member — no-hit and multi-hit
                // (a duplicated version segment across members) both
                // refuse rather than guess.
                let mut hits = pool.iter().filter(|u| {
                    versioned_model_segs(u).is_some_and(|(_, vs)| version_pin_matches(pin, vs))
                });
                return match (hits.next(), hits.next()) {
                    (Some(u), None) => Some(u),
                    _ => None,
                };
            }
            // No literal pin — the `targets` `None` arm: a kwarg that
            // isn't a literal rides the bare-ref resolution, an
            // unversioned member takes it outright, an all-versioned
            // family hands it to the latest.
            if let Some(u) = pool.iter().find(|u| versioned_model_segs(u).is_none()) {
                return Some(u);
            }
            family_latest(dag, &pool)
        }
        DepCallKind::Source { source, table } => {
            let members = sources.get(&(source.as_str(), table.as_str()))?;
            match caller_pool(members, caller_pkg).as_slice() {
                [u] => Some(u),
                _ => None,
            }
        }
    }
}

/// The `ref()`/`source()`/`config()` calls in one node's `raw_code`,
/// resolvable against its graph parents. Spans index the raw text;
/// `file` is the node's `original_file_path` — the file the position
/// names.
pub(crate) struct CallSites<'a> {
    dag: &'a Dag,
    /// The extracted calls + the comment-masked text argument parsing
    /// sees — shared with every other rule via the resolver.
    sites: Rc<Sites<'a>>,
}

impl<'a> CallSites<'a> {
    /// The node's `original_file_path` — the file positions name.
    pub(crate) fn file(&self) -> &'a str {
        self.sites.file
    }

    /// The `(line, column, end_line, end_column)` of `span` in the raw
    /// text — the same shape [`byte_span_position`](super::byte_span_position)
    /// computes, resolved against the per-node line index (binary search +
    /// a single line's char count) rather than a full-text walk per label.
    fn position(&self, span: &Range<usize>) -> Option<(u32, u32, u32, u32)> {
        let text = self.sites.text;
        if span.end < span.start
            || span.end > text.len()
            || !text.is_char_boundary(span.start)
            || !text.is_char_boundary(span.end)
        {
            return None;
        }
        let (line, column) = self.offset_position(span.start);
        let (end_line, end_column) = self.offset_position(span.end);
        Some((line, column, end_line, end_column))
    }

    /// The 1-based `(line, column)` of `offset` — a char column like
    /// [`byte_span_position`](super::byte_span_position), so `\r`, tabs,
    /// and multibyte text count the same way.
    fn offset_position(&self, offset: usize) -> (u32, u32) {
        let text = self.sites.text;
        let lines = self.sites.lines.get_or_init(|| {
            let mut v = vec![0u32];
            v.extend(text.match_indices('\n').map(|(i, _)| i as u32 + 1));
            v
        });
        let idx = lines.partition_point(|&o| o as usize <= offset) - 1;
        let column = text[lines[idx] as usize..offset].chars().count() as u32 + 1;
        (idx as u32 + 1, column)
    }

    /// Whether the node's raw code calls `ref()`/`source()` (`dbt.ref`/
    /// `dbt.source` in `.py`) at all —
    /// regardless of whether any call produced a graph edge. `root_models`
    /// uses it to tell a genuinely parentless model from one whose only
    /// refs failed to resolve (source mode never parses installed
    /// packages, so a `ref('pkg_model')` drops without an edge — the model
    /// is not a root, it is under-connected).
    pub(crate) fn has_dependency_calls(&self) -> bool {
        self.sites.py_dep_calls
            || self
                .sites
                .sites
                .iter()
                .any(|c| matches!(c.kind, CallKind::Ref { .. } | CallKind::Source { .. }))
    }

    /// Every literal `ref()`/`source()` call in the node's raw SQL — the
    /// `unresolved_references` rule's judgment input and the possible-edge
    /// index's call stream, in text order. Argument names resolve to owned
    /// strings (a call the extractor couldn't read a literal name for —
    /// `ref(var('x'))` — carries no `CallKind` name and never appears
    /// here); `in_if_arm` marks a call sitting inside an `{% if %}`/
    /// `{% elif %}`/`{% else %}` arm — the rule skips those (a
    /// Jinja-switched reference may legitimately not exist in the taken
    /// arm). `{% for %}` membership does NOT mark a call: a loop body's
    /// literal ref still runs every render.
    pub(crate) fn dep_calls(&self) -> &[DepCall] {
        self.sites
            .dep_calls
            .get_or_init(|| extract_dep_calls(&self.sites))
            .as_slice()
    }

    /// Every call span resolving to the parent `uid`, sorted by position —
    /// a duplicated `ref()`/`source()` reports every site.
    pub(crate) fn calls_to(&self, parent: &str) -> Vec<Range<usize>> {
        let segs: Vec<&str> = parent.split('.').collect();
        let mut spans: Vec<Range<usize>> = self
            .sites
            .sites
            .iter()
            .filter(|c| self.targets(c, parent, &segs))
            .map(|c| c.span.clone())
            .collect();
        spans.sort_by_key(|r| r.start);
        spans
    }

    /// The fewest parents from `parents` that must execute in ONE render —
    /// `multiple_sources_joined`'s count. Calls landing in mutually
    /// exclusive `{% if %}`/`{% elif %}`/`{% else %}`/`{% for %}` arms of
    /// the same chain never run together, so parents reachable only
    /// through pairwise-exclusive arms count once (a parent with no
    /// resolvable call site can't be arm-attributed — it counts as
    /// unconditional, the under-reporting-safe side). `parents` should
    /// already be deduped.
    pub(crate) fn simultaneous_parents(&self, parents: &[String]) -> usize {
        // Each group is a set of parents whose every arm path pair is
        // mutually exclusive — the merge is greedy on the (uid-sorted)
        // input, deterministic either way.
        let mut groups: Vec<Vec<Vec<ArmStep>>> = Vec::new();
        for parent in parents {
            let mut sigs: Vec<Vec<ArmStep>> = self
                .calls_to(parent)
                .iter()
                .map(|span| self.sites.branches.arm_path(span.start).to_vec())
                .collect();
            if sigs.is_empty() {
                sigs.push(Vec::new()); // unattributed → unconditional
            }
            match groups.iter_mut().find(|g| {
                g.iter()
                    .all(|member| sigs.iter().all(|b| arms_exclusive(member, b)))
            }) {
                Some(g) => g.extend(sigs),
                None => groups.push(sigs),
            }
        }
        groups.len()
    }

    /// Does `call` resolve to the `parent` uid (`segs` = `parent`'s `.`
    /// split, precomputed once per [`calls_to`](CallSites::calls_to) so
    /// per-site checks don't re-split)? Mirrors what the edges already
    /// encode: `source('s','t')` hits `source.*.s.t`; `ref()` hits
    /// `model|seed|snapshot.*.<name>` by NAME alone — the loader's
    /// name-index resolution ignores the package arg (`ref('pkg','x')`
    /// lands on the local `x`), so a call naming any package still marks
    /// the edge it produced. A versioned ref wants exactly `.v<pin>`
    /// (`version='02'` is `.v02`, never `.v2` — the rendered arg is
    /// verbatim) and never an unversioned same-named uid; a bare
    /// `ref('x')` lands on an unversioned uid or the family's latest
    /// version.
    fn targets(&self, call: &CallSite, parent: &str, segs: &[&str]) -> bool {
        match &call.kind {
            CallKind::Source { source, table } => {
                matches!(segs, ["source", _, s, t]
                    if self.sites.masked[source.clone()] == **s
                        && self.sites.masked[table.clone()] == **t)
            }
            CallKind::Ref { name, .. } => {
                let [kind, _, uname, ..] = segs else {
                    return false;
                };
                if !matches!(*kind, "model" | "seed" | "snapshot")
                    || self.sites.masked[name.clone()] != **uname
                {
                    return false;
                }
                let uid_vseg = segs.get(3);
                match call
                    .args
                    .as_ref()
                    .and_then(|a| call_version(&self.sites.masked[a.clone()]))
                {
                    Some(pin) => {
                        let Some(vseg) = uid_vseg else { return false };
                        version_pin_matches(&pin, vseg)
                    }
                    // No literal pin: a kwarg that isn't a literal rides
                    // the dynamic-pin resolution (the family's latest),
                    // the bare-ref case.
                    None => uid_vseg.is_none() || self.is_latest(parent, segs),
                }
            }
            CallKind::Config => false,
        }
    }

    /// `parent` is the newest version of its name among this node's
    /// parents — where a bare `ref('x')` lands (`detail.latest_version`
    /// when the manifest carries it, else the highest `.vN` suffix among
    /// the same kind/package/name parents — the pick [`family_latest`]
    /// makes over the same-kind family).
    fn is_latest(&self, parent: &str, segs: &[&str]) -> bool {
        if let Some((version, latest)) = self
            .dag
            .detail(parent)
            .and_then(|d| d.version.as_ref().zip(d.latest_version.as_ref()))
        {
            return normalize_version(version) == normalize_version(latest);
        }
        let [kind, pkg, name, ..] = segs else {
            return false;
        };
        let candidates: Vec<&str> = self
            .sites
            .parents
            .iter()
            .filter_map(|p| {
                // `k.pk.n.vN` — compare kind/pkg/name without collecting.
                let mut it = p.split('.');
                match (it.next(), it.next(), it.next()) {
                    (Some(k), Some(pk), Some(n)) if k == *kind && pk == *pkg && n == *name => {
                        Some(p.as_str())
                    }
                    _ => None,
                }
            })
            .collect();
        family_latest(self.dag, &candidates) == Some(parent)
    }

    /// The byte range of the LAST resolvable `materialized=` literal across
    /// the node's `config()` calls, with its value — the argument that
    /// declared the materialization (the same last-wins scan the loader's
    /// `materialized` extractor runs, on the same comment-masked text it
    /// sees; the mask preserves byte length, so the offsets index `text`).
    pub(crate) fn materialized_arg(&self) -> Option<(Range<usize>, String)> {
        let mut last = None;
        for site in &self.sites.sites {
            let (CallKind::Config, Some(args)) = (&site.kind, &site.args) else {
                continue;
            };
            let Some(expr) = config_arg(&self.sites.masked[args.clone()], "materialized") else {
                continue;
            };
            let expr = expr.trim();
            let Some(serde_json::Value::String(s)) = lit_value(expr) else {
                continue;
            };
            // `expr` is a subslice of `self.sites.masked` — the offset is
            // the pointer delta (equal length ⇒ valid in `text` too).
            let start = expr.as_ptr() as usize - self.sites.masked.as_ptr() as usize;
            last = Some((start..start + expr.len(), s));
        }
        last
    }

    /// Attach `primary` + `secondary` labeled spans to `f`: the primary
    /// becomes the finding's location + label, the rest its `labels`
    /// (position-sorted; a span duplicating the primary — or an earlier
    /// secondary, as one call can only have produced one edge — is
    /// dropped). A span that can't map to a valid position is skipped;
    /// when the primary itself fails, nothing is attached — a finding
    /// with labels but no location loses them in the envelope anyway.
    pub(crate) fn annotate(
        &self,
        f: Finding,
        primary: (Range<usize>, impl Into<String>),
        secondary: Vec<(Range<usize>, String)>,
    ) -> Finding {
        let Some((line, column, end_line, end_column)) = self.position(&primary.0) else {
            return f;
        };
        // The span list is tiny (≤ a few calls per edge) — a linear dedup
        // beats a HashSet alloc per finding.
        let mut seen: Vec<(usize, usize)> = vec![(primary.0.start, primary.0.end)];
        let labels: Vec<Label> = secondary
            .into_iter()
            .filter(|(span, _)| {
                let key = (span.start, span.end);
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            })
            .filter_map(|(span, message)| {
                self.position(&span)
                    .map(|(line, column, end_line, end_column)| Label {
                        line,
                        column: Some(column),
                        end_line: Some(end_line),
                        end_column: Some(end_column),
                        message,
                    })
            })
            .collect();
        f.with_span(self.file(), line, column, end_line, end_column)
            .with_label(primary.1.into())
            .with_labels(labels)
    }
}

/// One literal `ref()`/`source()` call [`CallSites::dep_calls`] yields —
/// the `unresolved_references` rule's judgment unit.
pub(crate) struct DepCall {
    /// The call's span in the raw text (`{{ }}`/`{% %}` excluded) — the
    /// finding's label target.
    pub span: Range<usize>,
    /// The call sits inside an `{% if %}`/`{% elif %}`/`{% else %}` arm —
    /// the rule never judges it (a Jinja-switched reference may not
    /// exist in the taken arm; mutual exclusivity is handled the same
    /// way — every arm call is skipped, not just provably-exclusive
    /// pairs, since an `if` arm by itself already gates the call).
    /// `{% for %}` membership does not mark a call.
    pub in_if_arm: bool,
    pub kind: DepCallKind,
}

/// What a [`DepCall`] names.
pub(crate) enum DepCallKind {
    Ref {
        /// `ref('x')` → `x`; `ref('pkg','x')` → `x`.
        name: String,
        /// `ref('pkg','x')` → `pkg`.
        package: Option<String>,
        /// A `v=`/`version=` kwarg is present at all — judged against
        /// the `RefScope::versions` table both load modes record.
        versioned: bool,
        /// The version kwarg's literal value RENDERED — `v=2`/`v='2'` →
        /// `"2"`, `v='v2'` → `"v2"`, `v=1.10` → `"1.1"`. `None` with
        /// `versioned` means the kwarg didn't read literal
        /// (`v=var('ver')`) — `unresolved_references` can't judge the
        /// pin, while edge resolution treats it the way `targets` does:
        /// the bare-ref case, landing on the family's latest.
        version: Option<String>,
    },
    Source {
        source: String,
        table: String,
    },
}

/// The [`DepCall`]s inside one node's [`Sites`] — the body
/// [`CallSites::dep_calls`] memoizes. A call the extractor couldn't read a
/// literal name for (`ref(var('x'))`) carries no `CallKind` name and never
/// appears.
fn extract_dep_calls(sites: &Sites) -> Vec<DepCall> {
    let masked: &str = &sites.masked;
    let mut out = Vec::new();
    for site in &sites.sites {
        let kind = match &site.kind {
            CallKind::Ref {
                name,
                package,
                version: _,
            } => {
                // A `v=`/`version=` kwarg judges by its literal rendered
                // the way dbt spells the version (a number's `str()` —
                // `1.10` → `"1.1"` — a string verbatim): `v='v3'` names
                // `.vv3`, never `.v3`. A kwarg that isn't a literal is the
                // nonliteral `v=var('x')` case — `versioned` true,
                // `version` none — which a consumer must skip rather than
                // judge as unversioned.
                let args = site.args.as_ref().map(|a| &masked[a.clone()]);
                DepCallKind::Ref {
                    name: masked[name.clone()].to_string(),
                    package: package.as_ref().map(|p| masked[p.clone()].to_string()),
                    versioned: args
                        .is_some_and(|a| kwarg(a, "v").is_some() || kwarg(a, "version").is_some()),
                    version: args.and_then(call_version),
                }
            }
            CallKind::Source { source, table } => DepCallKind::Source {
                source: masked[source.clone()].to_string(),
                table: masked[table.clone()].to_string(),
            },
            CallKind::Config => continue,
        };
        out.push(DepCall {
            span: site.span.clone(),
            in_if_arm: sites
                .branches
                .arm_path(site.span.start)
                .iter()
                .any(|step| step.kind == ArmKind::If),
            kind,
        });
    }
    out
}
