use std::io;

use herdr_core_tui::model::{Axis, LayoutNode, Operation, Tab, Workspace};

use crate::client::ui_action::{FrontendCommand, FrontendRequest};
use crate::client::ui_domain as schema;

use super::{Backend, LaunchToken, Placement};

pub(crate) enum Action {
    Complete(Box<schema::ResponseResult>),
    Launch(LaunchToken),
    Runtime(FrontendCommand),
    ReloadConfig,
    Pending,
}

impl Backend {
    pub(crate) fn command(
        &mut self,
        owner: Option<&str>,
        request: FrontendRequest,
    ) -> io::Result<Action> {
        let owner = owner.filter(|id| *id != self.local_id()).map(str::to_owned);
        if !self.owner_present(owner.as_deref()) {
            return Err(io::Error::other(
                "the target core machine is no longer configured",
            ));
        }
        use FrontendCommand::*;
        if matches!(
            &request.method,
            WorkspaceFocus(_) | TabFocus(_) | PaneFocus(_) | PaneZoom(_)
        ) {
            self.relinquish_launch_focus();
        }
        match request.method {
            WorkspaceCreate(params) => {
                let folder = params.cwd.unwrap_or_else(|| {
                    self.default_folder(owner.as_deref(), params.source_workspace_id.as_deref())
                });
                let custom_name = params.label.is_some();
                let name = params.label.unwrap_or_else(|| folder_label(&folder));
                let token = self.begin_launch(
                    Placement::Workspace {
                        owner,
                        name,
                        custom_name,
                    },
                    folder,
                    params.env,
                    params.focus,
                    Some(request.id),
                )?;
                return Ok(Action::Launch(token));
            }
            TabCreate(params) => {
                let workspace =
                    self.workspace_target(owner.as_deref(), params.workspace_id.as_deref())?;
                let folder = params
                    .cwd
                    .unwrap_or_else(|| self.default_folder(owner.as_deref(), Some(&workspace.id)));
                let custom_name = params.label.is_some();
                let name = params
                    .label
                    .unwrap_or_else(|| (workspace.tabs.len() + 1).to_string());
                let token = self.begin_launch(
                    Placement::Tab {
                        workspace: workspace.id,
                        name,
                        custom_name,
                    },
                    folder,
                    params.env,
                    params.focus,
                    Some(request.id),
                )?;
                return Ok(Action::Launch(token));
            }
            PaneSplit(params) => {
                if params
                    .ratio
                    .is_some_and(|ratio| !ratio.is_finite() || ratio <= 0.0 || ratio >= 1.0)
                {
                    return Err(io::Error::other("split ratio must be between zero and one"));
                }
                let (workspace, tab, pane) =
                    self.pane_target(owner.as_deref(), params.target_pane_id.as_deref())?;
                if params
                    .workspace_id
                    .as_deref()
                    .is_some_and(|id| id != workspace.id)
                {
                    return Err(io::Error::other(
                        "split target is not in the requested workspace",
                    ));
                }
                let folder = params
                    .cwd
                    .unwrap_or_else(|| self.default_folder(owner.as_deref(), Some(&workspace.id)));
                let axis = match params.direction {
                    schema::SplitDirection::Right => Axis::Horizontal,
                    schema::SplitDirection::Down => Axis::Vertical,
                };
                let token = self.begin_launch(
                    Placement::Split {
                        workspace: workspace.id,
                        tab: tab.id,
                        target: pane,
                        axis,
                        ratio: params.ratio,
                        passthrough: params.right_click == schema::PaneRightClickTarget::Pane,
                    },
                    folder,
                    params.env,
                    params.focus,
                    Some(request.id),
                )?;
                return Ok(Action::Launch(token));
            }
            WorkspaceFocus(params) => {
                let workspace =
                    self.workspace_target(owner.as_deref(), Some(&params.workspace_id))?;
                self.view.workspace = Some(workspace.id);
                self.view.active_owner = owner;
            }
            TabFocus(params) => {
                let (workspace, tab) = self.tab_target(owner.as_deref(), Some(&params.tab_id))?;
                self.view.workspace = Some(workspace.id.clone());
                self.view.tabs.insert(workspace.id, tab.id);
                self.view.active_owner = owner;
            }
            PaneFocus(params) => {
                self.focus_pane(owner.as_deref(), &params.pane_id)?;
            }
            WorkspaceRename(params) => {
                let workspace =
                    self.workspace_target(owner.as_deref(), Some(&params.workspace_id))?;
                self.apply_operation(
                    owner,
                    Operation::RenameWorkspace {
                        workspace: workspace.id,
                        name: params.label,
                    },
                )?;
            }
            TabRename(params) => {
                let (workspace, tab) = self.tab_target(owner.as_deref(), Some(&params.tab_id))?;
                self.apply_operation(
                    owner,
                    Operation::RenameTab {
                        workspace: workspace.id,
                        tab: tab.id,
                        name: params.label,
                    },
                )?;
            }
            PaneRename(params) => {
                let (workspace, tab, pane) =
                    self.pane_target(owner.as_deref(), Some(&params.pane_id))?;
                self.apply_operation(
                    owner,
                    Operation::RenamePane {
                        workspace: workspace.id,
                        tab: tab.id,
                        pane,
                        name: params.label,
                    },
                )?;
            }
            WorkspaceClose(params) => {
                let workspace =
                    self.workspace_target(owner.as_deref(), Some(&params.workspace_id))?;
                let ids = if params.close_group {
                    let key = self
                        .repository(owner.as_deref(), &workspace.folder)
                        .ok_or_else(|| {
                            io::Error::other(
                                "repository metadata is not available for this workspace",
                            )
                        })?
                        .key
                        .clone();
                    self.document
                        .workspaces
                        .iter()
                        .filter(|entry| entry.machine == owner)
                        .filter(|entry| {
                            self.repository(owner.as_deref(), &entry.folder)
                                .is_some_and(|repo| repo.key == key)
                        })
                        .map(|entry| entry.id.clone())
                        .collect::<Vec<_>>()
                } else {
                    vec![workspace.id]
                };
                for workspace in ids {
                    self.apply_operation(owner.clone(), Operation::CloseWorkspace { workspace })?;
                }
                self.view.repair(&self.document);
            }
            TabClose(params) => {
                let (workspace, tab) = self.tab_target(owner.as_deref(), Some(&params.tab_id))?;
                self.apply_operation(
                    owner,
                    Operation::CloseTab {
                        workspace: workspace.id,
                        tab: tab.id,
                    },
                )?;
                self.view.repair(&self.document);
            }
            PaneClose(params) => {
                let (workspace, tab, pane) =
                    self.pane_target(owner.as_deref(), Some(&params.pane_id))?;
                self.close_pane_view(owner, workspace.id, tab.id, pane)?;
                self.view.repair(&self.document);
            }
            TabMove(params) => {
                let (workspace, tab) = self.tab_target(owner.as_deref(), Some(&params.tab_id))?;
                let mut others = workspace.tabs.iter().filter(|entry| entry.id != tab.id);
                let before = others
                    .nth(params.insert_index)
                    .map(|entry| entry.id.clone());
                self.apply_operation(
                    owner,
                    Operation::MoveTab {
                        workspace: workspace.id,
                        tab: tab.id,
                        before,
                    },
                )?;
            }
            WorkspaceMove(params) => {
                let workspace =
                    self.workspace_target(owner.as_deref(), Some(&params.workspace_id))?;
                let before = self
                    .document
                    .workspaces
                    .iter()
                    .filter(|entry| entry.machine == owner && entry.id != workspace.id)
                    .nth(params.insert_index)
                    .map(|entry| entry.id.clone());
                self.apply_operation(
                    owner,
                    Operation::MoveWorkspaces {
                        workspaces: vec![workspace.id],
                        before,
                    },
                )?;
            }
            WorkspaceMoveBlock(params) => {
                for id in &params.workspace_ids {
                    self.workspace_target(owner.as_deref(), Some(id))?;
                }
                if let Some(id) = &params.before_workspace_id {
                    self.workspace_target(owner.as_deref(), Some(id))?;
                }
                self.apply_operation(
                    owner,
                    Operation::MoveWorkspaces {
                        workspaces: params.workspace_ids,
                        before: params.before_workspace_id,
                    },
                )?;
            }
            LayoutSetSplitRatio { params, split_id } => {
                let captured = split_id.ok_or_else(|| {
                    io::Error::other("the split handle has no captured identity; begin a new drag")
                })?;
                let (workspace, tab) = if let Some(pane) = params.pane_id.as_deref() {
                    let (workspace, tab, _) = self.pane_target(owner.as_deref(), Some(pane))?;
                    (workspace, tab)
                } else {
                    self.tab_target(owner.as_deref(), params.tab_id.as_deref())?
                };
                let split = tab
                    .layout
                    .split_at(&params.path)
                    .ok_or_else(|| io::Error::other("the split handle no longer exists"))?
                    .to_owned();
                if split != captured {
                    return Err(io::Error::other(
                        "the split layout changed during this drag; begin a new drag",
                    ));
                }
                self.apply_operation(
                    owner,
                    Operation::SetRatio {
                        workspace: workspace.id,
                        tab: tab.id,
                        split,
                        ratio: params.ratio,
                    },
                )?;
            }
            PaneInputSet(params) => {
                let (workspace, tab, pane) =
                    self.pane_target(owner.as_deref(), Some(&params.pane_id))?;
                self.apply_operation(
                    owner,
                    Operation::SetPaneInput {
                        workspace: workspace.id,
                        tab: tab.id,
                        pane,
                        passthrough: params.right_click == schema::PaneRightClickTarget::Pane,
                    },
                )?;
            }
            PaneZoom(params) => {
                let (_, tab, pane) =
                    self.pane_target(owner.as_deref(), params.pane_id.as_deref())?;
                self.view.focus_pane(&self.document, &pane);
                let zoomed = self.view.zoomed.contains(&tab.id);
                let enable = match params.mode {
                    schema::PaneZoomMode::Toggle => !zoomed,
                    schema::PaneZoomMode::On => true,
                    schema::PaneZoomMode::Off => false,
                };
                if enable {
                    self.view.zoomed.insert(tab.id);
                } else {
                    self.view.zoomed.remove(&tab.id);
                }
                self.view.active_owner = owner;
            }
            PaneSwap(params)
                if params.source_pane_id.is_some() && params.target_pane_id.is_some() =>
            {
                self.swap_views(
                    owner.as_deref(),
                    params.source_pane_id.as_deref().expect("matched source"),
                    params.target_pane_id.as_deref().expect("matched target"),
                )?;
            }
            ServerReloadConfig(_) => return Ok(Action::ReloadConfig),
            command @ (WorktreeCreate(_) | WorktreeList(_) | WorktreeOpen(_)
            | WorktreeRemove(_)) => {
                self.worktree_command(owner, request.id, command)?;
                return Ok(Action::Pending);
            }
            command @ (PaneFocusDirection(_)
            | PaneResize(_)
            | PaneSwap(_)
            | PaneClear(_)
            | PaneCopyMotion(_)
            | PaneCopySearch(_)
            | PaneEditScrollback(_)
            | PaneLinkActivate(_)
            | PaneLinkResolve(_)
            | PaneScroll(_)
            | PaneSelectionRead(_)
            | CommandInvoke(_)
            | ProductAnnouncementDismiss(_)
            | ReleaseNotesDismiss(_)) => return Ok(Action::Runtime(command)),
            IntegrationInstall(_) | IntegrationList(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "legacy integrations are not available in the core-backed TUI",
                ));
            }
        }
        self.revision += 1;
        self.remember_focus();
        Ok(Action::Complete(Box::new(schema::ResponseResult::Ok {})))
    }

    pub(super) fn follow_folder(
        &self,
        owner: Option<&str>,
        workspace: Option<&str>,
    ) -> Option<String> {
        let workspace = workspace
            .and_then(|id| self.document.workspace(id))
            .or_else(|| {
                self.view
                    .active_workspace(&self.document)
                    .filter(|entry| entry.machine.as_deref() == owner)
            });
        if let Some(workspace) = workspace {
            if let Some(tab) = self
                .view
                .tabs
                .get(&workspace.id)
                .and_then(|id| workspace.tab(id))
                .or_else(|| workspace.tabs.first())
            {
                if let Some(pane) = self
                    .view
                    .panes
                    .get(&tab.id)
                    .and_then(|id| tab.pane(id))
                    .or_else(|| tab.panes.first())
                {
                    let address = herdr_core_tui::model::terminal_address(owner, &pane.terminal);
                    if let Some(record) = self.inventory.get(&address) {
                        if let Some(cwd) = record
                            .pointer("/foreground/cwd")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| record["cwd"].as_str())
                        {
                            return Some(cwd.into());
                        }
                    }
                }
            }
            return Some(workspace.folder.clone());
        }
        None
    }

    pub(crate) fn focus_pane(&mut self, owner: Option<&str>, id: &str) -> io::Result<()> {
        let owner = owner.filter(|id| *id != self.local_id()).map(str::to_owned);
        let (_, _, pane) = self.pane_target(owner.as_deref(), Some(id))?;
        self.relinquish_launch_focus();
        self.view.focus_pane(&self.document, &pane);
        self.view.active_owner = owner;
        self.remember_focus();
        self.revision += 1;
        Ok(())
    }

    pub(crate) fn swap_views(
        &mut self,
        owner: Option<&str>,
        source: &str,
        target: &str,
    ) -> io::Result<()> {
        let owner = owner.filter(|id| *id != self.local_id()).map(str::to_owned);
        let (workspace, tab, source) = self.pane_target(owner.as_deref(), Some(source))?;
        let (_, target_tab, target) = self.pane_target(owner.as_deref(), Some(target))?;
        if tab.id != target_tab.id {
            return Err(io::Error::other(
                "pane swap requires both panes in the same tab",
            ));
        }
        let expected = tab.layout;
        let mut layout = expected.clone();
        swap_leaves(&mut layout, &source, &target);
        self.apply_operation(
            owner,
            Operation::SetTabLayout {
                workspace: workspace.id,
                tab: tab.id,
                expected,
                layout,
            },
        )
    }

    pub(crate) fn workspace_target(
        &self,
        owner: Option<&str>,
        id: Option<&str>,
    ) -> io::Result<Workspace> {
        let workspace = match id {
            Some(id) => self.document.workspace(id),
            None => self
                .view
                .active_workspace(&self.document)
                .filter(|workspace| workspace.machine.as_deref() == owner)
                .or_else(|| {
                    self.document
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.machine.as_deref() == owner)
                }),
        }
        .ok_or_else(|| io::Error::other("the target workspace no longer exists"))?;
        if workspace.machine.as_deref() != owner {
            return Err(io::Error::other(
                "the workspace belongs to a different core machine",
            ));
        }
        Ok(workspace.clone())
    }

    pub(crate) fn tab_target(
        &self,
        owner: Option<&str>,
        id: Option<&str>,
    ) -> io::Result<(Workspace, Tab)> {
        if let Some(id) = id {
            for workspace in &self.document.workspaces {
                if let Some(tab) = workspace.tab(id) {
                    if workspace.machine.as_deref() != owner {
                        return Err(io::Error::other(
                            "the tab belongs to a different core machine",
                        ));
                    }
                    return Ok((workspace.clone(), tab.clone()));
                }
            }
            return Err(io::Error::other("the target tab no longer exists"));
        }
        let workspace = self.workspace_target(owner, None)?;
        let tab = self
            .view
            .tabs
            .get(&workspace.id)
            .and_then(|id| workspace.tab(id))
            .or_else(|| workspace.tabs.first())
            .ok_or_else(|| io::Error::other("the workspace has no terminal tabs"))?
            .clone();
        Ok((workspace, tab))
    }

    pub(crate) fn pane_target(
        &self,
        owner: Option<&str>,
        id: Option<&str>,
    ) -> io::Result<(Workspace, Tab, String)> {
        if let Some(id) = id {
            for workspace in &self.document.workspaces {
                for tab in &workspace.tabs {
                    if tab.pane(id).is_some() {
                        if workspace.machine.as_deref() != owner {
                            return Err(io::Error::other(
                                "the pane belongs to a different core machine",
                            ));
                        }
                        return Ok((workspace.clone(), tab.clone(), id.into()));
                    }
                }
            }
            return Err(io::Error::other("the target pane no longer exists"));
        }
        let (workspace, tab) = self.tab_target(owner, None)?;
        let pane = self
            .view
            .panes
            .get(&tab.id)
            .and_then(|id| tab.pane(id))
            .or_else(|| tab.panes.first())
            .ok_or_else(|| io::Error::other("the tab has no terminal panes"))?
            .id
            .clone();
        Ok((workspace, tab, pane))
    }
}

pub(super) fn swap_leaves(layout: &mut LayoutNode, first: &str, second: &str) {
    match layout {
        LayoutNode::Pane { id, .. } if id == first => *id = second.into(),
        LayoutNode::Pane { id, .. } if id == second => *id = first.into(),
        LayoutNode::Pane { .. } => {}
        LayoutNode::Split {
            first: left,
            second: right,
            ..
        } => {
            swap_leaves(left, first, second);
            swap_leaves(right, first, second);
        }
    }
}

pub(super) fn folder_label(folder: &str) -> String {
    folder
        .trim_end_matches('/')
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(folder)
        .to_owned()
}
