use super::*;

impl ClientShellState {
    pub(crate) fn receive_endpoint_error(&mut self, message: String) -> bool {
        self.push_endpoint_notice(
            ClientEndpointNoticeKind::Rejected,
            "paste_rejected",
            "Paste rejected",
            message,
        )
    }
    pub(crate) fn focus_endpoint_target(
        &mut self,
        target: ClientEndpointFocusTarget,
    ) -> Vec<ClientShellAction> {
        #[cfg(windows)]
        if !self.notification_target_is_current(&self.active_endpoint_id, &target) {
            return Vec::new();
        }
        if matches!(target, ClientEndpointFocusTarget::Agent(_)) {
            let mut outcome = ClientShellInput::default();
            self.focus_or_activate(self.active_endpoint_id.clone(), target, &mut outcome);
            return outcome.actions;
        }
        let method = match target {
            ClientEndpointFocusTarget::Agent(_) => unreachable!("agent target resolved above"),
            ClientEndpointFocusTarget::Workspace(workspace_id) => {
                crate::client::ui_action::FrontendCommand::WorkspaceFocus(
                    crate::client::ui_domain::WorkspaceTarget { workspace_id },
                )
            }
            ClientEndpointFocusTarget::Tab(tab_id) => {
                crate::client::ui_action::FrontendCommand::TabFocus(
                    crate::client::ui_domain::TabTarget { tab_id },
                )
            }
            ClientEndpointFocusTarget::Pane(pane_id) => {
                crate::client::ui_action::FrontendCommand::PaneFocus(
                    crate::client::ui_domain::PaneTarget { pane_id },
                )
            }
            #[cfg(windows)]
            ClientEndpointFocusTarget::Notification { pane_id, .. } => {
                crate::client::ui_action::FrontendCommand::PaneFocus(
                    crate::client::ui_domain::PaneTarget { pane_id },
                )
            }
        };
        let mut outcome = ClientShellInput::default();
        self.push_endpoint_method(method, &mut outcome);
        outcome.actions
    }
    pub(crate) fn cancel_endpoint_request(&mut self, request_id: &str) -> bool {
        let Some(pending) = self.pending_requests.get(request_id) else {
            return false;
        };
        let boot_id = pending.boot_id.clone();
        let (repaint, actions) = self.handle_endpoint_result(
            &boot_id,
            request_id,
            Err(ClientShellEndpointError {
                code: Some("endpoint_cancelled".into()),
                message: "This server action was interrupted. Check its state before retrying."
                    .into(),
            }),
        );
        debug_assert!(
            actions.is_empty(),
            "cancellation must not start another action"
        );
        repaint
    }
}
