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
//! A bare NAME atom is an exact match by default; when it contains `*`, `?`
//! or `[` it becomes a dbt-style glob (`*` any run, `?` one char, `[set]` /
//! `[!set]` a char class — fnmatch semantics, case-sensitive), so `stg_*`,
//! `tag:…` and `path:…` compose the same way dbt selectors do. A pattern
//! without glob characters keeps the exact-match behavior — no pattern
//! syntax is interpreted otherwise.
//!
//! `path:` follows dbt's own `PathSelectorMethod`: the pattern is a glob
//! over paths RELATIVE TO THE PROJECT ROOT (`*`/`?`/`[…]` never cross a
//! `/`, a `**` segment spans directories), and a node matches when its
//! `original_file_path` or any ancestor directory of it matches, or its
//! `patch_path` matches the pattern itself — so `path:models/staging`
//! selects everything under that dir, and `path:models/**/schema.yml`
//! reaches the `.yml`-patched nodes too.
//! For compatibility the pre-dbt-semantics form — the path RELATIVE TO the
//! resource directory (`path:staging` for `models/staging/stg.sql`) — still
//! matches, but [`select_report`] surfaces a deprecation note naming the
//! project-relative rewrite each CLI surface prints once. The legacy
//! substring read (`path:marts` also hitting `stg_marts.sql`) is gone.
//!
//! `2+name` = the node plus ≤2 upstream hops; `name+` = node + all downstream;
//! `+name+` = both directions. `state:modified[+]` selects the [`DagDiff`]
//! changed set (added ∪ modified) when a diff is loaded — the in-app
//! equivalent of `dbt --select state:modified+`; `state:new` is the added
//! side alone.
//!
//! The evaluation stays permissive on purpose — an unknown atom or a `state:`
//! selector without a diff just yields an empty set, so the interactive `!`
//! search never errors mid-typing. Command-line surfaces (`--select`,
//! `--exclude`, `dbtl ls`, `.dbtl.yml` `exclude:`) share the ONE strict
//! grammar check [`validate_selector`] instead.
//!
//! Returned set = `unique_id`s.

use std::collections::{BTreeSet, HashMap};

use crate::dag::{normalize_version, versioned_model_segs};
use crate::{is_exposure, Dag, DagDiff};

/// Evaluate `spec` over `dag` (with optional `diff` for `state:` selectors) →
/// the matching `unique_id` set. Union across whitespace-separated terms,
/// intersection across each term's comma-separated factors.
pub fn select(dag: &Dag, diff: Option<&DagDiff>, spec: &str) -> BTreeSet<String> {
    select_report(dag, diff, spec).0
}

/// [`select`] plus the evaluation's deprecation notes — currently the
/// `path:` legacy resource-relative form still matching (each note names
/// the project-relative rewrite). Non-interactive surfaces print them once
/// per invocation on stderr (`warning: …`); the interactive `!` search
/// ignores them — a partially typed spec warns about nothing.
pub fn select_report(
    dag: &Dag,
    diff: Option<&DagDiff>,
    spec: &str,
) -> (BTreeSet<String>, Vec<String>) {
    let mut out = BTreeSet::new();
    let mut notes = BTreeSet::new();
    for term in spec.split_whitespace() {
        out.extend(evaluate_term(dag, diff, term, &mut notes));
    }
    (out, notes.into_iter().collect())
}

/// One comma-separated intersection term.
fn evaluate_term(
    dag: &Dag,
    diff: Option<&DagDiff>,
    term: &str,
    notes: &mut BTreeSet<String>,
) -> BTreeSet<String> {
    let mut acc: Option<BTreeSet<String>> = None;
    for factor in term.split(',') {
        let set = evaluate_factor(dag, diff, factor, notes);
        acc = Some(match acc {
            None => set,
            Some(prev) => prev.intersection(&set).cloned().collect(),
        });
    }
    acc.unwrap_or_default()
}

