#[path = "tests/connection.rs"]
mod connection;
use crate::core_test_support as core_support;
#[path = "tests/checkout_removal.rs"]
mod checkout_removal;
#[path = "tests/launch.rs"]
mod launch;
#[path = "tests/layout_discard.rs"]
mod layout_discard;
#[path = "tests/machines.rs"]
mod machines;

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc;

use herdr_core_tui::connection::Client;
use herdr_core_tui::model::ViewState;
use serde_json::{json, Value};

use crate::client::ui_action::{FrontendCommand, FrontendRequest};
use crate::client::ui_domain as schema;

use super::{Action, Backend, LaunchOutcome};
use core_support::{wait_until, CoreSession};

fn backend(session: &CoreSession) -> Backend {
    let (wake, _) = mpsc::sync_channel(1);
    let mut backend = Backend::from_client(session.client(), ViewState::default(), wake).unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.layouts_ready()
    });
    backend
}

fn command(backend: &mut Backend, method: FrontendCommand) -> Action {
    backend
        .command(
            None,
            FrontendRequest {
                id: herdr_core_tui::model::new_id(),
                method,
            },
        )
        .unwrap()
}

fn create(backend: &mut Backend, folder: &str, name: &str) -> super::LaunchToken {
    let action = command(
        backend,
        FrontendCommand::WorkspaceCreate(schema::WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: Some(folder.into()),
            focus: true,
            label: Some(name.into()),
            env: HashMap::new(),
        }),
    );
    let Action::Launch(token) = action else {
        panic!("workspace creation must reserve a launch")
    };
    assert!(backend.saved_address(&token.pane).is_none());
    let mut placed = false;
    wait_until(|| {
        let change = backend.poll().unwrap();
        for outcome in &change.launches {
            if let LaunchOutcome::Failed { error, .. } = outcome {
                panic!("launch failed: {error}");
            }
        }
        placed |= change.launches.iter().any(|outcome| matches!(outcome, LaunchOutcome::Placed { token: completed, .. } if completed.id == token.id));
        placed
    });
    wait_until(|| {
        backend.poll().unwrap();
        !backend.pending()
    });
    token
}

fn start(client: &Client, cwd: &str, script: &str) -> Value {
    client.request("terminal.run", json!({"argv": ["/bin/sh", "-c", script], "cwd": cwd, "no_parent": true, "size": {"cols": 80, "rows": 24}})).unwrap()
}

#[test]
fn original_layout_actions_save_real_core_programs_without_mutating_desktop_layouts() {
    let session = CoreSession::new();
    let client = session.client();
    let desktop = client
        .request("doc.get", json!({"name": "herdr-app"}))
        .unwrap();
    let mut backend = backend(&session);
    let first = create(&mut backend, session.root.to_str().unwrap(), "first");
    let address = backend.saved_address(&first.pane).unwrap();
    let pid = client
        .request("terminal.get", json!({"terminal": address}))
        .unwrap()["pid"]
        .clone();
    let action = command(
        &mut backend,
        FrontendCommand::PaneSplit(schema::PaneSplitParams {
            workspace_id: Some(first.workspace.clone()),
            target_pane_id: Some(first.pane.clone()),
            direction: schema::SplitDirection::Right,
            ratio: Some(0.35),
            cwd: None,
            focus: true,
            right_click: schema::PaneRightClickTarget::Pane,
            env: HashMap::new(),
        }),
    );
    let Action::Launch(second) = action else {
        panic!("split must reserve its new pane")
    };
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&second.pane).is_some() && !backend.pending()
    });
    command(
        &mut backend,
        FrontendCommand::PaneRename(schema::PaneRenameParams {
            pane_id: second.pane.clone(),
            label: Some("agent pane".into()),
        }),
    );
    command(
        &mut backend,
        FrontendCommand::PaneZoom(schema::PaneZoomParams {
            pane_id: Some(second.pane.clone()),
            mode: schema::PaneZoomMode::On,
        }),
    );
    let snapshot = backend.snapshot(None);
    assert_eq!(
        snapshot.focused_pane_id.as_deref(),
        Some(second.pane.as_str())
    );
    assert!(
        snapshot
            .tabs
            .iter()
            .find(|tab| tab.tab_id == first.tab)
            .unwrap()
            .zoomed
    );
    assert!(
        snapshot
            .panes
            .iter()
            .find(|pane| pane.pane_id == second.pane)
            .unwrap()
            .right_click_passthrough
    );
    command(
        &mut backend,
        FrontendCommand::PaneClose(schema::PaneTarget {
            pane_id: second.pane.clone(),
        }),
    );
    wait_until(|| {
        backend.poll().unwrap();
        !backend.pending()
    });
    assert!(backend.saved_address(&second.pane).is_none());
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["pid"],
        pid
    );
    let view = backend.view().clone();
    drop(backend);
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["state"],
        "live"
    );
    let (wake, _) = mpsc::sync_channel(1);
    let mut reopened = Backend::from_client(client.clone(), view, wake).unwrap();
    wait_until(|| {
        reopened.poll().unwrap();
        reopened.layouts_ready()
    });
    assert_eq!(
        reopened.saved_address(&first.pane).as_deref(),
        Some(address.as_str())
    );
    assert_eq!(
        client
            .request("doc.get", json!({"name": "herdr-app"}))
            .unwrap(),
        desktop
    );
    drop(reopened);
    session.finish();
}

