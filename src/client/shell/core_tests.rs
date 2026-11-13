use super::*;
use crate::client::{
    ui_action::CoreAction,
    ui_domain::AgentStatus,
    ui_model::{
        ClientShellAgent, CoreMachineView, CoreTerminalView, RuntimeControls, RuntimeTerminalTarget,
    },
};
use std::sync::Arc;

fn terminal(id: &str) -> CoreTerminalView {
    CoreTerminalView {
        target: RuntimeTerminalTarget {
            machine_id: "actual-core-owner".into(),
            terminal: id.into(),
        },
        machine_label: "Local".into(),
        label: format!("program {id}"),
        cwd: Some("/repo".into()),
        status: None,
    }
}
fn checkout(linked: bool, terminals: Vec<CoreTerminalView>) -> Arc<ClientShellAvailableCheckout> {
    Arc::new(ClientShellAvailableCheckout {
        machine_id: "actual-core-owner".into(),
        folder: if linked {
            "/repo-worktrees/feature"
        } else {
            "/repo"
        }
        .into(),
        label: if linked { "feature" } else { "repo" }.into(),
        branch: Some(if linked { "feature" } else { "main" }.into()),
        git_ahead_behind: None,
        worktree: Some(crate::client::ui_model::ClientShellWorktree {
            key: "actual-repo-key".into(),
            label: "repo".into(),
            is_linked_worktree: linked,
        }),
        terminals,
        error: None,
        agent_status: AgentStatus::Unknown,
    })
}
fn state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_core_catalog(&[CoreMachineView {
        machine_id: "actual-core-owner".into(),
        label: "Local".into(),
        local: true,
        status: ClientEndpointStatus::Online,
        diagnostic: None,
    }]);
    let mut snapshot = test_support::snapshot();
    snapshot.workspaces.clear();
    snapshot.tabs.clear();
    snapshot.panes.clear();
    snapshot.focused_workspace_id = None;
    snapshot.focused_tab_id = None;
    snapshot.focused_pane_id = None;
    snapshot.available_checkouts = vec![
        checkout(false, vec![terminal("main-program")]),
        checkout(true, vec![]),
    ];
    snapshot.runtime_controls = Some(RuntimeControls {
        held_input: 0,
        pending_launch: false,
        pending_layout: false,
    });
    snapshot.core_terminals = Some(vec![]);
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(PaneSurfaceFrame {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        surface_revision: 1,
        frame: crate::client::frame::FrameData::from_ratatui_buffer_with_hyperlinks(
            &Buffer::empty(Rect::new(0, 0, 4, 2)),
            None,
            &[],
        ),
        panes: Vec::new(),
        splits: Vec::new(),
        popup: None,
        graphics: Default::default(),
    });
    state
}
#[test]
fn complete_core_inventory_uses_regular_machine_tree_without_saved_view_ids() {
    let mut state = state();
    let frame = state.compose(110, 35).unwrap();
    assert_eq!(state.hits.machines.len(), 1);
    assert!(state.hits.workspaces.is_empty());
    assert_eq!(state.hits.available_checkouts.len(), 2);
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("machines"));
    assert!(text.contains("repo"));
    let linked = state
        .hits
        .available_checkouts
        .iter()
        .find(|hit| hit.checkout.folder.ends_with("feature"))
        .unwrap();
    assert!(linked.group_toggle.is_none());
    let captured = Arc::clone(&linked.checkout);
    let mut outcome = ClientShellInput::default();
    state.open_available_checkout(captured, &mut outcome);
    assert!(
        matches!(outcome.actions.as_slice(),[ClientShellAction::Core(CoreAction::OpenCheckout(target))] if target.machine_id=="actual-core-owner" && target.folder=="/repo-worktrees/feature")
    );
}