/// `[@N+] atom [+]` — an atom set plus its bounded neighbourhood.
fn evaluate_factor(
    dag: &Dag,
    diff: Option<&DagDiff>,
    factor: &str,
    notes: &mut BTreeSet<String>,
) -> BTreeSet<String> {
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
    let base = evaluate_atom(dag, diff, atom, notes);
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

/// The atom's direct match set (before hop expansion). `notes` collects
/// the deprecation texts an atom's evaluation produced (the `path:` legacy
/// form), deduplicated by the `BTreeSet` so a spec repeating one atom notes
/// once.
fn evaluate_atom(
    dag: &Dag,
    diff: Option<&DagDiff>,
    atom: &str,
    notes: &mut BTreeSet<String>,
) -> BTreeSet<String> {
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
        Some(("path", p)) => {
            // `path:x/` reads like `path:x` — `Path` drops the trailing
            // slash the same way.
            let p = p.trim_end_matches('/');
            let re = path_glob_regex(p);
            let mut set = BTreeSet::new();
            let mut legacy_only = BTreeSet::new();
            for (uid, n) in dag.nodes() {
                let d = dag.detail(uid);
                // dbt's PathSelectorMethod: the project-root-relative
                // `original_file_path` and each of its ancestor directories
                // are match targets; `patch_path` must match the selector
                // itself (`ymlfp in paths` — no ancestor walk).
                let current = d.is_some_and(|d| {
                    d.original_file_path
                        .as_deref()
                        .is_some_and(|ofp| path_or_ancestors_match(&re, ofp))
                        || d.patch_path.as_deref().is_some_and(|pp| re.is_match(pp))
                });
                if current {
                    set.insert(uid.clone());
                    continue;
                }
                // The legacy form — the path relative to the node's
                // resource directory (`n.path`) with the same
                // file-or-ancestor reading — still matches but notes a
                // deprecation: the match only exists because this
                // resource-relative form did.
                if n.path
                    .as_deref()
                    .is_some_and(|np| path_or_ancestors_match(&re, np))
                {
                    set.insert(uid.clone());
                    legacy_only.insert(uid.clone());
                }
            }
            if !legacy_only.is_empty() {
                // The project-relative rewrite each matched node suggests —
                // usually one (`path:staging` → `path:models/staging`), one
                // per resource root when several matched.
                let mut rewrites = BTreeSet::new();
                for uid in &legacy_only {
                    if let Some(r) = legacy_path_rewrite(dag, uid, p) {
                        rewrites.insert(format!("'path:{r}'"));
                    }
                }
                let suggestion = if rewrites.is_empty() {
                    String::new()
                } else {
                    format!(
                        ": {}",
                        rewrites.into_iter().collect::<Vec<_>>().join(" or ")
                    )
                };
                notes.insert(format!(
                    "selector '{atom}' uses the deprecated resource-relative path form \
                     — write it project-relative instead{suggestion}"
                ));
            }
            set
        }
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
            ("new", Some(d)) => d.added.iter().cloned().collect(),
            _ => BTreeSet::new(),
        },
        _ => {
            if atom.contains(['*', '?', '[']) {
                // Glob atoms match on the NAME only — like dbt's bare-name
                // glob. (A `key:value` the arms above didn't claim still
                // lands here; node names can't contain ':', so the literal
                // `key:` prefix simply never matches — same as today.)
                // The regex compiles ONCE per atom, not per node. A
                // versioned model also tries its `name.vseg` spelling, so
                // `fct_orders.v*` reaches every version.
                let re = glob_regex(atom);
                nodes()
                    .filter(|(uid, n)| {
                        re.is_match(&n.name)
                            || versioned_model_segs(uid)
                                .is_some_and(|(name, vseg)| re.is_match(&format!("{name}.{vseg}")))
                    })
                    .map(|(uid, _)| uid.clone())
                    .collect()
            } else {
                nodes()
                    .filter(|(uid, n)| name_atom_matches(uid, &n.name, atom))
                    .map(|(uid, _)| uid.clone())
                    .collect()
            }
        }
    }
}

/// Bare-name atom match — the declared `name` as ever, or a versioned
/// model's `name.vseg` spelling (`fct_orders.v1`, the same name `ls` /
/// lint / the TUI list show — see [`crate::list_name`]). The version
/// compares prefix-free like `ref(…, v=…)`: `fct_orders.1` and
/// `fct_orders.v1` both land on `model.<pkg>.fct_orders.v1`, and a bare
/// `fct_orders` still matches every version of the family (dbt's
/// name-selector semantics — versions share one model name).
fn name_atom_matches(uid: &str, name: &str, atom: &str) -> bool {
    if name == atom {
        return true;
    }
    let Some((aname, aver)) = atom.split_once('.') else {
        return false;
    };
    versioned_model_segs(uid)
        .is_some_and(|(n, vseg)| aname == n && normalize_version(aver) == normalize_version(vseg))
}

/// Strictly validate a selector `spec` for the non-interactive surfaces
/// (`--select`, `--exclude`, `dbtl ls`, `.dbtl.yml` `exclude:`) — every
/// factor must carry a known atom, hops must be well formed, and no `,`/
/// whitespace may separate nothing. `Ok(())` means [`select`] evaluates
/// exactly what the user wrote; `Err` names the offending piece for a
/// usage-error report.
///
/// The permissive [`select`] treats a typo (`foo:bar`) as an empty match —
/// right for an interactive `!` search, wrong for a CI flag where it would
/// silently check nothing. This shares the SAME grammar `select` parses so
/// the two can't drift: the error cases are exactly the inputs the lenient
/// evaluation would misread.
pub fn validate_selector(spec: &str) -> Result<(), String> {
    if spec.trim().is_empty() {
        return Err("empty selector".to_string());
    }
    for term in spec.split_whitespace() {
        for factor in term.split(',') {
            validate_factor(factor)?;
        }
    }
    Ok(())
}

/// Whether any atom in `spec` is a `state:` selector — used to decide
/// whether the invocation needs a `--diff` baseline (and, for `ls`/`docs`,
/// whether `--diff` is meaningful at all). Call after [`validate_selector`]
/// — a malformed spec reports its own error first, so this only needs to
/// read well-formed input.
pub fn selector_uses_state(spec: &str) -> bool {
    spec.split_whitespace()
        .flat_map(|term| term.split(','))
        .map(factor_atom)
        .any(|atom| atom.starts_with("state:"))
}

/// The atom text a factor carries — the same leading `+`/`<N>+` then
/// trailing `+` split [`evaluate_factor`] performs (`x+` reads as atom `x`
/// plus the downstream hop; `2+` is `N+` with no atom). Whatever remains is
/// the atom, possibly malformed — the caller decides what to do with it.
fn factor_atom(factor: &str) -> &str {
    let rest = match factor.split_once('+') {
        Some(("", rest)) => rest,
        Some((lead, rest)) if lead.chars().all(|c| c.is_ascii_digit()) => rest,
        Some((lead, "")) => lead,
        _ => factor,
    };
    rest.strip_suffix('+').unwrap_or(rest)
}

