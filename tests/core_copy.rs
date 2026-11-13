#![cfg(unix)]

mod core_support;
#[path = "core_support/pty.rs"]
mod tui_pty;

use std::path::Path;

use core_support::CoreSession;
use herdr_core_tui::connection::Client;
use herdr_core_tui::model::{Operation, Tab, Workspace};
use herdr_core_tui::persistence::LayoutStore;
use serde_json::json;
use tui_pty::{terminal_text, TuiPty};

fn seed_program(session: &CoreSession, client: &Client, script: &str) -> String {
    let address = client
        .request(
            "terminal.run",
            json!({
                "argv": ["/bin/sh", "-c", script], "no_parent": true,
                "size": {"cols": 96, "rows": 38}
            }),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut workspace = Workspace::new("copy-workspace", session.root.to_str().unwrap(), None);
    workspace.tabs.push(Tab::new("copy-terminal", &address));
    let mut store = LayoutStore::load(client, None).unwrap();
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(client).unwrap();
    address
}

fn spawn_tui(session: &CoreSession) -> TuiPty {
    let config = session.root.join("copy-test.toml");
    std::fs::write(&config, "onboarding = false\n").unwrap();
    TuiPty::spawn_with_env(
        session,
        Path::new(env!("CARGO_BIN_EXE_herdr")),
        &[("HERDR_CONFIG_PATH", config.into_os_string())],
    )
}

#[test]
fn regular_copy_mode_copies_a_unicode_word_through_the_real_core_terminal() {
    let session = CoreSession::new();
    let client = session.client();
    let address = seed_program(
        &session,
        &client,
        "stty -echo; printf 'COPY_BEGIN\\r\\nalpha界 beta.gamma\\r\\nCOPY_END\\r\\n'; \
         while IFS= read -r line; do printf 'TUI_INPUT:%s\\r\\n' \"$line\"; done",
    );
    let mut tui = spawn_tui(&session);
    tui.wait_text("COPY_END");
    tui.prefix(b"[");
    tui.send(b"gj0vey");
    tui.wait_clipboard("alpha界".as_bytes());
    assert!(!terminal_text(&client, &address).contains("TUI_INPUT:"));
    tui.detach();
    session.finish();
}

#[test]
fn regular_copy_search_reaches_history_beyond_the_returned_match_window() {
    let session = CoreSession::new();
    let client = session.client();
    let address = seed_program(
        &session,
        &client,
        "stty -echo; i=0; while [ \"$i\" -lt 1105 ]; do \
            printf 'needle %s\\r\\n' \"$i\"; i=$((i+1)); \
         done; printf 'SEARCH_READY\\r\\n'; \
         while IFS= read -r line; do printf 'TUI_INPUT:%s\\r\\n' \"$line\"; done",
    );
    let mut tui = spawn_tui(&session);
    tui.wait_text("SEARCH_READY");
    tui.prefix(b"[");
    tui.send(b"G?needle\r");
    tui.wait_text("1105/1105");
    tui.send(b"y");
    tui.wait_clipboard(b"needle");
    assert!(!terminal_text(&client, &address).contains("TUI_INPUT:"));
    tui.detach();
    session.finish();
}
