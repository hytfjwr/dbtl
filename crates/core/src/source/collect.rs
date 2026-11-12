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

/// Strip Jinja comments (`{# #}`) ONLY — deliberately NOT `--` / `/* */`.
///
/// dbt renders Jinja BEFORE the SQL (and its comments) exists, so a
/// `{{ ref() }}` inside a `--` or `/* */` SQL comment IS a real dependency in
/// the manifest; only `{# #}` hides Jinja from dbt. The extractors must see
/// exactly what dbt sees, or source mode silently drops edges the manifest has
/// (the consistency contract).
pub(super) fn strip_jinja_comments(sql: &str) -> String {
    strip_jinja_comments_with(sql, " ")
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
