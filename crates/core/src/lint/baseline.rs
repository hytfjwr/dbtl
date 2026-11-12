//! The `.dbtl-baseline.json` format: a frozen set of lint findings, keyed by
//! [`Finding::key`] identity — edge rules on `(rule, node_uid, related)`, node
//! rules on `(rule, node_uid)` — so reordering or renumbering findings never
//! changes what matches. The module is deliberately pure: parsing, writing
//! and applying are free functions with no file IO, and the `baseline:` path
//! resolution lives in the project-config loader / CLI wiring.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{removed_rule, Finding};

/// The `schema` stamp every baseline file carries. `from_json` rejects any
/// other value: a misread baseline would match nothing, which never lets a
/// frozen violation through — it silently un-freezes them ALL instead.
/// Rejecting up front turns that into one readable error rather than a
/// wall of re-flagged findings.
pub const BASELINE_SCHEMA: &str = "dbtl.lint-baseline/v1";

/// The conventional file name `lint.baseline` resolves to when the config
/// doesn't override it.
pub const DEFAULT_BASELINE_NAME: &str = ".dbtl-baseline.json";

/// A parsed lint baseline: the identity keys of the findings it freezes.
/// `entries` is a sorted set so the serialized form is deduplicated and
/// byte-stable regardless of write order. Rows naming a rule the registry
/// has dropped never land in `entries` — they are counted into
/// `removed_rule_entries` at parse so the caller can warn once per id
/// while the stale rows silently match nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Baseline {
    /// `(rule, node, related)` — the exact triple `Finding::key()` returns.
    entries: BTreeSet<BaselineEntry>,
    /// Removed rule id → how many entries the parsed file carried for it.
    /// Kept separate from `entries` so a removed rule can neither freeze
    /// a current finding nor count toward the frozen-set total.
    pub removed_rule_entries: BTreeMap<&'static str, usize>,
}

/// One baseline row. `related` is the sorted parent/child endpoint list for
/// edge rules, `[]` for node rules (a node finding's `related` is context,
/// not identity).
type BaselineEntry = (String, String, Vec<String>);

impl Baseline {
    /// Collect `findings`' identity keys into a baseline.
    pub fn from_findings(findings: &[Finding]) -> Self {
        Baseline {
            entries: findings
                .iter()
                .map(|f| {
                    let (rule, node, related) = f.key();
                    (rule.name().to_string(), node.to_string(), related.to_vec())
                })
                .collect(),
            removed_rule_entries: BTreeMap::new(),
        }
    }

    /// Parse baseline text. Errors on malformed JSON and on a `schema`
    /// mismatch (the expected value is quoted in the error). Entries for a
    /// REMOVED rule are not kept — they are counted in
    /// `removed_rule_entries` so the caller can warn once per id; unknown
    /// but never-registered rule names still parse (they just never match,
    /// the same as a stale-but-live entry).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let file: BaselineFile =
            serde_json::from_str(text).map_err(|e| format!("invalid lint baseline: {e}"))?;
        if file.schema != BASELINE_SCHEMA {
            return Err(format!(
                "invalid lint baseline schema {:?} (expected {:?})",
                file.schema, BASELINE_SCHEMA
            ));
        }
        let mut removed_rule_entries: BTreeMap<&'static str, usize> = BTreeMap::new();
        let entries = file
            .findings
            .into_iter()
            .filter_map(|f| {
                if let Some(removed) = removed_rule(&f.rule) {
                    *removed_rule_entries.entry(removed.id).or_default() += 1;
                    return None;
                }
                // A hand-written file may carry `related` unsorted or
                // duplicated — normalize it to `Finding::key()`'s
                // canonical order so an edge entry still matches.
                let mut related = f.related;
                related.sort();
                related.dedup();
                Some((f.rule, f.node, related))
            })
            .collect();
        Ok(Baseline {
            entries,
            removed_rule_entries,
        })
    }

    /// Serialize: schema-stamped, `findings` sorted by `(rule, node,
    /// related)`, deduplicated, 2-space indent, trailing newline — identical
    /// bytes for identical content on every write.
    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct Entry<'a> {
            rule: &'a str,
            node: &'a str,
            related: &'a [String],
        }
        #[derive(Serialize)]
        struct File<'a> {
            schema: &'a str,
            findings: Vec<Entry<'a>>,
        }
        let file = File {
            schema: BASELINE_SCHEMA,
            findings: self
                .entries
                .iter()
                .map(|(rule, node, related)| Entry {
                    rule,
                    node,
                    related,
                })
                .collect(),
        };
        // Serializing strings cannot fail.
        let mut s = serde_json::to_string_pretty(&file).expect("baseline JSON serialization");
        s.push('\n');
        s
    }

    /// Flag every finding whose `key()` appears in the baseline `baselined`.
    /// A stale entry for a live rule simply never matches; entries for
    /// REMOVED rules never reach `entries` at all (see [`Baseline::from_json`]).
    pub fn apply(&self, findings: &mut [Finding]) {
        for f in findings.iter_mut() {
            let (rule, node, related) = f.key();
            if self
                .entries
                .contains(&(rule.name().to_string(), node.to_string(), related.to_vec()))
            {
                f.baselined = true;
            }
        }
    }

    /// The number of frozen entries (for reports and tests).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The on-disk shape. `related` defaults to `[]` so hand-written baselines
/// can omit it on node findings.
#[derive(Debug, Deserialize)]
struct BaselineFile {
    schema: String,
    #[serde(default)]
    findings: Vec<BaselineEntryFile>,
}

#[derive(Debug, Deserialize)]
struct BaselineEntryFile {
    rule: String,
    node: String,
    #[serde(default)]
    related: Vec<String>,
}
