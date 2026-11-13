use super::*;

impl ClientShellState {
    pub(in crate::client::shell) fn link_hover_blocks_patch(
        &self,
        patch: &crate::client::frame::PaneSurfacePatch,
    ) -> bool {
        self.link_hover.as_ref().is_some_and(|hover| {
            !hover.regions.is_empty()
                && patch
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == hover.target.pane_id)
        })
    }
}
