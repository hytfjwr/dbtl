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
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use crate::dag::Dag;
use crate::source::{call_sites_masked, kwarg, lit_value, mask_jinja_comments, CallKind, CallSite};

use super::{Finding, Label};

/// The name a message shows for `uid` — `source.p.raw.orders` reads
/// `raw.orders`, a versioned `model.p.dim.v2` reads `dim.v2`, everything
/// else the node name (the uid's last segment, which `NodeInfo.name`
/// agrees with).
pub(crate) fn display_name(dag: &Dag, uid: &str) -> String {
    match uid.split('.').collect::<Vec<_>>().as_slice() {
        ["source", _, source, table] => format!("{source}.{table}"),
        ["model", _, name, version] => format!("{name}.{version}"),
        [_, _, name] => (*name).to_string(),
        _ => dag
            .get(uid)
            .map_or_else(|| uid.to_string(), |n| n.name.clone()),
    }
}

/// [`display_name`] over a uid list, in the same order.
pub(crate) fn display_names(dag: &Dag, uids: &[String]) -> Vec<String> {
    uids.iter().map(|u| display_name(dag, u)).collect()
}

/// `a, b, c` — names past the fifth collapse to `and N more`
/// (`a, b, c, d, e and 2 more`), the shared enumeration cap.
pub(crate) fn name_list(names: &[String]) -> String {
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

/// A version string compared prefix-free — `v2` and `2` are the same
/// version (`ref('x', v='v2')` and `ref('x', v=2)` both land on `.v2`).
fn normalize_version(s: &str) -> &str {
    s.trim_start_matches(['v', 'V'])
}

/// The numeric version a `model.<pkg>.<name>.vN` uid carries.
fn version_number(uid: &str) -> Option<u64> {
    normalize_version(uid.split('.').nth(3)?).parse().ok()
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
    /// Byte offset of each line start (index 0 = offset 0) — built on the
    /// first positioned label so `position` is a binary search + one
    /// line's char count instead of a full-text scan per label.
    lines: std::cell::OnceCell<Vec<u32>>,
}

/// The per-`run_rules` call-site cache every graph rule shares —
/// [`SqlParseResolver`](crate::SqlParseResolver)'s memo shape. Without it
/// each rule re-parses the same `raw_code`: the extraction (mask + regex
/// scan) dominates lint runtime, so `for_node` memoizes by uid — a node
/// with no positionable calls caches `None` the same way.
pub(crate) struct CallSiteResolver<'a> {
    dag: &'a Dag,
    memo: RefCell<HashMap<String, Option<Rc<Sites<'a>>>>>,
}

impl<'a> CallSiteResolver<'a> {
    pub(crate) fn new(dag: &'a Dag) -> Self {
        Self {
            dag,
            memo: RefCell::new(HashMap::new()),
        }
    }

    /// The calls `uid`'s raw SQL makes — `None` for a node without raw
    /// code (sources/seeds/exposures), without a declared file, or whose
    /// file isn't SQL (`dbt.ref()` in a Python model gets no position).
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
        if !file.ends_with(".sql") {
            return None;
        }
        let text = self.dag.raw_code(uid)?;
        // One mask pass feeds both the call scan and the arg parsing —
        // free when the file carries no `{#` comments.
        let masked = mask_jinja_comments(text);
        let sites = call_sites_masked(&masked);
        Some(Sites {
            file,
            text,
            parents: self.dag.direct_parents(uid), // &[String] — no clone
            masked,
            sites,
            lines: std::cell::OnceCell::new(),
        })
    }

    fn make(&self, sites: Rc<Sites<'a>>) -> CallSites<'a> {
        CallSites {
            dag: self.dag,
            sites,
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

    /// Does `call` resolve to the `parent` uid (`segs` = `parent`'s `.`
    /// split, precomputed once per [`calls_to`](CallSites::calls_to) so
    /// per-site checks don't re-split)? Mirrors what the edges already
    /// encode: `source('s','t')` hits `source.*.s.t`; `ref()` hits
    /// `model|seed|snapshot.*.<name>` by NAME alone — the loader's
    /// name-index resolution ignores the package arg (`ref('pkg','x')`
    /// lands on the local `x`), so a call naming any package still marks
    /// the edge it produced. A versioned ref wants that `.vN` — or any
    /// same-named unversioned uid, the name-only approximation the source
    /// loader makes — and a bare `ref('x')` lands on an unversioned uid or
    /// the family's latest version.
    fn targets(&self, call: &CallSite, parent: &str, segs: &[&str]) -> bool {
        match &call.kind {
            CallKind::Source { source, table } => {
                matches!(segs, ["source", _, s, t]
                    if self.sites.masked[source.clone()] == **s
                        && self.sites.masked[table.clone()] == **t)
            }
            CallKind::Ref { name, version, .. } => {
                let [kind, _, uname, ..] = segs else {
                    return false;
                };
                if !matches!(*kind, "model" | "seed" | "snapshot")
                    || self.sites.masked[name.clone()] != **uname
                {
                    return false;
                }
                let uid_version = segs.get(3).map(|v| normalize_version(v));
                match version {
                    Some(v) => {
                        let Some(uv) = uid_version else { return true };
                        // Compare as numbers first (`v=02` and `.v2` are
                        // the same version), then as normalized text
                        // (non-numeric spellings like `v='x'`).
                        let v = normalize_version(&self.sites.masked[v.clone()]);
                        match (v.parse::<u64>(), uv.parse::<u64>()) {
                            (Ok(a), Ok(b)) => a == b,
                            _ => v == uv,
                        }
                    }
                    None => uid_version.is_none() || self.is_latest(parent, segs),
                }
            }
            CallKind::Config => false,
        }
    }

    /// `parent` is the newest version of its name among this node's
    /// parents — where a bare `ref('x')` lands (`detail.latest_version`
    /// when the manifest carries it, else the highest `.vN` suffix among
    /// the same kind/package/name parents).
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
        let Some(max) = self
            .sites
            .parents
            .iter()
            .filter_map(|p| match p.split('.').collect::<Vec<_>>().as_slice() {
                [k, pk, n, ..] if k == kind && pk == pkg && n == name => version_number(p),
                _ => None,
            })
            .max()
        else {
            return false;
        };
        version_number(parent) == Some(max)
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
            let Some(expr) = kwarg(&self.sites.masked[args.clone()], "materialized") else {
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
        let mut seen = std::collections::HashSet::from([(primary.0.start, primary.0.end)]);
        let labels: Vec<Label> = secondary
            .into_iter()
            .filter(|(span, _)| seen.insert((span.start, span.end)))
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
