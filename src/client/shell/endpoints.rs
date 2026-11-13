use super::*;

#[derive(Clone, Debug)]
pub(crate) struct ClientEndpointAgentViewProjection {
    generation: Option<u64>,
    boot_id: String,
    revision: u64,
    pub(crate) view: Result<Option<crate::client::ui_domain::AgentViewSetParams>, ()>,
}

#[derive(Clone, Debug)]
pub(crate) struct ClientShellEndpoint {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) label: String,
    pub(crate) status: ClientEndpointStatus,
    pub(crate) snapshot: Option<Box<ClientShellSnapshot>>,
    /// Connection generation that produced `snapshot`. `None` is reserved for local tests.
    pub(crate) snapshot_generation: Option<u64>,
    pub(crate) agent_recency: HashMap<String, u64>,
    pub(super) agent_presentation: super::endpoint_agent_state::EndpointAgentPresentation,
    pub(crate) agent_view_projection: Option<ClientEndpointAgentViewProjection>,
    pub(super) pending_agent_view_projection: Option<ClientEndpointAgentViewProjection>,
    pub(crate) agent_view_projection_supported: bool,
    pub(crate) methods: Option<HashSet<String>>,
}

pub(super) struct MachineHit {
    pub(super) rect: Rect,
    pub(super) status_badge: Rect,
    pub(super) collapse_toggle: Rect,
    pub(super) endpoint_id: ClientEndpointId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClientEndpointFocusTarget {
    Agent(crate::client::ui_model::RuntimeTerminalTarget),
    Workspace(String),
    Tab(String),
    Pane(String),
    #[cfg(windows)]
    Notification {
        pane_id: String,
        boot_id: String,
    },
}

impl ClientShellState {
    pub(crate) fn set_endpoint_status(
        &mut self,
        endpoint_id: &ClientEndpointId,
        status: ClientEndpointStatus,
    ) {
        if matches!(
            status,
            ClientEndpointStatus::Online | ClientEndpointStatus::Disabled
        ) {
            self.clear_machine_diagnostic(endpoint_id);
        }
        if endpoint_id == &self.active_endpoint_id && status != ClientEndpointStatus::Online {
            self.pending_workspace_highlight = None;
        }
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.status = status;
        }
    }