#[test]
fn global_core_agents_exist_before_views_and_attachment_reuses_the_same_program() {
    let session = CoreSession::new();
    let client = session.client();
    let program = start(
        &client,
        session.root.to_str().unwrap(),
        r"printf '\033]7501;state=working:app=codex\007'; exec sleep 60",
    );
    let address = program["address"].as_str().unwrap();
    let mut backend = backend(&session);
    wait_until(|| {
        backend.poll().unwrap();
        !backend.snapshot(None).agents.is_empty()
    });
    assert!(backend.document().workspaces.is_empty());
    let agent = backend.snapshot(None).agents.remove(0);
    assert!(agent.view.is_none());
    assert_eq!(agent.agent_status, schema::AgentStatus::Working);
    let target = agent.runtime_target.unwrap();
    let Action::Launch(token) = backend.attach_runtime(&target).unwrap() else {
        panic!("an unplaced terminal needs a saved view")
    };
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&token.pane).is_some()
    });
    assert_eq!(backend.saved_address(&token.pane).as_deref(), Some(address));
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["pid"],
        program["pid"]
    );
    assert!(backend.snapshot(None).agents[0].view.is_some());
    backend.stop_runtime(&target).unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["state"]
            == "finished"
    });
    drop(backend);
    session.finish();
}

#[test]
fn owner_inventory_verifies_the_core_and_preserves_foreground_working_directories() {
    let session = CoreSession::new();
    let client = session.client();
    let folder = session.root.join("foreground checkout");
    std::fs::create_dir(&folder).unwrap();
    let folder = std::fs::canonicalize(folder).unwrap();
    let script = format!(
        "cd '{}'; exec sleep 60",
        folder.to_str().unwrap().replace('\'', "'\\''")
    );
    let program = start(&client, session.root.to_str().unwrap(), &script);
    let address = program["address"].as_str().unwrap();
    wait_until(|| {
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()
            .pointer("/foreground/cwd")
            .and_then(Value::as_str)
            == folder.to_str()
    });
    let id = client
        .hello()
        .pointer("/core/id")
        .and_then(Value::as_str)
        .unwrap();
    let inventory = super::safety::owner_inventory(&client, None, id).unwrap();
    let record = inventory
        .iter()
        .find(|record| record["address"] == address)
        .unwrap();
    assert_eq!(
        record.pointer("/foreground/cwd").and_then(Value::as_str),
        folder.to_str()
    );
    let error = super::safety::owner_inventory(&client, None, "another-core").unwrap_err();
    assert!(error.to_string().contains("different core"));
    assert!(folder.is_dir());
    session.finish();
}

