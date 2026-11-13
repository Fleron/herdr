use std::collections::HashMap;

use crate::client::endpoint::ClientEndpointId;
use crate::client::frame::{
    SemanticNotification, SemanticNotificationKind, SemanticNotificationSound,
};
use crate::client::shell::ClientShellNotificationEffect;
use crate::client::ui_domain::AgentStatus;
use crate::client::ui_model::{ClientShellAgent, ClientShellSnapshot, RuntimeTerminalTarget};
use crate::config::SoundConfig;

#[derive(Default)]
pub(super) struct NotificationTracker {
    owners: HashMap<ClientEndpointId, Observation>,
}

struct Observation {
    boot_id: String,
    states: HashMap<RuntimeTerminalTarget, AgentStatus>,
}

pub(super) struct RuntimeNotification {
    pub event: SemanticNotification,
    pub target: RuntimeTerminalTarget,
}

impl NotificationTracker {
    pub fn observe(
        &mut self,
        endpoint: &ClientEndpointId,
        machine_label: &str,
        snapshot: &ClientShellSnapshot,
        online: bool,
    ) -> Vec<RuntimeNotification> {
        if !online {
            self.owners.remove(endpoint);
            return Vec::new();
        }
        let observation = Observation {
            boot_id: snapshot.boot_id.clone(),
            states: snapshot
                .agents
                .iter()
                .filter_map(|agent| Some((agent.runtime_target.clone()?, agent.agent_status)))
                .collect(),
        };
        let Some(previous) = self.owners.insert(endpoint.clone(), observation) else {
            return Vec::new();
        };
        if previous.boot_id != snapshot.boot_id {
            return Vec::new();
        }
        snapshot
            .agents
            .iter()
            .filter_map(|agent| {
                let target = agent.runtime_target.as_ref()?;
                let old_status = previous
                    .states
                    .get(target)
                    .copied()
                    .unwrap_or(AgentStatus::Unknown);
                let kind = transition(old_status, agent.agent_status)?;
                let label = agent
                    .display_agent
                    .as_deref()
                    .or(agent.name.as_deref())
                    .or(agent.agent.as_deref())?;
                let (kind, text, sound) = match kind {
                    SemanticNotificationKind::NeedsAttention => {
                        (kind, "needs attention", SemanticNotificationSound::Request)
                    }
                    SemanticNotificationKind::Finished => {
                        (kind, "finished", SemanticNotificationSound::Done)
                    }
                    _ => return None,
                };
                Some(RuntimeNotification {
                    target: target.clone(),
                    event: SemanticNotification {
                        kind,
                        title: format!("{label} {text}"),
                        body: Some(context(machine_label, snapshot, agent)),
                        sound: Some(sound),
                        agent: agent.agent.clone(),
                        workspace_id: agent.view.as_ref().map(|view| view.workspace_id.clone()),
                        tab_id: agent.view.as_ref().map(|view| view.tab_id.clone()),
                        pane_id: agent.view.as_ref().map(|view| view.pane_id.clone()),
                        position: None,
                    },
                })
            })
            .collect()
    }
}

fn transition(previous: AgentStatus, current: AgentStatus) -> Option<SemanticNotificationKind> {
    if current == previous {
        return None;
    }
    match current {
        AgentStatus::Blocked => Some(SemanticNotificationKind::NeedsAttention),
        AgentStatus::Done if matches!(previous, AgentStatus::Working | AgentStatus::Blocked) => {
            Some(SemanticNotificationKind::Finished)
        }
        _ => None,
    }
}

fn context(
    machine_label: &str,
    snapshot: &ClientShellSnapshot,
    agent: &ClientShellAgent,
) -> String {
    let workspace = agent.view.as_ref().and_then(|view| {
        snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == view.workspace_id)
    });
    let mut parts = vec![machine_label];
    if let Some(workspace) = workspace {
        parts.push(&workspace.label);
        if let Some(tab) = agent
            .view
            .as_ref()
            .and_then(|view| snapshot.tabs.iter().find(|tab| tab.tab_id == view.tab_id))
        {
            parts.push(&tab.label);
        }
    } else if let Some(cwd) = agent.cwd.as_deref() {
        parts.push(cwd);
    }
    parts.join(" · ")
}