/// One comma-separated factor under [`validate_selector`].
fn validate_factor(factor: &str) -> Result<(), String> {
    if factor.is_empty() {
        return Err("an empty selector term (a ',' needs a selector on both sides)".to_string());
    }
    // Mirror evaluate_factor's hop split: a leading `+` or `<N>+` is fine,
    // a NON-numeric lead before a non-empty rest is `a+b`-style nonsense —
    // the lenient evaluation would read it as the literal name `a+b`.
    if let Some((lead, rest)) = factor.split_once('+') {
        if !lead.is_empty() && !lead.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
            return Err(format!(
                "invalid hop '{lead}+' in '{factor}' — hop counts must be a number"
            ));
        }
    }
    let atom = factor_atom(factor);
    if atom.is_empty() {
        return Err(format!("'{factor}' — a '+' hop needs a selector to expand"));
    }
    if atom.contains('+') {
        return Err(format!(
            "invalid factor '{factor}' — only one leading and one trailing '+' hop are allowed"
        ));
    }
    validate_atom(atom)
}

/// The known `method:` forms under [`validate_selector`]; a bare name or
/// glob (no `:`) always parses.
fn validate_atom(atom: &str) -> Result<(), String> {
    // `config.*` is checked before the generic `method:` split — the key is
    // dotted (`config.materialized:`), like `evaluate_atom` handles it.
    if let Some(c) = atom.strip_prefix("config.") {
        return match c.split_once(':') {
            Some(("materialized", "")) => {
                Err("config.materialized: needs a non-empty value".to_string())
            }
            Some(("materialized", _)) => Ok(()),
            _ => Err(format!(
                "unknown config selector 'config.{c}' (supported: config.materialized:<value>)"
            )),
        };
    }
    if let Some(m) = atom.strip_prefix("meta:") {
        let key = m.split_once('=').map_or(m, |(k, _)| k);
        if key.is_empty() {
            return Err("meta: needs a key (meta:<key> or meta:<key>=<value>)".to_string());
        }
        return Ok(());
    }
    match atom.split_once(':') {
        None => Ok(()), // bare name / glob
        Some(("tag", "")) => Err("tag: needs a non-empty value".to_string()),
        Some(("tag", _)) => Ok(()),
        Some(("path", "")) => Err("path: needs a non-empty value".to_string()),
        Some(("path", _)) => Ok(()),
        Some(("exposure", "")) => Err("exposure: needs a non-empty value".to_string()),
        Some(("exposure", _)) => Ok(()),
        Some(("state", "modified" | "new")) => Ok(()),
        Some(("state", s)) => Err(format!(
            "unknown state selector 'state:{s}' (supported: modified, new)"
        )),
        Some((method, _)) => Err(format!(
            "unknown selector method '{method}:' in '{atom}' (supported: tag:, path:, \
             config.materialized:, exposure:, state:, meta:, or a node name)"
        )),
    }
}

/// Translate a dbt-style glob (Python `fnmatch` semantics: `*` = any run,
/// `?` = exactly one char, `[set]`/`[!set]` = a char class — a `]` right
/// after the opening bracket is a literal and ranges like `a-z` work;
/// case-sensitive) into an anchored regex. The translation is total — a
/// malformed class degrades to literals rather than erroring, matching
/// fnmatch's "an unmatched `[` is a literal `[`" rule — and the regex keeps
/// matching free of the exponential backtracking a naive recursive matcher
/// risks on adversarial `*a*a*a*`-style patterns. `pub(crate)` — lint's
/// `hard_coded_references.ignore` patterns match by the same glob.
pub(crate) fn glob_regex(pattern: &str) -> regex::Regex {
    // `(?s)` mirrors fnmatch's DOTALL `.*` (node names can't contain `\n`
    // anyway); `\A…\z` anchors the whole input.
    let mut out = String::from("\\A(?s)");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                let mut negated = false;
                if chars.peek() == Some(&'!') {
                    negated = true;
                    chars.next();
                }
                // Class body up to the first `]` that is not the first body
                // char (fnmatch: `[]x]` is a class containing `]` and `x`).
                let mut body = String::new();
                let mut first = true;
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == ']' && !first {
                        closed = true;
                        break;
                    }
                    first = false;
                    body.push(c);
                }
                if closed {
                    out.push('[');
                    if negated {
                        out.push('^');
                    }
                    for c in body.chars() {
                        // Inside a class, `[` `\` `]` `^` need escaping for
                        // the regex crate; `-` keeps its range meaning.
                        if matches!(c, '[' | '\\' | ']' | '^') {
                            out.push('\\');
                        }
                        out.push(c);
                    }
                    out.push(']');
                } else {
                    // Unterminated `[` — emit it (and the consumed body) as
                    // literals, like fnmatch does.
                    out.push_str("\\[");
                    if negated {
                        out.push('!');
                    }
                    out.push_str(&regex::escape(&body));
                }
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out.push_str("\\z");
    // The translation produces only literals/classes/`.*`/`.`, so it always
    // compiles; the fallback can never match (it is not a valid string).
    regex::Regex::new(&out).unwrap_or_else(|_| regex::Regex::new("\\A\\z").unwrap())
}