#[test]
fn git_metadata_checks_primary_identity_and_groups_linked_checkouts_on_the_owner() {
    let session = CoreSession::new();
    let client = session.client();
    let root = session.repository("project name");
    let linked = session.root.join("linked checkout");
    session.git(
        &root,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    let metadata = super::metadata::discover(
        &client,
        None,
        BTreeSet::from([
            root.to_str().unwrap().to_owned(),
            linked.to_str().unwrap().to_owned(),
            session.root.to_str().unwrap().to_owned(),
        ]),
    );
    let primary = &metadata.repositories[root.to_str().unwrap()];
    let feature = &metadata.repositories[linked.to_str().unwrap()];
    assert!(!primary.linked);
    assert!(feature.linked);
    assert_eq!(primary.primary, root.to_str().unwrap());
    assert_eq!(feature.primary, primary.primary);
    assert_eq!(feature.key, primary.key);
    assert_eq!(feature.branch.as_deref(), Some("feature"));
    assert!(metadata.errors.contains_key(session.root.to_str().unwrap()));
    session.finish();
}

#[test]
fn queued_workspace_reordering_survives_detach_and_late_launch_does_not_steal_focus() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let first = create(&mut backend, session.root.to_str().unwrap(), "first");
    let second = create(&mut backend, session.root.to_str().unwrap(), "second");
    command(
        &mut backend,
        FrontendCommand::WorkspaceMove(schema::WorkspaceMoveParams {
            workspace_id: second.workspace.clone(),
            insert_index: 0,
        }),
    );
    let action = command(
        &mut backend,
        FrontendCommand::TabCreate(schema::TabCreateParams {
            workspace_id: Some(second.workspace.clone()),
            cwd: None,
            focus: true,
            label: None,
            env: HashMap::new(),
        }),
    );
    let Action::Launch(pending) = action else {
        panic!("tab launch must be reserved")
    };
    assert!(pending.focus);
    command(
        &mut backend,
        FrontendCommand::WorkspaceFocus(schema::WorkspaceTarget {
            workspace_id: first.workspace.clone(),
        }),
    );
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&pending.pane).is_some() && !backend.pending()
    });
    assert_eq!(
        backend.view().workspace.as_deref(),
        Some(first.workspace.as_str())
    );
    assert_eq!(backend.document().workspaces[0].id, second.workspace);
    let view = backend.view().clone();
    let client = backend.client().clone();
    drop(backend);
    let (wake, _) = mpsc::sync_channel(1);
    let mut backend = Backend::from_client(client, view, wake).unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.layouts_ready()
    });
    assert_eq!(backend.document().workspaces[0].id, second.workspace);
    assert_eq!(
        backend.view().workspace.as_deref(),
        Some(first.workspace.as_str())
    );
    drop(backend);
    session.finish();
}