    pub(crate) fn activate_endpoint_projection(&mut self, endpoint_id: &ClientEndpointId) -> bool {
        let pending_agent_reveal = self
            .pending_agent_reveal
            .take_if(|(target_endpoint, _)| target_endpoint == endpoint_id);
        let agent_body_height = self.hits.agent_body.height;
        let Some(endpoint) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return false;
        };
        if endpoint.status != ClientEndpointStatus::Online {
            return false;
        }
        let Some(snapshot) = endpoint.snapshot.clone() else {
            return false;
        };
        let generation = endpoint.snapshot_generation;
        let switching_endpoint = endpoint_id != &self.active_endpoint_id;
        let agent_scroll = self.agent_scroll;
        if switching_endpoint {
            self.active_endpoint_id = endpoint_id.clone();
            self.pane_surface = None;
            self.pending_pane_surface = None;
        }
        self.apply_active_snapshot(snapshot, generation);
        if switching_endpoint {
            // The aggregate agent list belongs to the client, not one endpoint.
            self.agent_scroll = agent_scroll;
        }
        if let Some((_, pane_id)) = pending_agent_reveal {
            self.reveal_endpoint_agent(endpoint_id, &pane_id, agent_body_height);
        }
        true
    }

    pub(crate) fn endpoint_status(
        &self,
        endpoint_id: &ClientEndpointId,
    ) -> Option<ClientEndpointStatus> {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .map(|endpoint| endpoint.status)
    }

    pub(crate) fn endpoint_has_snapshot(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .is_some_and(|endpoint| endpoint.snapshot.is_some())
    }

    pub(crate) fn endpoint_is_online(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.endpoint_status(endpoint_id) == Some(ClientEndpointStatus::Online)
            && self.endpoint_has_snapshot(endpoint_id)
    }

    pub(crate) fn endpoint_agent_view(
        endpoint: &ClientShellEndpoint,
    ) -> Option<&Result<Option<crate::client::ui_domain::AgentViewSetParams>, ()>> {
        let snapshot = endpoint.snapshot.as_deref()?;
        let projection = endpoint.agent_view_projection.as_ref()?;
        (projection.generation == endpoint.snapshot_generation
            && projection.boot_id == snapshot.boot_id
            && projection.revision == snapshot.revision)
            .then_some(&projection.view)
    }

    pub(crate) fn endpoint_label(&self, endpoint_id: &ClientEndpointId) -> &str {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .map_or("Unknown endpoint", |endpoint| endpoint.label.as_str())
    }

    pub(crate) fn active_endpoint_label(&self) -> &str {
        self.endpoint_label(&self.active_endpoint_id)
    }

    pub(crate) fn multi_endpoint_active(&self) -> bool {
        self.endpoints.len() > 1
            || self
                .endpoints
                .iter()
                .any(|endpoint| matches!(endpoint.endpoint_id, ClientEndpointId::Core { .. }))
    }

    #[cfg(test)]
    pub(crate) fn set_snapshot(&mut self, snapshot: Box<ClientShellSnapshot>) {
        let endpoint_id = self.active_endpoint_id.clone();
        self.set_endpoint_snapshot(&endpoint_id, snapshot);
    }

    pub(super) fn supports_endpoint_method(
        &self,
        method: &crate::client::ui_action::FrontendCommand,
    ) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.methods.as_ref())
            .is_none_or(|methods| methods.contains(method.name()))
    }

    pub(super) fn focused_tab_count(&self) -> usize {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return 0;
        };
        snapshot
            .tabs
            .iter()
            .filter(|tab| {
                Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref()
            })
            .count()
    }

    fn cache_endpoint_snapshot_with_surface(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: Option<u64>,
        mut snapshot: Box<ClientShellSnapshot>,
        acknowledge_surface: bool,
    ) {
        snapshot.commands.retain(|command| {
            command.action != crate::client::ui_model::ClientShellCommandAction::Unknown
        });
        let Some(index) = self
            .endpoints
            .iter()
            .position(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if self.endpoints[index].snapshot_generation == generation
            && self.endpoints[index]
                .snapshot
                .as_deref()
                .is_some_and(|previous| {
                    previous.boot_id == snapshot.boot_id && previous.revision > snapshot.revision
                })
        {
            return;
        }
        let boot_changed = self.endpoints[index]
            .snapshot
            .as_deref()
            .is_some_and(|previous| previous.boot_id != snapshot.boot_id);
        if boot_changed {
            self.retire_endpoint_notifications(endpoint_id);
        }
        self.endpoints[index]
            .agent_presentation
            .project_snapshot_for_generation(&mut snapshot, generation);
        let presented_surface = if acknowledge_surface && endpoint_id == &self.active_endpoint_id {
            self.pane_surface.as_ref()
        } else {
            None
        };
        if let Some(surface) = presented_surface {
            self.endpoints[index]
                .agent_presentation
                .acknowledge_surface(&mut snapshot, surface, self.outer_focused);
        }
        let previous = self.endpoints[index].snapshot.as_deref();
        let mut next_recency = self
            .endpoints
            .iter()
            .flat_map(|endpoint| endpoint.agent_recency.values())
            .copied()
            .max()
            .unwrap_or_default();
        let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
        agents.sort_by_key(|agent| agent.state_change_seq);
        let mut recency = self.endpoints[index].agent_recency.clone();
        for agent in agents {
            let changed = previous
                .and_then(|snapshot| {
                    snapshot
                        .agents
                        .iter()
                        .find(|previous| previous.agent_id == agent.agent_id)
                })
                .is_none_or(|previous| previous.state_change_seq != agent.state_change_seq);
            if changed {
                next_recency = next_recency.saturating_add(1);
                recency.insert(agent.agent_id.clone(), next_recency);
            }
        }
        recency.retain(|pane_id, _| {
            snapshot
                .agents
                .iter()
                .any(|agent| &agent.agent_id == pane_id)
        });
        let endpoint = &mut self.endpoints[index];
        endpoint.agent_recency = recency;
        endpoint.snapshot_generation = generation;
        endpoint.snapshot = Some(snapshot);
        let pending_matches =
            endpoint
                .pending_agent_view_projection
                .as_ref()
                .is_some_and(|projection| {
                    projection.generation == generation
                        && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                            projection.boot_id == snapshot.boot_id
                                && projection.revision == snapshot.revision
                        })
                });
        if pending_matches {
            endpoint.agent_view_projection = endpoint.pending_agent_view_projection.take();
        } else {
            endpoint.pending_agent_view_projection = None;
            if endpoint
                .agent_view_projection
                .as_ref()
                .is_some_and(|projection| {
                    projection.generation != generation
                        || endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                            projection.boot_id != snapshot.boot_id
                                || projection.revision != snapshot.revision
                        })
                })
            {
                endpoint.agent_view_projection = None;
            }
        }
    }

    pub(crate) fn acknowledge_active_surface_agents(&mut self, surface: &PaneSurfaceFrame) -> bool {
        let Some(index) = self
            .endpoints
            .iter()
            .position(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        else {
            return false;
        };
        let changed = {
            let endpoint = &mut self.endpoints[index];
            let Some(snapshot) = endpoint.snapshot.as_deref_mut() else {
                return false;
            };
            endpoint
                .agent_presentation
                .acknowledge_surface(snapshot, surface, self.outer_focused)
        };
        if changed {
            self.snapshot = self.endpoints[index].snapshot.clone();
        }
        changed
    }

    #[cfg(test)]
    pub(crate) fn set_endpoint_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_with_surface(endpoint_id, None, snapshot, true);
        self.apply_cached_endpoint_snapshot(endpoint_id);
    }

    pub(crate) fn cache_endpoint_snapshot_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_with_surface(endpoint_id, Some(generation), snapshot, true);
    }

    pub(crate) fn set_endpoint_snapshot_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_for_generation(endpoint_id, generation, snapshot);
        self.apply_cached_endpoint_snapshot(endpoint_id);
    }

    fn apply_cached_endpoint_snapshot(&mut self, endpoint_id: &ClientEndpointId) {
        let Some((snapshot, generation)) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| {
                endpoint
                    .snapshot
                    .clone()
                    .map(|snapshot| (snapshot, endpoint.snapshot_generation))
            })
        else {
            return;
        };
        if endpoint_id == &self.active_endpoint_id {
            self.apply_active_snapshot(snapshot, generation);
        }
    }
}

