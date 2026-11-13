#![cfg(unix)]

mod core_support;

use std::collections::HashSet;
use std::sync::mpsc;

use core_support::{wait_until, CoreSession};
use herdr_core_tui::connection::{Client, Display};
use herdr_core_tui::surfaces::{Input, PaneSpec, Streams};
use serde_json::json;

fn start(client: &Client, script: &str) -> String {
    client
        .request(
            "terminal.run",
            json!({
                "argv": ["/bin/sh", "-c", script], "no_parent": true,
                "size": {"cols": 80, "rows": 24}
            }),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .into()
}

fn manager(client: &Client) -> Streams {
    let (wake, _) = mpsc::sync_channel(1);
    Streams::new(client.clone(), Display::default(), None, 1024 * 1024, wake)
}

fn spec(id: &str, address: &str) -> PaneSpec {
    PaneSpec {
        id: id.into(),
        address: address.into(),
        cols: 80,
        rows: 24,
        focused: true,
        live: true,
        completion: None,
    }
}

fn retained(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| (*id).into()).collect()
}

fn contains(streams: &mut Streams, id: &str, text: &str) -> bool {
    let Some(pane) = streams.pane_mut(id) else {
        return false;
    };
    pane.view.prepare().unwrap();
    pane.view
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .contains(text)
}

#[test]
fn queued_input_waits_for_initial_modes_and_detach_preserves_the_program() {
    let session = CoreSession::new();
    let client = session.client();
    let address = start(&client, "stty -echo; printf 'READY\n'; while IFS= read -r line; do printf 'GOT:%s\n' \"$line\"; done");
    let mut streams = manager(&client);
    let mut pane = spec("view", &address);
    streams
        .reconcile(&[pane.clone()], &retained(&["view"]))
        .unwrap();
    assert!(!streams.pane("view").unwrap().input_ready());
    streams
        .input("view", Input::Bytes(b"queued-before-paint\n".to_vec()))
        .unwrap();
    assert_eq!(streams.pane("view").unwrap().pending_input(), 1);
    wait_until(|| {
        streams.poll().unwrap();
        contains(&mut streams, "view", "GOT:queued-before-paint")
    });
    assert_eq!(streams.pane("view").unwrap().pending_input(), 0);
    pane.cols = 100;
    pane.rows = 30;
    streams.reconcile(&[pane], &retained(&["view"])).unwrap();
    wait_until(|| {
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["cols"]
            == 100
    });
    drop(streams);
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["state"],
        "live"
    );
    session.finish();
}

#[test]
fn duplicate_views_keep_an_observer_through_focus_and_hidden_view_changes() {
    let session = CoreSession::new();
    let client = session.client();
    let address = start(&client, "stty -echo; printf '\\033[?2048hOBS_READY\\n'; while IFS= read -r line; do printf 'GOT:%s\\n' \"$line\"; done");
    let mut streams = manager(&client);
    let first = spec("first", &address);
    let mut second = spec("second", &address);
    second.focused = false;
    second.cols = 60;
    streams
        .reconcile(
            &[first.clone(), second.clone()],
            &retained(&["first", "second"]),
        )
        .unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        contains(&mut streams, "first", "OBS_READY")
            && contains(&mut streams, "second", "OBS_READY")
    });
    assert!(streams.pane("first").unwrap().input_ready());
    assert!(!streams.pane("second").unwrap().input_ready());
    second.focused = true;
    streams
        .reconcile(&[second], &retained(&["first", "second"]))
        .unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        streams.pane("second").unwrap().input_ready()
    });
    assert!(!streams.pane("first").unwrap().input_ready());
    streams
        .input("second", Input::Bytes(b"focused-second\n".to_vec()))
        .unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        contains(&mut streams, "second", "GOT:focused-second")
    });
    assert!(streams.pane("first").unwrap().error.is_none());
    assert!(streams.pane("second").unwrap().error.is_none());
    session.finish();
}

