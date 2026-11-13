#![cfg(unix)]

mod core_support;
#[path = "core_support/remote.rs"]
mod remote_fixture;
#[path = "core_support/pty.rs"]
mod tui_pty;

use remote_fixture::{execute, RemoteFixture};

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use core_support::wait_until;
use herdr_core_tui::connection::{AttachOptions, Attachment, Client, StreamEvent};
use herdr_core_tui::inventory::Inventory;
use herdr_core_tui::model::{local_terminal, Operation, Tab, Workspace};
use herdr_core_tui::persistence::LayoutStore;
use herdr_core_tui::worktrees::{Error as WorktreeError, Worktrees};
use serde_json::{json, Value};
use tui_pty::{layout, terminal, wait_layout, wait_terminal_text, TuiPty};

fn desktop(client: &Client, owner: Option<&str>) -> Value {
    client
        .request("doc.get", json!({"name": "herdr-app", "on": owner}))
        .unwrap()
}

fn seed_desktop(client: &Client, owner: Option<&str>, value: &str) -> Value {
    client
        .request(
            "doc.set",
            json!({"name": "herdr-app", "on": owner, "version": 0,
        "value": {"sentinel": value}}),
        )
        .unwrap();
    desktop(client, owner)
}

fn read_until(attachment: &Attachment, marker: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bytes = Vec::new();
    while !bytes.windows(marker.len()).any(|window| window == marker) {
        assert!(
            Instant::now() < deadline,
            "remote did not produce {marker:?}"
        );
        match attachment.recv_timeout(Duration::from_millis(100)) {
            Ok(StreamEvent::Data(chunk)) => {
                bytes.extend_from_slice(&chunk);
                attachment.credit(chunk.len()).unwrap();
            }
            Ok(StreamEvent::Resize { .. }) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            event => panic!("remote attachment ended: {event:?}"),
        }
    }
}

#[test]
fn custom_transport_remote_ui_saves_on_owner_and_reopens_the_same_program() {
    let fixture = RemoteFixture::new();
    let client = fixture.client();
    let machines = client.request("machine.list", json!({})).unwrap();
    let owner = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["name"] == "box")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let local_desktop = seed_desktop(client, None, "local-desktop");
    let remote_desktop = seed_desktop(client, Some("box"), "remote-desktop");
    let folder = fixture.remote.root.join("remote workspace");
    std::fs::create_dir(&folder).unwrap();
    let folder = std::fs::canonicalize(folder).unwrap();
    let binary = Path::new(env!("CARGO_BIN_EXE_herdr"));
    let mut tui = TuiPty::spawn(&fixture.local, binary);
    tui.wait_text("machines");
    tui.wait_screen(|screen| {
        screen
            .lines()
            .any(|line| line.contains("box") && line.contains('●'))
    });
    tui.click_text("box");
    tui.wait_screen(|screen| {
        screen
            .lines()
            .any(|line| line.contains("new") && line.contains("box"))
    });
    tui.create_workspace(folder.to_str().unwrap());
    let saved = wait_layout(client, Some(owner), |document| {
        document.workspaces.len() == 1
    });
    let local_address = saved.workspaces[0].tabs[0].panes[0].terminal.clone();
    assert!(!local_address.contains('/'));
    let address = format!("{owner}/{local_address}");
    tui.print_marker("REMOTE_UI_EXECUTED");
    wait_terminal_text(client, &address, "REMOTE_UI_EXECUTED");
    tui.wait_text("REMOTE_UI_EXECUTED");
    assert_eq!(terminal(client, &address)["cwd"], folder.to_str().unwrap());
    assert!(terminal(client, &address)
        .get("parent")
        .is_none_or(Value::is_null));
    let pid = terminal(client, &address)["pid"].as_u64().unwrap();
    assert!(
        layout(client, None).workspaces.is_empty(),
        "remote arrangement must not be written to the local document"
    );
    assert_eq!(desktop(client, None), local_desktop);
    assert_eq!(desktop(client, Some("box")), remote_desktop);
    client
        .request(
            "machine.rename",
            json!({"machine": "box", "new_name": "renamed"}),
        )
        .unwrap();
    tui.prefix(b"W");
    tui.wait_text("rename workspace");
    tui.replace_text("Renamed remote");
    let renamed = wait_layout(client, Some(owner), |document| {
        document.workspaces.len() == 1 && document.workspaces[0].name == "Renamed remote"
    });
    assert_eq!(renamed.workspaces[0].id, saved.workspaces[0].id);
    assert_eq!(renamed.workspaces[0].tabs, saved.workspaces[0].tabs);
    assert!(layout(client, None).workspaces.is_empty());
    assert_eq!(desktop(client, Some("renamed")), remote_desktop);
    tui.detach();
    assert_eq!(terminal(client, &address)["state"], "live");
    assert_eq!(terminal(client, &address)["pid"].as_u64(), Some(pid));

    let mut reopened = TuiPty::spawn(&fixture.local, binary);
    reopened.wait_text("Renamed remote");
    reopened.print_marker("REMOTE_UI_REOPENED");
    wait_terminal_text(client, &address, "REMOTE_UI_REOPENED");
    reopened.wait_text("REMOTE_UI_REOPENED");
    assert_eq!(layout(client, Some(owner)), renamed);
    assert_eq!(terminal(client, &address)["pid"].as_u64(), Some(pid));
    reopened.detach();
    assert_eq!(desktop(client, None), local_desktop);
    assert_eq!(desktop(client, Some("renamed")), remote_desktop);
    fixture.finish();
}