pub(super) fn endpoint_status_presentation(
    status: ClientEndpointStatus,
    palette: &Palette,
) -> (&'static str, &'static str, ratatui::style::Color) {
    match status {
        ClientEndpointStatus::Connecting => ("◐", "connecting", palette.yellow),
        ClientEndpointStatus::Online => ("●", "online", palette.green),
        ClientEndpointStatus::Reconnecting => ("◐", "reconnecting", palette.yellow),
        ClientEndpointStatus::Attention => ("!", "attention", palette.red),
        ClientEndpointStatus::Disabled => ("·", "disabled", palette.overlay0),
    }
}

pub(super) fn local_endpoint() -> ClientShellEndpoint {
    ClientShellEndpoint {
        endpoint_id: ClientEndpointId::Local,
        label: "Local".into(),
        status: ClientEndpointStatus::Online,
        snapshot: None,
        snapshot_generation: None,
        agent_recency: HashMap::new(),
        agent_presentation: Default::default(),
        agent_view_projection: None,
        pending_agent_view_projection: None,
        agent_view_projection_supported: false,
        methods: None,
    }
}

pub(super) fn agent_focus_target(
    agent: &crate::client::ui_model::ClientShellAgent,
) -> Option<ClientEndpointFocusTarget> {
    agent
        .runtime_target
        .clone()
        .map(ClientEndpointFocusTarget::Agent)
        .or_else(|| {
            agent
                .view
                .as_ref()
                .map(|view| ClientEndpointFocusTarget::Pane(view.pane_id.clone()))
        })
}

legacy_shell_extensions! {
    #[path = "endpoints/legacy.rs"]
    mod legacy;
}