#[test]
fn normal_exit_finishes_the_view_without_reporting_a_connection_error() {
    let session = CoreSession::new();
    let client = session.client();
    let address = start(&client, "printf 'EXIT_READY\n'; IFS= read -r line; exit 7");
    let mut streams = manager(&client);
    streams
        .reconcile(&[spec("view", &address)], &retained(&["view"]))
        .unwrap();
    streams
        .input("view", Input::Bytes(b"finish\n".to_vec()))
        .unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        streams.pane("view").unwrap().completion.is_some()
    });
    assert!(streams.pane("view").unwrap().error.is_none());
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["state"],
        "finished"
    );
    assert!(client.request("machine.list", json!({})).is_ok());
    session.finish();
}

#[test]
fn a_retargeted_view_holds_old_input_until_it_is_explicitly_discarded() {
    let session = CoreSession::new();
    let client = session.client();
    let original = start(&client, "stty -echo; exec cat");
    let replacement = start(&client, "stty -echo; exec cat");
    let mut streams = manager(&client);
    let retained = retained(&["view"]);
    streams
        .reconcile(&[spec("view", &original)], &retained)
        .unwrap();
    streams
        .input("view", Input::Bytes(b"HELD_FOR_ORIGINAL\n".to_vec()))
        .unwrap();
    streams
        .reconcile(&[spec("view", &replacement)], &retained)
        .unwrap();
    streams.poll().unwrap();
    let pane = streams.pane("view").unwrap();
    assert_eq!(pane.spec.address, original);
    assert_eq!(pane.pending_input(), 1);
    assert!(!pane.input_ready());
    assert!(pane
        .error
        .as_deref()
        .unwrap()
        .contains("Discard held input"));
    streams.discard_all_input();
    streams
        .reconcile(&[spec("view", &replacement)], &retained)
        .unwrap();
    streams
        .input(
            "view",
            Input::Bytes(b"NEW_INPUT_FOR_REPLACEMENT\n".to_vec()),
        )
        .unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        contains(&mut streams, "view", "NEW_INPUT_FOR_REPLACEMENT")
    });
    assert_eq!(streams.pane("view").unwrap().spec.address, replacement);
    assert!(!contains(&mut streams, "view", "HELD_FOR_ORIGINAL"));
    session.finish();
}

#[test]
fn reconnect_does_not_send_old_typing_to_a_kept_shell_replacement() {
    let session = CoreSession::new();
    let client = session.client();
    let record = client
        .request(
            "terminal.run",
            json!({
                "argv":["/bin/sh"], "keep":true, "no_parent":true,
                "cwd":session.root,
            }),
        )
        .unwrap();
    let address = record["address"].as_str().unwrap().to_owned();
    let pid = record["pid"].as_u64().unwrap();
    let mut streams = manager(&client);
    let specs = [spec("view", &address)];
    let retained = retained(&["view"]);
    streams.reconcile(&specs, &retained).unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        streams.pane("view").unwrap().input_ready()
    });
    streams
        .input(
            "view",
            Input::Bytes(b"printf OLD_INPUT_MUST_STAY_HELD\\n\n".to_vec()),
        )
        .unwrap();
    let revision = streams.pane("view").unwrap().revision;
    client.disconnect().unwrap();
    assert!(session
        .command("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let observer = session.client();
    wait_until(|| {
        observer
            .request("terminal.get", json!({"terminal":address}))
            .unwrap()["pid"]
            .as_u64()
            .is_some_and(|current| current != pid)
    });
    assert_eq!(streams.reconnect(observer.clone()), 1);
    streams.reconcile(&specs, &retained).unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        streams.pane("view").unwrap().revision > revision
    });
    assert_eq!(streams.pane("view").unwrap().pending_input(), 1);
    assert!(!streams.pane("view").unwrap().input_ready());
    let text = observer
        .request("terminal.read", json!({"terminal":address}))
        .unwrap();
    assert!(!text["text"]
        .as_str()
        .unwrap()
        .contains("OLD_INPUT_MUST_STAY_HELD"));
    streams.discard_all_input();
    streams
        .input(
            "view",
            Input::Bytes(b"printf NEW_INPUT_AFTER_DISCARD\\n\n".to_vec()),
        )
        .unwrap();
    wait_until(|| {
        streams.poll().unwrap();
        observer
            .request("terminal.read", json!({"terminal":address}))
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("NEW_INPUT_AFTER_DISCARD")
    });
    session.finish();
}
