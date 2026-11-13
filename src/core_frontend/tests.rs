use super::*;
use crate::client::frame::ClientPaneInputEvent;
use crate::client::ui_action::{CoreAction, FrontendCommand, FrontendRequest};
use crate::client::ui_domain as domain;
use crate::core_test_support::{wait_until, CoreSession};
use herdr_core_tui::model::{Operation, Tab, Workspace};
use herdr_core_tui::persistence::LayoutStore;
use serde_json::json;

fn app() -> (App, CoreSession, String) {
    let session = CoreSession::new();
    let client = session.client();
    let address = client
        .request(
            "terminal.run",
            json!({
                "argv":["/bin/sh","-c","stty -echo; printf READY; exec cat"],
                "no_parent":true,
            }),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut workspace = Workspace::new("workspace", session.root.to_str().unwrap(), None);
    workspace.tabs.push(Tab::new("terminal", &address));
    let mut store = LayoutStore::load(&client, None).unwrap();
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(&client).unwrap();
    let (wake, _) = mpsc::sync_channel(1);
    let mut backend =
        Backend::from_client(client.clone(), ViewState::default(), wake.clone()).unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.layouts_ready()
    });
    let config = Config::default();
    let shell = ClientShellState::new(
        ClientShellConfig::from_config(&config).with_startup_onboarding(false),
    );
    let streams = Streams::new(client, Display::default(), None, 1_000_000, wake.clone());
    let mut app = App {
        backend,
        shell,
        streams,
        output: output::Output::new(&config),
        config,
        surface: surface::SurfaceBuilder::default(),
        geometry: surface::Geometry::default(),
        installed_owner: None,
        requests: HashMap::new(),
        held_input: HashMap::new(),
        blocked_held_input: HashSet::new(),
        pending_focus: None,
        retained: HashSet::new(),
        clipboard_images: clipboard_images::Bridge::new(wake.clone()).unwrap(),
        wake,
        reconnect: None,
        reconnect_due: None,
        reconnect_backoff: Duration::from_millis(200),
        ownership_error: None,
        display: Display::default(),
        size: (120, 40),
        running: true,
        dirty: true,
        layout_dirty: true,
        surface_dirty: true,
        hits_dirty: true,
        view_dirty: false,
        command_namespace: "fixture".into(),
        detached_children: Vec::new(),
        notifications: notifications::NotificationTracker::default(),
        prefix_input_source: crate::platform::RealPrefixInputSource::default(),
        mouse_mode: host::MouseMode::new(Default::default()),
        keyboard_enhanced: false,
        keyboard_report_all: false,
    };
    app.sync_shell();
    (app, session, address)
}

#[test]
fn detach_waits_for_the_real_layout_save_acknowledgement() {
    let (mut app, session, _) = app();
    let workspace = app.backend.document().workspaces[0].id.clone();
    app.backend
        .apply_operation(
            None,
            Operation::RenameWorkspace {
                workspace: workspace.clone(),
                name: "saved rename".into(),
            },
        )
        .unwrap();
    assert!(app.backend.pending());
    app.request_detach();
    assert!(
        app.running,
        "pending arrangement edits must survive voluntary detach"
    );
    wait_until(|| {
        app.poll_backend().unwrap();
        !app.backend.pending()
    });
    app.request_detach();
    assert!(!app.running);
    let stored = LayoutStore::load(app.backend.client(), None).unwrap();
    assert_eq!(
        stored.document().workspace(&workspace).unwrap().name,
        "saved rename"
    );
    app.finish_clipboard_images().unwrap();
    drop(app);
    session.finish();
}

#[test]
fn detach_keeps_held_typing_until_explicit_discard_without_stopping_the_program() {
    let (mut app, session, address) = app();
    let pane = app.backend.document().workspaces[0].tabs[0].panes[0]
        .id
        .clone();
    let before = app
        .backend
        .client()
        .request("terminal.get", json!({"terminal":address}))
        .unwrap()["pid"]
        .clone();
    app.held_input
        .entry(pane.clone())
        .or_default()
        .push_back(ClientPaneInputEvent::Paste("UNSENT".into()));
    app.blocked_held_input.insert(pane);
    app.request_detach();
    assert!(app.running);
    assert_eq!(app.held_input.values().map(VecDeque::len).sum::<usize>(), 1);
    app.action(ClientShellAction::Core(CoreAction::DiscardHeldInput))
        .unwrap();
    app.request_detach();
    assert!(!app.running);
    let terminal = app
        .backend
        .client()
        .request("terminal.get", json!({"terminal":address}))
        .unwrap();
    assert_eq!(terminal["state"], "live");
    assert_eq!(terminal["pid"], before);
    app.finish_clipboard_images().unwrap();
    drop(app);
    session.finish();
}

