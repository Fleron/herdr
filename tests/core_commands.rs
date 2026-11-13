#![cfg(unix)]

mod core_support;
#[path = "core_support/pty.rs"]
mod tui_pty;

use std::path::Path;

use core_support::{wait_until, CoreSession};
use herdr_core_tui::model::{Operation, Tab, Workspace};
use herdr_core_tui::persistence::LayoutStore;
use serde_json::json;
use tui_pty::{terminal_text, wait_layout, TuiPty};

#[test]
fn configured_shell_and_overlay_commands_use_the_captured_core_pane_and_restore_focus() {
    let session = CoreSession::new();
    let client = session.client();
    let folder = session.root.join("command cwd");
    std::fs::create_dir(&folder).unwrap();
    let address = client
        .request(
            "terminal.run",
            json!({"argv":["/bin/sh","-c","printf '\\033]0;SOURCE_TITLE\\007SOURCE_READY'; exec cat"],"cwd":folder,"no_parent":true}),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut workspace = Workspace::new("command-workspace", session.root.to_str().unwrap(), None);
    workspace.tabs.push(Tab::new("command-tab", &address));
    let pane_id = workspace.tabs[0].panes[0].id.clone();
    let mut store = LayoutStore::load(&client, None).unwrap();
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(&client).unwrap();
    let saved = store.document().clone();
    let config = session.root.join("commands.toml");
    std::fs::write(
        &config,
        r#"onboarding = false
[[keys.command]]
key = "f8"
type = "shell"
command = "printf '%s\\n%s\\n' \"$PWD\" \"$HERDR_ACTIVE_PANE_ID\" > shell-result.txt"
[[keys.command]]
key = "f9"
type = "pane"
command = "printf OVERLAY_READY; IFS= read -r line; printf '%s' \"$line\" > overlay-result.txt"
[ui]
window_title = "{workspace}/{tab}/{terminal_title}"
"#,
    )
    .unwrap();
    let mut tui = TuiPty::spawn_with_env(
        &session,
        Path::new(env!("CARGO_BIN_EXE_herdr")),
        &[("HERDR_CONFIG_PATH", config.clone().into_os_string())],
    );
    tui.wait_text("SOURCE_READY");
    let expected_title = b"\x1b]0;command-workspace/command-tab/SOURCE_TITLE\x07";
    wait_until(|| {
        tui.output_bytes()
            .windows(expected_title.len())
            .any(|part| part == expected_title)
    });
    tui.send(b"\x1b[19~");
    let result = folder.join("shell-result.txt");
    wait_until(|| result.exists());
    assert_eq!(
        std::fs::read_to_string(&result).unwrap(),
        format!("{}\n{pane_id}\n", folder.canonicalize().unwrap().display())
    );
    assert_eq!(tui_pty::layout(&client, None), saved);
    tui.send(b"\x1b[20~");
    let overlay = wait_layout(&client, None, |document| {
        document.workspaces[0].tabs[0].panes.len() == 2
    });
    assert!(overlay.workspaces[0].tabs[0]
        .panes
        .iter()
        .any(|pane| pane.id == pane_id));
    tui.wait_text("OVERLAY_READY");
    tui.send(b"overlay-finished\r");
    let result = folder.join("overlay-result.txt");
    wait_until(|| result.exists());
    assert_eq!(std::fs::read_to_string(result).unwrap(), "overlay-finished");
    wait_layout(&client, None, |document| document == &saved);
    tui.wait_text("SOURCE_READY");
    tui.send(b"SOURCE_FOCUS_RESTORED\r");
    wait_until(|| terminal_text(&client, &address).contains("SOURCE_FOCUS_RESTORED"));
    let mut updated: toml::Value =
        toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    let ui = updated["ui"].as_table_mut().unwrap();
    ui.insert("mouse_capture".into(), toml::Value::Boolean(false));
    ui.insert(
        "window_title".into(),
        toml::Value::String("RELOADED_CORE_TITLE".into()),
    );
    std::fs::write(&config, toml::to_string(&updated).unwrap()).unwrap();
    tui.prefix(b"R");
    let expected_title = b"\x1b]0;RELOADED_CORE_TITLE\x07";
    wait_until(|| {
        tui.output_bytes()
            .windows(expected_title.len())
            .any(|part| part == expected_title)
    });
    assert!(tui
        .output_bytes()
        .windows(b"\x1b[?1000l".len())
        .any(|part| part == b"\x1b[?1000l"));
    tui.detach();
    session.finish();
}