/// The `path:` glob translation — pathlib `Path.glob` semantics like dbt's
/// own path selector: `*`, `?` and `[…]` never cross a `/` boundary, and a
/// whole-segment `**` spans zero or more directories (`a/**/b` matches
/// `a/b`, `a/**` matches `a` itself — the globbed set is a path PREFIX the
/// ancestor check then re-derives). The same total-fnmatch degradation
/// [`glob_regex`] applies: an unterminated `[` is a literal.
fn path_glob_regex(pattern: &str) -> regex::Regex {
    let mut out = String::from("\\A(?s)");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // A whole-segment `**` — zero or more path segments.
        if c == '*'
            && chars.get(i + 1) == Some(&'*')
            && (i == 0 || chars[i - 1] == '/')
            && chars.get(i + 2).is_none_or(|c| *c == '/')
        {
            let whole = i == 0;
            i += 2;
            match chars.get(i) {
                // `…/**` at the end matches the prefix dir itself too — fold
                // the just-emitted literal `/` into the optional tail.
                // `**` as the whole pattern matches every path.
                None => {
                    if !whole && out.ends_with('/') {
                        out.pop();
                        out.push_str("(?:/.*)?");
                    } else {
                        out.push_str(".*");
                    }
                }
                // `…/**/` at the end reads the same as `…/**`.
                Some('/') if i + 1 == chars.len() => {
                    if !whole && out.ends_with('/') {
                        out.pop();
                        out.push_str("(?:/.*)?");
                    } else {
                        out.push_str(".*");
                    }
                    i += 1;
                }
                // `**/…` — the segment plus its slash, both optional.
                Some('/') => {
                    out.push_str("(?:.*/)?");
                    i += 1;
                }
                _ => unreachable!("checked above"),
            }
            continue;
        }
        match c {
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => {
                // Same fnmatch class as `glob_regex`, `/`-bounded.
                let mut j = i + 1;
                let mut negated = false;
                if chars.get(j) == Some(&'!') {
                    negated = true;
                    j += 1;
                }
                let mut body = String::new();
                let mut first = true;
                let mut closed = false;
                while let Some(&c) = chars.get(j) {
                    if c == ']' && !first {
                        closed = true;
                        j += 1;
                        break;
                    }
                    first = false;
                    body.push(c);
                    j += 1;
                }
                if !closed {
                    // Unterminated `[` — literals, like `glob_regex`.
                    out.push_str("\\[");
                    if negated {
                        out.push('!');
                    }
                    out.push_str(&regex::escape(&body));
                    i = j;
                    continue;
                }
                let emit_body = |out: &mut String, body: &str| {
                    for c in body.chars() {
                        // Inside a class, `[` `\` `]` `^` need escaping for
                        // the regex crate; `-` keeps its range meaning.
                        if matches!(c, '[' | '\\' | ']' | '^') {
                            out.push('\\');
                        }
                        out.push(c);
                    }
                };
                if negated {
                    // `[^…]` must still refuse `/` — append it to the
                    // exclusion set.
                    out.push_str("[^");
                    emit_body(&mut out, &body);
                    out.push('/');
                    out.push(']');
                } else {
                    // A positive class can't match `/` either — strip it
                    // (an all-`/` body then matches nothing: `[^\s\S]`).
                    let body: String = body.chars().filter(|c| *c != '/').collect();
                    if body.is_empty() {
                        out.push_str("[^\\s\\S]");
                    } else {
                        out.push('[');
                        emit_body(&mut out, &body);
                        out.push(']');
                    }
                }
                i = j;
                continue;
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    out.push_str("\\z");
    // Only literals/classes/segment-bound wildcards are emitted, so it
    // always compiles; the fallback can never match.
    regex::Regex::new(&out).unwrap_or_else(|_| regex::Regex::new("\\A\\z").unwrap())
}

/// Does `re` match `path` or any of its ancestor directories — dbt's
/// `ofp in paths or any(parent in paths)` check. The trailing `/` walk
/// visits each strict prefix at a `/` boundary (`a/b/c.sql` → `a/b`, `a`).
fn path_or_ancestors_match(re: &regex::Regex, path: &str) -> bool {
    let mut rest = path;
    loop {
        if re.is_match(rest) {
            return true;
        }
        let Some((dir, _)) = rest.rsplit_once('/') else {
            return false;
        };
        rest = dir;
    }
}

/// The project-relative rewrite a legacy-matched node suggests for `path:`.
/// `n.path` is the node's path under its resource directory; when
/// `original_file_path` carries it as a suffix (`models/` + `staging/x.sql`),
/// the rewrite is that resource root prepended to the pattern — else the
/// node's own file path is the most honest suggestion.
fn legacy_path_rewrite(dag: &Dag, uid: &str, pattern: &str) -> Option<String> {
    let n = dag.get(uid)?;
    let np = n.path.as_deref()?;
    let ofp = dag
        .detail(uid)
        .and_then(|d| d.original_file_path.as_deref());
    match ofp {
        // Boundary-checked suffix: `mstaging/x.sql` must not suffix-match
        // `staging/x.sql` — the prefix cut needs a `/` before `np`.
        Some(ofp)
            if ofp.len() > np.len()
                && ofp.ends_with(np)
                && ofp.as_bytes()[ofp.len() - np.len() - 1] == b'/' =>
        {
            Some(format!("{}{}", &ofp[..ofp.len() - np.len()], pattern))
        }
        Some(ofp) => Some(ofp.to_string()),
        // No project-relative anchor to suggest from — a bare `n.path`
        // rewrite would still be the deprecated form (models carry
        // resource-dir-relative paths). Better the note's generic tail.
        None => None,
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
    /// parameterizable by test. `original_file_path` is set the way real
    /// manifests carry it — `models/` + the resource-relative `path` — so
    /// `path:` exercises the project-root-relative (dbt) read.
    pub(super) fn dag() -> Dag {
        let mk = |uid: &str, name: &str, path: &str, mat: &str, deps: &[&str], tags: &[&str]| {
            (
                uid.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    path: Some(path.into()),
                    original_file_path: Some(format!("models/{path}")),
                    raw_code: Some("select 1".into()),
                    config: crate::RawConfig {
                        materialized: Some(mat.into()),
                        ..Default::default()
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
            unresolved_adjacent: Default::default(),
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
            docs: Default::default(),
        })
    }

    fn names(set: &BTreeSet<String>) -> Vec<String> {
        set.iter()
            .map(|u| u.rsplit('.').next().unwrap().to_string())
            .collect()
    }

    /// `stg -> fct.v1 / fct.v2 / fct.vbeta`, `fct.v2 -> use_all` — the graph
    /// a real `versions:`-ed model compiles to (several nodes sharing the
    /// declared name `fct`, plus one consumer pinned to v2).
    fn versioned_dag() -> Dag {
        let mk = |uid: &str, name: &str| {
            (
                uid.to_string(),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            )
        };
        let mut nodes = HashMap::from([mk("model.p.stg", "stg"), mk("model.p.use_all", "use_all")]);
        for v in ["v1", "v2", "vbeta"] {
            nodes.insert(
                format!("model.p.fct.{v}"),
                RawNode {
                    name: "fct".into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            );
        }
        Dag::build(&RawManifest {
            unresolved_adjacent: Default::default(),
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
                ("model.p.fct.v1".into(), vec!["model.p.stg".into()]),
                ("model.p.fct.v2".into(), vec!["model.p.stg".into()]),
                ("model.p.fct.vbeta".into(), vec!["model.p.stg".into()]),
                ("model.p.use_all".into(), vec!["model.p.fct.v2".into()]),
            ]),
            child_map: HashMap::from([
                (
                    "model.p.stg".into(),
                    vec![
                        "model.p.fct.v1".into(),
                        "model.p.fct.v2".into(),
                        "model.p.fct.vbeta".into(),
                    ],
                ),
                ("model.p.fct.v2".into(), vec!["model.p.use_all".into()]),
            ]),
            docs: Default::default(),
        })
    }

    #[test]
    fn version_qualified_name_atoms_pick_one_spelling() {
        let d = versioned_dag();
        // dbt's `model.vN` selector semantics — the `vseg` spelling every
        // surface displays — plus the prefix-free `ref(…, v=…)` forms.
        assert_eq!(names(&select(&d, None, "fct.v1")), vec!["v1"]);
        assert_eq!(names(&select(&d, None, "fct.1")), vec!["v1"]);
        assert_eq!(names(&select(&d, None, "fct.V2")), vec!["v2"]);
        assert_eq!(names(&select(&d, None, "fct.vbeta")), vec!["vbeta"]);
        // The bare family name still hits every version — dbt's own
        // name-selector semantics (versions share one model name).
        assert_eq!(names(&select(&d, None, "fct")), vec!["v1", "v2", "vbeta"]);
        // A version no node carries matches nothing, and the qualified
        // spelling doesn't leak onto another model's versions.
        assert!(select(&d, None, "fct.v9").is_empty());
        assert!(select(&d, None, "use_all.v1").is_empty());
    }

    #[test]
    fn versioned_globs_and_hops() {
        let d = versioned_dag();
        // A glob matches the `name.vseg` spelling AND the bare name — both
        // reach every version of the family.
        assert_eq!(
            names(&select(&d, None, "fct.v*")),
            vec!["v1", "v2", "vbeta"]
        );
        assert_eq!(names(&select(&d, None, "f*")), vec!["v1", "v2", "vbeta"]);
        assert_eq!(names(&select(&d, None, "fct.v?")), vec!["v1", "v2"]);
        // Hops expand from the one version the atom picked.
        assert_eq!(names(&select(&d, None, "fct.v2+")), vec!["v2", "use_all"]);
        assert_eq!(names(&select(&d, None, "+fct.v1")), vec!["v1", "stg"]);
    }

    #[test]
    fn bare_name_and_attr_selectors() {
        let d = dag();
        assert_eq!(names(&select(&d, None, "fct")), vec!["fct"]);
        assert_eq!(names(&select(&d, None, "tag:nightly")), vec!["int", "stg"]);
        // The legacy resource-relative spelling still selects — and notes.
        let (set, notes) = select_report(&d, None, "path:staging");
        assert_eq!(names(&set), vec!["stg"]);
        assert_eq!(
            notes,
            vec![
                "selector 'path:staging' uses the deprecated resource-relative \
                 path form — write it project-relative instead: \
                 'path:models/staging'"
                    .to_string()
            ]
        );
        // The project-root-relative (dbt) spelling selects without noting.
        let (set, notes) = select_report(&d, None, "path:models/staging");
        assert_eq!(names(&set), vec!["stg"]);
        assert!(notes.is_empty());
        assert_eq!(
            names(&select(&d, None, "config.materialized:table")),
            vec!["fct", "other"]
        );
    }

    /// A `path:`-semantics testbed: a model + a source + an exposure under
    /// `models/staging`/`models/marts`, plus `stg_marts` (a shared-prefix
    /// name the old substring read would false-positive) and a `seed` dir
    /// model patched by `models/marts/schema.yml` (the `patch_path` case).
    /// Sources/exposures carry project-relative `path` already — the same
    /// shape real manifests produce.
    fn path_dag() -> Dag {
        let mk_model = |uid: &str, path: &str, patch: Option<&str>| {
            (
                uid.to_string(),
                RawNode {
                    name: uid.rsplit('.').next().unwrap().into(),
                    resource_type: "model".into(),
                    path: Some(path.into()),
                    original_file_path: Some(format!("models/{path}")),
                    patch_path: patch.map(|p| format!("p://{p}")),
                    ..Default::default()
                },
            )
        };
        let mut nodes = HashMap::from([
            mk_model("model.p.stg", "staging/stg.sql", None),
            mk_model("model.p.mart", "marts/mart.sql", None),
            mk_model("model.p.stg_marts", "marts/stg_marts.sql", None),
            mk_model(
                "model.p.seeded",
                "seeded.sql",
                Some("models/marts/schema.yml"),
            ),
        ]);
        nodes.extend([mk_model("model.p.deep", "marts/core/deep.sql", None)]);
        let sources = HashMap::from([(
            "source.p.raw.stg".to_string(),
            crate::manifest::RawSource {
                name: "stg".into(),
                path: Some("models/staging/schema.yml".into()),
                original_file_path: Some("models/staging/schema.yml".into()),
                ..Default::default()
            },
        )]);
        let exposures = HashMap::from([(
            "exposure.p.dash".to_string(),
            crate::manifest::RawExposure {
                name: "dash".into(),
                path: Some("marts/schema.yml".into()),
                original_file_path: Some("models/marts/schema.yml".into()),
                ..Default::default()
            },
        )]);
        Dag::build(&RawManifest {
            unresolved_adjacent: Default::default(),
            metadata: Default::default(),
            nodes,
            sources,
            exposures,
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            parent_map: HashMap::new(),
            child_map: HashMap::new(),
            docs: Default::default(),
        })
    }

    /// uids, keeping resource type — path tests mix models/sources/exposures.
    fn uids(set: &BTreeSet<String>) -> Vec<String> {
        set.iter()
            .map(|u| {
                let mut s = u.split('.');
                format!("{}.{}", s.next().unwrap(), s.next_back().unwrap())
            })
            .collect()
    }

    /// dbt `PathSelectorMethod`: match `original_file_path`, any ancestor
    /// dir of it, or `patch_path` — all project-root-relative.
    #[test]
    fn path_matches_original_file_path_ancestors_and_patch() {
        let d = path_dag();
        // An ancestor dir of `original_file_path` selects the node —
        // model AND source AND exposure under `models/marts`/`staging`.
        assert_eq!(
            uids(&select(&d, None, "path:models/staging")),
            vec!["model.stg", "source.stg"]
        );
        assert_eq!(
            uids(&select(&d, None, "path:models/marts")),
            vec![
                "exposure.dash",
                "model.deep",
                "model.mart",
                "model.stg_marts"
            ]
        );
        // `model.seeded` does NOT match here: `patch_path` gets no
        // ancestor walk in dbt — `models/marts/schema.yml` must match the
        // selector itself, and a directory isn't that file (the glob
        // forms below that DO name it select it).
        // The project root itself selects everything (5 models + source +
        // exposure).
        assert_eq!(select(&d, None, "path:models").len(), 7);
        // An exact file path matches too (dbt `ofp in paths`).
        assert_eq!(
            uids(&select(&d, None, "path:models/marts/mart.sql")),
            vec!["model.mart"]
        );
        // Boundary-respecting: `mart` is neither an ancestor segment nor a
        // substring of `stg_marts.sql` — the old substring read is gone.
        assert!(select(&d, None, "path:models/mart").is_empty());
        assert!(select(&d, None, "path:mart").is_empty());
    }

    /// `path:` globs: `*`/`?`/`[…]` never cross `/`, a `**` segment spans
    /// directories — pathlib `Path.glob` semantics like dbt's.
    #[test]
    fn path_globs() {
        let d = path_dag();
        // `*` stays inside its segment.
        assert_eq!(
            uids(&select(&d, None, "path:models/marts/*.sql")),
            vec!["model.mart", "model.stg_marts"]
        );
        // `**` spans — and `a/**` includes `a` itself (the ancestor check
        // then re-derives everything beneath). `seeded` joins through
        // `patch_path`: `models/marts/**` matches `schema.yml` itself.
        assert_eq!(
            uids(&select(&d, None, "path:models/marts/**")),
            vec![
                "exposure.dash",
                "model.deep",
                "model.mart",
                "model.seeded",
                "model.stg_marts"
            ]
        );
        assert_eq!(
            uids(&select(&d, None, "path:**/deep.sql")),
            vec!["model.deep"]
        );
        // `stg*` is a FILE-name glob — the source's `schema.yml` doesn't
        // start with `stg`, so only the two models match.
        assert_eq!(
            uids(&select(&d, None, "path:models/**/stg*")),
            vec!["model.stg", "model.stg_marts"]
        );
        // `?` — one char, segment-bound.
        assert_eq!(
            uids(&select(&d, None, "path:models/ma?ts/mart.sql")),
            vec!["model.mart"]
        );
        // `[…]` classes, segment-bound — `[a-s]*` also reaches `deep` via
        // its `core` ancestor dir and `dash` via `schema.yml`.
        assert_eq!(
            uids(&select(&d, None, "path:models/marts/[a-s]*")),
            vec![
                "exposure.dash",
                "model.deep",
                "model.mart",
                "model.seeded",
                "model.stg_marts"
            ]
        );
        assert_eq!(
            uids(&select(&d, None, "path:models/marts/[!m]*.sql")),
            vec!["model.stg_marts"]
        );
    }

    /// The legacy resource-relative read (`n.path`) still matches — once —
    /// with a note naming the project-relative rewrite; nodes matching only
    /// the new semantics never produce it.
    #[test]
    fn path_legacy_form_still_matches_with_a_deprecation_note() {
        let d = path_dag();
        // `path:marts` = the old spelling of `path:models/marts`; it still
        // selects every `n.path`-under-`marts/` node (the exposure's
        // declaring yml included — but NOT `seeded`, whose path is
        // `seeded.sql`), noting the rewrite once for the whole spec.
        let (set, notes) = select_report(&d, None, "path:marts path:marts");
        assert_eq!(
            uids(&set),
            vec![
                "exposure.dash",
                "model.deep",
                "model.mart",
                "model.stg_marts"
            ]
        );
        assert_eq!(
            notes,
            vec![
                "selector 'path:marts' uses the deprecated resource-relative \
                 path form — write it project-relative instead: \
                 'path:models/marts'"
                    .to_string()
            ]
        );
        // A pattern matching via BOTH forms notes only for the nodes the
        // current form missed — `path:models` matches current everywhere.
        let (_, notes) = select_report(&d, None, "path:models");
        assert!(notes.is_empty());
        // A spec's notes dedupe across terms and are sorted.
        let (_, notes) = select_report(&d, None, "path:staging path:marts tag:x");
        assert_eq!(
            notes,
            vec![
                "selector 'path:marts' uses the deprecated resource-relative \
                 path form — write it project-relative instead: \
                 'path:models/marts'"
                    .to_string(),
                "selector 'path:staging' uses the deprecated resource-relative \
                 path form — write it project-relative instead: \
                 'path:models/staging'"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn windows_manifest_paths_normalize_to_slashes() {
        // A manifest produced by `dbt compile` on Windows carries
        // `\`-separated `path`/`original_file_path` — `Dag::build`
        // normalizes both so `path:` selectors and the legacy `n.path`
        // read see `/` separators like every other platform.
        let nodes = HashMap::from([(
            "model.p.mart".to_string(),
            RawNode {
                name: "mart".into(),
                resource_type: "model".into(),
                path: Some("marts\\mart.sql".into()),
                original_file_path: Some("models\\marts\\mart.sql".into()),
                ..Default::default()
            },
        )]);
        let d = Dag::build(&RawManifest {
            unresolved_adjacent: Default::default(),
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
            docs: Default::default(),
        });
        let (set, notes) = select_report(&d, None, "path:models/marts");
        assert_eq!(uids(&set), vec!["model.mart"]);
        // The new semantics match — no legacy-compat note fires.
        assert!(notes.is_empty());
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
            unresolved_adjacent: Default::default(),
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
            docs: Default::default(),
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
            unresolved_adjacent: Default::default(),
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
            docs: Default::default(),
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

    #[test]
    fn bare_name_globs() {
        let d = dag(); // models: stg, int, fct, other
                       // `*` — any run (empty included).
        assert_eq!(names(&select(&d, None, "s*")), vec!["stg"]);
        assert_eq!(
            names(&select(&d, None, "*t*")),
            vec!["fct", "int", "other", "stg"]
        );
        assert_eq!(
            names(&select(&d, None, "*")),
            vec!["fct", "int", "other", "stg"]
        );
        // `?` — exactly one char.
        assert_eq!(names(&select(&d, None, "f?t")), vec!["fct"]);
        assert!(select(&d, None, "f?").is_empty());
        // `[set]`, `[!set]`, ranges — anchored on both ends.
        assert_eq!(names(&select(&d, None, "[fs]*")), vec!["fct", "stg"]);
        assert_eq!(names(&select(&d, None, "[!fs]*")), vec!["int", "other"]);
        assert_eq!(names(&select(&d, None, "[a-i]nt")), vec!["int"]);
        // An unterminated `[` degrades to a literal — no such name here.
        assert!(select(&d, None, "stg[").is_empty());
        // A glob-free name keeps exact-match semantics (prefixes don't match).
        assert_eq!(names(&select(&d, None, "fct")), vec!["fct"]);
        assert!(select(&d, None, "fc").is_empty());
        assert!(select(&d, None, "fctx").is_empty());
        // Globs compose with intersection and hops like any atom.
        assert_eq!(names(&select(&d, None, "f*,tag:daily")), vec!["fct"]);
        assert_eq!(
            names(&select(&d, None, "*+")),
            vec!["fct", "int", "other", "stg"],
            "glob + trailing-hop: every node seeds its downstream expansion"
        );
    }

    /// A diff where `stg` is a NEW node and `other` merely changed — the
    /// `state:` atoms must read the two sets differently.
    fn changed_diff() -> DagDiff {
        let mut d = DagDiff::default();
        d.added.insert("model.p.stg".to_string());
        d.modified
            .insert("model.p.other".to_string(), vec!["SQL changed".to_string()]);
        d
    }

    #[test]
    fn state_new_selects_added_only() {
        let d = dag();
        let diff = changed_diff();
        assert_eq!(
            names(&select(&d, Some(&diff), "state:new")),
            vec!["stg"],
            "state:new = added only"
        );
        assert_eq!(
            names(&select(&d, Some(&diff), "state:modified")),
            vec!["other", "stg"],
            "state:modified = added ∪ modified"
        );
        // Hop expansion rides both forms.
        assert_eq!(
            names(&select(&d, Some(&diff), "state:new+")),
            vec!["fct", "int", "stg"],
            "state:new+ = added nodes and their downstream"
        );
        // No diff → both are empty sets (the permissive-eval contract).
        assert!(select(&d, None, "state:new").is_empty());
        assert!(select(&d, None, "state:modified").is_empty());
    }

    // ---- validate_selector -------------------------------------------------

    /// The accepted grammar — every shape `select` understands.
    #[test]
    fn validate_accepts_the_full_grammar() {
        for spec in [
            "stg",
            "stg_*",
            "tag:nightly",
            "path:marts/",
            "config.materialized:table",
            "exposure:dashboard",
            "state:modified",
            "state:new",
            "meta:owner",
            "meta:owner=alice",
            "+stg",
            "stg+",
            "+stg+",
            "2+stg",
            "2+stg+",
            "stg fct",                     // union
            "tag:nightly,stg",             // intersection
            "stg+ tag:nightly,path:marts", // compound
        ] {
            assert_eq!(validate_selector(spec), Ok(()), "{spec:?} must parse");
        }
    }

    /// Every malformed shape the lenient evaluation would silently misread —
    /// each rejected with an error naming the bad piece.
    #[test]
    fn validate_rejects_malformed_selectors() {
        let cases: &[(&str, &str)] = &[
            // unknown method:
            ("foo:bar", "unknown selector method 'foo:'"),
            ("state:", "unknown state selector 'state:'"),
            ("state:added", "unknown state selector 'state:added'"),
            ("config.tag:x", "unknown config selector 'config.tag:x'"),
            ("config:", "unknown selector method 'config:'"),
            // empty values:
            ("tag:", "tag: needs a non-empty value"),
            ("path:", "path: needs a non-empty value"),
            ("exposure:", "exposure: needs a non-empty value"),
            (
                "config.materialized:",
                "config.materialized: needs a non-empty value",
            ),
            ("meta:", "meta: needs a key"),
            ("meta:=v", "meta: needs a key"),
            // hops with no atom:
            ("+", "a '+' hop needs a selector"),
            ("2+", "a '+' hop needs a selector"),
            ("++", "a '+' hop needs a selector"),
            // non-numeric / extra hops:
            ("a+b", "invalid hop 'a+'"),
            ("x+y+", "invalid hop 'x+'"),
            ("+a+b", "only one leading and one trailing"),
            // broken intersections / specs:
            ("a,,b", "an empty selector term"),
            ("a,", "an empty selector term"),
            (",a", "an empty selector term"),
            ("   ", "empty selector"),
            ("", "empty selector"),
        ];
        for (spec, needle) in cases {
            let Err(msg) = validate_selector(spec) else {
                panic!("{spec:?} must be rejected");
            };
            assert!(
                msg.contains(needle),
                "{spec:?}: error {msg:?} must mention {needle:?}"
            );
        }
    }

    /// Validation accepts exactly what evaluation reads — `x+` is the atom
    /// `x` plus a downstream hop, never the (atom-less) `x+`-as-upstream
    /// misread.
    #[test]
    fn validate_matches_select_for_edge_hops() {
        // `x+` — trailing hop on a bare name: valid.
        assert_eq!(validate_selector("stg+"), Ok(()));
        // `2+` — a bounded upstream hop with NO atom: invalid.
        assert!(validate_selector("2+").is_err());
        // `+x` / `+x+` — unbounded upstream (plus downstream): valid.
        assert_eq!(validate_selector("+stg"), Ok(()));
        assert_eq!(validate_selector("+stg+"), Ok(()));
        // `meta:k=v+` — hop applies to the whole meta atom: valid.
        assert_eq!(validate_selector("meta:k=v+"), Ok(()));
        // `state:modified+` — hop applies to the state atom: valid.
        assert_eq!(validate_selector("state:modified+"), Ok(()));
    }

    #[test]
    fn selector_uses_state_flags_only_state_atoms() {
        assert!(selector_uses_state("state:modified"));
        assert!(selector_uses_state("state:new+"));
        assert!(selector_uses_state("2+state:modified"));
        assert!(selector_uses_state("stg state:new,tag:x"));
        assert!(!selector_uses_state("stg"));
        assert!(
            !selector_uses_state("tag:state:x"),
            "a state-looking VALUE isn't the method"
        );
    }
}
