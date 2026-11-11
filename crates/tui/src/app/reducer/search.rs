use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- search ----
        Action::SearchOpen => {
            // Over the help overlay, `/` arms the line filter instead — the
            // Search* editing actions route into it by mode (see dispatch's
            // editing pre-emption).
            if let Mode::Help(h) = &mut app.mode {
                h.editing = true;
                h.filter.get_or_insert_with(String::new);
                h.scroll = 0;
                return Arm::Early(Outcome::cont());
            }
            let target = if app.home_active() {
                // `/` on home searches models for a TAB to open —
                // no list cursor exists, so there is no origin to restore.
                SearchTarget::Home
            } else if app.ui_state.focus() == Focus::List {
                SearchTarget::List
            } else {
                SearchTarget::Lineage
            };
            // The search's restore point: the list CURSOR (what Esc lands
            // back on), not the root — under a detached root the cursor sits
            // on a different model. A home-target search has NO origin —
            // restoring one would re-root nothing yet still record the
            // unseen list-cursor model into the Recent MRU.
            let origin = if matches!(target, SearchTarget::Home) {
                None
            } else {
                app.list_sel_uid()
            };
            app.mode = Mode::Search(SearchState {
                target,
                query: String::new(),
                origin_uid: origin,
                match_idx: 0,
                hist: None,
            });
            app.refilter(); // no-op for a lineage-target search
            Outcome::cont()
        }
        Action::SearchType(c) => {
            // Route by mode: the palette and the search share the action, never
            // the state. In the palette, typing only ever NARROWS the candidate
            // list, so reset `selected` to 0 (which also satisfies the
            // "selection clamps when the filter shrinks" contract).
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.push(c);
                    s.hist = None; // editing leaves the history browse
                    s.match_idx = 0; // query changed → matches changed
                    let lineage = s.target == SearchTarget::Lineage;
                    app.refilter();
                    // A hidden (collapsed-away) match auto-expands its group so
                    // the loop's anchor has a rect to land on.
                    if lineage {
                        app.reveal_current_lineage_match();
                    }
                }
                Mode::Palette(p) => {
                    p.query.push(c);
                    p.selected = 0;
                }
                // The help `/`-filter: typing edits it (and pulls the overlay
                // back to the top — the match set changed under it).
                Mode::Help(h) if h.editing => {
                    if let Some(f) = h.filter.as_mut() {
                        f.push(c);
                    }
                    h.scroll = 0;
                }
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchBackspace => {
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.pop();
                    s.hist = None;
                    s.match_idx = 0;
                    let lineage = s.target == SearchTarget::Lineage;
                    app.refilter();
                    if lineage {
                        app.reveal_current_lineage_match();
                    }
                }
                Mode::Palette(p) => {
                    p.query.pop();
                    // Reset to the top EXACTLY like the SearchType arm, so the
                    // highlighted row is a pure function of the query — the same
                    // query always lands the same row regardless of arrival path
                    // (typing vs. backspacing). (Previously kept-and-clamped, which
                    // left ~1/3 of backspace transitions on an unrelated command.)
                    p.selected = 0;
                }
                Mode::Help(h) if h.editing => {
                    if let Some(f) = h.filter.as_mut() {
                        f.pop();
                    }
                    h.scroll = 0;
                }
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchMove(dir) => {
            // Command palette: move `selected` over the SELECTABLE rows (the
            // section headers are display-only, never a target), clamped at
            // both ends (no wrap), mirroring a list cursor. An `@`-prefixed
            // query lists MODELS (`palette_selectable_rows` routes to the
            // model rows) — commands otherwise.
            if let Mode::Palette(p) = &app.mode {
                let count = app.palette_selectable_rows(&p.query).len();
                let next = match dir {
                    Direction::Down if count > 0 => (p.selected + 1).min(count - 1),
                    Direction::Up => p.selected.saturating_sub(1),
                    _ => p.selected,
                };
                if let Mode::Palette(p) = &mut app.mode {
                    p.selected = next;
                }
                return Arm::Early(Outcome::cont());
            }
            // List search: while the query is EMPTY — or an Up/Down browse is
            // already live — the arrows recall the saved search history
            // (older on Up, newer on Down; Down past the newest entry returns
            // to the live empty query). A non-empty live query keeps the
            // original filtered-list move. Lineage search: cycle matches.
            let target = match &app.mode {
                Mode::Search(s) => Some((s.target, s.query.clone(), s.hist)),
                _ => None,
            };
            match target {
                Some((SearchTarget::List, query, hist)) => {
                    let browsing = hist.is_some() || query.is_empty();
                    let nav = matches!(dir, Direction::Up | Direction::Down);
                    if browsing && nav && !app.search_history.is_empty() {
                        let next: Option<usize> = match (dir, hist) {
                            (Direction::Up, None) => Some(0),
                            (Direction::Up, Some(i)) => {
                                Some((i + 1).min(app.search_history.len() - 1))
                            }
                            // Down past the newest (or at the live position):
                            // back to the live empty query — a no-op when
                            // already live.
                            (Direction::Down, Some(0)) | (Direction::Down, None) => None,
                            (Direction::Down, Some(i)) => Some(i - 1),
                            _ => hist,
                        };
                        let recalled = next.map(|i| app.search_history[i].clone());
                        if let Mode::Search(s) = &mut app.mode {
                            s.hist = next;
                            s.query = recalled.unwrap_or_default();
                            s.match_idx = 0;
                        }
                        app.refilter();
                    } else {
                        // Browsing IS selecting: a moved cursor adopts
                        // its model — same changed-index guard as move keys.
                        let before = app.ui_state.selected();
                        match dir {
                            Direction::Down => {
                                reduce_selection(&mut app.ui_state, Action::MoveDown)
                            }
                            Direction::Up => reduce_selection(&mut app.ui_state, Action::MoveUp),
                            _ => {}
                        }
                        if app.ui_state.selected() != before {
                            app.adopt_list_selection();
                        }
                    }
                }
                Some((SearchTarget::Lineage, query, _)) => {
                    let count = app.lineage_matches(&query).len();
                    if count > 0 {
                        if let Mode::Search(s) = &mut app.mode {
                            s.match_idx = match dir {
                                Direction::Down => (s.match_idx + 1) % count,
                                Direction::Up => (s.match_idx + count - 1) % count,
                                _ => s.match_idx,
                            };
                        }
                        // Cycling to a hidden match expands its group — the
                        // anchor needs the match drawn to land on it.
                        app.reveal_current_lineage_match();
                    }
                }
                Some((SearchTarget::Home, query, hist)) => {
                    // Same recall contract as the list target: Up/Down on an
                    // EMPTY query (or a live browse) walks `search_history`;
                    // a typed query moves `match_idx` over the result rows
                    // (`home_data` recomputes them — the reducer and the
                    // renderer share one source).
                    let browsing = hist.is_some() || query.is_empty();
                    let nav = matches!(dir, Direction::Up | Direction::Down);
                    if browsing && nav && !app.search_history.is_empty() {
                        let next: Option<usize> = match (dir, hist) {
                            (Direction::Up, None) => Some(0),
                            (Direction::Up, Some(i)) => {
                                Some((i + 1).min(app.search_history.len() - 1))
                            }
                            (Direction::Down, Some(0)) | (Direction::Down, None) => None,
                            (Direction::Down, Some(i)) => Some(i - 1),
                            _ => hist,
                        };
                        let recalled = next.map(|i| app.search_history[i].clone());
                        if let Mode::Search(s) = &mut app.mode {
                            s.hist = next;
                            s.query = recalled.unwrap_or_default();
                            s.match_idx = 0;
                        }
                    } else {
                        let count = app.home_search_len();
                        if count > 0 {
                            if let Mode::Search(s) = &mut app.mode {
                                s.match_idx = match dir {
                                    Direction::Down => (s.match_idx + 1).min(count - 1),
                                    Direction::Up => s.match_idx.saturating_sub(1),
                                    _ => s.match_idx,
                                };
                            }
                        }
                    }
                }
                None => {}
            }
            Outcome::cont()
        }
        Action::SearchCancel => {
            // Palette cancel just closes the overlay — no filter/list to restore.
            if matches!(app.mode, Mode::Palette(_)) {
                app.mode = app.idle_mode();
                return Arm::Early(Outcome::cont());
            }
            let origin = match &app.mode {
                Mode::Search(s) => s.origin_uid.clone(),
                _ => None,
            };
            // Cancel restores the pre-search root — folds untouched (the
            // unfold-on-confirm policy is the CONFIRM arm's alone).
            app.close_search(origin, false);
            Outcome::cont()
        }
        Action::SearchConfirm => {
            // Help `/`-filter: Enter COMMITS the filter — editing ends but
            // the filter stays applied (Esc clears it outright). A committed
            // EMPTY filter normalizes away (Some("") filters nothing yet
            // would still ride the title and swallow an Esc). Outside editing
            // Help swallows the action — no Enter binding exists there anyway.
            if let Mode::Help(h) = &mut app.mode {
                if h.editing {
                    h.editing = false;
                    if h.filter.as_deref() == Some("") {
                        h.filter = None;
                    }
                }
                return Arm::Early(Outcome::cont());
            }
            // Palette confirm: resolve the selected row (headers aren't
            // selectable, so `selected` always lands on a Command or Model —
            // `None` only when zero candidates exist, which still closes).
            // `selected` can overshoot when the candidates shrank under a
            // FIXED query (e.g. a --watch reload mid-palette): clamp to the
            // last row rather than treating it as "no candidates".
            // `PaletteRow` is owned, so the mode borrow ends before mutation.
            if let Mode::Palette(p) = &app.mode {
                let new_tab = p.new_tab;
                let rows = app.palette_selectable_rows(&p.query);
                let chosen = if rows.is_empty() {
                    None
                } else {
                    rows.get(p.selected.min(rows.len() - 1)).cloned()
                };
                return Arm::Early(match chosen {
                    Some(PaletteRow::Command {
                        action,
                        unavailable,
                        ..
                    }) => {
                        // A greyed command can't run: keep the palette open
                        // and toast WHY (the "reason on Enter" rule).
                        if let Some(reason) = unavailable {
                            if let Mode::Palette(p) = &mut app.mode {
                                p.selected = p.selected.min(rows.len() - 1);
                            }
                            app.set_notice(reason);
                            Outcome::cont()
                        } else {
                            app.record_palette_run(action);
                            app.mode = app.idle_mode();
                            // Close FIRST, then apply the chosen action and
                            // propagate its WHOLE Outcome (so "quit" quits and
                            // "$EDITOR" yields the effect) — same contract the
                            // plain `palette_candidates` path had. The flag
                            // carries the resolved action's re-anchor intent —
                            // the loop's dispatched-key check can't see it.
                            let mut out = apply_action(app, action);
                            out.reanchor |= crate::action::reanchors_lineage(action);
                            out
                        }
                    }
                    // `@` model search: Enter re-roots to the model — the
                    // "active tab root" rule (`jump_to` records history like
                    // a click does). The `+` cell's `new_tab` palette instead
                    // opens the model AS a new tab (dedupes to its tab when
                    // already open).
                    Some(PaletteRow::Model { uid, .. }) => {
                        app.mode = app.idle_mode();
                        if new_tab {
                            app.open_node_as_tab(&uid);
                        } else {
                            app.jump_to(&uid);
                        }
                        Outcome::cont()
                    }
                    Some(PaletteRow::Header(_)) | None => {
                        app.mode = app.idle_mode();
                        Outcome::cont()
                    }
                });
            }
            let target = match &app.mode {
                Mode::Search(s) => Some((s.target, s.query.clone())),
                _ => None,
            };
            match target {
                Some((SearchTarget::Lineage, _)) => {
                    // Jump to the currently-cycled match: if it is a selectable
                    // model, re-root to it (recording history); else just leave.
                    let hit = app.current_lineage_match();
                    app.mode = app.idle_mode();
                    if let Some(uid) = hit {
                        app.jump_to(&uid);
                    }
                }
                Some((SearchTarget::Home, query)) => {
                    // Home search: Enter opens the highlighted result AS A
                    // TAB (dedupes) and records the query — the same
                    // `search_history` the list target's recall browses. The
                    // close restores nothing (home has no list cursor).
                    app.record_search_query(&query);
                    let picked = app.home_search_pick();
                    app.close_search(None, false);
                    if let Some(uid) = picked {
                        app.open_node_as_tab(&uid);
                    }
                }
                other => {
                    // List search: keep the highlighted match — the list
                    // CURSOR's model (resolve by id in the full list) — and
                    // record the confirmed query: the `/`-mode Up/Down recall
                    // browses this history.
                    if let Some((SearchTarget::List, query)) = &other {
                        app.record_search_query(query);
                    }
                    let chosen = app.list_sel_uid();
                    // A confirmed match may live in a folded group (search
                    // ignores folds): unfold its group so the pick lands.
                    app.close_search(chosen, true);
                }
            }
            Outcome::cont()
        }
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
