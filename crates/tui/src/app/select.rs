use super::*;

impl App {
    /// Re-root the lineage to `unique_id`, recording the move in the active
    /// tab's back stack (and clearing its forward stack) — the browser-style
    /// history for every selection change (§2 T1: picking a model in the list,
    /// clicking, searching, cycling bookmarks all land here). No-op if the
    /// target isn't a selectable model. A re-root also ends any column focus:
    /// "jump to that model" means land on the model's own lineage, not keep
    /// the trace's filtered view.
    pub fn jump_to(&mut self, unique_id: &str) {
        let current = self.selected_unique_id();
        if self.reroot_select(unique_id) {
            // A successful re-root is the overview's drill-in gesture: leave the
            // whole-graph view and land on the rooted lineage of the target.
            if self.ui_state.overview() {
                self.ui_state.toggle_overview();
            }
            if let Some(cur) = current {
                if cur != unique_id {
                    self.tab_mut().back.push(cur);
                    self.tab_mut().forward.clear();
                }
            }
        }
    }

    /// The shared "land on this model" select: `select_by_unique_id` plus the
    /// re-root contract — a successful move ends the column trace, UNLESS the
    /// target IS the traced column's node (clicking/Entering the trace's own
    /// root box is interacting WITH the trace, not leaving it). A failed
    /// select (a non-model lineage node) leaves the focus alone — the same
    /// rule the columns modal's Enter applies. [`jump_to`], history, search
    /// close, the bookmark/gap cycles, and the mouse's list click all go
    /// through here so the contract can't drift between paths.
    ///
    /// [`jump_to`]: App::jump_to
    pub fn reroot_select(&mut self, unique_id: &str) -> bool {
        let trace_root = self.tab().column_focus.as_ref().map(|f| f.col.node.clone());
        if !self.select_by_unique_id(unique_id) {
            return false;
        }
        if trace_root.as_deref() != Some(unique_id) {
            self.tab_mut().column_focus = None;
        }
        true
    }

    /// Go back one step in the active tab's re-root history (pushes the current
    /// onto its forward stack).
    pub fn history_back(&mut self) {
        if let Some(prev) = self.tab_mut().back.pop() {
            if let Some(cur) = self.selected_unique_id() {
                self.tab_mut().forward.push(cur);
            }
            self.reroot_select(&prev);
        }
    }

    /// Go forward one step in the active tab's re-root history (pushes the
    /// current onto its back stack).
    pub fn history_forward(&mut self) {
        if let Some(next) = self.tab_mut().forward.pop() {
            if let Some(cur) = self.selected_unique_id() {
                self.tab_mut().back.push(cur);
            }
            self.reroot_select(&next);
        }
    }

    /// The active tab's root node (any resource kind — resolved on `dag`,
    /// never the list), `None` for a root-less tab. The single accessor for
    /// "the rooted selection"; every `selected_*` projection derives from it.
    /// Distinct from [`list_sel_uid`](App::list_sel_uid) when the root is
    /// filtered out of the active list (the §2 detach).
    pub fn selected_node(&self) -> Option<&NodeInfo> {
        self.tab().root.as_deref().and_then(|uid| self.dag.get(uid))
    }

    /// The active tab's root `unique_id`, if any — what the lineage roots on
    /// and the tab label names.
    pub fn selected_unique_id(&self) -> Option<String> {
        self.tab().root.clone()
    }

    /// The `unique_id` under the list cursor (the model the row highlight
    /// sits on in the ACTIVE list) — what list-driven actions act on: the
    /// list-focus arm of [`focus_target`](App::focus_target), `o`, the
    /// bookmark toggle, a list-search confirm. `None` on an empty list.
    /// Distinct from [`selected_unique_id`](App::selected_unique_id) only
    /// while the root is filtered out of the list.
    pub fn list_sel_uid(&self) -> Option<String> {
        self.active_list()
            .model_at(self.ui_state.selected())
            .map(|n| n.unique_id.clone())
    }

    /// §2 T1: adopt the list cursor's model as the active tab's root — the
    /// browser-link rule that lands every list selection on the tab's own
    /// history. Called after every list-move arm (and by the event loop's
    /// wheel over the list pane); a same-uid landing is a no-op inside
    /// [`jump_to`](App::jump_to).
    pub fn adopt_list_selection(&mut self) {
        if let Some(uid) = self.list_sel_uid() {
            self.jump_to(&uid);
        }
    }

    /// The armed overlay target (`pending_node`) preferred over the list
    /// selection. `pending_node` is armed ONLY for the duration an overlay
    /// owns the action stream — `Mode::Yank`'s modal stash and the node
    /// menu's invoke dispatch window — so selection-scoped actions (the
    /// `y` yanks, bookmark, path anchor, editor open) must consult it or a
    /// node the selection can never land on (a source/seed/snapshot: the
    /// list holds models only) would silently act on the selected model
    /// instead. It can never be read stale: `Mode::Yank`'s close clears it
    /// and `MenuInvoke` clears it right after the apply.
    pub(super) fn pending_or_selection(&self) -> Option<String> {
        self.pending_node.clone().or_else(|| self.list_sel_uid())
    }

