#![cfg(unix)]

mod core_support;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use core_support::{wait_until, CoreSession};
use herdr_core_tui::connection::{AttachOptions, Attachment, Client, StreamEvent, Subscription};
use serde_json::{json, Value};

fn run(client: &Client, script: &str) -> String {
    let result = client
        .request(
            "terminal.run",
            json!({"argv": ["/bin/sh", "-c", script], "no_parent": true, "size": {"cols": 80, "rows": 24}}),
        )
        .unwrap();
    result["address"].as_str().unwrap().to_owned()
}

fn output_until(attachment: &Attachment, expected: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while !output
        .windows(expected.len())
        .any(|window| window == expected)
    {
        assert!(
            Instant::now() < deadline,
            "terminal did not produce {expected:?}"
        );
        match attachment.recv_timeout(Duration::from_millis(100)) {
            Ok(StreamEvent::Data(bytes)) => {
                output.extend_from_slice(&bytes);
                attachment.credit(bytes.len()).unwrap();
            }
            Ok(StreamEvent::Resize { .. }) => {}
            Ok(event) => panic!("terminal stream ended before expected output: {event:?}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("terminal stream disconnected: {error}"),
        }
    }
    output
}

fn event_until(subscription: &mut Subscription, expected: impl Fn(&Value) -> bool) -> Value {
    let mut found = None;
    wait_until(|| {
        while let Some(event) = subscription.try_event().unwrap() {
            if expected(&event) {
                found = Some(event);
                return true;
            }
        }
        false
    });
    found.unwrap()
}

#[test]
fn real_core_streams_input_events_credit_and_detach() {
    let session = CoreSession::new();
    let client = session.client();
    let (wake, wakes) = mpsc::sync_channel(1);
    client.set_waker(wake);
    wakes.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut events = client.subscribe().unwrap();
    assert!(events.snapshot["seq"].is_u64());
    assert!(events.snapshot["terminals"].is_array());
    let address = run(
        &client,
        "stty -echo; printf 'INITIAL_READY\\n'; while IFS= read -r line; do case \"$line\" in flood) i=0; while [ \"$i\" -lt 6000 ]; do printf 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\\n'; i=$((i+1)); done; printf 'FLOOD_FINISHED\\n';; size) stty size;; *) printf 'REPLY:%s\\n' \"$line\";; esac; done",
    );
    let created = event_until(&mut events, |event| {
        event["method"] == "event" && event["params"]["type"] == "created"
    });
    assert_eq!(created["params"]["terminal"], address);
    let attachment = client
        .attach(&AttachOptions::new(&address, 80, 24))
        .unwrap();
    assert_eq!(
        attachment.automatic_input,
        client.supports("automatic_input")
    );
    output_until(&attachment, b"INITIAL_READY");
    attachment.send_data(b"hello\n").unwrap();
    output_until(&attachment, b"REPLY:hello");
    attachment.resize(120, 40).unwrap();
    attachment.focus(true).unwrap();
    attachment.send_data(b"size\n").unwrap();
    output_until(&attachment, b"40 120");
    attachment.send_data(b"flood\n").unwrap();
    let output = output_until(&attachment, b"FLOOD_FINISHED");
    assert!(
        output.len() > 256 * 1024,
        "output must cross the core's initial stream-credit window"
    );
    attachment.close().unwrap();
    drop(attachment);
    let terminal = client
        .request("terminal.get", json!({"terminal": address}))
        .unwrap();
    assert_eq!(terminal["state"], "live");
    let reopened = client
        .attach(&AttachOptions::new(&address, 80, 24))
        .unwrap();
    reopened.send_data(b"after-detach\n").unwrap();
    output_until(&reopened, b"REPLY:after-detach");
    drop(reopened);
    drop(events);
    drop(client);
    session.finish();
}

#[test]
fn real_core_rpc_errors_and_connection_loss_are_distinct_from_program_exit() {
    let session = CoreSession::new();
    let client = session.client();
    let error = client
        .request("terminal.get", json!({"terminal": "t999999"}))
        .unwrap_err();
    assert_eq!(error.kind(), Some("not_found"));
    assert!(!error.is_transport());
    let address = run(&client, "printf 'STILL_RUNNING\\n'; exec /bin/cat");
    let attachment = client
        .attach(&AttachOptions::new(&address, 80, 24))
        .unwrap();
    output_until(&attachment, b"STILL_RUNNING");
    let marker = session.root.join("exec-started");
    let pending = client.clone();
    let marker_argument = marker.to_string_lossy().into_owned();
    let (done, result) = mpsc::channel();
    let request = std::thread::spawn(move || {
        let reply = pending.request(
            "machine.exec",
            json!({"argv": ["/bin/sh", "-c", "printf started > \"$1\"; sleep 30", "sh", marker_argument]}),
        );
        done.send(reply).unwrap();
    });
    wait_until(|| marker.is_file());
    client.disconnect().unwrap();
    assert!(result
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err()
        .is_transport());
    request.join().unwrap();
    loop {
        match attachment.recv_timeout(Duration::from_secs(2)).unwrap() {
            StreamEvent::Lost => break,
            StreamEvent::Data(_) => {}
            event => panic!("connection loss must not report program exit: {event:?}"),
        }
    }
    let fresh = session.client();
    let terminal = fresh
        .request("terminal.get", json!({"terminal": address}))
        .unwrap();
    assert_eq!(terminal["state"], "live");
    drop(fresh);
    drop(attachment);
    drop(client);
    session.finish();
}

#[test]
fn real_core_restart_preserves_terminal_identity_for_a_new_attachment() {
    let session = CoreSession::new();
    let client = session.client();
    let address = run(
        &client,
        "stty -echo; printf 'BEFORE_RESTART\\n'; exec /bin/cat",
    );
    let attachment = client
        .attach(&AttachOptions::new(&address, 80, 24))
        .unwrap();
    output_until(&attachment, b"BEFORE_RESTART");
    session.successful(&["server", "restart"]);
    loop {
        match attachment.recv_timeout(Duration::from_secs(5)).unwrap() {
            StreamEvent::Lost => break,
            StreamEvent::Data(_) => {}
            event => panic!("core restart must lose only the connection: {event:?}"),
        }
    }
    let fresh = session.client();
    let terminal = fresh
        .request("terminal.get", json!({"terminal": address}))
        .unwrap();
    assert_eq!(terminal["state"], "live");
    let mut options = AttachOptions::new(&address, 80, 24);
    options.resumed = true;
    let reopened = fresh.attach(&options).unwrap();
    output_until(&reopened, b"BEFORE_RESTART");
    reopened.send_data(b"AFTER_RESTART\n").unwrap();
    output_until(&reopened, b"AFTER_RESTART");
    drop(reopened);
    drop(fresh);
    drop(attachment);
    drop(client);
    session.finish();
}
