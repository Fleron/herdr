use super::ui_action::{FrontendCommand, FrontendRequest, UiRequest};
use super::ui_model::*;

impl From<FrontendRequest> for crate::api::schema::Request {
    fn from(value: FrontendRequest) -> Self {
        Self {
            id: value.id,
            method: match value.method {
                FrontendCommand::CommandInvoke(params) => {
                    crate::api::schema::Method::CommandInvoke(params)
                }
                FrontendCommand::IntegrationInstall(params) => {
                    crate::api::schema::Method::IntegrationInstall(params)
                }
                FrontendCommand::IntegrationList(params) => {
                    crate::api::schema::Method::IntegrationList(params)
                }
                FrontendCommand::LayoutSetSplitRatio { params, .. } => {
                    crate::api::schema::Method::LayoutSetSplitRatio(params)
                }
                FrontendCommand::PaneClear(params) => crate::api::schema::Method::PaneClear(params),
                FrontendCommand::PaneClose(params) => crate::api::schema::Method::PaneClose(params),
                FrontendCommand::PaneCopyMotion(params) => {
                    crate::api::schema::Method::PaneCopyMotion(params)
                }
                FrontendCommand::PaneCopySearch(params) => {
                    crate::api::schema::Method::PaneCopySearch(params)
                }
                FrontendCommand::PaneEditScrollback(params) => {
                    crate::api::schema::Method::PaneEditScrollback(params)
                }
                FrontendCommand::PaneFocus(params) => crate::api::schema::Method::PaneFocus(params),
                FrontendCommand::PaneFocusDirection(params) => {
                    crate::api::schema::Method::PaneFocusDirection(params)
                }
                FrontendCommand::PaneInputSet(params) => {
                    crate::api::schema::Method::PaneInputSet(params)
                }
                FrontendCommand::PaneLinkActivate(params) => {
                    crate::api::schema::Method::PaneLinkActivate(params)
                }
                FrontendCommand::PaneLinkResolve(params) => {
                    crate::api::schema::Method::PaneLinkResolve(params)
                }
                FrontendCommand::PaneRename(params) => {
                    crate::api::schema::Method::PaneRename(params)
                }
                FrontendCommand::PaneResize(params) => {
                    crate::api::schema::Method::PaneResize(params)
                }
                FrontendCommand::PaneScroll(params) => {
                    crate::api::schema::Method::PaneScroll(params)
                }
                FrontendCommand::PaneSelectionRead(params) => {
                    crate::api::schema::Method::PaneSelectionRead(params)
                }
                FrontendCommand::PaneSplit(params) => crate::api::schema::Method::PaneSplit(params),
                FrontendCommand::PaneSwap(params) => crate::api::schema::Method::PaneSwap(params),
                FrontendCommand::PaneZoom(params) => crate::api::schema::Method::PaneZoom(params),
                FrontendCommand::ProductAnnouncementDismiss(params) => {
                    crate::api::schema::Method::ProductAnnouncementDismiss(params)
                }
                FrontendCommand::ReleaseNotesDismiss(params) => {
                    crate::api::schema::Method::ReleaseNotesDismiss(params)
                }
                FrontendCommand::ServerReloadConfig(params) => {
                    crate::api::schema::Method::ServerReloadConfig(params)
                }
                FrontendCommand::TabClose(params) => crate::api::schema::Method::TabClose(params),
                FrontendCommand::TabCreate(params) => crate::api::schema::Method::TabCreate(params),
                FrontendCommand::TabFocus(params) => crate::api::schema::Method::TabFocus(params),
                FrontendCommand::TabMove(params) => crate::api::schema::Method::TabMove(params),
                FrontendCommand::TabRename(params) => crate::api::schema::Method::TabRename(params),
                FrontendCommand::WorkspaceClose(params) => {
                    crate::api::schema::Method::WorkspaceClose(params)
                }
                FrontendCommand::WorkspaceCreate(params) => {
                    crate::api::schema::Method::WorkspaceCreate(params)
                }
                FrontendCommand::WorkspaceFocus(params) => {
                    crate::api::schema::Method::WorkspaceFocus(params)
                }
                FrontendCommand::WorkspaceMove(params) => {
                    crate::api::schema::Method::WorkspaceMove(params)
                }
                FrontendCommand::WorkspaceMoveBlock(params) => {
                    crate::api::schema::Method::WorkspaceMoveBlock(params)
                }
                FrontendCommand::WorkspaceRename(params) => {
                    crate::api::schema::Method::WorkspaceRename(params)
                }
                FrontendCommand::WorktreeCreate(params) => {
                    crate::api::schema::Method::WorktreeCreate(params)
                }
                FrontendCommand::WorktreeList(params) => {
                    crate::api::schema::Method::WorktreeList(params)
                }
                FrontendCommand::WorktreeOpen(params) => {
                    crate::api::schema::Method::WorktreeOpen(params)
                }
                FrontendCommand::WorktreeRemove(params) => {
                    crate::api::schema::Method::WorktreeRemove(params)
                }
            },
        }
    }
}

