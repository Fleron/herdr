use super::*;
use std::path::PathBuf;

pub(in crate::client::shell) fn path_for_local_endpoint(socket_path: &Path) -> PathBuf {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in socket_path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("local-{hash:016x}.json"))
}
/// SSH preferences belong to the target/session, not its per-process bridge socket.
pub(in crate::client::shell) fn path_for_remote_endpoint(target: &str, session: &str) -> PathBuf {
    use sha2::{Digest as _, Sha256};

    let mut hash = Sha256::new();
    for part in [target, session] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("remote-{:x}.json", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_paths_are_stable_and_distinct() {
        let first = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let again = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let second = path_for_local_endpoint(Path::new("/run/herdr/two.sock"));
        assert_eq!(first, again);
        assert_ne!(first, second);
    }
    #[test]
    fn remote_preferences_distinguish_targets_sessions_and_component_boundaries() {
        let original = path_for_remote_endpoint("dev@build", "agents");
        assert_eq!(original, path_for_remote_endpoint("dev@build", "agents"));
        assert_ne!(original, path_for_remote_endpoint("other@build", "agents"));
        assert_ne!(original, path_for_remote_endpoint("dev@build", "other"));
        assert_ne!(
            path_for_remote_endpoint("ab", "c"),
            path_for_remote_endpoint("a", "bc")
        );
        assert_ne!(
            original,
            path_for_local_endpoint(Path::new("dev@build/agents"))
        );
    }
}
