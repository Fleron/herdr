#![cfg(unix)]

mod core_support;
#[path = "core_support/pty.rs"]
mod tui_pty;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use core_support::{wait_until, CoreSession};
use herdr_core_tui::connection::Client;
use herdr_core_tui::model::{Axis, Document, Operation, Pane, Tab, Workspace};
use herdr_core_tui::persistence::LayoutStore;
use serde_json::{json, Value};
use tui_pty::{layout, terminal, wait_layout, wait_terminal_text, TuiPty};

fn desktop_document(client: &Client) -> Value {
    client
        .request("doc.get", json!({"name": "herdr-app"}))
        .unwrap()
}

#[test]
fn regular_sidebar_shows_repository_worktrees_and_global_agents_before_attachment() {
    let session = CoreSession::new();
    let client = session.client();
    let repository = session.repository("sidebar-project");
    let branch = session.root.join("sidebar-linked");
    session.git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "sidebar-feature",
            branch.to_str().unwrap(),
        ],
    );
    let start = |name: &str, cwd: &Path, script: &str| {
        client
            .request(
                "terminal.run",
                json!({"argv":["/bin/sh","-c",script],"cwd":cwd,"name":name,"no_parent":true}),
            )
            .unwrap()["address"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let agent=start("global-before-attach",&branch,"printf '\\033]7501;app=codex:state=blocked:kind=question\\007AGENT_BEFORE_ATTACH\\n'; exec cat");
    let shell = start(
        "plain-shell-recovery",
        &repository,
        "printf SHELL_BEFORE_ATTACH; exec cat",
    );
    wait_until(|| terminal(&client, &agent)["status"]["state"] == "blocked");
    assert!(layout(&client, None).workspaces.is_empty());
    let mut tui = TuiPty::spawn(&session, Path::new(env!("CARGO_BIN_EXE_herdr")));
    tui.wait_screen(|screen| {
        screen.contains("agents")
            && screen.contains("sidebar-project")
            && screen.contains("sidebar-feat")
            && screen.contains("codex")
    });
    assert!(
        layout(&client, None).workspaces.is_empty(),
        "the original sidebar projects inventory without attaching programs"
    );
    tui.capture("regular-sidebar-before-attachment");
    tui.action("attach terminal");
    tui.wait_text("plain-shell-recovery");
    tui.wait_text("global-before-attach");
    tui.choose_terminal("plain-shell-recovery");
    let saved = wait_layout(&client, None, |document| !document.workspaces.is_empty());
    assert_eq!(saved.workspaces[0].tabs[0].panes[0].terminal, shell);
    tui.wait_text("SHELL_BEFORE_ATTACH");
    tui.capture("regular-sidebar-attached");
    tui.detach();
    assert_eq!(terminal(&client, &shell)["state"], "live");
    assert_eq!(terminal(&client, &agent)["state"], "live");
    session.finish();
}

#[test]
fn reconnect_to_a_different_core_never_retargets_terminal_input_or_saved_layouts() {
    let first = CoreSession::new();
    let second = CoreSession::new();
    let first_client = first.client();
    let second_client = second.client();
    assert_ne!(
        first_client.hello()["core"]["id"],
        second_client.hello()["core"]["id"]
    );
    let start = |client: &Client, marker: &str| {
        client.request("terminal.run", json!({
        "argv": ["/bin/sh", "-c", format!("stty -echo; printf '{marker}\\n'; exec /bin/cat")],
        "no_parent": true,
    })).unwrap()
    };
    let old = start(&first_client, "ORIGINAL_CORE_READY");
    let other = start(&second_client, "OTHER_CORE_READY");
    let address = old["address"].as_str().unwrap();
    assert_eq!(other["address"].as_str(), Some(address));
    let mut store = LayoutStore::load(&first_client, None).unwrap();
    let mut workspace = Workspace::new("original owner", first.root.to_str().unwrap(), None);
    workspace.tabs.push(Tab::new("original program", address));
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(&first_client).unwrap();
    let original_layout = layout(&first_client, None);
    let untouched = second_client
        .request("doc.get", json!({"name": "herdr-tui"}))
        .unwrap();
    let selector = first.root.join("selected-core");
    std::fs::write(&selector, "first").unwrap();
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let launch = |session: &CoreSession| {
        format!(
        "exec env -i PATH={} HOME={} SHELL=/bin/sh HC_SESSION={} XDG_RUNTIME_DIR={} XDG_STATE_HOME={} XDG_CONFIG_HOME={} {} \"$@\"\n",
        quote(&std::env::var("PATH").unwrap()), quote(session.root.to_str().unwrap()),
        quote(&session.session), quote(session.root.join("run").to_str().unwrap()),
        quote(session.root.join("state").to_str().unwrap()), quote(session.root.join("config").to_str().unwrap()),
        quote(session.binary.to_str().unwrap()),
    )
    };
    let launcher = first.root.join("select-core");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nif [ \"$(cat {})\" = second ]; then\n{}fi\n{}",
            quote(selector.to_str().unwrap()),
            launch(&second),
            launch(&first),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let extra = [("HC_BIN", launcher.into_os_string())];
    let mut tui = TuiPty::spawn_with_env(&first, Path::new(env!("CARGO_BIN_EXE_herdr")), &extra);
    tui.wait_text("ORIGINAL_CORE_READY");
    std::fs::write(selector, "second").unwrap();
    first.successful(&["server", "stop"]);
    tui.wait_text("core identity changed");
    tui.send(b"MUST_NOT_REACH_OTHER_CORE\r");
    tui.prefix(b"W");
    tui.wait_text("core identity changed");
    assert!(!tui.text().contains("rename workspace"));
    tui.action("Retry connections");
    tui.wait_text("core identity changed");
    tui.action("discard unsaved layout changes");
    tui.wait_text("Discard unsaved layout changes?");
    tui.send(b"\r");
    tui.wait_screen(|screen| !screen.contains("Discard unsaved layout changes?"));
    assert!(!tui_pty::terminal_text(&second_client, address).contains("MUST_NOT_REACH_OTHER_CORE"));
    assert_eq!(
        second_client
            .request("doc.get", json!({"name": "herdr-tui"}))
            .unwrap(),
        untouched
    );
    assert_eq!(terminal(&second_client, address)["pid"], other["pid"]);
    tui.detach();
    second.finish();
    assert_eq!(layout(&first.client(), None), original_layout);
    first.finish();
}

#[test]
fn natural_exit_closes_the_view_and_kept_recovery_preserves_it_without_losing_core() {
    let session = CoreSession::new();
    let client = session.client();
    let mut tui = TuiPty::spawn(&session, Path::new(env!("CARGO_BIN_EXE_herdr")));
    tui.wait_text("machines");
    tui.create_workspace(session.root.to_str().unwrap());
    let saved = wait_layout(&client, None, |document| document.workspaces.len() == 1);
    let address = &saved.workspaces[0].tabs[0].panes[0].terminal;
    tui.print_marker("PROGRAM_BEFORE_RECOVERY");
    wait_terminal_text(&client, address, "PROGRAM_BEFORE_RECOVERY");
    let pid = terminal(&client, address)["pid"].as_u64().unwrap();
    assert!(std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    wait_until(|| {
        terminal(&client, address)["pid"]
            .as_u64()
            .is_some_and(|current| current != pid)
    });
    assert_eq!(layout(&client, None), saved);
    tui.print_marker("KEPT_PROGRAM_RECOVERED");
    wait_terminal_text(&client, address, "KEPT_PROGRAM_RECOVERED");
    tui.send(b"exit 7\r");
    wait_until(|| terminal(&client, address)["state"] == "finished");
    wait_layout(&client, None, |document| document.workspaces.is_empty());
    tui.wait_screen(|screen| !screen.contains("KEPT_PROGRAM_RECOVERED"));
    tui.action("Help");
    tui.wait_text("keybinds");
    tui.send(b"\x1b");
    tui.wait_screen(|screen| !screen.contains("keybinds"));
    tui.create_workspace(session.root.to_str().unwrap());
    let replacement = wait_layout(&client, None, |document| document.workspaces.len() == 1);
    let fresh = &replacement.workspaces[0].tabs[0].panes[0].terminal;
    assert_ne!(fresh, address);
    tui.print_marker("CORE_STILL_USABLE_AFTER_EXIT");
    wait_terminal_text(&client, fresh, "CORE_STILL_USABLE_AFTER_EXIT");
    tui.detach();
    session.finish();
}

#[test]
fn duplicate_terminal_observer_repaints_resize_mode_without_sending_forbidden_input() {
    let session = CoreSession::new();
    let client = session.client();
    let record = client.request("terminal.run", json!({
        "argv": ["/bin/sh", "-c", "stty -echo; printf '\\033[?2048hOBSERVER_MODE_READY\\n'; exec /bin/cat"],
        "no_parent": true,
    })).unwrap();
    let address = record["address"].as_str().unwrap();
    let pid = record["pid"].as_u64().unwrap();
    let mut store = LayoutStore::load(&client, None).unwrap();
    let mut workspace = Workspace::new("observer modes", session.root.to_str().unwrap(), None);
    let tab = Tab::new("same program twice", address);
    let workspace_id = workspace.id.clone();
    let tab_id = tab.id.clone();
    let target = tab.panes[0].id.clone();
    workspace.tabs.push(tab);
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store
        .apply(Operation::SplitPane {
            workspace: workspace_id,
            tab: tab_id,
            target,
            pane: Pane::new(address),
            split: "observer-split".into(),
            axis: Axis::Vertical,
        })
        .unwrap();
    store.save(&client).unwrap();
    let saved = layout(&client, None);
    let mut tui = TuiPty::spawn(&session, Path::new(env!("CARGO_BIN_EXE_herdr")));
    tui.wait_screen(|screen| screen.matches("OBSERVER_MODE_READY").count() == 2);
    tui.resize(146, 50);
    tui.send(b"OBSERVER_AFTER_RESIZE\r");
    wait_terminal_text(&client, address, "OBSERVER_AFTER_RESIZE");
    tui.wait_screen(|screen| screen.matches("OBSERVER_AFTER_RESIZE").count() == 2);
    assert!(!tui
        .text()
        .contains("observer attachments cannot send input"));
    tui.detach();
    assert_eq!(layout(&client, None), saved);
    assert_eq!(terminal(&client, address)["state"], "live");
    assert_eq!(terminal(&client, address)["pid"].as_u64(), Some(pid));
    session.finish();
}

fn assert_programs_unchanged(client: &Client, programs: &BTreeMap<String, u64>) {
    for (address, pid) in programs {
        let observed = terminal(client, address);
        assert_eq!(observed["state"], "live", "{address}");
        assert_eq!(
            observed["pid"].as_u64(),
            Some(*pid),
            "{address} was restarted instead of retained"
        );
    }
}

fn programs(client: &Client, addresses: &[String]) -> BTreeMap<String, u64> {
    addresses
        .iter()
        .map(|address| {
            let observed = terminal(client, address);
            assert!(observed.get("parent").is_none_or(Value::is_null));
            assert_eq!(observed["state"], "live");
            (address.clone(), observed["pid"].as_u64().unwrap())
        })
        .collect()
}

fn pane_addresses(document: &Document) -> Vec<String> {
    document
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.tabs.iter())
        .flat_map(|tab| tab.panes.iter().map(|pane| pane.terminal.clone()))
        .collect()
}

fn confirm_worktree(tui: &mut TuiPty, branch: &str) {
    tui.right_click_sidebar_text(branch);
    tui.click_text("Delete worktree checkout...");
    tui.wait_text("Remove worktree?");
    tui.send(b"\r");
}

#[test]
fn default_tui_controls_preserve_programs_layout_and_focus_across_detach_and_core_restart() {
    let session = CoreSession::new();
    let client = session.client();
    client
        .request(
            "doc.set",
            json!({"name": "herdr-app", "version": 0, "value": {"sentinel": "desktop-layout"}}),
        )
        .unwrap();
    let desktop = desktop_document(&client);
    let host = client
        .request(
            "terminal.run",
            json!({"argv": ["/bin/cat"], "name": "hosting-terminal", "no_parent": true}),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let folder = session.root.join("workspace");
    std::fs::create_dir(&folder).unwrap();
    let folder = std::fs::canonicalize(folder).unwrap();
    let extra = [
        ("HC_TERMINAL_ID", host.clone().into()),
        ("HC_TERMINAL_SOCKET", session.socket().into_os_string()),
    ];
    let binary = Path::new(env!("CARGO_BIN_EXE_herdr"));
    let mut tui = TuiPty::spawn_with_env(&session, binary, &extra);
    tui.wait_text("machines");
    tui.create_workspace(folder.to_str().unwrap());
    let initial = wait_layout(&client, None, |document| document.workspaces.len() == 1);
    let left = initial.workspaces[0].tabs[0].panes[0].terminal.clone();
    tui.print_marker("FIRST_PANE_EXECUTED");
    wait_terminal_text(&client, &left, "FIRST_PANE_EXECUTED");
    tui.wait_text("FIRST_PANE_EXECUTED");
    tui.send(b"printf '\\033]7501;app=codex\\007'\r");
    tui.print_marker("STATUS_REMAINS_UNREPORTED");
    wait_terminal_text(&client, &left, "STATUS_REMAINS_UNREPORTED");
    assert!(terminal(&client, &left)["status"].is_null());
    tui.action("Copy mode");
    tui.wait_screen(|screen| screen.contains(" COPY "));
    let pasted_marker = "COPY_MODE_PASTE_EXECUTED";
    let encoded: String = pasted_marker
        .bytes()
        .map(|byte| format!("\\{byte:03o}"))
        .collect();
    tui.send(format!("\x1b[200~printf '{encoded}\\n'\r\x1b[201~").as_bytes());
    tui.send(b"q");
    tui.print_marker("COPY_MODE_EXITED");
    wait_terminal_text(&client, &left, "COPY_MODE_EXITED");
    assert!(!tui_pty::terminal_text(&client, &left).contains(pasted_marker));
    tui.send(
        b"sh -c 'printf \"\\033]7501;app=codex:state=blocked:kind=question\\007\"; exec cat'\r",
    );
    wait_until(|| terminal(&client, &left)["status"]["state"] == "blocked");
    tui.wait_screen(|screen| {
        screen.lines().any(|line| {
            line.split_once('│')
                .is_some_and(|(sidebar, _)| sidebar.contains("codex"))
        })
    });
    tui.send(b"\x03");
    tui.print_marker("STATUS_REPORTER_RETURNED_TO_SHELL");
    wait_terminal_text(&client, &left, "STATUS_REPORTER_RETURNED_TO_SHELL");

    tui.prefix(b"W");
    tui.wait_text("rename workspace");
    tui.replace_text("Primary");
    wait_layout(&client, None, |document| {
        document.workspaces[0].name == "Primary"
    });
    tui.prefix(b"P");
    tui.wait_text("rename pane");
    tui.replace_text("left-pane");
    wait_layout(&client, None, |document| {
        document.workspaces[0].tabs[0].panes[0].name.as_deref() == Some("left-pane")
    });
    tui.prefix_and_print(b'v', "RIGHT_PANE_EXECUTED");
    let split = wait_layout(&client, None, |document| {
        document.workspaces[0].tabs[0].panes.len() == 2
    });
    let right = split.workspaces[0].tabs[0].panes[1].terminal.clone();
    wait_terminal_text(&client, &right, "RIGHT_PANE_EXECUTED");
    assert!(!tui_pty::terminal_text(&client, &left).contains("RIGHT_PANE_EXECUTED"));
    tui.prefix(b"h");
    tui.print_marker("FOCUS_RETURNED_LEFT");
    wait_terminal_text(&client, &left, "FOCUS_RETURNED_LEFT");
    let before_ratio = layout(&client, None).workspaces[0].tabs[0].layout.clone();
    tui.prefix(b"r");
    tui.send(b"l\r");
    wait_layout(&client, None, |document| {
        document.workspaces[0].tabs[0].layout != before_ratio
    });
    let before_zoom = terminal(&client, &left)["cols"].as_u64().unwrap();
    tui.prefix(b"z");
    wait_until(|| terminal(&client, &left)["cols"].as_u64().unwrap() > before_zoom);
    tui.prefix(b"z");
    wait_until(|| terminal(&client, &left)["cols"].as_u64().unwrap() == before_zoom);
    tui.resize(150, 48);
    wait_until(|| terminal(&client, &left)["cols"].as_u64().unwrap() > before_zoom);

    tui.prefix_and_print(b'c', "SECOND_TAB_EXECUTED");
    let two_tabs = wait_layout(&client, None, |document| {
        document.workspaces[0].tabs.len() == 2
    });
    let second_tab = two_tabs.workspaces[0].tabs[1].panes[0].terminal.clone();
    wait_terminal_text(&client, &second_tab, "SECOND_TAB_EXECUTED");
    assert!(!tui_pty::terminal_text(&client, &left).contains("SECOND_TAB_EXECUTED"));
    assert!(!tui_pty::terminal_text(&client, &right).contains("SECOND_TAB_EXECUTED"));
    tui.prefix(b"T");
    tui.wait_text("rename tab");
    tui.replace_text("Second");
    wait_layout(&client, None, |document| {
        document.workspaces[0].tabs[1].name == "Second"
    });
    tui.prefix_and_print(b'p', "FIRST_TAB_RESELECTED");
    wait_terminal_text(&client, &left, "FIRST_TAB_RESELECTED");
    tui.prefix(b"n");
    tui.prefix(b"x");
    let saved = wait_layout(&client, None, |document| {
        document.workspaces[0].tabs.len() == 1
    });
    assert_eq!(saved.workspaces[0].tabs[0].panes.len(), 2);
    assert_eq!(terminal(&client, &second_tab)["state"], "live");
    tui.action("attach terminal");
    tui.wait_text("attach terminal");
    assert!(
        !tui.text().contains("hosting-terminal"),
        "the TUI must not offer its own hosting terminal"
    );
    tui.send(b"\x1b");
    tui.wait_screen(|screen| !screen.contains("attach terminal"));
    let retained = programs(&client, &[host, left.clone(), right, second_tab]);
    tui.detach();
    assert_programs_unchanged(&client, &retained);
    assert_eq!(layout(&client, None), saved);
    assert_eq!(desktop_document(&client), desktop);

    let mut reopened = TuiPty::spawn_with_env(&session, binary, &extra);
    reopened.wait_text("Primary");
    reopened.print_marker("REOPENED_WITH_SAME_FOCUS");
    wait_terminal_text(&client, &left, "REOPENED_WITH_SAME_FOCUS");
    reopened.wait_text("REOPENED_WITH_SAME_FOCUS");
    assert_eq!(
        pane_addresses(&layout(&client, None)),
        pane_addresses(&saved)
    );
    session.successful(&["server", "restart"]);
    let fresh = session.client();
    reopened.wait_screen(|screen| {
        screen.contains("REOPENED_WITH_SAME_FOCUS")
            && !screen.contains("Connecting terminal")
            && !screen.contains("Terminal unavailable")
    });
    reopened.print_marker("AFTER_CORE_RESTART");
    wait_terminal_text(&fresh, &left, "AFTER_CORE_RESTART");
    reopened.wait_text("AFTER_CORE_RESTART");
    assert_programs_unchanged(&fresh, &retained);
    reopened.detach();
    assert_eq!(layout(&fresh, None), saved);
    assert_eq!(desktop_document(&fresh), desktop);
    drop(fresh);
    drop(client);
    session.finish();
}

#[test]
fn tui_worktree_create_open_refuse_dirty_and_in_use_then_remove_without_deleting_branch() {
    let session = CoreSession::new();
    let client = session.client();
    let repository = session.repository("ui repository");
    let binary = Path::new(env!("CARGO_BIN_EXE_herdr"));
    let mut tui = TuiPty::spawn(&session, binary);
    tui.wait_text("machines");
    tui.create_workspace(repository.to_str().unwrap());
    let initial = wait_layout(&client, None, |document| document.workspaces.len() == 1);
    let original = initial.workspaces[0].tabs[0].panes[0].terminal.clone();
    let original_pid = terminal(&client, &original)["pid"].clone();
    tui.prefix(b"G");
    tui.wait_text("new worktree");
    tui.replace_text("ui-feature");
    let created = wait_layout(&client, None, |document| document.workspaces.len() == 2);
    let checkout = created.workspaces[1].folder.clone();
    let first = created.workspaces[1].tabs[0].panes[0].terminal.clone();
    let first_pid = terminal(&client, &first)["pid"].clone();
    tui.send(b"pwd\r");
    wait_terminal_text(&client, &first, &checkout);
    tui.action("Open worktree");
    tui.wait_screen(|screen| screen.contains("open worktree") && !screen.contains("Actions"));
    tui.choose_when_ready("ui-feature");
    tui.wait_screen(|screen| !screen.contains("open worktree"));
    assert_eq!(layout(&client, None), created);
    assert_eq!(terminal(&client, &first)["pid"], first_pid);
    tui.print_marker("OPENED_EXISTING_WORKTREE");
    wait_terminal_text(&client, &first, "OPENED_EXISTING_WORKTREE");

    tui.action("Remove worktree");
    tui.wait_text("delete worktree checkout?");
    tui.send(b"\r");
    tui.wait_text("worktree is in use");
    assert!(Path::new(&checkout).is_dir());
    tui.send(b"\x1b");
    tui.wait_screen(|screen| !screen.contains("delete worktree checkout?"));
    tui.close_workspace();
    wait_layout(&client, None, |document| document.workspaces.len() == 1);
    {
        let address = &first;
        assert_eq!(terminal(&client, address)["state"], "live");
        tui.action("attach terminal");
        tui.choose_terminal(address);
        wait_layout(&client, None, |document| {
            pane_addresses(document).contains(address)
        });
        tui.print_marker("ATTACHED_WORKTREE_FOCUSED");
        wait_terminal_text(&client, address, "ATTACHED_WORKTREE_FOCUSED");
        tui.wait_text("ATTACHED_WORKTREE_FOCUSED");
        tui.action("stop terminal");
        tui.wait_text("Stop terminal?");
        tui.wait_text("Other viewers");
        tui.send(b"\x1b");
        tui.wait_screen(|screen| !screen.contains("Stop terminal?"));
        assert_eq!(
            terminal(&client, address)["state"],
            "live",
            "Escape must cancel destructive confirmation"
        );
        tui.action("stop terminal");
        tui.wait_text("Stop terminal?");
        tui.send(b"\r");
        tui.wait_screen(|screen| !screen.contains("Stop terminal?"));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let record = terminal(&client, address);
            if record["state"] == "finished" {
                break;
            }
            let screen = tui.text();
            assert!(
                Instant::now() < deadline && !screen.contains("Action failed"),
                "Stop did not finish {address}: {record}\n{screen}\n{}",
                client
                    .request("terminal.list", json!({"all":true}))
                    .unwrap()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        wait_layout(&client, None, |document| {
            !pane_addresses(document).contains(address)
        });
    }
    assert_eq!(terminal(&client, &original)["state"], "live");
    assert_eq!(terminal(&client, &original)["pid"], original_pid);
    let dirty = Path::new(&checkout).join("untracked.txt");
    std::fs::write(&dirty, "keep this data\n").unwrap();
    confirm_worktree(&mut tui, "ui-feature");
    tui.wait_text("has changes");
    assert_eq!(std::fs::read_to_string(&dirty).unwrap(), "keep this data\n");
    std::fs::remove_file(dirty).unwrap();
    confirm_worktree(&mut tui, "ui-feature");
    wait_until(|| !Path::new(&checkout).exists());
    tui.wait_screen(|screen| !screen.contains("Remove worktree?"));
    assert!(!Path::new(&checkout).exists());
    session.git(
        &repository,
        &["show-ref", "--verify", "refs/heads/ui-feature"],
    );
    tui.detach();
    drop(client);
    session.finish();
}
