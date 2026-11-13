use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use super::{backend, core_support::wait_until, core_support::CoreSession};

pub(super) fn add_machine(
    local: &CoreSession,
    remote: &CoreSession,
    blocked: &std::path::Path,
) -> serde_json::Value {
    drop(remote.client());
    let client = local.client();
    let connector = local.root.join("remote-core-connector");
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let script = format!(
        "#!/bin/sh\nif test -e {}; then echo 'fixture connection deliberately unavailable' >&2; exit 1; fi\nexec env -i HOME={} PATH={} SHELL=/bin/sh TERM=xterm-256color HC_SESSION={} XDG_RUNTIME_DIR={} XDG_STATE_HOME={} XDG_CONFIG_HOME={} {} connect\n",
        quote(blocked.to_str().unwrap()),
        quote(remote.root.to_str().unwrap()), quote(&std::env::var("PATH").unwrap()),
        quote(&remote.session), quote(remote.root.join("run").to_str().unwrap()),
        quote(remote.root.join("state").to_str().unwrap()), quote(remote.root.join("config").to_str().unwrap()),
        quote(remote.binary.to_str().unwrap()),
    );
    std::fs::write(&connector, script).unwrap();
    std::fs::set_permissions(&connector, std::fs::Permissions::from_mode(0o700)).unwrap();
    client
        .request(
            "machine.add",
            json!({
                "name": "box", "via": quote(connector.to_str().unwrap()), "session": remote.session,
                "temporary": true
            }),
        )
        .unwrap()
}

#[test]
fn machine_retry_uses_fresh_permanent_identity_and_reports_real_connection_errors() {
    let local = CoreSession::new();
    let remote = CoreSession::new();
    let blocked = local.root.join("connection-blocked");
    let profile = add_machine(&local, &remote, &blocked);
    let client = local.client();
    let id = profile["id"].as_str().unwrap();
    let mut backend = backend(&local);
    wait_until(|| {
        backend.poll().unwrap();
        backend.machine_views().iter().any(|machine| {
            machine.machine_id == id
                && machine.status == crate::client::endpoint::ClientEndpointStatus::Online
        })
    });
    assert!(backend.retry_machine("removed-identity").is_err());
    client
        .request(
            "machine.rename",
            json!({"machine":"box", "new_name":"renamed"}),
        )
        .unwrap();
    backend.retry_machine(id).unwrap();
    let mut completion = None;
    wait_until(|| {
        completion = backend
            .poll()
            .unwrap()
            .machine_retries
            .into_iter()
            .find(|outcome| outcome.machine_id == id)
            .or(completion.take());
        completion.is_some()
    });
    assert!(completion.take().unwrap().error.is_none());

    std::fs::write(&blocked, b"blocked").unwrap();
    remote.successful(&["server", "stop"]);
    wait_until(|| {
        backend.poll().unwrap();
        backend
            .machine_views()
            .iter()
            .find(|machine| machine.machine_id == id)
            .and_then(|machine| machine.diagnostic.as_deref())
            .is_some_and(|message| message.contains("fixture connection deliberately unavailable"))
    });
    backend.retry_machine(id).unwrap();
    wait_until(|| {
        completion = backend
            .poll()
            .unwrap()
            .machine_retries
            .into_iter()
            .find(|outcome| outcome.machine_id == id)
            .or(completion.take());
        completion.is_some()
    });
    assert!(completion
        .take()
        .unwrap()
        .error
        .unwrap()
        .contains("fixture connection deliberately unavailable"));
    std::fs::remove_file(&blocked).unwrap();
    backend.retry_machine(id).unwrap();
    wait_until(|| {
        completion = backend
            .poll()
            .unwrap()
            .machine_retries
            .into_iter()
            .find(|outcome| outcome.machine_id == id)
            .or(completion.take());
        completion.is_some()
    });
    assert!(completion.take().unwrap().error.is_none());
    client
        .request("machine.rm", json!({"machine":"renamed"}))
        .unwrap();
    backend.retry_machine(id).unwrap();
    wait_until(|| {
        completion = backend
            .poll()
            .unwrap()
            .machine_retries
            .into_iter()
            .find(|outcome| outcome.machine_id == id)
            .or(completion.take());
        completion.is_some()
    });
    assert!(completion
        .unwrap()
        .error
        .unwrap()
        .contains("no longer configured"));
    assert!(backend.document().workspaces.is_empty());
    drop(backend);
    local.finish();
    remote.finish();
}