#[test]
fn held_input_replay_keeps_its_captured_target_during_a_later_launch() {
    let (mut app, session, address) = app();
    let pane = app.backend.document().workspaces[0].tabs[0].panes[0]
        .id
        .clone();
    app.reconcile().unwrap();
    wait_until(|| {
        app.streams.poll().unwrap();
        app.streams
            .pane(&pane)
            .is_some_and(|pane| pane.input_ready())
    });
    let action = app
        .backend
        .command(
            None,
            FrontendRequest {
                id: herdr_core_tui::model::new_id(),
                method: FrontendCommand::WorkspaceCreate(domain::WorkspaceCreateParams {
                    source_workspace_id: None,
                    cwd: Some(session.root.to_str().unwrap().into()),
                    focus: true,
                    label: Some("later launch".into()),
                    env: Default::default(),
                }),
            },
        )
        .unwrap();
    let crate::client::core_backend::Action::Launch(token) = action else {
        panic!("launch must reserve a real view")
    };
    assert_ne!(token.pane, pane);
    app.pending_focus = Some(token.pane);
    app.held_input
        .entry(pane.clone())
        .or_default()
        .push_back(ClientPaneInputEvent::Paste("CAPTURED_TARGET\n".into()));
    app.flush_held_input().unwrap();
    assert_eq!(app.streams.pane(&pane).unwrap().pending_input(), 1);
    assert!(app.held_input.is_empty());
    wait_until(|| {
        app.streams.poll().unwrap();
        app.backend
            .client()
            .request("terminal.read", json!({"terminal":address}))
            .unwrap()["text"]
            .as_str()
            .is_some_and(|text| text.contains("CAPTURED_TARGET"))
    });
    let terminal = app
        .backend
        .client()
        .request("terminal.get", json!({"terminal":address}))
        .unwrap();
    assert_eq!(terminal["state"], "live");
    app.finish_clipboard_images().unwrap();
    drop(app);
    session.finish();
}

#[test]
fn reconnect_resupplies_retained_image_bytes_after_the_shell_cache_resets() {
    let (mut app, session, address) = app();
    let pane = app.backend.document().workspaces[0].tabs[0].panes[0]
        .id
        .clone();
    app.display.cell = Some([8, 16]);
    app.streams.set_display(app.display.clone()).unwrap();
    app.shell.set_graphics_cell_size(8, 16);
    app.reconcile().unwrap();
    wait_until(|| {
        app.streams.poll().unwrap();
        app.streams
            .pane(&pane)
            .is_some_and(|pane| pane.input_ready())
    });
    app.streams
        .input(
            &pane,
            TerminalInput::Bytes(
                b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,c=1,r=1,q=2;/wAA/w==\x1b\\\n".to_vec(),
            ),
        )
        .unwrap();
    wait_until(|| {
        app.streams.poll().unwrap();
        app.streams
            .pane(&pane)
            .unwrap()
            .view
            .terminal()
            .kitty_graphics_may_have_placements()
            .unwrap()
    });
    app.refresh_surface().unwrap();
    let first = app.shell.compose(120, 40).unwrap();
    assert!(first.graphics.operations.iter().any(|operation| matches!(
        operation,
        crate::kitty_graphics::GraphicsOperation::Upload { .. }
    )));
    let epoch = app.shell.graphics_delivery_epoch();
    app.backend.disconnected();
    let client = session.client();
    let subscription = client.subscribe().unwrap();
    app.backend.reconnect(client.clone(), subscription).unwrap();
    app.streams.reconnect(client);
    app.sync_shell();
    assert_ne!(app.shell.graphics_delivery_epoch(), epoch);
    app.layout_dirty = true;
    app.refresh_surface().unwrap();
    let restored = app.shell.compose(120, 40).unwrap();
    assert!(restored.graphics.operations.iter().any(|operation| matches!(operation, crate::kitty_graphics::GraphicsOperation::Upload {..})), "the retained terminal still owns these image bytes; the reset shell must receive them again");
    assert_eq!(
        app.backend
            .client()
            .request("terminal.get", json!({"terminal":address}))
            .unwrap()["state"],
        "live"
    );
    app.finish_clipboard_images().unwrap();
    drop(app);
    session.finish();
}