#[test]
fn worktree_actions_use_real_git_and_refuse_removing_a_running_programs_checkout() {
    let session = CoreSession::new();
    let repository = session.repository("worktree actions");
    let client = session.client();
    let mut backend = backend(&session);
    let root = create(&mut backend, repository.to_str().unwrap(), "repository");
    let checkout = session.root.join("feature checkout");
    let request_id = "create-worktree".to_owned();
    assert!(matches!(
        backend
            .command(
                None,
                FrontendRequest {
                    id: request_id.clone(),
                    method: FrontendCommand::WorktreeCreate(schema::WorktreeCreateParams {
                        workspace_id: Some(root.workspace),
                        cwd: None,
                        branch: Some("feature".into()),
                        base: Some("HEAD".into()),
                        path: Some(checkout.to_str().unwrap().into()),
                        label: Some("feature work".into()),
                        focus: true,
                        trust_repository: false,
                    })
                }
            )
            .unwrap(),
        Action::Pending
    ));
    let mut created = None;
    wait_until(|| {
        let change = backend.poll().unwrap();
        for completion in change.completions {
            if completion.id == request_id {
                created = Some(completion.result);
            }
        }
        created.is_some()
    });
    let schema::ResponseResult::WorktreeCreated {
        workspace,
        root_pane,
        worktree,
        ..
    } = created.unwrap().unwrap()
    else {
        panic!("worktree create result")
    };
    assert_eq!(
        worktree.path,
        std::fs::canonicalize(&checkout).unwrap().to_str().unwrap()
    );
    assert!(worktree.is_linked_worktree);
    assert!(checkout.is_dir());
    wait_until(|| {
        backend.poll().unwrap();
        backend.repository(None, &worktree.path).is_some()
    });
    let checkout_target = crate::client::ui_model::CheckoutTarget {
        machine_id: backend.local_id().to_owned(),
        folder: worktree.path.clone(),
        label: worktree.label.clone(),
        repo_key: Some(
            backend
                .repository(None, &worktree.path)
                .unwrap()
                .key
                .clone(),
        ),
    };
    let remove = |backend: &mut Backend, id: &str| {
        backend
            .remove_checkout(&checkout_target, Some(id.into()))
            .unwrap();
    };
    remove(&mut backend, "remove-in-use");
    let mut refusal = None;
    wait_until(|| {
        for completion in backend.poll().unwrap().completions {
            if completion.id == "remove-in-use" {
                refusal = Some(completion.result);
            }
        }
        refusal.is_some()
    });
    assert!(refusal.unwrap().unwrap_err().contains("in use"));
    assert!(!backend.pending_mutation());
    assert!(checkout.is_dir());
    let target = crate::client::ui_model::RuntimeTerminalTarget {
        machine_id: backend.local_id().to_owned(),
        terminal: root_pane.terminal_id,
    };
    backend.stop_runtime(&target).unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        client
            .request("terminal.get", json!({"terminal": target.terminal}))
            .unwrap()["state"]
            == "finished"
    });
    remove(&mut backend, "remove-unused");
    let mut removed = None;
    wait_until(|| {
        for completion in backend.poll().unwrap().completions {
            if completion.id == "remove-unused" {
                removed = Some(completion.result);
            }
        }
        removed.is_some()
    });
    removed.unwrap().unwrap();
    assert!(!checkout.exists());
    assert!(backend
        .document()
        .workspace(&workspace.workspace_id)
        .is_none());
    session.git(&repository, &["show-ref", "--verify", "refs/heads/feature"]);
    drop(backend);
    session.finish();
}

#[test]
fn failed_preparation_retains_the_reserved_view_and_retries_without_losing_its_identity() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let folder = session.root.join("created after failed launch");
    let token = backend
        .begin_launch(
            super::Placement::Workspace {
                owner: None,
                name: "recoverable".into(),
                custom_name: true,
            },
            folder.to_str().unwrap().into(),
            Default::default(),
            true,
            None,
        )
        .unwrap();
    backend.disconnected();
    let client = session.client();
    let subscription = client.subscribe().unwrap();
    backend.reconnect(client, subscription).unwrap();
    let mut failed = false;
    wait_until(|| {
        failed |= backend.poll().unwrap().launches.iter().any(|outcome| matches!(outcome,
            LaunchOutcome::Failed { token: completed, uncertain: false, .. } if completed.id == token.id));
        failed
    });
    assert_eq!(backend.pending_launch().unwrap().pane, token.pane);
    assert!(backend.saved_address(&token.pane).is_none());
    std::fs::create_dir(&folder).unwrap();
    backend.retry_pending_launches();
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&token.pane).is_some()
    });
    assert_eq!(backend.document().workspaces.len(), 1);
    assert_eq!(
        backend.document().workspaces[0].tabs[0].panes[0].id,
        token.pane
    );
    drop(backend);
    session.finish();
}

#[test]
fn kept_program_recovery_preserves_the_view_and_definitive_exit_closes_it() {
    let session = CoreSession::new();
    let client = session.client();
    let mut backend = backend(&session);
    let token = create(&mut backend, session.root.to_str().unwrap(), "lifecycle");
    let address = backend.saved_address(&token.pane).unwrap();
    let pid = client
        .request("terminal.get", json!({"terminal": address}))
        .unwrap()["pid"]
        .as_u64()
        .unwrap();
    let status = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    wait_until(|| {
        backend.poll().unwrap();
        assert!(
            backend.saved_address(&token.pane).is_some(),
            "recovering kept programs retain their saved view"
        );
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["pid"]
            .as_u64()
            .is_some_and(|next| next != pid)
    });
    client
        .request(
            "terminal.send",
            json!({"terminal": address, "text": "exit\n", "force": true}),
        )
        .unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.document().workspace(&token.workspace).is_none() && !backend.pending()
    });
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["state"],
        "finished"
    );
    drop(backend);
    session.finish();
}

