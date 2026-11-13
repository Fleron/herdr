use std::collections::{HashMap, HashSet, VecDeque};

#[path = "shell/actions.rs"]
mod actions;
#[path = "shell/agent_sidebar.rs"]
mod agent_sidebar;
#[path = "shell/aggregate_navigation.rs"]
mod aggregate_navigation;
#[path = "shell/machine_diagnostics.rs"]
mod machine_diagnostics;
#[path = "shell/workspace_navigation.rs"]
mod workspace_navigation;
use workspace_navigation::{PendingWorkspaceHighlight, WorkspaceNavigationTarget};
#[path = "shell/checkouts.rs"]
mod checkouts;
#[path = "shell/composition.rs"]
mod composition;
#[path = "shell/config.rs"]
mod config;
#[path = "shell/context_menu.rs"]
mod context_menu;
#[path = "shell/copy_mode.rs"]
mod copy_mode;
#[path = "shell/endpoint_agent_state.rs"]
mod endpoint_agent_state;
#[path = "shell/endpoint_agents.rs"]
mod endpoint_agents;
#[path = "shell/endpoint_navigation.rs"]
mod endpoint_navigation;
#[path = "shell/endpoint_notices.rs"]
mod endpoint_notices;
#[path = "shell/endpoint_sidebar.rs"]
mod endpoint_sidebar;
#[path = "shell/endpoints.rs"]
mod endpoints;
pub(crate) use endpoints::*;
#[path = "shell/global_menu.rs"]
mod global_menu;
#[path = "shell/graphics.rs"]
mod graphics;
#[path = "shell/input.rs"]
mod input;
#[path = "shell/input_source.rs"]
mod input_source;
#[path = "shell/link_hover.rs"]
mod link_hover;
#[path = "shell/mobile.rs"]
mod mobile;
#[path = "shell/mouse.rs"]
mod mouse;
#[path = "shell/notification_policy.rs"]
mod notification_policy;
#[path = "shell/notifications.rs"]
mod notifications;
#[path = "shell/overlay_input.rs"]
mod overlay_input;
#[path = "shell/preferences.rs"]
mod preferences;
#[path = "shell/render.rs"]
mod render;
#[path = "shell/scroll.rs"]
mod scroll;
#[path = "shell/settings.rs"]
mod settings;
#[path = "shell/state.rs"]
mod state;
legacy_shell_extensions! {
    #[path = "shell/surface_patch.rs"]
    pub(super) mod surface_patch;
}
#[path = "shell/text_editor.rs"]
mod text_editor;
#[path = "shell/word_selection.rs"]
mod word_selection;
#[path = "shell/worktrees.rs"]
mod worktrees;
use text_editor::TextEditor;
use word_selection::ClientWordSelection;

pub(in crate::client::shell) use render::sidebar;
pub(crate) use state::*;

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use super::endpoint::{ClientEndpointId, ClientEndpointStatus};
use crate::client::frame::{
    ClientMousePosition, ClientPaneInputEvent, ClientSurfaceSize, FrameData, SemanticNotification,
    SemanticNotificationKind, SemanticNotificationSound,
};
use crate::config::{
    Config, LiveKeybindConfig, SidebarCollapsedModeConfig, SpacesSidebarConfig,
    TabBarPositionConfig,
};
use crate::palette::Palette;

use super::ui_action::UiRequest;
use super::ui_model::{
    ClientShellAvailableCheckout, ClientShellSnapshot, ClientShellTab, ClientShellWorkspace,
    PaneSurfaceFrame,
};

fn target_event_message(target: ClientInputTarget, event: ClientPaneInputEvent) -> UiRequest {
    match target {
        ClientInputTarget::Pane(pane_id) => UiRequest::PaneInput {
            pane_id,
            events: vec![event],
        },
        ClientInputTarget::Popup(terminal_id) => UiRequest::PopupInput {
            terminal_id,
            events: vec![event],
        },
    }
}

fn push_target_event(
    target: ClientInputTarget,
    event: ClientPaneInputEvent,
    outcome: &mut ClientShellInput,
) {
    match target {
        ClientInputTarget::Pane(pane_id) => {
            if let Some(UiRequest::PaneInput {
                pane_id: pending_pane,
                events,
            }) = outcome.requests.last_mut()
            {
                if *pending_pane == pane_id {
                    events.push(event);
                    return;
                }
            }
            outcome.requests.push(target_event_message(
                ClientInputTarget::Pane(pane_id),
                event,
            ));
        }
        ClientInputTarget::Popup(terminal_id) => {
            if let Some(UiRequest::PopupInput {
                terminal_id: pending_terminal,
                events,
            }) = outcome.requests.last_mut()
            {
                if *pending_terminal == terminal_id {
                    events.push(event);
                    return;
                }
            }
            outcome.requests.push(target_event_message(
                ClientInputTarget::Popup(terminal_id),
                event,
            ));
        }
    }
}

