use ratatui::layout::Rect;

mod panes;
#[cfg(test)]
use crate::client::ui::release_notes;
mod scrollbar;
mod sidebar;
#[cfg(test)]
use crate::client::ui::status;
mod tab_surface;
pub(crate) use crate::client::ui::panes::{apply_pane_chrome, pane_inner_rect};
#[cfg(test)]
use crate::client::ui::text;
#[cfg(test)]
use crate::client::ui::widgets;

#[cfg(all(test, unix))]
pub(crate) use self::panes::popup_pane_rects;
use self::panes::resize_popup_pane;
pub(crate) use self::panes::{
    new_layout_terminal_sizes, new_pane_terminal_size, pane_is_scrolled_back, NewPanePlacement,
};
#[cfg(test)]
pub(crate) use self::release_notes::{
    release_notes_close_button_rect, release_notes_scroll_metrics, RELEASE_NOTES_MODAL_SIZE,
};
pub(crate) use self::scrollbar::render_pane_scrollbar_buffer;
#[cfg(test)]
pub(crate) use self::scrollbar::{
    release_notes_scrollbar_rect, scrollbar_offset_from_drag_row, scrollbar_offset_from_row,
    scrollbar_thumb,
};
pub(crate) use self::sidebar::{agent_panel_entries_from, AgentPanelEntry};
#[cfg(test)]
pub(crate) use self::status::render_copy_feedback_buffer;
pub(crate) use self::tab_surface::{
    compute_tab_surface, compute_tab_surface_for, render_tab_surface, resize_tab_surface,
    tab_surface_cursor, tab_surface_hyperlinks, TabSurfaceLayout, TabSurfaceTarget, TabSurfaceView,
};
#[cfg(test)]
pub(crate) use self::widgets::{centered_popup_rect, modal_stack_areas};

use crate::app::AppState;
use crate::terminal::TerminalRuntimeRegistry;

pub fn compute_view_with_runtime_registry(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        true,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

pub fn compute_view_with_cell_size(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    compute_view_internal(app, terminal_runtimes, area, true, cell_size);
}

pub(crate) fn compute_view_without_resizing_panes(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        false,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

fn compute_view_internal(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let TabSurfaceLayout { pane_infos, .. } =
        compute_tab_surface(app, terminal_runtimes, area, resize_panes, cell_size);

    if resize_panes {
        resize_background_tab_panes(app, terminal_runtimes, area, cell_size);
        resize_popup_pane(app, terminal_runtimes, area, cell_size);
    }

    app.view = crate::app::ViewState {
        terminal_area: area,
        pane_infos,
    };
}

fn resize_background_tab_panes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (workspace_index, workspace) in app.workspaces.iter().enumerate() {
        for tab_index in 0..workspace.tabs.len() {
            if app.active == Some(workspace_index) && tab_index == workspace.active_tab_index() {
                continue;
            }
            resize_tab_surface(
                app,
                terminal_runtimes,
                workspace_index,
                tab_index,
                area,
                cell_size,
            );
        }
    }
}
