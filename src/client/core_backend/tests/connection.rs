use super::{backend, core_support::wait_until, core_support::CoreSession, create};

#[test]
fn a_lost_event_stream_disconnects_the_backend_and_preserves_saved_views() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let token = create(&mut backend, session.root.to_str().unwrap(), "disconnect");
    let address = backend.saved_address(&token.pane).unwrap();
    session.successful(&["server", "stop"]);
    let mut notices = Vec::new();
    let mut transitioned = false;
    wait_until(|| {
        let change = backend
            .poll()
            .expect("transport loss is a connection state");
        transitioned |= change.connection;
        notices.extend(change.notices);
        !backend.connected()
    });
    assert!(transitioned);
    assert!(notices
        .iter()
        .any(|notice| notice.contains("Core connection lost")));
    assert_eq!(backend.saved_address(&token.pane), Some(address));
    let change = backend.poll().unwrap();
    assert!(
        !change.connection,
        "connection transitions are reported once"
    );
    drop(backend);
    session.finish();
}
