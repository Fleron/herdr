use std::io;

use crate::client::ui_domain as schema;

use super::Backend;

impl Backend {
    pub(crate) fn pane_info(
        &self,
        owner: Option<&str>,
        pane_id: &str,
    ) -> io::Result<schema::PaneInfo> {
        let (workspace, tab, pane_id) = self.pane_target(owner, Some(pane_id))?;
        let pane = tab.pane(&pane_id).expect("pane target validated").clone();
        let address = herdr_core_tui::model::terminal_address(owner, &pane.terminal);
        let record = self.inventory.get(&address);
        let snapshot = self.snapshot(owner);
        let agent = snapshot.agents.iter().find(|agent| {
            agent
                .view
                .as_ref()
                .is_some_and(|view| view.pane_id == pane_id)
        });
        let text = |pointer: &str| {
            record
                .and_then(|record| record.pointer(pointer))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        Ok(schema::PaneInfo {
            pane_id: pane_id.clone(),
            terminal_id: pane.terminal.clone(),
            workspace_id: workspace.id,
            tab_id: tab.id,
            focused: snapshot.focused_pane_id.as_deref() == Some(&pane_id),
            cwd: text("/cwd"),
            foreground_cwd: text("/foreground/cwd"),
            restore_error: None,
            label: pane.name.clone(),
            agent: agent.and_then(|agent| agent.agent.clone()),
            title: text("/status/title"),
            terminal_title: text("/title"),
            terminal_title_stripped: text("/title")
                .as_deref()
                .and_then(crate::client::terminal_title::stripped_terminal_title),
            display_agent: agent.and_then(|agent| agent.display_agent.clone()),
            agent_status: record
                .map(super::projection::status)
                .unwrap_or(schema::AgentStatus::Unknown),
            state_labels: agent
                .map(|agent| agent.state_labels.iter().cloned().collect())
                .unwrap_or_default(),
            tokens: agent
                .map(|agent| agent.tokens.iter().cloned().collect())
                .unwrap_or_default(),
            agent_session: None,
            scroll: None,
            revision: self.revision,
        })
    }

    pub(crate) fn tab_info(&self, owner: Option<&str>, id: &str) -> io::Result<schema::TabInfo> {
        let (workspace, tab) = self.tab_target(owner, Some(id))?;
        let snapshot = self.snapshot(owner);
        let projected = snapshot
            .tabs
            .iter()
            .find(|entry| entry.tab_id == tab.id)
            .ok_or_else(|| io::Error::other("the tab projection is unavailable"))?;
        Ok(schema::TabInfo {
            tab_id: tab.id,
            workspace_id: workspace.id,
            number: projected.number,
            label: projected.label.clone(),
            focused: projected.focused,
            pane_count: tab.panes.len(),
            agent_status: projected.agent_status,
        })
    }

    pub(crate) fn workspace_info(
        &self,
        owner: Option<&str>,
        id: &str,
    ) -> io::Result<schema::WorkspaceInfo> {
        let workspace = self.workspace_target(owner, Some(id))?;
        let snapshot = self.snapshot(owner);
        let projected = snapshot
            .workspaces
            .iter()
            .find(|entry| entry.workspace_id == workspace.id)
            .ok_or_else(|| io::Error::other("the workspace projection is unavailable"))?;
        Ok(schema::WorkspaceInfo {
            workspace_id: workspace.id,
            number: projected.number,
            label: projected.label.clone(),
            focused: projected.focused,
            pane_count: workspace.tabs.iter().map(|tab| tab.panes.len()).sum(),
            tab_count: workspace.tabs.len(),
            active_tab_id: projected.active_tab_id.clone(),
            agent_status: projected.agent_status,
            tokens: projected.tokens.iter().cloned().collect(),
            worktree: self.repository(owner, &workspace.folder).map(|repository| {
                schema::WorkspaceWorktreeInfo {
                    repo_key: repository.key.clone(),
                    repo_name: repository.label.clone(),
                    repo_root: repository.primary.clone(),
                    checkout_path: repository.root.clone(),
                    is_linked_worktree: repository.linked,
                }
            }),
        })
    }
}
