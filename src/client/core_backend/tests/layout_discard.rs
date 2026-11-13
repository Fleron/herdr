use std::path::PathBuf;
use std::time::Duration;

use herdr_core_tui::model::{Document, Operation, Tab, Workspace};
use serde_json::json;

use super::{
    backend, core_support::wait_until, core_support::CoreSession, machines::add_machine, start,
};

struct UnwritableDocument {
    path: PathBuf,
    backup: PathBuf,
}

impl UnwritableDocument {
    fn new(session: &CoreSession) -> Self {
        let path = session
            .root
            .join("state/herdr-core")
            .join(&session.session)
            .join("docs.json");
        let backup = path.with_extension("saved");
        std::fs::rename(&path, &backup).unwrap();
        std::fs::create_dir(&path).unwrap();
        Self { path, backup }
    }
}

impl Drop for UnwritableDocument {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
        let _ = std::fs::rename(&self.backup, &self.path);
    }
}

#[test]
fn disconnected_discard_is_local_ignores_late_updates_and_never_replays_old_edits() {
    let local = CoreSession::new();
    let remote = CoreSession::new();
    let profile = add_machine(&local, &remote, &local.root.join("unused-connection-gate"));
    let owner = profile["id"].as_str().unwrap();
    let local_client = local.client();
    let remote_client = remote.client();
    let local_program = start(&local_client, local.root.to_str().unwrap(), "exec sleep 60");
    let remote_program = start(
        &remote_client,
        remote.root.to_str().unwrap(),
        "exec sleep 60",
    );
    let mut local_workspace = Workspace::new("local", local.root.to_str().unwrap(), None);
    local_workspace.tabs.push(Tab::new(
        "local",
        local_program["address"].as_str().unwrap(),
    ));
    let local_id = local_workspace.id.clone();
    let mut remote_workspace =
        Workspace::new("remote", remote.root.to_str().unwrap(), Some(owner.into()));
    remote_workspace.tabs.push(Tab::new(
        "remote",
        remote_program["address"].as_str().unwrap(),
    ));
    let remote_id = remote_workspace.id.clone();
    let mut backend = backend(&local);
    backend
        .apply_operation(None, Operation::AddWorkspace(local_workspace))
        .unwrap();
    backend
        .apply_operation(
            Some(owner.into()),
            Operation::AddWorkspace(remote_workspace),
        )
        .unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        !backend.pending()
    });
    let before_local = local_client
        .request("doc.get", json!({"name":"herdr-tui"}))
        .unwrap();
    let before_remote = remote_client
        .request("doc.get", json!({"name":"herdr-tui"}))
        .unwrap();
    let unwritable = UnwritableDocument::new(&local);
    backend
        .apply_operation(
            None,
            Operation::RenameWorkspace {
                workspace: local_id.clone(),
                name: "discarded-first".into(),
            },
        )
        .unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.stores[&None]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("cannot save document"))
    });
    backend
        .apply_operation(
            None,
            Operation::RenameWorkspace {
                workspace: local_id.clone(),
                name: "discarded-second".into(),
            },
        )
        .unwrap();
    let late = loop {
        let update = backend
            .layout_updates
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        if update.owner.is_none() {
            break update;
        }
        backend
            .stores
            .get_mut(&update.owner)
            .unwrap()
            .update(update);
    };
    backend.client().disconnect().unwrap();
    backend.disconnected();
    assert!(backend.pending());
    backend.discard_pending().unwrap();
    assert!(
        !backend.pending(),
        "discard must not need a core reply or reconnection"
    );
    assert!(!backend.stores[&None].loaded);
    assert!(
        backend.document().workspace(&local_id).is_none(),
        "an unknown saved baseline cannot be projected as certain"
    );
    assert!(backend.document().workspace(&remote_id).is_some());
    backend.stores.get_mut(&None).unwrap().update(late);
    assert!(
        !backend.pending(),
        "a late pre-discard update cannot resurrect an edit"
    );
    assert!(!backend.stores[&None].loaded);
    assert!(backend.stores[&None].document.workspaces.is_empty());
    assert_eq!(
        local_client
            .request("doc.get", json!({"name":"herdr-tui"}))
            .unwrap(),
        before_local
    );
    assert_eq!(
        remote_client
            .request("doc.get", json!({"name":"herdr-tui"}))
            .unwrap(),
        before_remote
    );
    assert_eq!(
        local_client
            .request("terminal.get", json!({"terminal":local_program["address"]}))
            .unwrap()["pid"],
        local_program["pid"]
    );
    assert_eq!(
        remote_client
            .request(
                "terminal.get",
                json!({"terminal":remote_program["address"]})
            )
            .unwrap()["pid"],
        remote_program["pid"]
    );
    drop(unwritable);

    let client = local.client();
    let subscription = client.subscribe().unwrap();
    backend.reconnect(client, subscription).unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.layouts_ready() && !backend.pending()
    });
    assert_eq!(
        backend.document().workspace(&local_id).unwrap().name,
        "local"
    );
    assert_eq!(
        local_client
            .request("doc.get", json!({"name":"herdr-tui"}))
            .unwrap(),
        before_local
    );
    backend
        .apply_operation(
            None,
            Operation::RenameWorkspace {
                workspace: local_id.clone(),
                name: "after-discard".into(),
            },
        )
        .unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        !backend.pending()
    });
    let saved = local_client
        .request("doc.get", json!({"name":"herdr-tui"}))
        .unwrap();
    let saved = Document::from_value(saved["value"].clone()).unwrap();
    assert_eq!(saved.workspace(&local_id).unwrap().name, "after-discard");
    assert_eq!(
        remote_client
            .request("doc.get", json!({"name":"herdr-tui"}))
            .unwrap(),
        before_remote
    );
    drop(backend);
    local.finish();
    remote.finish();
}
