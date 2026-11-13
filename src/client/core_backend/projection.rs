use crate::client::ui_domain::AgentStatus;
use crate::client::ui_model::*;
use herdr_core_tui::model::{terminal_address, Workspace};
use serde_json::Value;
use std::collections::BTreeMap;

use super::Backend;

impl Backend {
    pub(crate) fn snapshot(&self, owner: Option<&str>) -> ClientShellSnapshot {
        let workspaces = self
            .document
            .workspaces
            .iter()
            .filter(|workspace| workspace.machine.as_deref() == owner)
            .collect::<Vec<_>>();
        let focused_workspace = self.active_workspace_for(owner);
        let focused_tab = focused_workspace.and_then(|workspace| {
            self.view
                .tabs
                .get(&workspace.id)
                .and_then(|id| workspace.tab(id))
                .or_else(|| workspace.tabs.first())
        });
        let focused_pane = focused_tab.and_then(|tab| {
            self.view
                .panes
                .get(&tab.id)
                .and_then(|id| tab.pane(id))
                .or_else(|| tab.panes.first())
        });
        let workspace_id = focused_workspace.map(|workspace| workspace.id.clone());
        let tab_id = focused_tab.map(|tab| tab.id.clone());
        let pane_id = focused_pane.map(|pane| pane.id.clone());
        let views = view_index(&self.document.workspaces, owner, pane_id.as_deref());
        let agents = self
            .inventory
            .live()
            .filter(|record| record_owner(record) == owner)
            .filter_map(|record| self.agent(record, owner, pane_id.as_deref(), &views))
            .collect::<Vec<_>>();
        let core_terminals = self.core_terminals(owner);
        let available_checkouts = self.available_checkouts(owner, &core_terminals);
        let mut projected_workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut panes = Vec::new();
        for (index, workspace) in workspaces.into_iter().enumerate() {
            let active_tab = self
                .view
                .tabs
                .get(&workspace.id)
                .and_then(|id| workspace.tab(id))
                .or_else(|| workspace.tabs.first());
            let repository = self.repository(owner, &workspace.folder);
            let new_workspace_cwd = self.default_folder(owner, Some(&workspace.id));
            projected_workspaces.push(ClientShellWorkspace {
                workspace_id: workspace.id.clone(),
                active_tab_id: active_tab.map(|tab| tab.id.clone()).unwrap_or_default(),
                new_workspace_label: self
                    .repository(owner, &new_workspace_cwd)
                    .map(|repo| super::actions::folder_label(&repo.root))
                    .unwrap_or_else(|| super::actions::folder_label(&new_workspace_cwd)),
                new_workspace_cwd,
                number: index + 1,
                label: workspace.name.clone(),
                custom_label: workspace
                    .extra
                    .get("custom_name")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                branch: repository.and_then(|repo| repo.branch.clone()),
                git_ahead_behind: repository.and_then(|repo| repo.ahead_behind),
                tokens: vec![("cwd".into(), workspace.folder.clone())],
                worktree: repository.map(|repo| ClientShellWorktree {
                    key: repo.key.clone(),
                    label: repo.label.clone(),
                    is_linked_worktree: repo.linked,
                }),
                focused: workspace_id.as_deref() == Some(workspace.id.as_str()),
                agent_status: aggregate(
                    agents
                        .iter()
                        .filter(|agent| {
                            agent
                                .view
                                .as_ref()
                                .is_some_and(|view| view.workspace_id == workspace.id)
                        })
                        .map(|agent| agent.agent_status),
                ),
            });
            for (index, tab) in workspace.tabs.iter().enumerate() {
                tabs.push(ClientShellTab {
                    tab_id: tab.id.clone(),
                    workspace_id: workspace.id.clone(),
                    number: index + 1,
                    label: tab.name.clone(),
                    custom_label: tab
                        .extra
                        .get("custom_name")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    zoomed: self.view.zoomed.contains(&tab.id),
                    focused: tab_id.as_deref() == Some(tab.id.as_str()),
                    agent_status: aggregate(
                        agents
                            .iter()
                            .filter(|agent| {
                                agent
                                    .view
                                    .as_ref()
                                    .is_some_and(|view| view.tab_id == tab.id)
                            })
                            .map(|agent| agent.agent_status),
                    ),
                });
                for pane in &tab.panes {
                    let address = terminal_address(owner, &pane.terminal);
                    let record = self.inventory.get(&address);
                    panes.push(ClientShellPane {
                        pane_id: pane.id.clone(),
                        workspace_id: workspace.id.clone(),
                        tab_id: tab.id.clone(),
                        label: pane.name.clone(),
                        cwd: record
                            .and_then(|record| record["cwd"].as_str())
                            .map(str::to_owned),
                        foreground_cwd: record.and_then(current_cwd).map(str::to_owned),
                        focused: pane_id.as_deref() == Some(pane.id.as_str()),
                        right_click_passthrough: pane
                            .extra
                            .get("right_click_passthrough")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        runtime_target: Some(RuntimeTerminalTarget {
                            machine_id: owner.unwrap_or_else(|| self.local_id()).to_owned(),
                            terminal: pane.terminal.clone(),
                        }),
                    });
                }
            }
        }
        ClientShellSnapshot {
            boot_id: format!("{}:{}", self.local_id(), self.generation),
            revision: self.revision,
            config_diagnostic: None,
            product_announcement: None,
            update_available: None,
            update_install_command: String::new(),
            server_keybindings_toml: None,
            latest_release_notes_available: false,
            integration_updates_available: false,
            worktree_directory: self.presentation.worktree_directory.clone(),
            release_notes: None,
            focused_workspace_id: workspace_id,
            focused_tab_id: tab_id,
            focused_pane_id: pane_id,
            tab_bar_right: self.tab_bar_status(
                owner,
                focused_tab.is_some_and(|tab| self.view.zoomed.contains(&tab.id)),
            ),
            tab_bar_right_separator: self.presentation.separator.clone(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: projected_workspaces,
            tabs,
            panes,
            agents,
            commands: self.commands.clone(),
            runtime_controls: None,
            core_terminals: Some(core_terminals),
            available_checkouts: available_checkouts
                .into_iter()
                .map(std::sync::Arc::new)
                .collect(),
        }
    }

    fn core_terminals(&self, owner: Option<&str>) -> Vec<CoreTerminalView> {
        let machine_label = owner
            .and_then(|id| self.profiles.iter().find(|profile| profile["id"] == id))
            .and_then(|profile| profile["name"].as_str())
            .unwrap_or("Local")
            .to_owned();
        self.inventory
            .live()
            .filter(|record| record_owner(record) == owner)
            .filter_map(|record| {
                let address = record["address"].as_str()?;
                let terminal = address
                    .split_once('/')
                    .map_or(address, |(_, terminal)| terminal)
                    .to_owned();
                let label = text(record.get("name"))
                    .or_else(|| text(record.pointer("/status/title")))
                    .or_else(|| {
                        record
                            .pointer("/foreground/argv/0")
                            .or_else(|| record.pointer("/argv/0"))
                            .and_then(Value::as_str)
                            .and_then(|program| program.rsplit('/').next())
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| terminal.clone());
                Some(CoreTerminalView {
                    target: RuntimeTerminalTarget {
                        machine_id: owner.unwrap_or_else(|| self.local_id()).to_owned(),
                        terminal,
                    },
                    machine_label: machine_label.clone(),
                    label,
                    cwd: current_cwd(record).map(str::to_owned),
                    status: text(record.pointer("/status/state")),
                })
            })
            .collect()
    }

    fn available_checkouts(
        &self,
        owner: Option<&str>,
        terminals: &[CoreTerminalView],
    ) -> Vec<ClientShellAvailableCheckout> {
        let represented = self
            .document
            .workspaces
            .iter()
            .filter(|workspace| workspace.machine.as_deref() == owner)
            .map(|workspace| {
                self.repository(owner, &workspace.folder)
                    .map(|repo| repo.root.as_str())
                    .unwrap_or_else(|| self.canonical_folder(owner, &workspace.folder))
            })
            .collect::<std::collections::BTreeSet<_>>();
        let placed = self
            .document
            .workspaces
            .iter()
            .filter(|workspace| workspace.machine.as_deref() == owner)
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .map(|pane| pane.terminal.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut checkouts = BTreeMap::<String, ClientShellAvailableCheckout>::new();
        if let Some(metadata) = self.metadata.get(&owner.map(str::to_owned)) {
            for (key, trees) in &metadata.worktrees {
                let Some(identity) = metadata.identities.get(key) else {
                    continue;
                };
                for tree in trees {
                    let folder = self.canonical_folder(owner, &tree.path);
                    if represented.contains(folder) {
                        continue;
                    }
                    let repository = self.repository(owner, &tree.path);
                    let error = metadata.unavailable.get(key).cloned().or_else(|| {
                        if tree.bare {
                            Some("A bare repository has no working directory to open".into())
                        } else if tree.prunable {
                            Some("This checkout is missing or prunable".into())
                        } else {
                            metadata.errors.get(&tree.path).cloned()
                        }
                    });
                    checkouts.insert(
                        folder.to_owned(),
                        ClientShellAvailableCheckout {
                            machine_id: owner.unwrap_or_else(|| self.local_id()).to_owned(),
                            folder: folder.to_owned(),
                            label: super::actions::folder_label(folder),
                            branch: tree.branch.clone(),
                            git_ahead_behind: repository.and_then(|repo| repo.ahead_behind),
                            worktree: Some(ClientShellWorktree {
                                key: key.clone(),
                                label: identity.label.clone(),
                                is_linked_worktree: folder != identity.primary,
                            }),
                            terminals: Vec::new(),
                            error,
                            agent_status: AgentStatus::Unknown,
                        },
                    );
                }
            }
        }
        for terminal in terminals.iter().cloned() {
            if placed.contains(terminal.target.terminal.as_str()) {
                continue;
            }
            let Some(cwd) = terminal.cwd.as_deref() else {
                continue;
            };
            let repository = self.repository(owner, cwd);
            let folder = repository
                .map(|repo| repo.root.as_str())
                .unwrap_or_else(|| self.canonical_folder(owner, cwd));
            if represented.contains(folder) {
                continue;
            }
            let checkout = checkouts.entry(folder.to_owned()).or_insert_with(|| {
                ClientShellAvailableCheckout {
                    machine_id: owner.unwrap_or_else(|| self.local_id()).to_owned(),
                    folder: folder.to_owned(),
                    label: super::actions::folder_label(folder),
                    branch: repository.and_then(|repo| repo.branch.clone()),
                    git_ahead_behind: repository.and_then(|repo| repo.ahead_behind),
                    worktree: repository.map(|repo| ClientShellWorktree {
                        key: repo.key.clone(),
                        label: repo.label.clone(),
                        is_linked_worktree: repo.linked,
                    }),
                    terminals: Vec::new(),
                    error: repository.and_then(|repo| {
                        self.metadata
                            .get(&owner.map(str::to_owned))?
                            .unavailable
                            .get(&repo.key)
                            .cloned()
                    }),
                    agent_status: AgentStatus::Unknown,
                }
            });
            let address = terminal_address(owner, &terminal.target.terminal);
            let state = self
                .inventory
                .get(&address)
                .map(status)
                .unwrap_or(AgentStatus::Unknown);
            checkout.agent_status = aggregate([checkout.agent_status, state].into_iter());
            checkout.terminals.push(terminal);
        }
        checkouts.into_values().collect()
    }

    fn agent(
        &self,
        record: &Value,
        owner: Option<&str>,
        focused_pane: Option<&str>,
        views: &BTreeMap<String, AgentViewTarget>,
    ) -> Option<ClientShellAgent> {
        let app = reported_app(record)?;
        let address = record["address"].as_str()?;
        let terminal = address.rsplit('/').next()?.to_owned();
        let machine_id = owner.unwrap_or_else(|| self.local_id()).to_owned();
        let view = views.get(&terminal).cloned();
        let cwd = current_cwd(record).map(str::to_owned);
        let status = status(record);
        let state = record.pointer("/status/state").and_then(Value::as_str);
        let mut labels = Vec::new();
        if let Some(state) = state {
            let key = match status {
                AgentStatus::Idle => "idle",
                AgentStatus::Working => "working",
                AgentStatus::Blocked => "blocked",
                AgentStatus::Done => "done",
                AgentStatus::Unknown => "unknown",
            };
            let text = record
                .pointer("/status/kind")
                .and_then(Value::as_str)
                .unwrap_or(state);
            labels.push((key.to_owned(), text.to_owned()));
        }
        let title = text(record.pointer("/status/title"));
        let terminal_title = text(record.get("title"));
        let mut tokens = Vec::new();
        for (key, value) in [
            ("cwd", cwd.clone()),
            ("message", text(record.pointer("/status/msg"))),
            (
                "progress",
                record
                    .pointer("/status/progress")
                    .and_then(Value::as_u64)
                    .map(|value| format!("{value}%")),
            ),
        ] {
            if let Some(value) = value {
                tokens.push((key.into(), value));
            }
        }
        Some(ClientShellAgent {
            agent_id: terminal.clone(),
            runtime_target: Some(RuntimeTerminalTarget {
                machine_id,
                terminal,
            }),
            focused: view
                .as_ref()
                .is_some_and(|view| Some(view.pane_id.as_str()) == focused_pane),
            view,
            name: text(record.get("name")),
            display_agent: Some(app.clone()),
            agent: Some(app),
            title,
            terminal_title_stripped: terminal_title
                .as_deref()
                .and_then(crate::client::terminal_title::stripped_terminal_title),
            terminal_title,
            agent_status: status,
            state_change_seq: record["status_ms"]
                .as_u64()
                .or_else(|| record["created_ms"].as_u64())
                .unwrap_or(0),
            state_labels: labels,
            tokens,
            cwd,
        })
    }
}

fn view_index(
    workspaces: &[Workspace],
    owner: Option<&str>,
    focused: Option<&str>,
) -> BTreeMap<String, AgentViewTarget> {
    let mut views = BTreeMap::new();
    for workspace in workspaces
        .iter()
        .filter(|workspace| workspace.machine.as_deref() == owner)
    {
        for tab in &workspace.tabs {
            for pane in &tab.panes {
                let target = AgentViewTarget {
                    pane_id: pane.id.clone(),
                    workspace_id: workspace.id.clone(),
                    tab_id: tab.id.clone(),
                };
                if focused == Some(pane.id.as_str()) {
                    views.insert(pane.terminal.clone(), target);
                } else {
                    views.entry(pane.terminal.clone()).or_insert(target);
                }
            }
        }
    }
    views
}

pub(super) fn record_owner(record: &Value) -> Option<&str> {
    record["address"]
        .as_str()?
        .split_once('/')
        .map(|(owner, _)| owner)
}

pub(super) fn current_cwd(record: &Value) -> Option<&str> {
    record
        .pointer("/foreground/cwd")
        .and_then(Value::as_str)
        .or_else(|| record["cwd"].as_str())
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn reported_app(record: &Value) -> Option<String> {
    if let Some(app) = text(record.pointer("/status/app")) {
        return Some(app);
    }
    let programs = [
        record.pointer("/foreground/argv/0"),
        record.pointer("/foreground/name"),
        record.pointer("/argv/0"),
    ];
    let known = programs
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|arg| {
            let name = arg.rsplit('/').next()?;
            let canonical = match name {
                "claude-code" => "claude",
                name => name,
            };
            herdr_core_tui::detect::parse_agent_label(canonical).map(|_| canonical.to_owned())
        });
    known.or_else(|| {
        record.pointer("/status/state").and_then(Value::as_str)?;
        programs
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find_map(|program| {
                program
                    .rsplit('/')
                    .next()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
            })
    })
}

pub(super) fn status(record: &Value) -> AgentStatus {
    if record["state"] == "finished" {
        return AgentStatus::Done;
    }
    match record.pointer("/status/state").and_then(Value::as_str) {
        Some("idle") => AgentStatus::Idle,
        Some("working") => AgentStatus::Working,
        Some("blocked" | "error") => AgentStatus::Blocked,
        Some("done") => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}

fn aggregate(states: impl Iterator<Item = AgentStatus>) -> AgentStatus {
    states
        .max_by_key(|state| match state {
            AgentStatus::Blocked => 4,
            AgentStatus::Working => 3,
            AgentStatus::Done => 2,
            AgentStatus::Idle => 1,
            AgentStatus::Unknown => 0,
        })
        .unwrap_or(AgentStatus::Unknown)
}
