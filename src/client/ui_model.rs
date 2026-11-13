pub(crate) use super::frame::{
    ClientShellPopupSize, ClientShellPopupSurface, PaneSurfacePane, PaneSurfaceSplitDirection,
    SurfaceRect,
};
use crate::client::frame::FrameData;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RuntimeTerminalTarget {
    pub machine_id: String,
    pub terminal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentViewTarget {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellSnapshot {
    pub runtime_controls: Option<RuntimeControls>,
    pub core_terminals: Option<Vec<CoreTerminalView>>,
    pub available_checkouts: Vec<std::sync::Arc<ClientShellAvailableCheckout>>,
    pub boot_id: String,
    pub revision: u64,
    pub config_diagnostic: Option<String>,
    pub product_announcement: Option<ClientShellProductAnnouncement>,
    pub update_available: Option<String>,
    pub update_install_command: String,
    pub server_keybindings_toml: Option<String>,
    pub latest_release_notes_available: bool,
    pub integration_updates_available: bool,
    pub worktree_directory: String,
    pub release_notes: Option<ClientShellReleaseNotes>,
    pub focused_workspace_id: Option<String>,
    pub focused_tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
    pub tab_bar_right: Vec<ClientShellTabStatusSegment>,
    pub tab_bar_right_separator: String,
    pub agent_view_label: Option<String>,
    pub agent_order: Vec<String>,
    pub workspaces: Vec<ClientShellWorkspace>,
    pub tabs: Vec<ClientShellTab>,
    pub panes: Vec<ClientShellPane>,
    pub agents: Vec<ClientShellAgent>,
    pub commands: Vec<ClientShellCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellProductAnnouncement {
    pub version: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub preview: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellReleaseNotes {
    pub version: String,
    pub body: String,
    pub preview: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub(crate) enum ClientShellCommandAction {
    Shell,
    Pane,
    Popup,
    PluginAction,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellCommand {
    pub command_id: String,
    pub binding_label: String,
    pub binding_labels: Vec<String>,
    pub action: ClientShellCommandAction,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellTabStatusSegment {
    pub text: String,
    pub accent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellWorkspace {
    pub workspace_id: String,
    pub active_tab_id: String,
    pub new_workspace_cwd: String,
    pub new_workspace_label: String,
    pub number: usize,
    pub label: String,
    pub custom_label: bool,
    pub branch: Option<String>,
    pub git_ahead_behind: Option<(usize, usize)>,
    pub tokens: Vec<(String, String)>,
    pub worktree: Option<ClientShellWorktree>,
    pub focused: bool,
    pub agent_status: crate::client::ui_domain::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellWorktree {
    pub key: String,
    pub label: String,
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellTab {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub custom_label: bool,
    pub zoomed: bool,
    pub focused: bool,
    pub agent_status: crate::client::ui_domain::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellPane {
    pub runtime_target: Option<RuntimeTerminalTarget>,
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub focused: bool,
    pub right_click_passthrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellAgent {
    pub cwd: Option<String>,
    pub agent_id: String,
    pub runtime_target: Option<RuntimeTerminalTarget>,
    pub view: Option<AgentViewTarget>,
    pub name: Option<String>,
    pub display_agent: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_status: crate::client::ui_domain::AgentStatus,
    pub state_change_seq: u64,
    pub state_labels: Vec<(String, String)>,
    pub tokens: Vec<(String, String)>,
    pub focused: bool,
}

impl From<crate::config::CustomCommandAction> for ClientShellCommandAction {
    fn from(action: crate::config::CustomCommandAction) -> Self {
        match action {
            crate::config::CustomCommandAction::Shell => Self::Shell,
            crate::config::CustomCommandAction::Pane => Self::Pane,
            crate::config::CustomCommandAction::Popup => Self::Popup,
            crate::config::CustomCommandAction::PluginAction => Self::PluginAction,
        }
    }
}
impl TryFrom<ClientShellCommandAction> for crate::config::CustomCommandAction {
    type Error = ();
    fn try_from(action: ClientShellCommandAction) -> Result<Self, Self::Error> {
        match action {
            ClientShellCommandAction::Shell => Ok(Self::Shell),
            ClientShellCommandAction::Pane => Ok(Self::Pane),
            ClientShellCommandAction::Popup => Ok(Self::Popup),
            ClientShellCommandAction::PluginAction => Ok(Self::PluginAction),
            ClientShellCommandAction::Unknown => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointAgentCompletions {
    pub boot_id: String,
    pub revision: u64,
    pub completions: std::collections::BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RuntimeControls {
    pub held_input: usize,
    pub pending_launch: bool,
    pub pending_layout: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoreTerminalView {
    pub target: RuntimeTerminalTarget,
    pub machine_label: String,
    pub label: String,
    pub cwd: Option<String>,
    pub status: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientShellAvailableCheckout {
    pub error: Option<String>,
    pub agent_status: crate::client::ui_domain::AgentStatus,
    pub machine_id: String,
    pub folder: String,
    pub label: String,
    pub branch: Option<String>,
    pub git_ahead_behind: Option<(usize, usize)>,
    pub worktree: Option<ClientShellWorktree>,
    pub terminals: Vec<CoreTerminalView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneSurfaceSplit {
    pub split_id: Option<String>,
    pub direction: PaneSurfaceSplitDirection,
    pub pos: u16,
    pub area: SurfaceRect,
    pub hit_rect: SurfaceRect,
    pub path: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneSurfaceFrame {
    pub boot_id: String,
    pub projection_revision: u64,
    pub surface_revision: u64,
    pub frame: FrameData,
    pub panes: Vec<PaneSurfacePane>,
    pub splits: Vec<PaneSurfaceSplit>,
    pub popup: Option<Box<ClientShellPopupSurface>>,
    pub graphics: crate::kitty_graphics::model::SurfaceGraphicsScene,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckoutTarget {
    pub machine_id: String,
    pub folder: String,
    pub label: String,
    pub repo_key: Option<String>,
}
impl ClientShellAvailableCheckout {
    pub(crate) fn target(&self) -> CheckoutTarget {
        CheckoutTarget {
            machine_id: self.machine_id.clone(),
            folder: self.folder.clone(),
            label: self.label.clone(),
            repo_key: self.worktree.as_ref().map(|tree| tree.key.clone()),
        }
    }
}

core_shell_extensions! {
    pub(crate) use crate::client::shell::core_adapter::CoreMachineView;
}
