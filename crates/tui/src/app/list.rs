use super::*;

impl App {
    /// The list currently driving the left pane: the search view if a search
    /// is narrowing it, else the persistent-filter/fold view, else the full
    /// list. Selection indexes into THIS list.
    pub fn active_list(&self) -> &ModelList {
        self.filter.as_ref().unwrap_or_else(|| self.base_list())
    }

    /// The list a search narrows FROM: the persistent-filter view when one is
    /// active, else the full list. FOLD-FREE on purpose — §10: a list search
    /// ignores the folded groups so matches inside them still surface.
    pub(super) fn unfolded_base(&self) -> &ModelList {
        self.filtered_base.as_ref().unwrap_or(&self.model_list)
    }

    /// The list the left pane shows (and selection indexes into) when no
    /// search narrows it: the folded view when groups are folded, else the
    /// unfolded base. `close_search` lands here — the fold state restores
    /// the moment the search view drops.
    fn base_list(&self) -> &ModelList {
        self.folded_base
            .as_ref()
            .unwrap_or_else(|| self.unfolded_base())
    }

    /// The short status/title tag for the active persistent filter. BARE text
    /// (no brackets): the list title and the status segment each add their own
    /// `[..]` wrapper.
    pub fn list_filter_label(&self) -> Option<&'static str> {
        match self.list_filter {
            ListFilter::All => None,
            ListFilter::Untested => Some("untested"),
            ListFilter::Bookmarked => Some("marked"),
        }
    }

    /// Recompute the persistent-filter AND folded views from the current
    /// `model_list` / `bookmarks` / `folded_groups` and re-resolve the
    /// selection by `unique_id` (else top). The single writer of BOTH view
    /// caches — the list pipeline is
    /// `model_list → T/* persistent filter → folded_groups → / search`, so
    /// sort / filter / fold / search inputs can never disagree about which
    /// stage rebuilt what. Called on every input any stage depends on: the
    /// `T`/`*`/`-` toggles, a group-header click, a sort change, a reload,
    /// and a bookmark toggle while the Bookmarked filter is live. With a
    /// list search open it defers selection to `refilter`, which narrows
    /// the fold-free base.
    pub fn apply_list_filter(&mut self) {
        let current = self.selected_unique_id();
        self.filtered_base = match self.list_filter {
            ListFilter::All => None,
            ListFilter::Untested => Some(crate::model_list::filter_model_list(
                &self.model_list,
                coverage_gap,
            )),
            ListFilter::Bookmarked => Some(crate::model_list::filter_model_list(
                &self.model_list,
                |m| self.bookmarks.contains(&m.unique_id),
            )),
        };
        self.folded_base = if self.folded_groups.is_empty() {
            None
        } else {
            Some(crate::model_list::fold_model_list(
                self.unfolded_base(),
                &self.folded_groups,
            ))
        };
        if matches!(&self.mode, Mode::Search(s) if s.target == SearchTarget::List) {
            self.refilter();
            return;
        }
        self.ui_state.set_model_count(self.active_list().len());
        let resolved = current
            .as_deref()
            .is_some_and(|uid| self.select_by_unique_id(uid));
        if !resolved {
            self.ui_state.set_selected(0);
        }
    }

    /// `-` on the list: fold/unfold the group owning the model the CURSOR
    /// sits on (the highlighted row — detach-aware, so it is the cursor's
    /// group, not necessarily the root's). The group-header mouse click's
    /// keyboard twin.
    ///
    /// A folded group's models leave model space, so the cursor can never
    /// sit inside one — under cursor semantics `-` could only ever FOLD.
    /// When no model resolves under the cursor at all (the all-folded empty
    /// list), fall back to the ROOT's group: the root is the one "selected
    /// model" that can live inside a folded group, and toggling it is the
    /// only `-` path that can re-OPEN one (§10's 畳む / 開く — and what the
    /// all-folded empty-state hint promises).
    pub(crate) fn toggle_selected_group_fold(&mut self) {
        let key = self
            .active_list()
            .group_of_model(self.ui_state.selected())
            .map(|(k, _)| k.to_string())
            .or_else(|| {
                let uid = self.selected_unique_id()?;
                let base = self.unfolded_base();
                base.models
                    .iter()
                    .position(|m| m.unique_id == uid)
                    .and_then(|i| base.group_of_model(i))
                    .map(|(k, _)| k.to_string())
            });
        if let Some(key) = key {
            self.toggle_group_fold(&key);
        }
    }

    /// Fold/unfold group `key` through the shared [`apply_list_filter`]
    /// rebuild — the single writer of all view caches. On a FOLD that hides
    /// the tab's root the uid re-resolve fails (a folded group leaves model
    /// space), so the cursor is re-seated at the slot the group's models
    /// vacated — where the NEXT surviving model lands (§10's 次の見えるモデ
    /// ル — the next group's first row, or the new tail's last model when
    /// the folded group was last). The root itself stays on the hidden
    /// model (the §2 detach: the lineage keeps it while the list hides it,
    /// same as a filtered-out root).
    pub(crate) fn toggle_group_fold(&mut self, key: &str) {
        // Computed BEFORE the rebuild, while the pre-fold list is live: the
        // folded group's first model index, when the ROOT sits inside it.
        let rescue_slot = if self.folded_groups.contains(key) {
            None
        } else {
            let list = self.active_list();
            self.selected_unique_id()
                .as_deref()
                .and_then(|uid| list.models.iter().position(|m| m.unique_id == uid))
                .and_then(|i| list.group_of_model(i))
                .and_then(|(k, start)| (k == key).then_some(start))
        };
        if !self.folded_groups.insert(key.to_string()) {
            self.folded_groups.remove(key);
        }
        self.apply_list_filter();
        if let Some(slot) = rescue_slot {
            self.ui_state
                .set_selected(slot.min(self.ui_state.model_count().saturating_sub(1)));
        }
    }

    /// Move the selection to the model with this NAME in the active list, if
    /// present (dbt model names are project-unique). The `--select` startup
    /// flag's primitive.
    pub fn select_by_name(&mut self, name: &str) -> bool {
        let uid = self
            .active_list()
            .models
            .iter()
            .find(|n| n.name == name)
            .map(|n| n.unique_id.clone());
        uid.is_some_and(|uid| self.select_by_unique_id(&uid))
    }

    /// Whether `uid` can anchor a tab's root — the same rule `reload`'s
    /// tab-prune and the persisted-state restore share: the node must exist
    /// AND still be a `model` (the only kind the list can re-root onto).
    pub(crate) fn is_rootable(&self, uid: &str) -> bool {
        self.dag
            .get(uid)
            .is_some_and(|n| n.resource_type == "model")
    }

    /// Rebuild the filtered list view for the current list-search query and
    /// re-resolve the selection **by unique_id** (prefer the current selection if
    /// it still matches, else the search origin, else the top). No-op unless a
    /// list search is active. This is the only place that touches `filter`, so
    /// the active list, its `model_count`, and the selection stay coherent — the
    /// guard against the two-coordinate-space desync.
    pub fn refilter(&mut self) {
        let (query, origin) = match &self.mode {
            Mode::Search(s) if s.target == SearchTarget::List => {
                (s.query.clone(), s.origin_uid.clone())
            }
            _ => return,
        };
        let current = self.selected_node().map(|n| n.unique_id.clone());
        // `!`-prefixed queries are dbt selectors, not fuzzy name matches:
        // `!tag:nightly`, `!+fct_orders`, `!state:modified+` (with --diff).
        // `?`-prefixed queries are FULL-TEXT: match description, column names,
        // and raw SQL — "who references this table/column" (`/?raw_events`).
        // The search base is FOLD-FREE (`unfolded_base`): a folded group's
        // members still match and render while the search runs — §10's
        // 検索中は畳みを無視する rule — and `close_search` lands back on the
        // folded `base_list` when the query ends.
        let filtered = if let Some(spec) = query.strip_prefix('!') {
            if spec.trim().is_empty() {
                // A bare `!` is the selector PREFIX, not a query — keep the
                // full list while the user is still typing (`select("")`
                // would be a zero-term empty set and flash a wrong
                // "no match" reason at the first keystroke).
                build_filtered_model_list(self.unfolded_base(), "")
            } else {
                let set = crate::select::select(&self.dag, self.diff(), spec);
                crate::model_list::filter_model_list(self.unfolded_base(), |m| {
                    set.contains(&m.unique_id)
                })
            }
        } else if let Some(needle) = query.strip_prefix('?') {
            let needle = needle.to_lowercase();
            crate::model_list::filter_model_list(self.unfolded_base(), |m| {
                self.full_text_match(&m.unique_id, &needle)
            })
        } else {
            build_filtered_model_list(self.unfolded_base(), &query)
        };
        self.ui_state.set_model_count(filtered.len());
        self.filter = Some(filtered);
        // Prefer keeping the current node if it survived the filter, else the
        // origin, else fall back to the top of the (possibly empty) list.
        let mut resolved = false;
        if let Some(uid) = &current {
            resolved = self.select_by_unique_id(uid);
        }
        if !resolved {
            if let Some(uid) = &origin {
                resolved = self.select_by_unique_id(uid);
            }
        }
        if !resolved {
            self.ui_state.set_selected(0);
        }
    }

    /// Full-text predicate behind `/<query>`: case-insensitive substring over
    /// the node's description, column names, and raw (uncompiled) SQL — the
    /// surfaces the name-only fuzzy match can't reach ("who selects from
    /// `raw_events`", "who writes `customer_id`").
    pub(super) fn full_text_match(&self, uid: &str, needle: &str) -> bool {
        let hit = |s: &str| s.to_lowercase().contains(needle);
        if let Some(d) = self.dag.detail(uid) {
            if d.description.as_deref().is_some_and(hit) {
                return true;
            }
            if d.columns.iter().any(|c| hit(&c.name)) {
                return true;
            }
        }
        self.dag.raw_code(uid).is_some_and(hit)
    }

    /// Leave search mode, dropping the filter and restoring the folded
    /// persistent base, then select `restore_uid` (the search origin on
    /// cancel, or the chosen match on confirm) in that list.
    ///
    /// `unfold` marks the CONFIRM path: the `/` search deliberately narrows
    /// the fold-free base, so a confirmed match can live inside a folded
    /// group — where the just-restored fold view can't re-resolve it and the
    /// pick would silently drop. An explicit selection wins over the view
    /// pref: the match's OWN group unfolds (every other fold stays put —
    /// 畳みに戻る still holds for what the user didn't pick). `false`
    /// (cancel / a tab switch's `None`) mutates nothing: restoring the
    /// origin inside a still-folded group is the ordinary §2 detach.
    pub(super) fn close_search(&mut self, restore_uid: Option<String>, unfold: bool) {
        self.filter = None;
        self.mode = self.idle_mode();
        if unfold {
            if let Some(group) = restore_uid
                .as_deref()
                .and_then(|uid| {
                    let base = self.unfolded_base();
                    base.models
                        .iter()
                        .position(|m| m.unique_id == uid)
                        .and_then(|i| base.group_of_model(i))
                })
                .map(|(k, _)| k.to_string())
            {
                if self.folded_groups.remove(&group) {
                    self.apply_list_filter();
                }
            }
        }
        // Back to the FOLDED base — the folds the search ignored restore.
        self.ui_state.set_model_count(self.base_list().len());
        if let Some(uid) = restore_uid {
            self.reroot_select(&uid);
        }
    }
}