pub(super) fn emit(effects: Vec<ClientShellNotificationEffect>, sound_config: &SoundConfig) {
    for effect in effects {
        match effect {
            ClientShellNotificationEffect::Sound { sound, agent } => {
                let agent = agent.as_deref().and_then(crate::detect::parse_agent_label);
                if sound_config.allows(agent) {
                    crate::sound::play(sound, sound_config);
                }
            }
            ClientShellNotificationEffect::Terminal { title, body } => {
                if let Err(error) =
                    crate::terminal_notify::show_notification(&title, body.as_deref())
                {
                    tracing::warn!(%error, "failed to emit terminal notification");
                }
            }
            ClientShellNotificationEffect::System { title, body } => {
                if let Err(error) =
                    crate::platform::show_desktop_notification(&title, body.as_deref())
                {
                    tracing::warn!(%error, "failed to emit system notification");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(machine: &str, status: AgentStatus) -> ClientShellSnapshot {
        let mut snapshot = crate::client::shell::test_support::snapshot();
        snapshot.workspaces.clear();
        snapshot.tabs.clear();
        snapshot.panes.clear();
        snapshot.agents = vec![ClientShellAgent {
            cwd: Some("/repo".into()),
            agent_id: "program".into(),
            runtime_target: Some(RuntimeTerminalTarget {
                machine_id: machine.into(),
                terminal: "program".into(),
            }),
            view: None,
            name: None,
            display_agent: Some("Codex".into()),
            agent: Some("codex".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }];
        snapshot
    }

    fn endpoint(machine: &str) -> ClientEndpointId {
        ClientEndpointId::Core {
            machine_id: machine.into(),
            local: false,
        }
    }

    #[test]
    fn reported_transitions_notify_once_without_initial_or_reconnect_history() {
        let endpoint = endpoint("machine");
        let mut tracker = NotificationTracker::default();
        let mut snapshot = snapshot("machine", AgentStatus::Blocked);
        assert!(tracker
            .observe(&endpoint, "box", &snapshot, true)
            .is_empty());
        snapshot.agents[0].agent_status = AgentStatus::Working;
        assert!(tracker
            .observe(&endpoint, "box", &snapshot, true)
            .is_empty());
        snapshot.agents[0].agent_status = AgentStatus::Done;
        let events = tracker.observe(&endpoint, "box", &snapshot, true);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.kind, SemanticNotificationKind::Finished);
        assert_eq!(events[0].event.sound, Some(SemanticNotificationSound::Done));
        assert_eq!(events[0].event.body.as_deref(), Some("box · /repo"));
        assert!(events[0].event.pane_id.is_none());
        assert_eq!(
            events[0].target,
            snapshot.agents[0].runtime_target.clone().unwrap()
        );
        assert!(tracker
            .observe(&endpoint, "box", &snapshot, true)
            .is_empty());
        snapshot.agents[0].agent_status = AgentStatus::Blocked;
        assert_eq!(tracker.observe(&endpoint, "box", &snapshot, true).len(), 1);
        snapshot.boot_id = "replacement-connection".into();
        assert!(tracker
            .observe(&endpoint, "box", &snapshot, true)
            .is_empty());
        snapshot.agents[0].agent_status = AgentStatus::Idle;
        assert!(tracker
            .observe(&endpoint, "box", &snapshot, true)
            .is_empty());
        snapshot.agents[0].agent_status = AgentStatus::Unknown;
        assert!(tracker
            .observe(&endpoint, "box", &snapshot, true)
            .is_empty());
    }

    #[test]
    fn equal_terminal_names_on_distinct_core_owners_do_not_share_notifications() {
        let mut tracker = NotificationTracker::default();
        for machine in ["first", "second"] {
            assert!(tracker
                .observe(
                    &endpoint(machine),
                    machine,
                    &snapshot(machine, AgentStatus::Working),
                    true
                )
                .is_empty());
        }
        let blocked = snapshot("first", AgentStatus::Blocked);
        let events = tracker.observe(&endpoint("first"), "first", &blocked, true);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target.machine_id, "first");
        assert!(tracker
            .observe(
                &endpoint("second"),
                "second",
                &snapshot("second", AgentStatus::Working),
                true
            )
            .is_empty());
        assert!(tracker
            .observe(&endpoint("first"), "first", &blocked, false)
            .is_empty());
        assert!(tracker
            .observe(&endpoint("first"), "first", &blocked, true)
            .is_empty());
    }
}
