use super::*;

impl ClientShellState {
    pub(crate) fn receive_notification(
        &mut self,
        endpoint_id: &ClientEndpointId,
        event: SemanticNotification,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        self.receive_targeted_notification(endpoint_id, event, None, now)
    }
}