fn runtime_agent(status: AgentStatus) -> ClientShellAgent {
    ClientShellAgent {
        agent_id: "reported-agent".into(),
        runtime_target: Some(terminal("actual-program").target),
        view: None,
        cwd: Some("/repo".into()),
        name: Some("agent".into()),
        display_agent: None,
        agent: Some("codex".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    }
}

#[test]
fn agent_sidebar_preserves_unknown_core_status_without_a_saved_view() {
    let mut snapshot = test_support::snapshot();
    snapshot.workspaces.clear();
    snapshot.tabs.clear();
    snapshot.panes.clear();
    snapshot.agents = vec![runtime_agent(AgentStatus::Unknown)];
    let mut source_config = Config::default();
    source_config.ui.sidebar.agents.rows = vec![vec![crate::config::AgentSidebarToken::StateText]];
    let config = ClientShellConfig::from_config(&source_config);
    let area = Rect::new(0, 0, 40, 12);
    for (status, expected) in [
        (AgentStatus::Unknown, "unknown"),
        (AgentStatus::Idle, "idle"),
    ] {
        snapshot.agents[0].agent_status = status;
        let mut buffer = Buffer::empty(area);
        agent_sidebar::render_agent_panel(
            &mut buffer,
            area,
            &snapshot,
            &config,
            &mut 0,
            &mut ShellHitMap::default(),
        );
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains(if expected == "unknown" {
            "idle"
        } else {
            "unknown"
        }));
    }
}

#[test]
fn core_reported_status_bypasses_legacy_completion_inference() {
    let mut presentation = endpoint_agent_state::EndpointAgentPresentation::default();
    let mut snapshot = test_support::snapshot();
    snapshot.runtime_controls = Some(RuntimeControls::default());
    snapshot.agents = vec![runtime_agent(AgentStatus::Working)];
    for (index, status) in [
        AgentStatus::Working,
        AgentStatus::Idle,
        AgentStatus::Done,
        AgentStatus::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        snapshot.agents[0].agent_status = status;
        snapshot.agents[0].state_change_seq = index as u64 + 1;
        presentation.project_snapshot_for_generation(&mut snapshot, Some(1));
        assert_eq!(snapshot.agents[0].agent_status, status);
    }
}

#[test]
fn core_machine_retry_preserves_permanent_owner_and_shows_the_fresh_diagnostic() {
    let mut state = state();
    let mut machines = vec![
        CoreMachineView {
            machine_id: "actual-core-owner".into(),
            label: "Local".into(),
            local: true,
            status: ClientEndpointStatus::Online,
            diagnostic: None,
        },
        CoreMachineView {
            machine_id: "remote-permanent-owner".into(),
            label: "before-rename".into(),
            local: false,
            status: ClientEndpointStatus::Attention,
            diagnostic: Some("Core update required. hc machine update before-rename".into()),
        },
    ];
    state.set_core_catalog(&machines);
    let endpoint = ClientEndpointId::Core {
        machine_id: "remote-permanent-owner".into(),
        local: false,
    };
    state.refresh_hit_map(110, 35);
    let hit = state
        .hits
        .machines
        .iter()
        .find(|hit| hit.endpoint_id == endpoint)
        .unwrap();
    let badge = hit.status_badge;
    let outcome = state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: badge.x,
            row: badge.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    )]);
    assert!(outcome.repaint);
    assert_eq!(state.take_machine_retries(), vec![endpoint.clone()]);
    machines[1].label = "after-rename".into();
    machines[1].status = ClientEndpointStatus::Connecting;
    machines[1].diagnostic = None;
    state.set_core_catalog(&machines);
    let error = "Authentication required. hc machine login after-rename".to_string();
    assert!(state.finish_machine_retry(&endpoint, Some(error.clone())));
    let notice = state.visible_endpoint_notice.as_ref().unwrap();
    assert_eq!(notice.title, "after-rename: connection needs attention");
    assert_eq!(notice.body, error);
    assert!(state.retry_machine(&endpoint));
    assert_eq!(state.take_machine_retries(), vec![endpoint.clone()]);
    assert!(state.finish_machine_retry(&endpoint, None));
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn delayed_core_completion_rechecks_the_runtime_and_toast_attaches_its_captured_program() {
    let mut state = state();
    state.config.toast_delivery = crate::config::ToastDelivery::Herdr;
    state.config.toast_delay_seconds = 1;
    let endpoint = state.active_endpoint_id.clone();
    let mut snapshot = test_support::snapshot();
    snapshot.runtime_controls = Some(RuntimeControls::default());
    snapshot.workspaces.clear();
    snapshot.tabs.clear();
    snapshot.panes.clear();
    snapshot.agents = vec![runtime_agent(AgentStatus::Done)];
    let target = snapshot.agents[0].runtime_target.clone().unwrap();
    let event = SemanticNotification {
        kind: SemanticNotificationKind::Finished,
        title: "agent finished".into(),
        body: Some("Local · /repo".into()),
        sound: Some(SemanticNotificationSound::Done),
        agent: Some("codex".into()),
        workspace_id: None,
        tab_id: None,
        pane_id: None,
        position: None,
    };
    state.set_snapshot(Box::new(snapshot.clone()));
    let now = std::time::Instant::now();
    assert!(state
        .receive_runtime_notification(&endpoint, event.clone(), target.clone(), now)
        .0
        .is_empty());
    snapshot.agents[0].agent_status = AgentStatus::Working;
    state.set_snapshot(Box::new(snapshot.clone()));
    assert!(state
        .tick_notifications(now + std::time::Duration::from_secs(2))
        .0
        .is_empty());
    assert!(state.visible_notification.is_none());

    snapshot.agents[0].agent_status = AgentStatus::Done;
    state.set_snapshot(Box::new(snapshot));
    let delivered = now + std::time::Duration::from_secs(3);
    state.receive_runtime_notification(&endpoint, event, target.clone(), delivered);
    let effects = state
        .tick_notifications(delivered + std::time::Duration::from_secs(2))
        .0;
    assert!(matches!(
        effects.as_slice(),
        [ClientShellNotificationEffect::Sound {
            sound: crate::sound_kind::Sound::Done,
            ..
        }]
    ));
    assert!(state
        .visible_notification
        .as_ref()
        .unwrap()
        .event
        .pane_id
        .is_none());
    let mut input = ClientShellInput::default();
    state.focus_visible_notification(&mut input);
    assert!(
        matches!(input.actions.as_slice(), [ClientShellAction::AttachTerminal {
        target: captured
    }] if captured == &target)
    );
}

