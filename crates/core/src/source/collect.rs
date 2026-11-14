use std::borrow::Cow;

use super::*;

/// Build a manifest-shaped `columns` object (`{name: {name,data_type,description}}`)
/// preserving definition order (the `Map` is order-preserving via `preserve_order`).
/// Column descriptions render `{{ doc() }}` refs like every other description.
pub(super) fn columns_json(
    cols: &[SchemaColumn],
    docs: &DocBlocks,
    warnings: &mut Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for c in cols {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "name".to_string(),
            serde_json::Value::String(c.name.clone()),
        );
        if let Some(dt) = &c.data_type {
            obj.insert(
                "data_type".to_string(),
                serde_json::Value::String(dt.clone()),
            );
        }
        if let Some(d) = &c.description {
            obj.insert(
                "description".to_string(),
                serde_json::Value::String(docs.render(d, warnings)),
            );
        }
        // Column-level `constraints:` ride INSIDE the column object — the
        // same wire position the real manifest records (no `columns` key
        // there; the constraint covers the column itself).
        if !c.constraints.is_empty() {
            obj.insert(
                "constraints".to_string(),
                serde_json::to_value(&c.constraints).expect("RawConstraint serializes"),
            );
        }
        map.insert(c.name.clone(), serde_json::Value::Object(obj));
    }
    map
}

/// Column names from a seed CSV header row (no types — and no descriptions to
/// render, but the shared builder keeps the signature uniform). Empty if
/// unreadable.
pub(super) fn csv_header_columns(
    file: &Path,
    docs: &DocBlocks,
    warnings: &mut Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let contents = read(file);
    let header = contents.lines().next().unwrap_or("");
    let cols: Vec<SchemaColumn> = header
        .split(',')
        .map(|c| SchemaColumn {
            name: c.trim().trim_matches('"').to_string(),
            ..Default::default()
        })
        .filter(|c| !c.name.is_empty())
        .collect();
    columns_json(&cols, docs, warnings)
}

/// Read a file to a string, or empty on error (a single unreadable model should
/// not abort the whole project scan).
pub(super) fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// The file stem (name without extension) as an owned string.
pub(super) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// The `<name>` family of a `<name>_v<ver>` file stem — dbt's convention for
/// splitting a versioned model's implementations across files
/// (`fct_orders_v2.sql` defines `fct_orders` v2). The suffix is `v` plus
/// non-empty version text; anything else has no family. Deliberately loose
/// about what follows the `v` — a `my_var.sql` stem earns `my` an assumed
/// name too, which can only under-report, the direction the
/// `unresolved_references` scope deliberately takes for guesses.
pub(super) fn version_family(stem: &str) -> Option<&str> {
    let (name, ver) = stem.rsplit_once('_')?;
    (ver.len() > 1
        && ver.starts_with('v')
        && ver[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'))
    .then_some(name)
}

/// Keep only resource paths that are safe to join onto the project root:
/// relative, with no `..` component. `dbt_project.yml` is untrusted input, so
/// a traversal value (`../../etc`, `/etc`) is skipped rather than letting the
/// model/seed/snapshot scan (and the schema.yml scan, which walks the same
/// lists) read files outside the project directory.
pub(super) fn contained_resource_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|p| {
            Path::new(p)
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        })
        .collect()
}

