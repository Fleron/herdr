use std::collections::HashMap;
use std::io;

use crate::client::core_backend::Action;
use crate::client::ui_domain::CommandInvokeParams;
use crate::config::CustomCommandAction;

impl super::App {
    pub(super) fn invoke_custom_command(
        &mut self,
        owner: Option<&str>,
        params: &CommandInvokeParams,
        completion: String,
    ) -> io::Result<Action> {
        let prefix = format!("cmd_{}_", self.command_namespace);
        let bindings = self.config.keybinds();
        let binding = params
            .command_id
            .strip_prefix(&prefix)
            .and_then(|index| index.parse::<usize>().ok())
            .and_then(|index| bindings.custom_commands.get(index))
            .ok_or_else(|| {
                io::Error::other(super::runtime::Error {
                    code: "command_not_found",
                    message: "Custom command configuration changed; select the command again"
                        .into(),
                })
            })?;
        let flag = match binding.action {
            CustomCommandAction::Shell => "-lc",
            CustomCommandAction::Pane => "-c",
            CustomCommandAction::Popup | CustomCommandAction::PluginAction => {
                return Err(io::Error::other(super::runtime::Error {
                    code: "unsupported",
                    message: "External tool popups and plugin actions are not available in this core port".into(),
                }));
            }
        };
        let pane = self.backend.focus_command_target(owner, params)?;
        let folder = self.backend.command_folder(owner, pane.as_deref())?;
        let mut env = HashMap::new();
        if let Some((workspace, tab, pane)) = pane.as_ref().and_then(|id| {
            self.backend
                .document()
                .workspaces
                .iter()
                .find_map(|workspace| {
                    workspace
                        .tabs
                        .iter()
                        .find_map(|tab| tab.pane(id).map(|pane| (workspace, tab, pane)))
                })
        }) {
            env.insert("HERDR_ACTIVE_WORKSPACE_ID".into(), workspace.id.clone());
            env.insert("HERDR_ACTIVE_TAB_ID".into(), tab.id.clone());
            env.insert("HERDR_ACTIVE_PANE_ID".into(), pane.id.clone());
            env.insert("HERDR_ACTIVE_PANE_CWD".into(), folder.clone());
        }
        let argv = vec!["/bin/sh".into(), flag.into(), binding.command.clone()];
        let action = if binding.action == CustomCommandAction::Shell {
            self.backend
                .run_background_command(owner, folder, argv, env, Some(completion))?
        } else {
            Action::Launch(self.backend.begin_overlay_command(
                owner,
                pane.as_deref(),
                folder,
                argv,
                env,
                Some(completion),
            )?)
        };
        self.view_dirty = true;
        self.sync_shell();
        Ok(action)
    }
}