#[test]
fn core_completion_in_the_active_tab_keeps_original_notification_suppression() {
    let mut state = state();
    state.config.toast_delivery = crate::config::ToastDelivery::Herdr;
    state.config.toast_delay_seconds = 0;
    let endpoint = state.active_endpoint_id.clone();
    let mut snapshot = test_support::snapshot();
    let agent = runtime_agent(AgentStatus::Done);
    let target = agent.runtime_target.clone().unwrap();
    snapshot.panes[0].runtime_target = Some(target.clone());
    snapshot.agents = vec![agent];
    state.set_snapshot(Box::new(snapshot));
    let event = SemanticNotification {
        kind: SemanticNotificationKind::Finished,
        title: "agent finished".into(),
        body: None,
        sound: Some(SemanticNotificationSound::Done),
        agent: Some("codex".into()),
        workspace_id: None,
        tab_id: None,
        pane_id: None,
        position: None,
    };
    let (effects, _) =
        state.receive_runtime_notification(&endpoint, event, target, std::time::Instant::now());
    assert!(effects.is_empty());
    assert!(state.visible_notification.is_none());
}
#[test]
fn captured_checkout_programs_survive_inventory_reordering_and_picker_has_no_total_cap() {
    let mut state = state();
    let programs = (0..257)
        .map(|index| terminal(&format!("program-{index}")))
        .collect::<Vec<_>>();
    let captured = checkout(true, programs);
    state.open_available_checkout(captured, &mut ClientShellInput::default());
    if let Some(ClientShellOverlay::CoreTerminals(overlay)) = &mut state.overlay {
        overlay.query = TextEditor::from("program-256");
    } else {
        panic!("multiple programs require explicit selection")
    }
    state.set_snapshot(Box::new(test_support::snapshot()));
    let outcome = state.handle_input_bytes(b"\r");
    assert!(
        matches!(outcome.actions.as_slice(),[ClientShellAction::AttachTerminal{target}] if target.machine_id=="actual-core-owner" && target.terminal=="program-256")
    );
}
#[test]
fn destructive_checkout_confirmation_captures_owner_path_and_keeps_programs_running_until_confirmed(
) {
    let mut state = state();
    let captured = checkout(true, vec![]).target();
    let mut outcome = ClientShellInput::default();
    state.request_core_action(CoreAction::RemoveCheckout(captured.clone()), &mut outcome);
    assert!(outcome.actions.is_empty());
    let Some(ClientShellOverlay::ConfirmRuntime(confirm)) = &state.overlay else {
        panic!("confirmation required")
    };
    assert!(confirm.detail.contains(&captured.folder));
    state.set_snapshot(Box::new(test_support::snapshot()));
    state.accept_confirmation(&mut outcome);
    assert!(
        matches!(outcome.actions.as_slice(),[ClientShellAction::Core(CoreAction::RemoveCheckout(target))] if target==&captured)
    );
}

