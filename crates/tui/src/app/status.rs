//! The status-line and pane-title producers the event loop's draw + hit-test
//! share: which contextual hints the `BINDINGS`
//! table offers in the current context, and which [`Action`] a status chip's
//! click dispatches. These are the Dag/App-aware seams — the pure placement
//! math lives in [`crate::ui::status_layout`] /
//! [`crate::ui::title_layout`], so the reducer stays size-unaware.

use crate::action::{status_hints, Action, StatusHint};
use crate::ui::{StatusChip, TitleLayout};

use super::App;

impl App {
    /// The contextual status hints for the CURRENT mode/focus — `BINDINGS`-
    /// generated (`status_hints`), so they can never teach a key that doesn't
    /// dispatch. A live column trace or the overview supersedes the pane
    /// context exactly like the context table prescribes. The event loop may
    /// REPLACE this list with a transient readout (splitter hover/drag —
    /// `action: None`, the pointer is the gesture).
    pub fn status_hints(&self) -> Vec<StatusHint> {
        status_hints(
            self.mode.kind(),
            self.ui_state.focus(),
            self.tab().column_focus.is_some(),
            self.ui_state.overview(),
        )
    }

    /// The action a status chip's click dispatches (the chip table) —
    /// `None` for the inert readouts (impact / scope / position / the
    /// persistent-filter tag). The lens chips go through
    /// [`Action::SetLens`], not `CycleLens`: a click can pick a specific lens
    /// directly, and `cov` toggles OUT again when its lens is already on.
    pub fn status_chip_action(&self, chip: StatusChip) -> Option<Action> {
        match chip {
            StatusChip::Notify => Some(Action::NotificationsOpen),
            StatusChip::Column => Some(Action::ColumnsOpen),
            StatusChip::Diff => Some(Action::DiffOpen),
            StatusChip::Run => Some(Action::SetLens(crate::ui::LineageLens::RunStatus)),
            StatusChip::Coverage => Some(Action::SetLens(
                if self.ui_state.lens() == crate::ui::LineageLens::Coverage {
                    crate::ui::LineageLens::Off
                } else {
                    crate::ui::LineageLens::Coverage
                },
            )),
            StatusChip::Bookmarks => Some(Action::BookmarkCycle),
            StatusChip::Sort => Some(Action::SortCycle),
            StatusChip::Focus => Some(Action::ToggleFocus),
            // In the overview the `view` chip reads `[overview]` and ResetView
            // is gated to a toast — the useful affordance there is leaving
            // the overview, same shape as `w`.
            StatusChip::View => Some(if self.ui_state.overview() {
                Action::ToggleOverview
            } else {
                Action::ResetView
            }),
            StatusChip::Impact | StatusChip::Scope | StatusChip::Position | StatusChip::Filter => {
                None
            }
        }
    }

    /// The lineage pane's title placement — the same `title_layout` call
    /// `draw_lineage_pane` makes internally, resolved from the same pieces
    /// the loop's [`RenderCtx`](crate::ui::RenderCtx) build supplies, so the
    /// hit-test's spans overlay exactly what the title row drew. The pane
    /// width is the only size-aware input; everything else is App state.
    /// `search` carries the query AND its cycled match position (the `i/N`).
    #[allow(clippy::too_many_arguments)]
    pub fn lineage_title(
        &self,
        search: Option<(&str, Option<(usize, usize)>)>,
        view_label: Option<&str>,
        breadcrumb: Option<&str>,
        overview: Option<&str>,
        col_suffix: Option<&str>,
        total_w: u16,
    ) -> TitleLayout {
        let sel_name = self
            .active_list()
            .model_at(self.ui_state.selected())
            .map(|n| n.name.as_str())
            .unwrap_or("-");
        let caret = crate::ui::chrome(self.glyph_mode).caret;
        crate::ui::title_layout(
            sel_name,
            search,
            view_label,
            breadcrumb,
            overview,
            self.ui_state.lens(),
            col_suffix,
            caret,
            total_w,
        )
    }
}