#[test]
fn command_overlay_restores_original_focus_and_zoom_without_stealing_later_navigation() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let root = create(&mut backend, session.root.to_str().unwrap(), "overlay");
    let overlay = backend
        .begin_overlay_command(
            None,
            Some(&root.pane),
            session.root.to_str().unwrap().into(),
            vec!["/bin/sh".into(), "-c".into(), "sleep 1".into()],
            HashMap::new(),
            None,
        )
        .unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&overlay.pane).is_some()
    });
    assert_eq!(backend.view().panes.get(&root.tab), Some(&overlay.pane));
    assert!(backend.view().zoomed.contains(&root.tab));
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&overlay.pane).is_none()
    });
    assert_eq!(backend.view().panes.get(&root.tab), Some(&root.pane));
    assert!(!backend.view().zoomed.contains(&root.tab));
    let overlay = backend
        .begin_overlay_command(
            None,
            Some(&root.pane),
            session.root.to_str().unwrap().into(),
            vec!["/bin/sh".into(), "-c".into(), "sleep 1".into()],
            HashMap::new(),
            None,
        )
        .unwrap();
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&overlay.pane).is_some()
    });
    backend.focus_pane(None, &root.pane).unwrap();
    backend.view_mut().zoomed.insert(root.tab.clone());
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&overlay.pane).is_none()
    });
    assert_eq!(backend.view().panes.get(&root.tab), Some(&root.pane));
    assert!(
        backend.view().zoomed.contains(&root.tab),
        "leaving an overlay preserves subsequent zoom decisions"
    );
    drop(backend);
    session.finish();
}

#[test]
fn stale_drag_cannot_resize_a_different_split_after_the_tree_collapses() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let root = create(&mut backend, session.root.to_str().unwrap(), "ratio");
    let split = |backend: &mut Backend, target: &str| {
        let Action::Launch(token) = command(
            backend,
            FrontendCommand::PaneSplit(schema::PaneSplitParams {
                workspace_id: Some(root.workspace.clone()),
                target_pane_id: Some(target.into()),
                direction: schema::SplitDirection::Right,
                ratio: Some(0.5),
                cwd: None,
                focus: true,
                right_click: schema::PaneRightClickTarget::Pane,
                env: HashMap::new(),
            }),
        ) else {
            panic!("split reservation")
        };
        wait_until(|| {
            backend.poll().unwrap();
            backend.saved_address(&token.pane).is_some()
        });
        token
    };
    let second = split(&mut backend, &root.pane);
    let captured = backend
        .document()
        .workspace(&root.workspace)
        .unwrap()
        .tab(&root.tab)
        .unwrap()
        .layout
        .split_at(&[])
        .unwrap()
        .to_owned();
    let _third = split(&mut backend, &second.pane);
    command(
        &mut backend,
        FrontendCommand::PaneClose(schema::PaneTarget { pane_id: root.pane }),
    );
    let current = backend
        .document()
        .workspace(&root.workspace)
        .unwrap()
        .tab(&root.tab)
        .unwrap()
        .layout
        .clone();
    assert_ne!(current.split_at(&[]), Some(captured.as_str()));
    let error = backend
        .command(
            None,
            FrontendRequest {
                id: "old-drag".into(),
                method: FrontendCommand::LayoutSetSplitRatio {
                    params: schema::LayoutSetSplitRatioParams {
                        tab_id: Some(root.tab.clone()),
                        pane_id: None,
                        path: Vec::new(),
                        ratio: 0.8,
                    },
                    split_id: Some(captured),
                },
            },
        )
        .err()
        .expect("captured split identity must be checked");
    assert!(error.to_string().contains("changed"));
    assert_eq!(
        backend
            .document()
            .workspace(&root.workspace)
            .unwrap()
            .tab(&root.tab)
            .unwrap()
            .layout,
        current
    );
    drop(backend);
    session.finish();
}

