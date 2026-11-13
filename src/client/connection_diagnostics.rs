legacy_shell_extensions! {
    pub(crate) const REMOTE_PREFERENCES_ENV_VAR: &str = "HERDR_REMOTE_PREFERENCES_IDENTITY";
}

pub(crate) fn ssh_error_requires_authentication(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if message.contains("host key verification failed")
        || message.contains("remote host identification has changed")
    {
        return false;
    }
    (message.contains("permission denied")
        && ["(publickey", "(keyboard-interactive", "(password"]
            .iter()
            .any(|method| message.contains(method)))
        || (message.contains("signing failed")
            && (message.contains("sign_and_send_pubkey") || message.contains("agent")))
}
