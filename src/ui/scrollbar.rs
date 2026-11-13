use crate::app::AppState;
pub(crate) use crate::client::ui::scrollbar::*;
use crate::layout::PaneInfo;
use ratatui::Frame;

pub(super) fn render_pane_scrollbar(
    app: &AppState,
    frame: &mut Frame,
    info: &PaneInfo,
    rt: &crate::terminal::TerminalRuntime,
) {
    let Some(metrics) = rt.scroll_metrics() else {
        return;
    };
    let Some(track) = pane_scrollbar_rect(info) else {
        return;
    };
    render_pane_scrollbar_buffer(
        frame.buffer_mut(),
        metrics,
        track,
        &app.palette,
        info.is_focused,
    );
}

fn pane_scrollbar_rect(info: &PaneInfo) -> Option<ratatui::layout::Rect> {
    info.scrollbar_rect
}