/// Path relative to the project root (for `original_file_path`), forward-slashed.
pub(super) fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Collect `(file, sub_root)` pairs with `ext` under each of `subdirs` (relative
/// to `root`), sorted by file path so unique-id generation and name-collision
/// resolution are OS-independent. `sub_root` (`root/<subdir>`) lets `path` be made
/// relative to the paths root the way dbt records it, distinct from the
/// project-root-relative `original_file_path`.
pub(super) fn collect_with_root(
    root: &Path,
    subdirs: &[String],
    ext: &str,
) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    for sub in subdirs {
        let sub_root = root.join(sub);
        let mut files = Vec::new();
        collect_files(&sub_root, ext, &mut files);
        for file in files {
            out.push((file, sub_root.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Recursively collect files with extension `ext` under `dir`. A missing dir is
/// silently skipped. Recursion is decided by `entry.file_type()`, which does
/// NOT follow symlinks: a symlinked directory is never entered, so a link loop
/// (e.g. `models/loop` -> `models`) can't recurse forever.
pub(super) fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_files(&path, ext, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

/// Mask Jinja comments (`{# #}`) to spaces — deliberately NOT `--` / `/* */`.
///
/// dbt renders Jinja BEFORE the SQL (and its comments) exists, so a
/// `{{ ref() }}` inside a `--` or `/* */` SQL comment IS a real dependency in
/// the manifest; only `{# #}` hides Jinja from dbt. The extractors must see
/// exactly what dbt sees, or source mode silently drops edges the manifest has
/// (the consistency contract).
///
/// Every comment byte (the `{#`/`#}` markers included, `\n` excepted) becomes
/// one space: tokens can never fuse across a comment AND the result has the
/// same byte length as the input, so an offset computed on the masked text
/// indexes the original text too (call-site positions).
///
/// A text without `{#` or `{%` needs no copy — the borrowed input returns
/// as-is.
pub fn mask_jinja_comments(text: &str) -> Cow<'_, str> {
    if !text.contains("{#") && !text.contains("{%") {
        return Cow::Borrowed(text);
    }
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'#' {
            // Match `(?s)\{#.*?#\}`'s shape: the FIRST `#}` closes the
            // comment; an unclosed `{#` is not a comment and stays.
            let mut j = i + 2;
            while j + 1 < b.len() && !(b[j] == b'#' && b[j + 1] == b'}') {
                j += 1;
            }
            if j + 1 >= b.len() {
                break;
            }
            for c in &mut out[i..j + 2] {
                if *c != b'\n' {
                    *c = b' ';
                }
            }
            i = j + 2;
        } else {
            i += 1;
        }
    }
    // Second pass, on the masked text (a `{# #}`-masked `{% … %}` opens
    // nothing): `{% comment %}…{% endcomment %}` and `{% raw %}…{% endraw %}`
    // are dead to every consumer of this text — Jinja executes no call
    // inside either, so a `ref()`/`source()`/`config()` there must not
    // reach the call scans, `disabled()`, or the arm index. Neither nests
    // (the first matching `{% end… %}` closes, the way the lexer eats the
    // body verbatim); an unclosed one masks to the end — dbt errors on it,
    // and the safe direction is under-reporting what it would never run.
    let mut i = 0;
    while i + 1 < out.len() {
        if out[i] == b'{' && out[i + 1] == b'%' {
            let Some(e) = jinja_tag_close(&out, i) else {
                i += 1;
                continue;
            };
            let word = tag_word(
                std::str::from_utf8(&out[i + 2..e]).expect("masking emits ASCII spaces only"),
            )
            .0;
            if word != "comment" && word != "raw" {
                i = e + 2;
                continue;
            }
            let end_word = if word == "raw" {
                "endraw"
            } else {
                "endcomment"
            };
            let mut j = e + 2;
            let close_end = loop {
                let Some(open) = find_subslice(&out, j, b"{%") else {
                    break out.len();
                };
                let Some(te) = jinja_tag_close(&out, open) else {
                    break out.len();
                };
                if tag_word(
                    std::str::from_utf8(&out[open + 2..te])
                        .expect("masking emits ASCII spaces only"),
                )
                .0 == end_word
                {
                    break te + 2;
                }
                j = te + 2;
            };
            for c in &mut out[i..close_end] {
                if *c != b'\n' {
                    *c = b' ';
                }
            }
            i = close_end;
        } else {
            i += 1;
        }
    }
    // Every replaced byte is ASCII (space or the kept '\n'), so the output
    // is valid UTF-8 of the same byte length as the input.
    Cow::Owned(String::from_utf8(out).expect("comment masking produces ASCII-only output"))
}

/// Strip Jinja comments from NON-SQL text (descriptions, docs-block bodies).
/// Jinja removes a `{# #}` comment entirely — no replacement space — so
/// `a{# c #}b` renders as `"ab"`. SQL keeps the space variant instead, so
/// tokens can never fuse across a comment (`from{# c #}t` stays `from t`).
pub(super) fn strip_jinja_comments_text(text: &str) -> String {
    strip_jinja_comments_with(text, "")
}

pub(super) fn strip_jinja_comments_with(text: &str, replacement: &str) -> String {
    static JINJA: OnceLock<Regex> = OnceLock::new();
    let jinja = JINJA.get_or_init(|| Regex::new(r"(?s)\{#.*?#\}").unwrap());
    jinja.replace_all(text, replacement).into_owned()
}