#[test]
fn stop_requires_confirmation_for_the_program_captured_when_menu_opened() {
    let mut state = state();
    let first = terminal("first-program");
    let second = terminal("second-program");
    let mut snapshot = test_support::snapshot();
    snapshot.runtime_controls = Some(RuntimeControls {
        held_input: 0,
        pending_launch: false,
        pending_layout: false,
    });
    snapshot.core_terminals = Some(vec![first.clone(), second.clone()]);
    snapshot.panes[0].runtime_target = Some(first.target.clone());
    state.set_snapshot(Box::new(snapshot.clone()));
    state.toggle_global_menu();
    snapshot.panes[0].runtime_target = Some(second.target.clone());
    state.set_snapshot(Box::new(snapshot));
    let index = global_menu::global_menu_items(state.snapshot.as_deref().unwrap(), true)
        .iter()
        .position(|(_, action)| *action == global_menu::ClientGlobalMenuAction::StopTerminal)
        .unwrap();
    let mut outcome = ClientShellInput::default();
    state.activate_global_menu_item(index, &mut outcome);
    assert!(outcome.actions.is_empty());
    let Some(ClientShellOverlay::ConfirmRuntime(confirm)) = &state.overlay else {
        panic!("Stop must require confirmation")
    };
    assert!(confirm.detail.contains("first-program"));
    state.accept_confirmation(&mut outcome);
    assert!(
        matches!(outcome.actions.as_slice(),[ClientShellAction::Core(CoreAction::StopTerminal(target))] if target==&first.target)
    );
}

#[test]
fn stop_can_be_cancelled_then_confirmed_through_the_regular_raw_key_route() {
    let mut state = state();
    let program = terminal("running-program");
    let mut snapshot = test_support::snapshot();
    snapshot.runtime_controls = Some(RuntimeControls::default());
    snapshot.core_terminals = Some(vec![program.clone()]);
    snapshot.panes[0].runtime_target = Some(program.target.clone());
    state.set_snapshot(Box::new(snapshot));
    let index = global_menu::global_menu_items(state.snapshot.as_deref().unwrap(), true)
        .iter()
        .position(|(_, action)| *action == global_menu::ClientGlobalMenuAction::StopTerminal)
        .unwrap();
    for accept in [false, true] {
        state.toggle_global_menu();
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::GlobalMenu(_))
        ));
        for _ in 0..index {
            state.handle_input_bytes(b"\x1b[B");
        }
        let selected = state.handle_input_bytes(b"\r");
        assert!(selected.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ConfirmRuntime(_))
        ));
        let outcome = state.handle_input_bytes(if accept { b"\r" } else { b"\x1b" });
        assert!(state.overlay.is_none());
        if accept {
            assert!(
                matches!(outcome.actions.as_slice(), [ClientShellAction::Core(
                CoreAction::StopTerminal(target)
            )] if target == &program.target)
            );
        } else {
            assert!(outcome.actions.is_empty());
        }
    }
}

#[test]
fn unviewed_repository_reuses_worktree_dialogs_with_its_actual_checkout_source() {
    let mut state = state();
    let target = checkout(false, vec![]).target();
    let outcome = state.begin_checkout_worktree(target.clone(), true);
    let [ClientShellAction::Endpoint { request, .. }] = outcome.actions.as_slice() else {
        panic!("repository discovery must use core worktree operation")
    };
    assert!(
        matches!(&request.method,crate::client::ui_action::FrontendCommand::WorktreeList(params) if params.workspace_id.is_none() && params.cwd.as_deref()==Some("/repo"))
    );
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request.id,
        Ok(crate::client::ui_domain::ResponseResult::WorktreeList {
            source: crate::client::ui_domain::WorktreeSourceInfo {
                repo_key: "actual-repo-key".into(),
                repo_name: "repo".into(),
                repo_root: "/repo".into(),
                source_checkout_path: "/repo".into(),
                source_workspace_id: None,
            },
            worktrees: vec![],
        }),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    let Some(ClientShellOverlay::WorktreeCreate(create)) = &mut state.overlay else {
        panic!("reuse original worktree dialog")
    };
    create.branch = TextEditor::from("feature");
    let mut outcome = ClientShellInput::default();
    state.submit_worktree_create(&mut outcome);
    assert!(
        matches!(outcome.actions.as_slice(),[ClientShellAction::Endpoint{request,..}] if matches!(&request.method,crate::client::ui_action::FrontendCommand::WorktreeCreate(params) if params.workspace_id.is_none() && params.cwd.as_deref()==Some("/repo") && params.branch.as_deref()==Some("feature")))
    );
}
