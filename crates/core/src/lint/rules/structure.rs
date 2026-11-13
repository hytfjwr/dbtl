//! `structure` — naming and file-layout conventions.

use super::*;

/// `naming_conventions`: a model whose [`lint_model_type`] is listed in
/// `naming_prefixes` must start with one of that type's prefixes. The type is
/// resolved prefix-first — `staging/fct_x` is a *marts*
/// model (not a misnamed staging one) and `reports/x` is `other`, which is
/// prefix-free unless the config defines an `other:` layer.
pub(super) fn naming_conventions(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let layer = lint_model_type(n, config);
        let Some(prefixes) = config.naming_prefixes.get(layer) else {
            continue;
        };
        if prefixes.is_empty() || prefixes.iter().any(|p| n.name.starts_with(p.as_str())) {
            continue;
        }
        out.push(Finding::new(
            LintRule::NamingConventions,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{layer} model {} doesn't match {}: {}",
                display_name(dag, uid),
                pluralize(prefixes.len(), "prefix"),
                prefixes.join(", ")
            ),
        ));
    }
}

/// `model_directories`: the prefix-derived layer
/// ([`lint_model_type_by_prefix`]) and the directory-derived layer
/// ([`lint_model_type_by_dir`]) both resolve and disagree — `staging/fct_x`
/// names a marts model filed under staging/. Either stage coming back
/// unresolved is NOT a violation (there's nothing to disagree with).
pub(super) fn model_directories(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if n.resource_type != "model" || !in_scope(n) {
            continue;
        }
        let (Some(prefix_layer), Some(dir_layer)) = (
            lint_model_type_by_prefix(n, config),
            lint_model_type_by_dir(n, config),
        ) else {
            continue;
        };
        if prefix_layer == dir_layer {
            continue;
        }
        out.push(Finding::new(
            LintRule::ModelDirectories,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{} is named like a '{prefix_layer}' model but lives in the '{dir_layer}' directory",
                display_name(dag, uid)
            ),
        ));
    }
}

/// `model_too_long`: a model's raw SQL runs past `max_lines` (default 300)
/// counted lines. A line counts when it carries something other than
/// whitespace or `{# #}` comment text — [`crate::source::mask_jinja_comments`]
/// blanks comment bytes length-preserving, so a Jinja-comment-only line
/// (or one inside a multi-line comment) reads as empty and never counts.
/// `.py` models are excluded on both axes [`is_sql_model`] knows, like
/// `trailing_semicolon`: `language == "python"` and the `.py` file. A
/// raw-text rule (`Requires::Graph` — `raw_code` lives on the graph).
/// The position lands on the file line where the count crosses the
/// budget — the `(max_lines + 1)`-th counted line; line 1 would be a
/// guess (the model isn't too long yet there).
pub(super) fn model_too_long(
    dag: &Dag,
    uids: &[&String],
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    config: &LintConfig,
    out: &mut Vec<Finding>,
) {
    let max_lines = config
        .int_param(LintRule::ModelTooLong, "max_lines")
        .expect("the registry declares model_too_long.max_lines");
    for uid in uids {
        let Some(n) = dag.get(uid) else { continue };
        if !is_sql_model(dag, n) || !in_scope(n) {
            continue;
        }
        if dag
            .detail(uid)
            .and_then(|d| d.original_file_path.as_deref())
            .is_some_and(|f| f.ends_with(".py"))
        {
            continue;
        }
        let Some(raw) = dag.raw_code(uid) else {
            continue;
        };
        // Count non-blank lines after the comment mask; the byte range of
        // the crossing line is remembered for the position while the loop
        // keeps walking so the message can report the true total.
        let masked = crate::source::mask_jinja_comments(raw);
        let mut counted = 0usize;
        let mut over: Option<std::ops::Range<usize>> = None;
        let mut start = 0usize;
        for seg in masked.split_inclusive('\n') {
            let content = seg.strip_suffix('\n').unwrap_or(seg);
            let content = content.strip_suffix('\r').unwrap_or(content);
            if !content.trim().is_empty() {
                counted += 1;
                if counted == max_lines + 1 {
                    over = Some(start..start + content.len());
                }
            }
            start += seg.len();
        }
        let Some(range) = over else { continue };
        let mut f = Finding::new(
            LintRule::ModelTooLong,
            (*uid).clone(),
            Vec::new(),
            format!(
                "{}'s SQL runs {} counted lines, over the max_lines budget of {}",
                display_name(dag, uid),
                counted,
                max_lines
            ),
        );
        if let Some((line, column, end_line, end_column)) =
            byte_span_position(raw, range.start, range.end)
        {
            if let Some(file) = dag
                .detail(uid)
                .and_then(|d| d.original_file_path.as_deref())
            {
                f = f
                    .with_span(file, line, column, end_line, end_column)
                    .with_label("over the line budget");
            }
        }
        out.push(f);
    }
}
