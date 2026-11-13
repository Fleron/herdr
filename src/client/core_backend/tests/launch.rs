use super::{backend, core_support::wait_until, core_support::CoreSession, create};
use serde_json::json;

#[test]
fn current_workspace_focus_wins_over_inactive_owner_memory_after_reopen() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let first = create(&mut backend, session.root.to_str().unwrap(), "first");
    let second = create(&mut backend, session.root.to_str().unwrap(), "second");
    wait_until(|| {
        backend.poll().unwrap();
        !backend.pending()
    });
    let mut view = backend.view().clone();
    view.owner_workspaces
        .insert(backend.local_id().to_owned(), first.workspace);
    drop(backend);
    let (wake, _) = std::sync::mpsc::sync_channel(1);
    let mut backend = super::super::Backend::from_client(session.client(), view, wake).unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.layouts_ready()
    });
    assert_eq!(backend.view().workspace, Some(second.workspace.clone()));
    assert_eq!(
        backend.active_workspace_for(None).unwrap().id,
        second.workspace
    );
    let snapshot = backend.snapshot(None);
    assert_eq!(snapshot.focused_workspace_id, backend.view().workspace);
    assert_eq!(snapshot.focused_tab_id, Some(second.tab));
    assert_eq!(snapshot.focused_pane_id, Some(second.pane));
    drop(backend);
    session.finish();
}

#[test]
fn stopping_a_kept_default_shell_finishes_the_shared_program_without_recovery() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let token = create(&mut backend, session.root.to_str().unwrap(), "kept shell");
    let address = backend.saved_address(&token.pane).unwrap();
    let client = session.client();
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["keep"],
        true
    );
    let target = crate::client::ui_model::RuntimeTerminalTarget {
        machine_id: backend.local_id().to_owned(),
        terminal: address.clone(),
    };
    backend.stop_runtime(&target).unwrap();
    wait_until(|| {
        let change = backend.poll().unwrap();
        assert!(change.notices.is_empty(), "{:?}", change.notices);
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["state"]
            == "finished"
    });
    drop(backend);
    session.finish();
}

#[test]
fn discarding_pending_placement_leaves_its_started_program_unviewed_and_alive() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let token = backend
        .begin_launch(
            super::super::Placement::Workspace {
                owner: None,
                name: "discard placement".into(),
                custom_name: false,
            },
            session.root.to_str().unwrap().into(),
            Default::default(),
            true,
            None,
        )
        .unwrap();
    backend.discard_pending_launches();
    wait_until(|| {
        let change = backend.poll().unwrap();
        assert!(change.launches.is_empty());
        backend.inventory().live().any(|record| {
            record
                .pointer("/labels/herdr.tui.launch")
                .and_then(serde_json::Value::as_str)
                == Some(token.id.as_str())
        })
    });
    assert!(backend.document().workspaces.is_empty());
    assert!(backend.pending_launch().is_none());
    assert!(backend.saved_address(&token.pane).is_none());
    drop(backend);
    session.finish();
}

#[test]
fn recovered_programs_do_not_authorize_typing_held_for_an_unconfirmed_launch() {
    for disconnected in [false, true] {
        let session = CoreSession::new();
        let client = session.client();
        let mut backend = backend(&session);
        let token = backend
            .begin_launch(
                super::super::Placement::Workspace {
                    owner: None,
                    name: "pending recovery".into(),
                    custom_name: false,
                },
                session.root.to_str().unwrap().into(),
                Default::default(),
                true,
                None,
            )
            .unwrap();
        if disconnected {
            backend.disconnected();
        }
        let mut original = None;
        wait_until(|| {
            let listing = client
                .request("terminal.list", json!({"all":true}))
                .unwrap();
            original = listing["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|record| {
                    record
                        .pointer("/labels/herdr.tui.launch")
                        .and_then(serde_json::Value::as_str)
                        == Some(&token.id)
                        && record["pid"].is_u64()
                })
                .cloned();
            original.is_some()
        });
        let original = original.unwrap();
        let address = original["address"].as_str().unwrap();
        let pid = original["pid"].as_u64().unwrap();
        assert!(std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap()
            .success());
        let mut replacement_pid = 0;
        wait_until(|| {
            let record = client
                .request("terminal.get", json!({"terminal": address}))
                .unwrap();
            replacement_pid = record["pid"].as_u64().unwrap_or_default();
            replacement_pid != 0 && replacement_pid != pid
        });
        if disconnected {
            let client = session.client();
            let subscription = client.subscribe().unwrap();
            backend.reconnect(client, subscription).unwrap();
        }
        let mut placed = None;
        wait_until(|| {
            for outcome in backend.poll().unwrap().launches {
                if let super::super::LaunchOutcome::Placed {
                    token: completed,
                    input_may_retarget,
                } = outcome
                {
                    if completed.id == token.id {
                        placed = Some(input_may_retarget);
                    }
                }
            }
            placed.is_some()
        });
        assert_eq!(placed, Some(true), "disconnect: {disconnected}");
        assert_eq!(
            backend.inventory().get(address).unwrap()["pid"],
            replacement_pid
        );
        assert_eq!(backend.saved_address(&token.pane).as_deref(), Some(address));
        drop(backend);
        session.finish();
    }
}
