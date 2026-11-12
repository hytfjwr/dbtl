//! The `ref()`/`source()`/`config()` call sites behind graph findings, plus
//! the shared display-name and wording helpers the rule messages use.
//!
//! [`CallSites`] runs the same extraction the source loader uses
//! ([`crate::source::call_sites`]) on a node's `raw_code` and maps each call
//! to the graph parent it produced, so a finding can point at the call that
//! created the edge instead of just naming the file. Spans index the raw
//! text — extraction masks `{# #}` comments length-preserving internally —
//! and become line/column positions through [`byte_span_position`].

use std::ops::Range;

use crate::dag::Dag;
use crate::source::{call_sites, kwarg, lit_value, mask_jinja_comments, CallKind, CallSite};

use super::{byte_span_position, Finding, Label};

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

/// The `ref()`/`source()`/`config()` calls in one node's `raw_code`,
/// resolvable against its graph parents. Spans index the raw text;
/// `file` is the node's `original_file_path` — the file the position
/// names.
pub(crate) struct CallSites<'a> {
    dag: &'a Dag,
    pub(crate) file: &'a str,
    /// The node's raw SQL — positions index it.
    text: &'a str,
    /// `text` with `{# #}` comments masked to spaces (same byte length) —
    /// the text argument parsing sees, matching the loader's extractors.
    masked: String,
    uid: &'a str,
    sites: Vec<CallSite>,
}

impl<'a> CallSites<'a> {
    /// The calls `uid`'s raw SQL makes — `None` for a node without raw
    /// code (sources/seeds/exposures), without a declared file, or whose
    /// file isn't SQL (`dbt.ref()` in a Python model gets no position).
    pub(crate) fn for_node(dag: &'a Dag, uid: &'a str) -> Option<Self> {
        let file = dag.detail(uid)?.original_file_path.as_deref()?;
        if !file.ends_with(".sql") {
            return None;
        }
        let text = dag.raw_code(uid)?;
        Some(Self {
            dag,
            file,
            text,
            masked: mask_jinja_comments(text),
            uid,
            sites: call_sites(text),
        })
    }

    /// The `(line, column, end_line, end_column)` of `span` in the raw text.
    fn position(&self, span: &Range<usize>) -> Option<(u32, u32, u32, u32)> {
        byte_span_position(self.text, span.start, span.end)
    }

    /// Every call span resolving to the parent `uid`, sorted by position —
    /// a duplicated `ref()`/`source()` reports every site.
    pub(crate) fn calls_to(&self, parent: &str) -> Vec<Range<usize>> {
        let mut spans: Vec<Range<usize>> = self
            .sites
            .iter()
            .filter(|c| self.targets(c, parent))
            .map(|c| c.span.clone())
            .collect();
        spans.sort_by_key(|r| r.start);
        spans
    }

    /// Does `call` resolve to the `parent` uid? Mirrors what the edges
    /// already encode: `source('s','t')` hits `source.*.s.t`; `ref()` hits
    /// `model|seed|snapshot.*.<name>` by NAME alone — the loader's
    /// name-index resolution ignores the package arg (`ref('pkg','x')`
    /// lands on the local `x`), so a call naming any package still marks
    /// the edge it produced. A versioned ref wants that `.vN` — or any
    /// same-named unversioned uid, the name-only approximation the source
    /// loader makes — and a bare `ref('x')` lands on an unversioned uid or
    /// the family's latest version.
    fn targets(&self, call: &CallSite, parent: &str) -> bool {
        let segs: Vec<&str> = parent.split('.').collect();
        match &call.kind {
            CallKind::Source { source, table } => {
                matches!(segs.as_slice(), ["source", _, s, t] if s == source && t == table)
            }
            CallKind::Ref { name, version, .. } => {
                let [kind, _, uname, ..] = segs.as_slice() else {
                    return false;
                };
                if !matches!(*kind, "model" | "seed" | "snapshot") || uname != name {
                    return false;
                }
                let uid_version = segs.get(3).map(|v| normalize_version(v));
                match version {
                    Some(v) => uid_version.is_none() || uid_version == Some(normalize_version(v)),
                    None => uid_version.is_none() || self.is_latest(parent),
                }
            }
            CallKind::Config => false,
        }
    }

    /// `parent` is the newest version of its name among this node's
    /// parents — where a bare `ref('x')` lands (`detail.latest_version`
    /// when the manifest carries it, else the highest `.vN` suffix among
    /// the same kind/package/name parents).
    fn is_latest(&self, parent: &str) -> bool {
        if let Some((version, latest)) = self
            .dag
            .detail(parent)
            .and_then(|d| d.version.as_ref().zip(d.latest_version.as_ref()))
        {
            return normalize_version(version) == normalize_version(latest);
        }
        let segs: Vec<&str> = parent.split('.').collect();
        let [kind, pkg, name, ..] = segs.as_slice() else {
            return false;
        };
        let Some(max) = self
            .dag
            .direct_parents(self.uid)
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
        for site in &self.sites {
            let (CallKind::Config, Some(args)) = (&site.kind, &site.args) else {
                continue;
            };
            let Some(expr) = kwarg(&self.masked[args.clone()], "materialized") else {
                continue;
            };
            let expr = expr.trim();
            let Some(serde_json::Value::String(s)) = lit_value(expr) else {
                continue;
            };
            // `expr` is a subslice of `self.masked` — the offset is the
            // pointer delta (equal length ⇒ valid in `text` too).
            let start = expr.as_ptr() as usize - self.masked.as_ptr() as usize;
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
        f.with_span(self.file, line, column, end_line, end_column)
            .with_label(primary.1.into())
            .with_labels(labels)
    }
}
