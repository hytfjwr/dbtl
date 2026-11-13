//! The root-level index pages — `owners.md` / `tags.md` / `groups.md`.
//!
//! Cross-cutting views over the project the README's `## Indexes` links to.
//! They are ALWAYS generated (even when the page would be empty — an empty
//! page says `No …` rather than disappearing) so the README's links always
//! resolve and the file set is stable across runs.
//!
//! Membership is project-wide (like the README's Summary): every node of the
//! page's resource-type set is listed, but only nodes whose page was
//! generated get a link — under a narrowed `--select` the rest are plain
//! names, the same rule every other page's links already follow.

use super::*;

/// The three index pages, `(relative_path, body)` — the caller wraps them
/// with the generated marker like every other page.
pub(super) fn index_pages(ctx: &PageInputs<'_>, owner_meta_key: &str) -> Vec<(String, String)> {
    vec![
        (
            "owners.md".to_string(),
            owners_markdown(ctx, owner_meta_key),
        ),
        ("tags.md".to_string(), tags_markdown(ctx)),
        ("groups.md".to_string(), groups_markdown(ctx)),
    ]
}

/// `name` rendered as a table-cell link to the node's generated page, or the
/// bare (label-escaped) name when no page was generated for it — the same
/// "generated pages only" link rule every page follows.
fn linked_name(node: &NodeInfo, from: &str, files: &BTreeMap<String, String>) -> String {
    let name = list_name(&node.unique_id, &node.name);
    match files.get(&node.unique_id) {
        Some(path) => format!("[{}]({})", md_label(&name), page_link(from, path)),
        None => md_cell(&name),
    }
}

/// Heading-safe text: a heading can't carry newlines, and leading/trailing
/// whitespace is presentation noise.
fn heading(value: &str) -> String {
    value.trim().replace(['\n', '\r'], " ")
}

/// Rows for a node table, sorted by display name then `unique_id` — the
/// page's stable order.
fn sorted_nodes<'a>(uids: impl Iterator<Item = &'a NodeInfo>) -> Vec<&'a NodeInfo> {
    let mut v: Vec<&NodeInfo> = uids.collect();
    v.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.unique_id.cmp(&b.unique_id))
    });
    v
}

/// `owners.md` — models grouped by the configured owner meta key's value.
/// The key that was used is stated once at the top (it is resolved by the
/// caller from `.dbtl.yml`'s `owner_meta_key`, defaulting to `owner`); models
/// without it land in a trailing `(no owner)` section.
fn owners_markdown(ctx: &PageInputs<'_>, key: &str) -> String {
    let mut s = String::from("# Owners\n\n");
    // The resolved key is config-supplied text — `code_span` keeps a key
    // containing backticks or newlines from breaking the inline-code span
    // (or smuggling a fake line into the page).
    s.push_str(&format!("Owner meta key: {}\n", code_span(key)));

    let mut groups: BTreeMap<&str, Vec<&NodeInfo>> = BTreeMap::new();
    let mut unowned: Vec<&NodeInfo> = Vec::new();
    for node in ctx
        .dag
        .nodes()
        .values()
        .filter(|n| n.resource_type == "model")
    {
        match ctx
            .dag
            .detail(&node.unique_id)
            .and_then(|d| d.meta.get(key))
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            Some(owner) => groups.entry(owner).or_default().push(node),
            None => unowned.push(node),
        }
    }

    if groups.is_empty() && unowned.is_empty() {
        s.push_str("\nNo owners.\n");
        return s;
    }

    let emit = |s: &mut String, models: Vec<&NodeInfo>| {
        s.push_str("\n| Name | Layer | Materialization | Tests |\n");
        s.push_str("| --- | --- | --- | --- |\n");
        for node in sorted_nodes(models.into_iter()) {
            let layer = first_dir(node).map_or("-", |l| l);
            let mat = node.materialized.as_deref().unwrap_or("-");
            s.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                linked_name(node, "owners.md", ctx.files),
                md_cell(layer),
                md_cell(mat),
                node.test_count
            ));
        }
    };

    for (owner, models) in groups {
        s.push_str(&format!("\n## {}\n", heading(owner)));
        emit(&mut s, models);
    }
    if !unowned.is_empty() {
        s.push_str("\n## (no owner)\n");
        emit(&mut s, unowned);
    }
    s
}