fn contains(rect: Rect, point: (u16, u16)) -> bool {
    rect.width > 0
        && rect.height > 0
        && point.0 >= rect.x
        && point.0 < rect.right()
        && point.1 >= rect.y
        && point.1 < rect.bottom()
}

fn pane_surface_topology_signature(surface: &PaneSurfaceFrame) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn write(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(PRIME);
        }
        *hash ^= 0xff;
        *hash = hash.wrapping_mul(PRIME);
    }

    let mut pane_ids = surface
        .panes
        .iter()
        .map(|pane| pane.pane_id.as_bytes())
        .collect::<Vec<_>>();
    pane_ids.sort_unstable();
    let mut hash = OFFSET;
    for pane_id in pane_ids {
        write(&mut hash, pane_id);
    }
    let mut splits = surface.splits.iter().collect::<Vec<_>>();
    splits.sort_by(|left, right| left.path.cmp(&right.path));
    for split in splits {
        write(
            &mut hash,
            &[match split.direction {
                crate::client::ui_model::PaneSurfaceSplitDirection::Horizontal => 0,
                crate::client::ui_model::PaneSurfaceSplitDirection::Vertical => 1,
            }],
        );
        write(
            &mut hash,
            &split
                .path
                .iter()
                .map(|right| u8::from(*right))
                .collect::<Vec<_>>(),
        );
    }
    hash
}

fn status_icon(
    status: crate::client::ui_domain::AgentStatus,
    style: crate::config::StatusIndicatorStyle,
) -> &'static str {
    use crate::client::ui_domain::AgentStatus;
    use crate::config::StatusIndicatorStyle;
    match (style, status) {
        (
            StatusIndicatorStyle::Dots,
            AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done,
        ) => "●",
        (StatusIndicatorStyle::Dots, AgentStatus::Idle) => "○",
        (StatusIndicatorStyle::Dots, AgentStatus::Unknown) => "·",
        (StatusIndicatorStyle::Symbols, AgentStatus::Blocked) => "×",
        (StatusIndicatorStyle::Symbols, AgentStatus::Working) => "◐",
        (StatusIndicatorStyle::Symbols, AgentStatus::Done) => "✓",
        (StatusIndicatorStyle::Symbols, AgentStatus::Idle) => "○",
        (StatusIndicatorStyle::Symbols, AgentStatus::Unknown) => "·",
    }
}

fn status_dot(status: crate::client::ui_domain::AgentStatus) -> &'static str {
    status_icon(status, crate::config::StatusIndicatorStyle::Dots)
}

fn status_priority(status: crate::client::ui_domain::AgentStatus) -> u8 {
    use crate::client::ui_domain::AgentStatus;
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

fn status_text(status: crate::client::ui_domain::AgentStatus) -> &'static str {
    use crate::client::ui_domain::AgentStatus;
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

fn status_color(
    status: crate::client::ui_domain::AgentStatus,
    palette: &Palette,
) -> ratatui::style::Color {
    use crate::client::ui_domain::AgentStatus;
    match status {
        AgentStatus::Working => palette.yellow,
        AgentStatus::Blocked => palette.red,
        AgentStatus::Done => palette.teal,
        AgentStatus::Idle => palette.green,
        AgentStatus::Unknown => palette.overlay0,
    }
}

fn panel_contrast_fg(palette: &Palette) -> ratatui::style::Color {
    match palette.panel_bg {
        ratatui::style::Color::Reset => palette.surface_dim,
        color => color,
    }
}

fn blit_pane_surface(target: &mut FrameData, source: &FrameData, area: Rect) {
    let copy_width = source.width.min(area.width);
    let copy_height = source.height.min(area.height);
    let hyperlink_base = target.hyperlinks.len() as u32;
    target.hyperlinks.extend(source.hyperlinks.iter().cloned());

    for row in 0..copy_height {
        for col in 0..copy_width {
            let source_index = row as usize * source.width as usize + col as usize;
            let target_x = area.x + col;
            let target_y = area.y + row;
            let target_index = target_y as usize * target.width as usize + target_x as usize;
            let (Some(source_cell), Some(target_cell)) = (
                source.cells.get(source_index),
                target.cells.get_mut(target_index),
            ) else {
                continue;
            };
            *target_cell = source_cell.clone();
            target_cell.hyperlink = source_cell.hyperlink.and_then(|index| {
                ((index as usize) < source.hyperlinks.len()).then_some(hyperlink_base + index)
            });
        }
    }

    target.cursor = source.cursor.as_ref().and_then(|cursor| {
        (cursor.x < copy_width && cursor.y < copy_height).then(|| {
            crate::client::frame::CursorState {
                x: area.x + cursor.x,
                y: area.y + cursor.y,
                visible: cursor.visible,
                shape: cursor.shape,
            }
        })
    });
    target.graphics.clear();
}

legacy_shell_characterization_tests!("shell/tests/mod.rs");

#[cfg(test)]
#[path = "shell/test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "shell/core_tests.rs"]
mod core_tests;

core_shell_extensions! {
    #[path = "shell/core_adapter.rs"]
    pub(crate) mod core_adapter;
}
