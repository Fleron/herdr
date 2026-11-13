use std::os::unix::fs::PermissionsExt;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use herdr_core_tui::connection::Client;
use serde_json::json;

use super::core_support::{wait_until, CoreSession};

pub struct RemoteFixture {
    pub local: CoreSession,
    pub remote: CoreSession,
    client: Option<Client>,
}

impl RemoteFixture {
    pub fn new() -> Self {
        Self::with_remote_start(|remote| drop(remote.client()))
    }

    pub fn with_remote_start(start: impl FnOnce(&CoreSession)) -> Self {
        let mut fixture = Self {
            local: CoreSession::new(),
            remote: CoreSession::new(),
            client: None,
        };
        start(&fixture.remote);
        fixture.client = Some(fixture.local.client());
        let connector = fixture.local.root.join("remote-core-connector");
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let remote = &fixture.remote;
        let script = format!(
            "#!/bin/sh\nexec env -i HOME={} PATH={} SHELL=/bin/sh TERM=xterm-256color HC_SESSION={} XDG_RUNTIME_DIR={} XDG_STATE_HOME={} XDG_CONFIG_HOME={} {} connect\n",
            quote(remote.root.to_str().unwrap()), quote(&std::env::var("PATH").unwrap()),
            quote(&remote.session), quote(remote.root.join("run").to_str().unwrap()),
            quote(remote.root.join("state").to_str().unwrap()), quote(remote.root.join("config").to_str().unwrap()),
            quote(remote.binary.to_str().unwrap()),
        );
        std::fs::write(&connector, script).unwrap();
        std::fs::set_permissions(&connector, std::fs::Permissions::from_mode(0o700)).unwrap();
        fixture.client().request("machine.add", json!({
            "name": "box", "via": quote(connector.to_str().unwrap()), "session": fixture.remote.session,
            "temporary": true
        })).unwrap();
        wait_until(|| {
            fixture.client().request("machine.list", json!({})).unwrap()["machines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|machine| machine["name"] == "box" && machine["state"] == "connected")
        });
        fixture
    }

    pub fn client(&self) -> &Client {
        self.client.as_ref().unwrap()
    }

    pub fn finish(mut self) {
        if let Some(client) = self.client.take() {
            client.disconnect().unwrap();
        }
        self.local.shutdown().unwrap();
        self.remote.shutdown().unwrap();
    }
}

impl Drop for RemoteFixture {
    fn drop(&mut self) {
        drop(self.client.take());
        if let Err(error) = self.local.shutdown() {
            eprintln!("local remote-test core cleanup failed: {error}");
        }
        if let Err(error) = self.remote.shutdown() {
            eprintln!("remote test core cleanup failed: {error}");
        }
    }
}

pub fn execute(client: &Client, owner: Option<&str>, argv: &[&str]) -> Vec<u8> {
    let result = client
        .request("machine.exec", json!({"on": owner, "argv": argv}))
        .unwrap();
    let stderr = STANDARD.decode(result["stderr"].as_str().unwrap()).unwrap();
    assert_eq!(result["code"], 0, "{}", String::from_utf8_lossy(&stderr));
    assert!(result["signal"].is_null());
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["truncated"], false);
    STANDARD.decode(result["stdout"].as_str().unwrap()).unwrap()
}
