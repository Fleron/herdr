use std::io;

use crate::client::ui_model::ClientShellCommand;
use crate::config::{Config, NewTerminalCwdConfig, ShellModeConfig};
use herdr_core_tui::connection::Client;

use super::Backend;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct TerminalDefaults {
    pub cwd: NewTerminalCwdConfig,
    pub shell: Option<ShellOverride>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ShellOverride {
    executable: String,
    mode: ShellModeConfig,
}

impl Backend {
    pub(crate) fn command_manifest(
        bindings: &[crate::config::CustomCommandKeybind],
        namespace: &str,
    ) -> Vec<ClientShellCommand> {
        bindings
            .iter()
            .enumerate()
            .map(|(index, binding)| ClientShellCommand {
                command_id: format!("cmd_{namespace}_{index}"),
                binding_label: binding.label.clone(),
                binding_labels: binding.bindings.labels(),
                action: binding.action.into(),
                description: binding.description.clone(),
            })
            .collect()
    }

    pub(crate) fn focus_command_target(
        &mut self,
        owner: Option<&str>,
        params: &crate::client::ui_domain::CommandInvokeParams,
    ) -> io::Result<Option<String>> {
        let owner = owner.filter(|id| *id != self.local_id()).map(str::to_owned);
        if !self.owner_present(owner.as_deref()) {
            return Err(io::Error::other(
                "the command's core machine is no longer configured",
            ));
        }
        let (workspace, tab, pane) = if let Some(id) = params.pane_id.as_deref() {
            let (workspace, tab, pane) = self.pane_target(owner.as_deref(), Some(id))?;
            (Some(workspace), Some(tab), Some(pane))
        } else if let Some(id) = params.tab_id.as_deref() {
            let (workspace, tab) = self.tab_target(owner.as_deref(), Some(id))?;
            (Some(workspace), Some(tab), None)
        } else {
            let workspace = if let Some(id) = params.workspace_id.as_deref() {
                Some(self.workspace_target(owner.as_deref(), Some(id))?)
            } else {
                self.active_workspace_for(owner.as_deref()).cloned()
            };
            let tab = workspace.as_ref().and_then(|workspace| {
                self.view
                    .tabs
                    .get(&workspace.id)
                    .and_then(|id| workspace.tab(id))
                    .or_else(|| workspace.tabs.first())
                    .cloned()
            });
            (workspace, tab, None)
        };
        if params.workspace_id.as_deref().is_some_and(|expected| {
            workspace.as_ref().map(|workspace| workspace.id.as_str()) != Some(expected)
        }) {
            return Err(io::Error::other(
                "the command target does not belong to the requested workspace",
            ));
        }
        if params
            .tab_id
            .as_deref()
            .is_some_and(|expected| tab.as_ref().map(|tab| tab.id.as_str()) != Some(expected))
        {
            return Err(io::Error::other(
                "the command pane does not belong to the requested tab",
            ));
        }
        let pane = pane.or_else(|| {
            let tab = tab.as_ref()?;
            self.view
                .panes
                .get(&tab.id)
                .and_then(|id| tab.pane(id))
                .or_else(|| tab.panes.first())
                .map(|pane| pane.id.clone())
        });
        if let Some(pane) = pane.as_deref() {
            self.focus_pane(owner.as_deref(), pane)?;
        } else {
            self.relinquish_launch_focus();
            self.remember_focus();
            self.view.workspace = workspace.as_ref().map(|workspace| workspace.id.clone());
            if let (Some(workspace), Some(tab)) = (workspace, tab) {
                self.view.tabs.insert(workspace.id, tab.id);
            }
            self.view.active_owner = owner;
            self.remember_focus();
            self.revision += 1;
        }
        Ok(pane)
    }

    pub(crate) fn set_terminal_defaults(&mut self, config: &Config) {
        let defaults = TerminalDefaults {
            cwd: config.terminal.new_cwd.clone(),
            shell: (!config.terminal.default_shell.trim().is_empty()).then(|| ShellOverride {
                executable: config.terminal.default_shell.trim().to_owned(),
                mode: config.terminal.shell_mode,
            }),
        };
        if self.defaults != defaults {
            self.defaults = defaults;
            self.metadata_due = std::time::Instant::now();
            self.revision += 1;
        }
    }

    pub(crate) fn default_folder(&self, owner: Option<&str>, workspace: Option<&str>) -> String {
        let metadata = self.metadata.get(&owner.map(str::to_owned));
        let home = metadata
            .and_then(|metadata| metadata.home.as_deref())
            .unwrap_or("~");
        let current = metadata
            .and_then(|metadata| metadata.current.as_deref())
            .unwrap_or(".");
        match &self.defaults.cwd {
            NewTerminalCwdConfig::Follow => self
                .follow_folder(owner, workspace)
                .unwrap_or_else(|| home.to_owned()),
            NewTerminalCwdConfig::Home => home.to_owned(),
            NewTerminalCwdConfig::Current => current.to_owned(),
            NewTerminalCwdConfig::Path(path) if path == "~" => home.to_owned(),
            NewTerminalCwdConfig::Path(path) if path.starts_with("~/") => {
                format!("{}/{}", home.trim_end_matches('/'), &path[2..])
            }
            NewTerminalCwdConfig::Path(path) if path.starts_with('/') => path.clone(),
            NewTerminalCwdConfig::Path(path) => format!("{}/{path}", current.trim_end_matches('/')),
        }
    }

    pub(crate) fn command_folder(
        &self,
        owner: Option<&str>,
        pane: Option<&str>,
    ) -> io::Result<String> {
        if !self.owner_present(owner) {
            return Err(io::Error::other(
                "the command's core machine is no longer configured",
            ));
        }
        let pane = pane.map(str::to_owned).or_else(|| {
            let workspace = self.active_workspace_for(owner)?;
            let tab = self
                .view
                .tabs
                .get(&workspace.id)
                .and_then(|id| workspace.tab(id))
                .or_else(|| workspace.tabs.first())?;
            self.view
                .panes
                .get(&tab.id)
                .and_then(|id| tab.pane(id))
                .or_else(|| tab.panes.first())
                .map(|pane| pane.id.clone())
        });
        let Some(pane) = pane else {
            return Ok(self
                .metadata
                .get(&owner.map(str::to_owned))
                .and_then(|metadata| metadata.current.clone())
                .unwrap_or_else(|| ".".into()));
        };
        let (workspace, tab, pane) = self.pane_target(owner, Some(&pane))?;
        let pane = tab.pane(&pane).expect("validated command pane");
        let address = herdr_core_tui::model::terminal_address(owner, &pane.terminal);
        Ok(self
            .inventory
            .get(&address)
            .and_then(|record| {
                record
                    .pointer("/foreground/cwd")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| record["cwd"].as_str())
            })
            .map(str::to_owned)
            .unwrap_or(workspace.folder))
    }

    pub(crate) fn canonical_folder<'a>(&'a self, owner: Option<&str>, folder: &'a str) -> &'a str {
        self.metadata
            .get(&owner.map(str::to_owned))
            .and_then(|metadata| metadata.directories.get(folder))
            .map(String::as_str)
            .unwrap_or(folder)
    }
}

