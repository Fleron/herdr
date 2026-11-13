use super::*;

impl ClientShellState {
    pub(crate) fn has_presented_surface(&self) -> bool {
        self.pane_surface.is_some()
    }
}