#[test]
fn configured_tabbar_and_window_title_use_owner_metadata_and_original_templates() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let root = create(&mut backend, session.root.to_str().unwrap(), "presentation");
    let mut config = crate::config::Config::default();
    config.ui.tab_bar_right = vec![
        crate::config::TabBarRightEntryConfig::Hostname,
        crate::config::TabBarRightEntryConfig::Text {
            text: " ready ".into(),
        },
        crate::config::TabBarRightEntryConfig::Datetime {
            format: "%Y".into(),
        },
        crate::config::TabBarRightEntryConfig::Zoom,
    ];
    config.ui.tab_bar_right_separator = " | ".into();
    config.ui.window_title = "{hostname}/{workspace}/{tab}/{pane}".into();
    backend.set_presentation_config(&config);
    command(
        &mut backend,
        FrontendCommand::PaneRename(schema::PaneRenameParams {
            pane_id: root.pane,
            label: Some("named".into()),
        }),
    );
    wait_until(|| {
        backend.poll().unwrap();
        backend.snapshot(None).tab_bar_right.len() == 3
    });
    let snapshot = backend.snapshot(None);
    let owner_host = backend
        .metadata
        .get(&None)
        .unwrap()
        .hostname
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.tab_bar_right[0].text, *owner_host);
    assert_eq!(snapshot.tab_bar_right[1].text, " ready ");
    assert_eq!(snapshot.tab_bar_right[2].text.len(), 4);
    assert_eq!(snapshot.tab_bar_right_separator, " | ");
    assert_eq!(
        backend.window_title(None).unwrap(),
        format!("{owner_host}/presentation/1/named")
    );
    drop(backend);
    session.finish();
}

#[test]
fn detached_custom_shell_command_runs_on_core_without_creating_a_saved_view() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    assert!(matches!(
        backend
            .run_background_command(
                None,
                session.root.to_str().unwrap().into(),
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf background > background-result; sleep 1".into()
                ],
                HashMap::new(),
                Some("background".into())
            )
            .unwrap(),
        Action::Pending
    ));
    let mut completed = false;
    wait_until(|| {
        for completion in backend.poll().unwrap().completions {
            if completion.id == "background" {
                completion.result.unwrap();
                completed = true;
            }
        }
        completed && session.root.join("background-result").exists()
    });
    assert_eq!(
        std::fs::read_to_string(session.root.join("background-result")).unwrap(),
        "background"
    );
    assert!(backend.document().workspaces.is_empty());
    drop(backend);
    session.finish();
}

#[test]
fn contextual_commands_follow_the_captured_pane_even_when_new_terminals_use_home() {
    let session = CoreSession::new();
    let client = session.client();
    let foreground = session.root.join("command context");
    std::fs::create_dir(&foreground).unwrap();
    let foreground = std::fs::canonicalize(foreground).unwrap();
    let program = start(
        &client,
        session.root.to_str().unwrap(),
        &format!("cd '{}'; exec sleep 30", foreground.display()),
    );
    let mut backend = backend(&session);
    let target = crate::client::ui_model::RuntimeTerminalTarget {
        machine_id: backend.local_id().to_owned(),
        terminal: program["address"].as_str().unwrap().into(),
    };
    let Action::Launch(token) = backend.attach_runtime(&target).unwrap() else {
        panic!("attach must reserve its view")
    };
    let mut config = crate::config::Config::default();
    config.terminal.new_cwd = crate::config::NewTerminalCwdConfig::Home;
    backend.set_terminal_defaults(&config);
    wait_until(|| {
        backend.poll().unwrap();
        backend.saved_address(&token.pane).is_some()
            && backend
                .command_folder(None, Some(&token.pane))
                .unwrap()
                .as_str()
                == foreground.to_str().unwrap()
    });
    assert_ne!(
        backend.default_folder(None, Some(&token.workspace)),
        foreground.to_str().unwrap()
    );
    assert_eq!(
        backend.command_folder(None, None).unwrap(),
        foreground.to_str().unwrap()
    );
    assert!(backend.command_folder(None, Some("closed-view")).is_err());
    assert!(backend
        .command_folder(Some("another-owner"), Some(&token.pane))
        .is_err());
    drop(backend);
    session.finish();
}

