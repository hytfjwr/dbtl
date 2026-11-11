use super::*;

impl App {
    // ---- node context menu -------------------------------------

    /// The `unique_id` the `a` key's node menu acts on: over the structure /
    /// SQL modal the modal's own node (same rule `y` uses); in Selection the
    /// focus target (the lineage cursor under right-pane focus, else the
    /// list selection).
    pub(super) fn menu_uid(&self) -> Option<String> {
        match &self.mode {
            Mode::Detail(dv) => Some(dv.model_id.clone()),
            Mode::Sql(sv) => Some(sv.model_id.clone()),
            _ => self.focus_target(),
        }
    }

    /// Which menu items are usable for `uid`: SQL preview only when the node
    /// actually has SQL (a source has none — the "unsupported
    /// actions are omitted"; a seed's EMPTY `raw_code` preview would open a
    /// blank modal, so blank counts as unavailable too), the editor row only
    /// when the node has a file path, and Bookmark only for models — the
    /// badge lives on a list row, which non-models don't have. Everything
    /// else is universal.
    fn menu_item_available(&self, uid: &str, action: crate::Action) -> bool {
        match action {
            crate::Action::SqlOpen | crate::Action::YankSql => self
                .dag
                .raw_code(uid)
                .is_some_and(|code| !code.trim().is_empty()),
            crate::Action::BookmarkToggle => self
                .dag
                .get(uid)
                .is_some_and(|n| n.resource_type == "model"),
            crate::Action::OpenEditor => self
                .dag
                .detail(uid)
                .and_then(|d| d.original_file_path.as_deref())
                .is_some(),
            _ => true,
        }
    }

    /// The menu rows for `uid`, generated from [`NODE_MENU_LAYOUT`]: every
    /// item slot resolves to the `(mode, action)` [`BINDINGS`] row it names
    /// (the action + key label + triggers all live there), separators are
    /// kept only between surviving groups (no leading/trailing/double rules),
    /// and unavailable items drop out first. An `uid` outside the Dag (a
    /// `+N more` marker, a stale target) yields an empty list — `open_menu`
    /// turns that into a notice instead of an empty box.
    pub fn menu_entries(&self, uid: &str) -> Vec<MenuEntry> {
        if !self.dag.contains(uid) {
            return Vec::new();
        }
        // `Re-root here` is honest only when Enter would actually re-root —
        // the target is a MODEL that isn't already the re-rooted node. On a
        // non-model (or the root itself) `DetailOpen` opens the structure
        // view instead, so the label says that.
        let re_roots = self.selected_unique_id().as_deref() != Some(uid)
            && self
                .dag
                .get(uid)
                .is_some_and(|n| n.resource_type == "model");
        let mut entries: Vec<MenuEntry> = Vec::new();
        for slot in NODE_MENU_LAYOUT {
            match slot {
                MenuSlot::Separator => {
                    if matches!(entries.last(), Some(MenuEntry::Item { .. })) {
                        entries.push(MenuEntry::Separator);
                    }
                }
                MenuSlot::Item(mode, action) => {
                    if !self.menu_item_available(uid, *action) {
                        continue;
                    }
                    let Some(binding) = crate::action::BINDINGS
                        .iter()
                        .position(|b| b.mode == *mode && b.action == *action)
                    else {
                        continue; // layout slot names a missing row — see tests
                    };
                    let label = menu_slot_label(menu_slot_mark(*action), re_roots);
                    entries.push(MenuEntry::Item { binding, label });
                }
            }
        }
        // A trailing separator left by a fully-filtered final group.
        if matches!(entries.last(), Some(MenuEntry::Separator)) {
            entries.pop();
        }
        entries
    }

    /// Open the node context menu over `uid` at `anchor`: the rows come from
    /// [`menu_entries`](Self::menu_entries) and the box's Esc/execute returns
    /// to whatever mode was live (a modal stays its own `back` — the same
    /// nested-restore mechanism `Mode::Yank` carries).
    pub fn open_menu(&mut self, uid: &str, anchor: MenuAnchor) {
        let items = self.menu_entries(uid);
        if items.is_empty() {
            self.set_notice("no actions for this node");
            return;
        }
        let name = self.node_name_or_uid(uid).to_string();
        let selected = items
            .iter()
            .position(|e| matches!(e, MenuEntry::Item { .. }))
            .unwrap_or(0);
        let back = match std::mem::replace(&mut self.mode, Mode::Selection) {
            Mode::Yank { back, .. } => back, // re-open keeps the outer destination
            Mode::Selection | Mode::Search(_) | Mode::Menu(_) => None,
            m => Some(Box::new(m)),
        };
        self.mode = Mode::Menu(MenuState {
            target: uid.to_string(),
            name,
            items,
            selected,
            back,
            anchor,
        });
    }

    /// A right-click opens the node menu for the clicked node — the same
    /// open path `a` uses, with the pointer cell as the anchor.
    pub(super) fn open_menu_for(&mut self, uid: &str, col: u16, row: u16) {
        self.open_menu(uid, MenuAnchor::Cell { col, row });
    }
}