/// `tags.md` — model/seed/snapshot nodes grouped by tag. A node with
/// several tags appears under each; untagged nodes appear nowhere.
/// (Sources can't carry tags in `NodeDetail` — `RawSource` has no `tags`
/// field — so they're not candidates.)
fn tags_markdown(ctx: &PageInputs<'_>) -> String {
    let mut s = String::from("# Tags\n");
    let mut groups: BTreeMap<&str, Vec<&NodeInfo>> = BTreeMap::new();
    for node in ctx
        .dag
        .nodes()
        .values()
        .filter(|n| matches!(n.resource_type.as_str(), "model" | "seed" | "snapshot"))
    {
        for tag in ctx
            .dag
            .detail(&node.unique_id)
            .map(|d| d.tags.as_slice())
            .unwrap_or(&[])
        {
            groups.entry(tag.as_str()).or_default().push(node);
        }
    }

    if groups.is_empty() {
        s.push_str("\nNo tags.\n");
        return s;
    }

    for (tag, nodes) in groups {
        s.push_str(&format!("\n## {}\n", heading(tag)));
        s.push_str("\n| Name | Type |\n| --- | --- |\n");
        for node in sorted_nodes(nodes.into_iter()) {
            s.push_str(&format!(
                "| {} | {} |\n",
                linked_name(node, "tags.md", ctx.files),
                md_cell(&node.resource_type)
            ));
        }
    }
    s
}

/// `groups.md` — models grouped by their dbt `group`, opened by a small
/// access-count table (the counts are exactly `NodeDetail::access` over all
/// models, `none` = undeclared — the same label each row's Access cell
/// uses). Ungrouped models land in a trailing `(no group)` section.
fn groups_markdown(ctx: &PageInputs<'_>) -> String {
    let mut s = String::from("# Groups\n");
    let models: Vec<&NodeInfo> = ctx
        .dag
        .nodes()
        .values()
        .filter(|n| n.resource_type == "model")
        .collect();

    if models.is_empty() {
        s.push_str("\nNo groups.\n");
        return s;
    }

    // Access counts over ALL models — the fixed-order display is the same
    // vocabulary `NodeDetail::access` uses (`none` = no access declared).
    s.push_str("\n| Access | Models |\n| --- | --- |\n");
    let mut counts: BTreeMap<Option<&str>, usize> = BTreeMap::new();
    for node in &models {
        *counts
            .entry(
                ctx.dag
                    .detail(&node.unique_id)
                    .and_then(|d| d.access.as_deref()),
            )
            .or_default() += 1;
    }
    for key in ["public", "protected", "private"] {
        s.push_str(&format!(
            "| {key} | {} |\n",
            counts.get(&Some(key)).unwrap_or(&0)
        ));
        counts.remove(&Some(key));
    }
    // A non-standard access value still counts — after the canonical rows,
    // sorted so the table stays deterministic.
    for (key, n) in &counts {
        if let Some(key) = key {
            s.push_str(&format!("| {} | {n} |\n", md_cell(key)));
        }
    }
    s.push_str(&format!("| none | {} |\n", counts.get(&None).unwrap_or(&0)));

    let mut groups: BTreeMap<&str, Vec<&NodeInfo>> = BTreeMap::new();
    let mut ungrouped: Vec<&NodeInfo> = Vec::new();
    for node in models {
        match ctx
            .dag
            .detail(&node.unique_id)
            .and_then(|d| d.group.as_deref())
            .map(str::trim)
            .filter(|g| !g.is_empty())
        {
            Some(group) => groups.entry(group).or_default().push(node),
            None => ungrouped.push(node),
        }
    }

    let emit = |s: &mut String, models: Vec<&NodeInfo>| {
        s.push_str("\n| Name | Access |\n| --- | --- |\n");
        for node in sorted_nodes(models.into_iter()) {
            let access = ctx
                .dag
                .detail(&node.unique_id)
                .and_then(|d| d.access.as_deref())
                .unwrap_or("none");
            s.push_str(&format!(
                "| {} | {} |\n",
                linked_name(node, "groups.md", ctx.files),
                md_cell(access)
            ));
        }
    };

    for (group, models) in groups {
        s.push_str(&format!("\n## {}\n", heading(group)));
        emit(&mut s, models);
    }
    if !ungrouped.is_empty() {
        s.push_str("\n## (no group)\n");
        emit(&mut s, ungrouped);
    }
    s
}