#[test]
fn custom_commands_validate_all_captured_parents_before_changing_focus() {
    let session = CoreSession::new();
    let mut backend = backend(&session);
    let first = create(&mut backend, session.root.to_str().unwrap(), "first");
    let second = create(&mut backend, session.root.to_str().unwrap(), "second");
    let mut params = schema::CommandInvokeParams {
        command_id: "opaque-command".into(),
        workspace_id: Some(first.workspace.clone()),
        tab_id: Some(first.tab.clone()),
        pane_id: Some(first.pane.clone()),
        selection: None,
    };
    let unchanged = serde_json::to_value(backend.view()).unwrap();
    params.workspace_id = Some(second.workspace.clone());
    assert!(backend.focus_command_target(None, &params).is_err());
    assert_eq!(serde_json::to_value(backend.view()).unwrap(), unchanged);
    params.workspace_id = Some(first.workspace.clone());
    params.tab_id = Some(second.tab.clone());
    assert!(backend.focus_command_target(None, &params).is_err());
    assert_eq!(serde_json::to_value(backend.view()).unwrap(), unchanged);
    params.tab_id = Some(first.tab.clone());
    params.pane_id = Some("closed-view".into());
    assert!(backend.focus_command_target(None, &params).is_err());
    assert_eq!(serde_json::to_value(backend.view()).unwrap(), unchanged);
    params.pane_id = Some(first.pane.clone());
    assert!(backend
        .focus_command_target(Some("missing-owner"), &params)
        .is_err());
    assert_eq!(serde_json::to_value(backend.view()).unwrap(), unchanged);
    let local_id = backend.local_id().to_owned();
    assert_eq!(
        backend
            .focus_command_target(Some(&local_id), &params)
            .unwrap(),
        Some(first.pane.clone())
    );
    assert_eq!(backend.view().workspace, Some(first.workspace.clone()));
    assert_eq!(backend.view().panes.get(&first.tab), Some(&first.pane));
    params.workspace_id = Some(second.workspace.clone());
    params.tab_id = Some(second.tab.clone());
    params.pane_id = None;
    assert_eq!(
        backend.focus_command_target(None, &params).unwrap(),
        Some(second.pane.clone())
    );
    assert_eq!(backend.view().workspace, Some(second.workspace));
    params.workspace_id = Some(first.workspace.clone());
    params.tab_id = None;
    assert_eq!(
        backend.focus_command_target(None, &params).unwrap(),
        Some(first.pane)
    );
    assert_eq!(backend.view().workspace, Some(first.workspace));
    drop(backend);
    session.finish();
}

#[test]
fn explicit_checkout_worktree_source_does_not_claim_the_unrelated_active_workspace() {
    let session = CoreSession::new();
    let active = session.repository("active");
    let selected = session.repository("selected");
    let mut backend = backend(&session);
    create(&mut backend, active.to_str().unwrap(), "active");
    backend
        .command(
            None,
            FrontendRequest {
                id: "checkout-list".into(),
                method: FrontendCommand::WorktreeList(schema::WorktreeListParams {
                    workspace_id: None,
                    cwd: Some(selected.to_str().unwrap().into()),
                    trust_repository: false,
                }),
            },
        )
        .unwrap();
    let mut result = None;
    wait_until(|| {
        for completion in backend.poll().unwrap().completions {
            if completion.id == "checkout-list" {
                result = Some(completion.result);
            }
        }
        result.is_some()
    });
    let schema::ResponseResult::WorktreeList { source, worktrees } = result.unwrap().unwrap()
    else {
        panic!("worktree list")
    };
    assert!(source.source_workspace_id.is_none());
    assert_eq!(source.source_checkout_path, selected.to_str().unwrap());
    assert!(worktrees
        .iter()
        .any(|tree| tree.path == selected.to_str().unwrap()));
    drop(backend);
    session.finish();
}
