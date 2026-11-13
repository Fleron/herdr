use super::*;

impl ClientShellState {
    pub(crate) fn receive_machine_unavailable(
        &mut self,
        id: &ClientEndpointId,
        message: String,
    ) -> bool {
        self.push_endpoint_notice(
            ClientEndpointNoticeKind::Unavailable,
            format!("machine-unavailable:{}:{message}", id.storage_key()),
            "Endpoint unavailable",
            message,
        )
    }
}
