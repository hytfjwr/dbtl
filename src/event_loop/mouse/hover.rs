use super::*;

/// The settled-hover tooltip's live state: which node's hover
/// armed it, the lineage viewport at arm time, when the rest began, and
/// whether the [`TOOLTIP_DELAY`] timer already fired. The loop owns the
/// clock; both the reducer and the renderer stay clock-free.
pub struct HoverTip {
    pub uid: String,
    /// The `(scroll_x, scroll_y)` the arm read — a viewport move since then
    /// left the anchored box pointing at stale content, so it re-arms.
    pub scroll: (usize, usize),
    pub since: Instant,
    pub visible: bool,
}

/// The stationary-hover delay before the tooltip shows.
pub const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// Reconcile the settled-hover state against the freshest inputs (the
/// dismissal inputs: press / drag / scroll / leave). The tip arms only while the
/// resolved hover rests on a lineage node that is ALSO a real Dag node —
/// a `+M more` marker is display chrome: it accents its frame but has no
/// `dag.get`, so arming would just dirty the 500ms flip for a `tip_view`
/// that is always `None`. A held button (press or pan), a viewport move
/// under a same-node pointer (the anchor went stale), or a target change
/// re-arms/drops; `visible` only ever flips on the idle-tick timer.
pub fn reconcile_hover_tip(
    hover_tip: &mut Option<HoverTip>,
    hover: Option<&HitTarget>,
    pressed: bool,
    scroll: (usize, usize),
    dag: &dbtl::Dag,
) {
    let hover_uid = match (hover, pressed) {
        (Some(HitTarget::LineageNode(uid)), false) => Some(uid.clone()),
        _ => None,
    };
    match (&mut *hover_tip, &hover_uid) {
        // Same node AND the same viewport: the rest continues undisturbed.
        (Some(t), Some(uid)) if t.uid == *uid && t.scroll == scroll => {}
        (tip, armed) => {
            *tip = armed
                .clone()
                .filter(|uid| dag.get(uid).is_some())
                .map(|uid| HoverTip {
                    uid,
                    scroll,
                    since: Instant::now(),
                    visible: false,
                });
        }
    }
}

/// Route a hover to the live-mode hit-test and track the resolved target —
/// the `Moved` redraw gate reads the change flag this returns. Menu rows
/// take a hover cursor: the pointer visibly selects (separators are skipped
/// by the arm).
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_hover(
    app: &mut App,
    mouse: &mut MouseRouter,
    outcome: &mut Outcome,
    at: ratatui::layout::Position,
    chrome: &ChromeGeom,
    lineage: Option<&Layout>,
    area: Rect,
    overlay: Option<Rect>,
) -> bool {
    let target = {
        let g = chrome.hit_geometry(app, lineage, area, overlay);
        hit_target(&g, at.x, at.y)
    };
    // Menu rows take a hover cursor — the pointer visibly selects
    // (separators are skipped by the arm).
    if let (Mode::Menu(_), Some(HitTarget::OverlayRow(i))) = (&app.mode, target.as_ref()) {
        apply_mouse(app, outcome, Action::MenuSelect(*i));
    }
    mouse.track_hover(target)
}