#[test]
fn two_real_cores_forward_streams_documents_and_safe_worktrees_across_remote_restart() {
    let fixture = RemoteFixture::new();
    let client = fixture.client();
    let machines = client.request("machine.list", json!({})).unwrap();
    let profiles = machines["machines"].as_array().unwrap();
    let owner = profiles
        .iter()
        .find(|machine| machine["name"] == "box")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let local_desktop = seed_desktop(client, None, "local-desktop");
    let remote_desktop = seed_desktop(client, Some("box"), "remote-desktop");
    assert_eq!(
        execute(
            client,
            Some("box"),
            &["/bin/sh", "-c", "printf %s \"$HOME\""]
        ),
        fixture.remote.root.to_str().unwrap().as_bytes()
    );
    let local_address = client
        .request(
            "terminal.run",
            json!({"argv": ["/bin/cat"], "name": "local-sentinel", "no_parent": true}),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let local_pid = terminal(client, &local_address)["pid"].as_u64().unwrap();
    let repository = fixture.remote.repository("remote repository");
    let repository = repository.to_str().unwrap();
    let trees = Worktrees::new(client, Some(owner));
    let tree = trees.create(repository, "remote-feature").unwrap();
    assert!(Path::new(&tree.path).starts_with(std::fs::canonicalize(&fixture.remote.root).unwrap()));
    assert_eq!(trees.open(repository, &tree.path).unwrap(), tree);
    let mut events = client.subscribe().unwrap();
    let mut inventory = Inventory::new(&events.snapshot).unwrap();
    assert!(!inventory.set_machines(profiles).unwrap().resubscribe);
    let result = client.request("terminal.run", json!({
        "argv": ["/bin/sh", "-c", "stty -echo; printf 'REMOTE_STREAM_READY\\n'; exec /bin/cat"],
        "on": owner, "cwd": tree.path, "no_parent": true,
    })).unwrap();
    let reported = result["address"].as_str().unwrap();
    assert!(reported.starts_with("box/"));
    let address = format!("{owner}/{}", reported.strip_prefix("box/").unwrap());
    assert_eq!(
        local_terminal(Some(owner), &address).unwrap(),
        local_address
    );
    wait_until(|| {
        while let Some(notification) = events.try_event().unwrap() {
            let change = inventory.apply(&notification).unwrap();
            assert!(!change.resubscribe);
        }
        inventory.get(&address).is_some()
    });
    let original_pid = terminal(client, &address)["pid"].as_u64().unwrap();
    let attachment = client
        .attach(&AttachOptions::new(&address, 80, 24))
        .unwrap();
    read_until(&attachment, b"REMOTE_STREAM_READY");
    attachment.send_data(b"REMOTE_INPUT_OBSERVED\n").unwrap();
    read_until(&attachment, b"REMOTE_INPUT_OBSERVED");
    wait_terminal_text(client, &address, "REMOTE_INPUT_OBSERVED");
    assert!(!tui_pty::terminal_text(client, &local_address).contains("REMOTE_INPUT_OBSERVED"));
    drop(attachment);

    let mut store = LayoutStore::load(client, Some(owner.into())).unwrap();
    let mut workspace = Workspace::new("remote", tree.path.clone(), Some(owner.into()));
    let workspace_id = workspace.id.clone();
    workspace.tabs.push(Tab::new(
        "shell",
        local_terminal(Some(owner), &address).unwrap(),
    ));
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(client).unwrap();
    let saved = layout(client, Some(owner));
    assert_eq!(
        saved.workspaces[0].tabs[0].panes[0].terminal,
        local_terminal(Some(owner), &address).unwrap()
    );
    assert!(layout(client, None).workspaces.is_empty());
    fixture.remote.successful(&["server", "restart"]);
    wait_until(
        || match client.request("terminal.get", json!({"terminal": address})) {
            Ok(record) => record["state"] == "live" && record["pid"].as_u64() == Some(original_pid),
            Err(error) if error.kind() == Some("unavailable") || error.is_transport() => false,
            Err(error) => panic!("remote restart failed: {error}"),
        },
    );
    store.refresh(client).unwrap();
    assert_eq!(layout(client, Some(owner)), saved);
    let mut options = AttachOptions::new(&address, 90, 30);
    options.resumed = true;
    let reopened = client.attach(&options).unwrap();
    reopened.send_data(b"AFTER_REMOTE_RESTART\n").unwrap();
    read_until(&reopened, b"AFTER_REMOTE_RESTART");
    drop(reopened);
    store
        .apply(Operation::CloseWorkspace {
            workspace: workspace_id,
        })
        .unwrap();
    store.save(client).unwrap();
    let records: Vec<_> = inventory.records().cloned().collect();
    assert!(matches!(
        trees.remove(repository, &tree.path, &records, &[]),
        Err(WorktreeError::InUse(_))
    ));
    client
        .request("terminal.kill", json!({"terminal": address}))
        .unwrap();
    wait_until(|| terminal(client, &address)["state"] == "finished");
    let dirty = format!("{}/untracked.txt", tree.path);
    execute(
        client,
        Some("box"),
        &["/bin/sh", "-c", "printf dirty > \"$1\"", "sh", &dirty],
    );
    assert!(matches!(
        trees.remove(repository, &tree.path, &[], &[]),
        Err(WorktreeError::Dirty(_))
    ));
    execute(client, Some("box"), &["rm", "--", &dirty]);
    trees.remove(repository, &tree.path, &[], &[]).unwrap();
    assert!(!trees
        .list(repository)
        .unwrap()
        .iter()
        .any(|entry| entry.path == tree.path));
    execute(
        client,
        Some("box"),
        &[
            "git",
            "-C",
            repository,
            "show-ref",
            "--verify",
            "refs/heads/remote-feature",
        ],
    );
    assert_eq!(desktop(client, None), local_desktop);
    assert_eq!(desktop(client, Some("box")), remote_desktop);
    assert_eq!(terminal(client, &local_address)["state"], "live");
    assert_eq!(
        terminal(client, &local_address)["pid"].as_u64(),
        Some(local_pid)
    );
    drop(events);
    fixture.finish();
}