impl ShellOverride {
    pub(super) fn argv(&self, client: &Client, owner: Option<&str>) -> io::Result<Vec<String>> {
        let login = match self.mode {
            ShellModeConfig::Login => true,
            ShellModeConfig::NonLogin => false,
            ShellModeConfig::Auto => {
                let action = "reading the shell owner's platform";
                let output = herdr_core_tui::machine::exec(
                    client,
                    owner,
                    None,
                    &["uname", "-s"],
                    &[],
                    action,
                )?
                .ensure_success(action)?;
                let name = String::from_utf8(output.stdout).map_err(io::Error::other)?;
                name.trim() == "Darwin"
            }
        };
        let mut argv = vec![self.executable.clone()];
        if login {
            let name = self
                .executable
                .rsplit('/')
                .next()
                .unwrap_or(&self.executable);
            if !matches!(
                name,
                "sh" | "bash"
                    | "dash"
                    | "zsh"
                    | "fish"
                    | "ksh"
                    | "mksh"
                    | "csh"
                    | "tcsh"
                    | "nu"
                    | "nushell"
                    | "xonsh"
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "login mode for shell {name:?} cannot be expressed through the current core; choose non-login mode or use the core default shell"
                    ),
                ));
            }
            argv.push("-l".into());
        }
        Ok(argv)
    }
}