    /// The `unique_id` a `y`-menu yank should act on: the structure modal's
    /// node (`pending_node`, stashed when `y` opened the menu over the modal)
    /// preferred over the list selection.
    pub(super) fn yank_target_uid(&self) -> Option<String> {
        self.pending_or_selection()
    }

    /// The uid the open/preview actions (`Enter`/`s`) should act on: the
    /// armed overlay stash preferred over the focus target (lineage cursor
    /// under right-pane focus, else the list selection). A menu opened on a
    /// non-model node arms `pending_node` because `reroot_select` can't land
    /// on it — without the stash the modal would open for the wrong node.
    pub(super) fn pending_or_focus(&self) -> Option<String> {
        self.pending_node.clone().or_else(|| self.focus_target())
    }

    /// Move the selection/cursor to `uid` (the menu's execution contract:
    /// act on the item's node, not wherever the selection happened to be).
    /// Right-pane focus means "the lineage cursor" — a node absent from the
    /// current subgraph can't take the cursor, so it re-roots instead; list
    /// focus means the list selection (`reroot_select`, the shared land-on
    /// path).
    pub(crate) fn move_selection_to(&mut self, uid: &str) {
        if self.ui_state.focus() == crate::ui::Focus::RightPane
            && self.lineage_display_subgraph().contains(uid)
        {
            self.commit_lineage_node(uid);
        } else {
            self.reroot_select(uid);
        }
    }

    /// The selected node's display name, if any.
    pub fn selected_name(&self) -> Option<String> {
        self.selected_node().map(|n| n.name.clone())
    }

    /// The density the lineage pipeline actually renders at: the overview
    /// forces Compact (a whole graph at 3-row boxes is unusably tall), else
    /// the user's `v` toggle. Shared by the cached layout AND `lineage_ascii`
    /// so the yank stays byte-identical to the pane.
    pub(crate) fn effective_density(&self) -> crate::Density {
        if self.ui_state.overview() {
            crate::Density::Compact
        } else {
            self.ui_state.density()
        }
    }

    /// The lineage pane's title body while the whole-graph overview is on
    /// (`"Overview: N nodes"`), `None` otherwise. Precomputed Dag-aware at the
    /// loop seam so `RenderCtx` stays Dag-free.
    pub fn overview_title(&self) -> Option<String> {
        self.ui_state
            .overview()
            .then(|| format!("Overview: {} nodes", self.dag.len()))
    }

    /// A node's display name, falling back to the `unique_id` itself when the node
    /// is unknown to the Dag — the repeated overlay-title idiom.
    pub fn node_name_or_uid(&self, uid: &str) -> String {
        self.dag
            .get(uid)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| uid.to_string())
    }

    /// `uid`'s declaring file path, resolved against the project root
    /// (`<root>/<original_file_path>`). `None` when the node has no recorded
    /// file path or the recorded path would escape the project root:
    /// `original_file_path` comes from an untrusted manifest.json — a crafted
    /// value (absolute, or with a `..` component) would point the `o` editor
    /// jump at an arbitrary file outside the project. Lexical check on
    /// purpose — `canonicalize` fails on not-yet-existing files, and a plain
    /// relative path must keep resolving exactly as before.
    pub fn file_path_for(&self, uid: &str) -> Option<String> {
        let ofp = self.dag.detail(uid)?.original_file_path.clone()?;
        let rel = Path::new(&ofp);
        if rel
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return None;
        }
        Some(self.project_root.join(rel).to_string_lossy().into_owned())
    }

    /// The list-cursor model's SQL file path — [`file_path_for`] on the
    /// list selection (`o` opens the model the row highlight sits on). `None`
    /// if the list is empty.
    pub fn selected_file_path(&self) -> Option<String> {
        self.list_sel_uid().and_then(|uid| self.file_path_for(&uid))
    }

    /// Move the selection to the model with this `unique_id` in the active
    /// list, if present. The selection IS the active tab's root (§2): a
    /// successful select moves the list cursor onto the match, writes the
    /// tab's `root`, and records the model in `recent` — while landing on a
    /// different uid clears the tab's sibling-collapse expansions (the
    /// "initialised on re-root" side of keeping them per root; a same-uid
    /// re-resolve keeps them — it is not a re-root). The selection-by-identity
    /// primitive used by reload-restore, mouse re-root, and lineage search.
    /// Returns whether the id was found.
    pub fn select_by_unique_id(&mut self, unique_id: &str) -> bool {
        let idx = self
            .active_list()
            .models
            .iter()
            .position(|n| n.unique_id == unique_id);
        match idx {
            Some(i) => {
                if self.selected_unique_id().as_deref() != Some(unique_id) {
                    self.tab_mut().collapse_expanded.clear();
                }
                self.ui_state.set_selected(i);
                self.set_root(Some(unique_id.to_string()));
                self.record_recent(unique_id);
                true
            }
            None => false,
        }
    }
}