impl From<UiRequest> for crate::protocol::ClientMessage {
    fn from(value: UiRequest) -> Self {
        match value {
            UiRequest::PaneInput { pane_id, events } => {
                Self::ClientShellPaneInput { pane_id, events }
            }
            UiRequest::PopupInput {
                terminal_id,
                events,
            } => Self::ClientShellPopupInput {
                terminal_id,
                events,
            },
            UiRequest::Focus { focused } => Self::ClientShellFocus { focused },
            UiRequest::HostTheme { update } => Self::ClientShellHostTheme { update },
        }
    }
}

impl From<crate::protocol::ClientShellSnapshot> for ClientShellSnapshot {
    fn from(value: crate::protocol::ClientShellSnapshot) -> Self {
        Self {
            runtime_controls: None,
            core_terminals: None,
            available_checkouts: Vec::new(),
            boot_id: value.boot_id,
            revision: value.revision,
            config_diagnostic: value.config_diagnostic,
            product_announcement: value.product_announcement.map(Into::into),
            update_available: value.update_available,
            update_install_command: value.update_install_command,
            server_keybindings_toml: value.server_keybindings_toml,
            latest_release_notes_available: value.latest_release_notes_available,
            integration_updates_available: value.integration_updates_available,
            worktree_directory: value.worktree_directory,
            release_notes: value.release_notes.map(Into::into),
            focused_workspace_id: value.focused_workspace_id,
            focused_tab_id: value.focused_tab_id,
            focused_pane_id: value.focused_pane_id,
            tab_bar_right: value.tab_bar_right.into_iter().map(Into::into).collect(),
            tab_bar_right_separator: value.tab_bar_right_separator,
            agent_view_label: value.agent_view_label,
            agent_order: value.agent_order,
            workspaces: value.workspaces.into_iter().map(Into::into).collect(),
            tabs: value.tabs.into_iter().map(Into::into).collect(),
            panes: value.panes.into_iter().map(Into::into).collect(),
            agents: value.agents.into_iter().map(Into::into).collect(),
            commands: value.commands.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<crate::protocol::ClientShellProductAnnouncement> for ClientShellProductAnnouncement {
    fn from(value: crate::protocol::ClientShellProductAnnouncement) -> Self {
        Self {
            version: value.version,
            id: value.id,
            title: value.title,
            body: value.body,
            preview: value.preview,
        }
    }
}

impl From<crate::protocol::ClientShellReleaseNotes> for ClientShellReleaseNotes {
    fn from(value: crate::protocol::ClientShellReleaseNotes) -> Self {
        Self {
            version: value.version,
            body: value.body,
            preview: value.preview,
        }
    }
}

impl From<crate::protocol::ClientShellCommandAction> for ClientShellCommandAction {
    fn from(value: crate::protocol::ClientShellCommandAction) -> Self {
        match value {
            crate::protocol::ClientShellCommandAction::Shell => Self::Shell,
            crate::protocol::ClientShellCommandAction::Pane => Self::Pane,
            crate::protocol::ClientShellCommandAction::Popup => Self::Popup,
            crate::protocol::ClientShellCommandAction::PluginAction => Self::PluginAction,
            crate::protocol::ClientShellCommandAction::Unknown => Self::Unknown,
        }
    }
}

impl From<crate::protocol::ClientShellCommand> for ClientShellCommand {
    fn from(value: crate::protocol::ClientShellCommand) -> Self {
        Self {
            command_id: value.command_id,
            binding_label: value.binding_label,
            binding_labels: value.binding_labels,
            action: value.action.into(),
            description: value.description,
        }
    }
}

impl From<crate::protocol::ClientShellTabStatusSegment> for ClientShellTabStatusSegment {
    fn from(value: crate::protocol::ClientShellTabStatusSegment) -> Self {
        Self {
            text: value.text,
            accent: value.accent,
        }
    }
}

impl From<crate::protocol::ClientShellWorkspace> for ClientShellWorkspace {
    fn from(value: crate::protocol::ClientShellWorkspace) -> Self {
        Self {
            workspace_id: value.workspace_id,
            active_tab_id: value.active_tab_id,
            new_workspace_label: crate::workspace::derive_label_from_cwd(std::path::Path::new(
                &value.new_workspace_cwd,
            )),
            new_workspace_cwd: value.new_workspace_cwd,
            number: value.number,
            label: value.label,
            custom_label: value.custom_label,
            branch: value.branch,
            git_ahead_behind: value.git_ahead_behind,
            tokens: value.tokens,
            worktree: value.worktree.map(Into::into),
            focused: value.focused,
            agent_status: value.agent_status,
        }
    }
}

impl From<crate::protocol::ClientShellWorktree> for ClientShellWorktree {
    fn from(value: crate::protocol::ClientShellWorktree) -> Self {
        Self {
            key: value.key,
            label: value.label,
            is_linked_worktree: value.is_linked_worktree,
        }
    }
}

impl From<crate::protocol::ClientShellTab> for ClientShellTab {
    fn from(value: crate::protocol::ClientShellTab) -> Self {
        Self {
            tab_id: value.tab_id,
            workspace_id: value.workspace_id,
            number: value.number,
            label: value.label,
            custom_label: value.custom_label,
            zoomed: value.zoomed,
            focused: value.focused,
            agent_status: value.agent_status,
        }
    }
}

impl From<crate::protocol::ClientShellPane> for ClientShellPane {
    fn from(value: crate::protocol::ClientShellPane) -> Self {
        Self {
            runtime_target: None,
            pane_id: value.pane_id,
            workspace_id: value.workspace_id,
            tab_id: value.tab_id,
            label: value.label,
            cwd: value.cwd,
            foreground_cwd: value.foreground_cwd,
            focused: value.focused,
            right_click_passthrough: value.right_click_passthrough,
        }
    }
}

impl From<crate::protocol::ClientShellAgent> for ClientShellAgent {
    fn from(value: crate::protocol::ClientShellAgent) -> Self {
        Self {
            cwd: None,
            agent_id: value.pane_id.clone(),
            runtime_target: None,
            view: Some(AgentViewTarget {
                pane_id: value.pane_id,
                workspace_id: value.workspace_id,
                tab_id: value.tab_id,
            }),
            name: value.name,
            display_agent: value.display_agent,
            agent: value.agent,
            title: value.title,
            terminal_title: value.terminal_title,
            terminal_title_stripped: value.terminal_title_stripped,
            agent_status: value.agent_status,
            state_change_seq: value.state_change_seq,
            state_labels: value.state_labels,
            tokens: value.tokens,
            focused: value.focused,
        }
    }
}

impl From<crate::api::schema::ResponseResult> for super::ui_domain::ResponseResult {
    fn from(value: crate::api::schema::ResponseResult) -> Self {
        match value {
            crate::api::schema::ResponseResult::WorktreeList { source, worktrees } => {
                Self::WorktreeList { source, worktrees }
            }
            crate::api::schema::ResponseResult::WorktreeCreated {
                workspace,
                tab,
                root_pane,
                worktree,
            } => Self::WorktreeCreated {
                workspace,
                tab,
                root_pane,
                worktree,
            },
            crate::api::schema::ResponseResult::WorktreeOpened {
                workspace,
                tab,
                root_pane,
                worktree,
                already_open,
            } => Self::WorktreeOpened {
                workspace,
                tab,
                root_pane,
                worktree,
                already_open,
            },
            crate::api::schema::ResponseResult::WorktreeRemoved {
                workspace_id,
                path,
                forced,
            } => Self::WorktreeRemoved {
                workspace_id,
                path,
                forced,
            },
            crate::api::schema::ResponseResult::PaneInfo { pane } => Self::PaneInfo { pane },
            crate::api::schema::ResponseResult::PaneSelection { pane_id, text } => {
                Self::PaneSelection { pane_id, text }
            }
            crate::api::schema::ResponseResult::PaneCopyMotion {
                pane_id,
                cursor,
                content_revision,
            } => Self::PaneCopyMotion {
                pane_id,
                cursor,
                content_revision,
            },
            crate::api::schema::ResponseResult::PaneCopySearch {
                pane_id,
                content_revision,
                matches,
                total,
                current,
                current_global,
            } => Self::PaneCopySearch {
                pane_id,
                content_revision,
                matches,
                total,
                current,
                current_global,
            },
            crate::api::schema::ResponseResult::IntegrationList { integrations } => {
                Self::IntegrationList { integrations }
            }
            crate::api::schema::ResponseResult::IntegrationInstall { target, details } => {
                Self::IntegrationInstall { target, details }
            }
            crate::api::schema::ResponseResult::PaneLinkResolved { regions } => {
                Self::PaneLinkResolved { regions }
            }
            crate::api::schema::ResponseResult::PaneLinkActivated { url, handled } => {
                Self::PaneLinkActivated { url, handled }
            }
            crate::api::schema::ResponseResult::ConfigReload {
                status,
                diagnostics,
            } => Self::ConfigReload {
                status,
                diagnostics,
            },
            crate::api::schema::ResponseResult::Ok {} => Self::Ok {},
            _ => Self::Ok {},
        }
    }
}

impl From<crate::protocol::PaneSurfaceSplit> for super::ui_model::PaneSurfaceSplit {
    fn from(value: crate::protocol::PaneSurfaceSplit) -> Self {
        Self {
            split_id: None,
            direction: value.direction,
            pos: value.pos,
            area: value.area,
            hit_rect: value.hit_rect,
            path: value.path,
        }
    }
}
impl From<crate::protocol::PaneSurfaceFrame> for super::ui_model::PaneSurfaceFrame {
    fn from(value: crate::protocol::PaneSurfaceFrame) -> Self {
        Self {
            boot_id: value.boot_id,
            projection_revision: value.projection_revision,
            surface_revision: value.surface_revision,
            frame: value.frame,
            panes: value.panes,
            splits: value.splits.into_iter().map(Into::into).collect(),
            popup: value.popup,
            graphics: value.graphics,
        }
    }
}
