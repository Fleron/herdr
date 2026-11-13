use super::endpoints::ClientShellEndpoint;
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct CoreMachineView {
    pub machine_id: String,
    pub label: String,
    pub local: bool,
    pub status: crate::client::endpoint::ClientEndpointStatus,
    pub diagnostic: Option<String>,
}

impl ClientShellState {
    pub(crate) fn set_core_catalog(
        &mut self,
        machines: &[crate::client::ui_model::CoreMachineView],
    ) {
        let mut next = Vec::with_capacity(machines.len());
        for machine in machines {
            let endpoint_id = ClientEndpointId::Core {
                machine_id: machine.machine_id.clone(),
                local: machine.local,
            };
            let previous = self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint_id == endpoint_id);
            next.push(ClientShellEndpoint {
                endpoint_id,
                label: machine.label.clone(),
                status: machine.status,
                snapshot: previous.and_then(|endpoint| endpoint.snapshot.clone()),
                snapshot_generation: previous.and_then(|endpoint| endpoint.snapshot_generation),
                agent_recency: previous
                    .map(|endpoint| endpoint.agent_recency.clone())
                    .unwrap_or_default(),
                agent_presentation: previous
                    .map(|endpoint| endpoint.agent_presentation.clone())
                    .unwrap_or_default(),
                agent_view_projection: None,
                pending_agent_view_projection: None,
                agent_view_projection_supported: false,
                methods: previous.and_then(|endpoint| endpoint.methods.clone()),
            });
        }
        if !next
            .iter()
            .any(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        {
            self.reset_endpoint_projection();
            if let Some(local) = next.iter().find(|endpoint| endpoint.endpoint_id.is_local()) {
                self.active_endpoint_id = local.endpoint_id.clone();
            }
            self.snapshot = None;
            self.pane_surface = None;
            self.mode = ClientShellMode::Terminal;
        }
        self.collapsed_endpoints
            .retain(|id| next.iter().any(|endpoint| &endpoint.endpoint_id == id));
        self.endpoints = next;
        self.reconcile_core_machine_diagnostics(machines);
    }
    pub(super) fn reconcile_core_machine_diagnostics(
        &mut self,
        machines: &[crate::client::ui_model::CoreMachineView],
    ) {
        let owners: HashSet<_> = machines
            .iter()
            .map(|machine| ClientEndpointId::Core {
                machine_id: machine.machine_id.clone(),
                local: machine.local,
            })
            .collect();
        self.machine_diagnostics
            .errors
            .retain(|id, _| owners.contains(id));
        self.machine_diagnostics
            .retry_requests
            .retain(|id| owners.contains(id));
        self.machine_diagnostics
            .retry_reports
            .retain(|id| owners.contains(id));
        for machine in machines {
            let id = ClientEndpointId::Core {
                machine_id: machine.machine_id.clone(),
                local: machine.local,
            };
            if machine.status == ClientEndpointStatus::Online {
                self.clear_machine_diagnostic(&id);
            } else if let Some(message) = machine.diagnostic.as_ref() {
                self.set_machine_diagnostic(&id, message.clone());
            } else {
                self.machine_diagnostics.errors.remove(&id);
            }
        }
    }
    pub(crate) fn finish_machine_retry(
        &mut self,
        id: &ClientEndpointId,
        error: Option<String>,
    ) -> bool {
        if let Some(error) = error {
            self.set_machine_diagnostic(id, error);
            return self.report_machine_retry(id);
        }
        let changed = self.machine_diagnostics.errors.contains_key(id)
            || self.machine_diagnostics.retry_reports.contains(id);
        self.clear_machine_diagnostic(id);
        changed
    }
    pub(crate) fn refresh_hit_map(&mut self, cols: u16, rows: u16) -> bool {
        self.compose_inner(cols, rows, false).is_some()
    }
    pub(crate) fn receive_runtime_notification(
        &mut self,
        endpoint_id: &ClientEndpointId,
        event: SemanticNotification,
        target: crate::client::ui_model::RuntimeTerminalTarget,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        self.receive_targeted_notification(endpoint_id, event, Some(target), now)
    }
    pub(crate) fn begin_checkout_worktree(
        &mut self,
        target: crate::client::ui_model::CheckoutTarget,
        create: bool,
    ) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        let correct_owner = matches!(&self.active_endpoint_id,ClientEndpointId::Core{machine_id,..} if machine_id==&target.machine_id);
        if !correct_owner {
            outcome.repaint |= self.receive_runtime_error(
                "Worktree source machine changed before opening the dialog".into(),
            );
            return outcome;
        }
        let source = ClientWorktreeSource::Checkout(target.clone());
        let kind = if create {
            PendingEndpointKind::PrepareWorktreeCreate { source }
        } else {
            PendingEndpointKind::PrepareWorktreeOpen { source }
        };
        self.push_endpoint_method_with_kind(
            crate::client::ui_action::FrontendCommand::WorktreeList(
                crate::client::ui_domain::WorktreeListParams {
                    workspace_id: None,
                    cwd: Some(target.folder),
                    trust_repository: false,
                },
            ),
            kind,
            &mut outcome,
        );
        outcome
    }
}

core_shell_extensions! {
    host;
    impl ClientShellState {
        pub(crate) fn graphics_delivery_epoch(&self) -> u64 {
            self.graphics.delivery_epoch()
        }
        pub(crate) fn palette(&self) -> &crate::palette::Palette {
            &self.config.palette
        }
    }
    impl ClientShellConfig {
        pub(crate) fn with_core_profile(self, core_id: &str) -> Self {
            self.with_preferences_path(path_for_core(core_id))
        }
    }
    fn path_for_core(core_id: &str) -> std::path::PathBuf {
        use sha2::{Digest as _, Sha256};
        crate::config::state_dir()
            .join("client-shell")
            .join(format!(
                "core-{:x}.json",
                Sha256::digest(core_id.as_bytes())
            ))
    }
}
