use super::*;
use crate::client::endpoint::SavedSshEndpoint;

impl ClientShellState {
    pub(crate) fn endpoint_projection_available(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.endpoints.iter().any(|endpoint| {
            &endpoint.endpoint_id == endpoint_id
                && endpoint.status == ClientEndpointStatus::Online
                && endpoint.snapshot.is_some()
        })
    }
    pub(crate) fn endpoint_boot_id(&self, endpoint_id: &ClientEndpointId) -> Option<&str> {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)?
            .snapshot
            .as_deref()
            .map(|snapshot| snapshot.boot_id.as_str())
    }
    pub(crate) fn endpoint_is_active(&self, endpoint_id: &ClientEndpointId) -> bool {
        &self.active_endpoint_id == endpoint_id
    }
    pub(crate) fn set_endpoint_catalog(&mut self, profiles: &[SavedSshEndpoint]) {
        let mut next = Vec::with_capacity(profiles.len().saturating_add(1));
        let local = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id.is_local())
            .cloned()
            .unwrap_or_else(local_endpoint);
        next.push(local);
        for profile in profiles {
            let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
            let previous = self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint_id == endpoint_id)
                .filter(|endpoint| {
                    profile.enabled && endpoint.status != ClientEndpointStatus::Disabled
                });
            next.push(ClientShellEndpoint {
                endpoint_id,
                label: profile.label.clone(),
                status: previous.map_or(
                    if profile.enabled {
                        ClientEndpointStatus::Connecting
                    } else {
                        ClientEndpointStatus::Disabled
                    },
                    |endpoint| endpoint.status,
                ),
                snapshot: previous.and_then(|endpoint| endpoint.snapshot.clone()),
                snapshot_generation: previous.and_then(|endpoint| endpoint.snapshot_generation),
                agent_recency: previous
                    .map(|endpoint| endpoint.agent_recency.clone())
                    .unwrap_or_default(),
                agent_presentation: previous
                    .map(|endpoint| endpoint.agent_presentation.clone())
                    .unwrap_or_default(),
                agent_view_projection: previous
                    .and_then(|endpoint| endpoint.agent_view_projection.clone()),
                pending_agent_view_projection: previous
                    .and_then(|endpoint| endpoint.pending_agent_view_projection.clone()),
                agent_view_projection_supported: previous
                    .is_some_and(|endpoint| endpoint.agent_view_projection_supported),
                methods: previous.and_then(|endpoint| endpoint.methods.clone()),
            });
        }

        if !next
            .iter()
            .any(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        {
            self.select_unavailable_local();
        }
        self.collapsed_endpoints.retain(|endpoint_id| {
            next.iter()
                .any(|endpoint| &endpoint.endpoint_id == endpoint_id)
        });
        self.endpoints = next;
    }
    pub(crate) fn select_unavailable_local(&mut self) {
        self.reset_endpoint_projection();
        self.active_endpoint_id = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id.is_local())
            .map(|endpoint| endpoint.endpoint_id.clone())
            .unwrap_or(ClientEndpointId::Local);
        self.mode = ClientShellMode::Terminal;
        self.snapshot = None;
        self.graphics.set_scope("local:unavailable");
        self.reconcile_input_source();
    }
    pub(crate) fn retire_endpoint(&mut self, endpoint_id: &ClientEndpointId) {
        self.clear_machine_diagnostic(endpoint_id);
        if endpoint_id == &self.active_endpoint_id {
            self.pending_workspace_highlight = None;
        }
        self.retire_endpoint_notifications(endpoint_id);
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.status = ClientEndpointStatus::Disabled;
            endpoint.snapshot = None;
            endpoint.snapshot_generation = None;
            endpoint.methods = None;
            endpoint.agent_recency.clear();
            endpoint.agent_presentation = Default::default();
            endpoint.agent_view_projection = None;
            endpoint.pending_agent_view_projection = None;
            endpoint.agent_view_projection_supported = false;
        }
    }
    pub(crate) fn mark_endpoint_disconnected(&mut self, endpoint_id: &ClientEndpointId) {
        self.set_endpoint_status(endpoint_id, ClientEndpointStatus::Reconnecting);
        if endpoint_id == &self.active_endpoint_id {
            let pending = self.pending_requests.keys().cloned().collect::<Vec<_>>();
            for request_id in pending {
                self.cancel_endpoint_request(&request_id);
            }
            self.pending_integration_installs = 0;
            self.pane_scroll_in_flight.clear();
            self.pane_scroll_queued.clear();
        }
    }
    pub(crate) fn set_endpoint_agent_view_projection_supported(
        &mut self,
        endpoint_id: &ClientEndpointId,
        supported: bool,
    ) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.agent_view_projection_supported = supported;
            if !supported {
                endpoint.agent_view_projection = None;
                endpoint.pending_agent_view_projection = None;
            }
        }
    }
    pub(crate) fn set_endpoint_methods_for(
        &mut self,
        endpoint_id: &ClientEndpointId,
        methods: Option<Vec<String>>,
    ) {
        let methods = methods.map(|methods| methods.into_iter().collect::<HashSet<_>>());
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.methods = methods;
        }
    }
    pub(crate) fn endpoint_snapshot_matches(
        &self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        boot_id: &str,
        revision: u64,
    ) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .is_some_and(|endpoint| {
                endpoint
                    .snapshot_generation
                    .is_none_or(|snapshot_generation| snapshot_generation == generation)
                    && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                        snapshot.boot_id == boot_id && snapshot.revision == revision
                    })
            })
    }
    pub(crate) fn endpoint_snapshot_identity(
        &self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> Option<(&str, u64)> {
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)?;
        if endpoint
            .snapshot_generation
            .is_some_and(|snapshot_generation| snapshot_generation != generation)
        {
            return None;
        }
        endpoint
            .snapshot
            .as_deref()
            .map(|snapshot| (snapshot.boot_id.as_str(), snapshot.revision))
    }
    pub(crate) fn set_endpoint_agent_completions(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        projection: crate::client::ui_model::EndpointAgentCompletions,
    ) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint
                .agent_presentation
                .receive_completions(Some(generation), projection);
        }
    }
    pub(crate) fn set_endpoint_agent_view_projection_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        projection: crate::client::endpoint::DecodedAgentViewProjection,
    ) {
        self.set_endpoint_agent_view_projection(
            endpoint_id,
            Some(generation),
            projection.boot_id,
            projection.revision,
            projection.view,
        );
    }
    #[cfg(test)]
    pub(crate) fn set_test_endpoint_agent_view(
        &mut self,
        endpoint_id: &ClientEndpointId,
        view: Option<crate::client::ui_domain::AgentViewSetParams>,
    ) {
        let Some((boot_id, revision)) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| {
                endpoint
                    .snapshot
                    .as_deref()
                    .map(|snapshot| (snapshot.boot_id.clone(), snapshot.revision))
            })
        else {
            return;
        };
        self.set_endpoint_agent_view_projection_supported(endpoint_id, true);
        self.set_endpoint_agent_view_projection(endpoint_id, None, boot_id, revision, Ok(view));
    }
    #[cfg(test)]
    pub(crate) fn set_test_endpoint_agent_view_projection(
        &mut self,
        endpoint_id: &ClientEndpointId,
        boot_id: &str,
        revision: u64,
        view: Option<crate::client::ui_domain::AgentViewSetParams>,
    ) {
        self.set_endpoint_agent_view_projection(
            endpoint_id,
            None,
            boot_id.to_owned(),
            revision,
            Ok(view),
        );
    }
    fn set_endpoint_agent_view_projection(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: Option<u64>,
        boot_id: String,
        revision: u64,
        view: Result<Option<crate::client::ui_domain::AgentViewSetParams>, ()>,
    ) {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if endpoint.snapshot_generation == generation
            && endpoint
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.boot_id == boot_id && snapshot.revision > revision)
        {
            return;
        }
        let next = ClientEndpointAgentViewProjection {
            generation,
            boot_id,
            revision,
            view,
        };
        let matches_snapshot = endpoint.snapshot_generation == next.generation
            && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                snapshot.boot_id == next.boot_id && snapshot.revision == next.revision
            });
        let slot = if matches_snapshot {
            &mut endpoint.agent_view_projection
        } else {
            &mut endpoint.pending_agent_view_projection
        };
        if slot.as_ref().is_some_and(|current| {
            current.generation == next.generation
                && current.boot_id == next.boot_id
                && current.revision >= next.revision
        }) {
            return;
        }
        *slot = Some(next);
    }
    /// A terminal normally starts focused. `None` means this host cannot report focus events,
    /// not that the endpoint has no viewer; activation therefore sends an explicit true baseline.
    pub(crate) fn host_focus_baseline(&self) -> bool {
        self.outer_focused.unwrap_or(true)
    }
    #[cfg(test)]
    pub(crate) fn set_endpoint_methods(&mut self, methods: Option<Vec<String>>) {
        let endpoint_id = self.active_endpoint_id.clone();
        self.set_endpoint_methods_for(&endpoint_id, methods);
    }
    #[cfg(test)]
    pub(crate) fn cache_endpoint_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_with_surface(endpoint_id, None, snapshot, true);
    }

    /// Metadata delivered while an endpoint surface is inactive must never advance this
    /// aggregate client's viewed watermark, even if the frozen source frame still exists.
    pub(crate) fn cache_endpoint_snapshot_inactive_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_with_surface(endpoint_id, Some(generation), snapshot, false);
    }
}
