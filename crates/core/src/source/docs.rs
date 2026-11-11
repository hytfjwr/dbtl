use super::*;

/// The `{% docs %}` blocks collected from a project's `.md` files, ready to
/// expand `{{ doc('name') }}` refs in descriptions.
pub(super) struct DocBlocks {
    /// `name -> trimmed body` (dbt `.strip()`s `block_contents` — matched).
    pub(super) blocks: BTreeMap<String, String>,
    /// The local project name: a 2-arg `doc('proj', 'x')` resolves locally
    /// (verified against real dbt); a different package segment never resolves
    /// in source mode, which sees no installed packages.
    pub(super) proj: String,
    /// `{{ doc('x') }}` / `{{ doc('pkg', 'x') }}` — the ONE Jinja call resolved
    /// in descriptions. Jinja whitespace control is honoured: `{{-` eats the
    /// whitespace immediately before the tag (group 2) and `-}}` eats the
    /// whitespace after (group 5), so the adjacent whitespace is captured in
    /// the match and only re-emitted when the dash is absent.
    pub(super) doc_ref_re: Regex,
    /// A loose `{{ doc... }}` detector: any `doc` call the strict pattern
    /// above can't interpret (bad arg count, a non-literal arg like `var`) is
    /// a parse error for real dbt — surfaced as a warning instead of silently
    /// passing the raw text through.
    pub(super) doc_detect_re: Regex,
    /// Dedup for warning emission: each distinct warning is pushed once into
    /// `warnings` (the shared Vec would make `contains` O(n) per ref).
    pub(super) warned: RefCell<BTreeSet<String>>,
}

impl DocBlocks {
    pub(super) fn new(proj: String, blocks: BTreeMap<String, String>) -> Self {
        DocBlocks {
            blocks,
            proj,
            doc_ref_re: Regex::new(
                r#"(\s*)\{\{(-?)\s*doc\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*['"]([^'"]+)['"]\s*)?\)\s*(-?)\}\}(\s*)"#,
            )
            .unwrap(),
            doc_detect_re: Regex::new(r#"\{\{-?\s*doc\b[^}]*-?\}\}"#).unwrap(),
            warned: RefCell::new(BTreeSet::new()),
        }
    }

    /// Push `warn` into `warnings` exactly once per distinct message.
    pub(super) fn warn_once(&self, warnings: &mut Vec<String>, warn: String) {
        if self.warned.borrow_mut().insert(warn.clone()) {
            warnings.push(warn);
        }
    }

    /// Expand `{{ doc('x') }}` / `{{ doc('pkg', 'x') }}` refs the way dbt
    /// renders them in descriptions: substitute the block's body. dbt renders
    /// the whole description as Jinja, so `{# #}` comments (and any doc()
    /// inside them) disappear first; `{{-`/`-}}` whitespace control consumes
    /// the adjacent whitespace. An unresolved ref (no such block — where dbt
    /// would hard-error — or a foreign package source mode can't see) keeps
    /// the raw `{{ doc(...) }}` text and pushes one dedup'd warning, like the
    /// loader's other recoverable oddities; an unparseable doc() call warns
    /// too (dbt parse-errors there as well).
    pub(super) fn render(&self, text: &str, warnings: &mut Vec<String>) -> String {
        let stripped = strip_jinja_comments_text(text);
        for m in self.doc_detect_re.find_iter(&stripped) {
            if self.doc_ref_re.captures(m.as_str()).is_none() {
                self.warn_once(
                    warnings,
                    format!(
                        "uninterpretable doc() call {} in a description; leaving the raw text",
                        m.as_str()
                    ),
                );
            }
        }
        self.doc_ref_re
            .replace_all(&stripped, |c: &regex::Captures| {
                let (pkg, name) = match c.get(4) {
                    Some(n) => (Some(c.get(3).unwrap().as_str()), n.as_str()),
                    None => (None, c.get(3).unwrap().as_str()),
                };
                let hit = match pkg {
                    Some(p) if p != self.proj => None,
                    _ => self.blocks.get(name),
                };
                match hit {
                    Some(body) => {
                        // Whitespace control: a dash eats the adjacent
                        // captured whitespace; no dash re-emits it.
                        let left = if c.get(2).unwrap().as_str() == "-" {
                            ""
                        } else {
                            c.get(1).unwrap().as_str()
                        };
                        let right = if c.get(5).unwrap().as_str() == "-" {
                            ""
                        } else {
                            c.get(6).unwrap().as_str()
                        };
                        format!("{left}{body}{right}")
                    }
                    None => {
                        let warn = match pkg {
                            Some(p) => format!(
                                "unresolved doc('{p}', '{name}') in a description: \
                                 package '{p}' is not this project; leaving the raw text"
                            ),
                            None => format!(
                                "unresolved doc('{name}') in a description: \
                                 no matching {{% docs %}} block; leaving the raw text"
                            ),
                        };
                        self.warn_once(warnings, warn);
                        c.get(0).unwrap().as_str().to_string()
                    }
                }
            })
            .into_owned()
    }
}

/// Collect `{% docs name %}…{% enddocs %}` blocks from every `.md` file under
/// `doc_paths` into a `name -> body` map, first-wins on a duplicate name (a
/// duplicate is a hard error for dbt; the loader warns instead, like its
/// duplicate model-name handling).
pub(super) fn collect_doc_blocks(
    dir: &Path,
    doc_paths: &[String],
    pat: &Patterns,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    // Overlapping doc/resource paths can collect the same file twice — dedup
    // by file (sorted, so equal files are adjacent) or its blocks double-report.
    let mut files = collect_with_root(dir, doc_paths, "md");
    files.dedup_by(|a, b| a.0 == b.0);
    let mut blocks = BTreeMap::new();
    for (file, _root) in files {
        // A `{% docs %}` tag inside `{# #}` is invisible to dbt (verified), so
        // extract from the Jinja-comment-stripped text like the .sql pass —
        // the text (not SQL) variant, because dbt renders a block body as
        // Jinja and a `{# c #}` inside it disappears entirely.
        let stripped = strip_jinja_comments_text(&read(&file));
        // Every `{% docs %}` must pair with `{% enddocs %}` — real dbt
        // hard-errors on a stray or unclosed tag; warn (degraded) and keep
        // collecting the well-formed blocks.
        let mut open: Option<String> = None;
        for tag in pat.doc_tags(&stripped) {
            match tag {
                DocTag::Open(name) => {
                    if let Some(prev) = open.replace(name) {
                        warnings.push(format!(
                            "unclosed {{% docs {prev} %}} in {}: nested {{% docs %}} tag",
                            rel_path(dir, &file)
                        ));
                    }
                }
                DocTag::Close => {
                    if open.take().is_none() {
                        warnings.push(format!(
                            "stray {{% enddocs %}} in {}: no open {{% docs %}} tag",
                            rel_path(dir, &file)
                        ));
                    }
                }
            }
        }
        if let Some(name) = open {
            warnings.push(format!(
                "unclosed {{% docs {name} %}} in {}: no matching {{% enddocs %}}",
                rel_path(dir, &file)
            ));
        }
        for (name, body) in pat.doc_blocks(&stripped) {
            if blocks.contains_key(&name) {
                warnings.push(format!(
                    "duplicate docs block '{name}': ignoring {}",
                    rel_path(dir, &file)
                ));
                continue;
            }
            blocks.insert(name, body);
        }
    }
    blocks
}
