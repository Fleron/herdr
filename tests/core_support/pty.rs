// Shared scenarios use different subsets of the real PTY controls.
#![allow(dead_code)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};

use herdr_core_tui::connection::Client;
use herdr_core_tui::model::Document;
use herdr_core_tui::persistence::DOCUMENT_NAME;

use super::core_support::CoreSession;

#[path = "../support/terminal_screen.rs"]
mod terminal_screen;

pub struct TuiPty {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    writer: Option<Box<dyn Write + Send>>,
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    cols: u16,
    rows: u16,
    resizes: Vec<(usize, u16, u16)>,
    config_path: PathBuf,
}

impl TuiPty {
    pub fn spawn(session: &CoreSession, binary: &Path) -> Self {
        Self::spawn_with_env(session, binary, &[])
    }

    pub fn spawn_with_env(
        session: &CoreSession,
        binary: &Path,
        extra: &[(&str, OsString)],
    ) -> Self {
        let config = session.root.join("tui-test-config.toml");
        std::fs::write(
            &config,
            "onboarding = false\n[keys]\nopen_worktree = \"f10\"\nremove_worktree = \"f11\"\n[ui]\nprompt_new_tab_name = false\n",
        ).unwrap();
        let cols = 120;
        let rows = 40;
        let pair = native_pty_system()
            .openpty(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(binary);
        command.env_clear();
        for (key, value) in session.command(binary).get_envs() {
            if let Some(value) = value {
                command.env(key, value);
            }
        }
        command.env("HERDR_CONFIG_PATH", &config);
        command.env("SSH_CONNECTION", "herdr-pty-test");
        for (key, value) in extra {
            command.env(key, value);
        }
        command.cwd(&session.root);
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut input = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = output.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match input.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => captured.lock().unwrap().extend_from_slice(&bytes[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            master: Some(pair.master),
            child,
            writer: Some(writer),
            output,
            reader: Some(reader),
            cols,
            rows,
            resizes: vec![(0, cols, rows)],
            config_path: config,
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let writer = self.writer.as_mut().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    pub fn prefix(&mut self, action: &[u8]) {
        self.send(b"\x02");
        self.send(action);
    }

    pub fn text(&self) -> String {
        let bytes = self.output.lock().unwrap().clone();
        let (_, cols, rows) = self.resizes[0];
        terminal_screen::text_with_resizes(&bytes, cols, rows, &self.resizes[1..])
    }

    pub fn output_bytes(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    pub fn wait_for_exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "TUI did not exit:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn clipboard_writes(&self) -> Vec<Vec<u8>> {
        let bytes = self.output.lock().unwrap().clone();
        let mut terminal = herdr_core_tui::ghostty::Terminal::new(self.cols, self.rows, 0).unwrap();
        terminal.write(&bytes);
        terminal.take_clipboard_writes()
    }

    pub fn wait_clipboard(&mut self, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self
                .clipboard_writes()
                .iter()
                .any(|bytes| bytes == expected)
            {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "TUI exited before copying text: {status:?}\n{}",
                    self.text()
                );
            }
            assert!(
                Instant::now() < deadline,
                "TUI did not copy expected text; clipboard={:?}\n{}",
                self.clipboard_writes(),
                self.text(),
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_screen(&mut self, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let screen = self.text();
            if predicate(&screen) {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("TUI exited before expected screen: {status:?}\n{screen}");
            }
            assert!(
                Instant::now() < deadline,
                "TUI did not show expected screen:\n{screen}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_text(&mut self, expected: &str) {
        eprintln!("waiting for TUI text {expected:?}");
        self.wait_screen(|screen| screen.contains(expected));
    }

    pub fn replace_text(&mut self, value: &str) {
        self.send(b"\x15");
        self.send(value.as_bytes());
        self.send(b"\r");
    }

    pub fn create_workspace(&mut self, folder: &str) {
        let mut config: toml::Value =
            toml::from_str(&std::fs::read_to_string(&self.config_path).unwrap()).unwrap();
        config.as_table_mut().unwrap().insert(
            "terminal".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "new_cwd".into(),
                toml::Value::String(folder.into()),
            )])),
        );
        std::fs::write(&self.config_path, toml::to_string(&config).unwrap()).unwrap();
        self.send(b"\x02R\x02N");
    }

    pub fn action(&mut self, query: &str) {
        match query {
            "Help" => self.prefix(b"?"),
            "Copy mode" => self.prefix(b"["),
            "Open worktree" => self.send(b"\x1b[21~"),
            "Remove worktree" => self.send(b"\x1b[23~"),
            _ => {
                self.click_text("menu");
                self.wait_text("retry connections");
                self.click_menu_item(&query.to_lowercase());
            }
        }
    }

    pub fn click_text(&mut self, label: &str) {
        self.mouse_text(label, 0);
    }

    fn click_menu_item(&mut self, label: &str) {
        self.wait_screen(|screen| {
            screen
                .lines()
                .any(|line| line.split('│').any(|part| part.trim() == label))
        });
        let screen = self.text();
        let (row, column) = screen
            .lines()
            .enumerate()
            .find_map(|(row, line)| {
                let mut offset = 0;
                for part in line.split('│') {
                    if part.trim() == label {
                        let start = offset + part.find(label)?;
                        return Some((row, unicode_width::UnicodeWidthStr::width(&line[..start])));
                    }
                    offset += part.len() + '│'.len_utf8();
                }
                None
            })
            .unwrap();
        self.mouse_at(row, column, 0);
    }

    pub fn right_click_text(&mut self, label: &str) {
        self.mouse_text(label, 2);
    }

    pub fn right_click_sidebar_text(&mut self, label: &str) {
        self.wait_text(label);
        let screen = self.text();
        let (row, prefix) = screen
            .lines()
            .enumerate()
            .find_map(|(row, line)| {
                let (sidebar, _) = line.split_once('│')?;
                sidebar.find(label).map(|byte| (row, &sidebar[..byte]))
            })
            .unwrap();
        let col = unicode_width::UnicodeWidthStr::width(prefix);
        self.mouse_at(row, col, 2);
    }

    fn mouse_text(&mut self, label: &str, button: u8) {
        self.wait_text(label);
        let screen = self.text();
        let (row, line, byte) = screen
            .lines()
            .enumerate()
            .find_map(|(row, line)| line.find(label).map(|byte| (row, line, byte)))
            .unwrap();
        let col = unicode_width::UnicodeWidthStr::width(&line[..byte]);
        self.mouse_at(row, col, button);
    }

    fn mouse_at(&mut self, row: usize, col: usize, button: u8) {
        self.send(
            format!(
                "\x1b[<{button};{};{}M\x1b[<{button};{};{}m",
                col + 1,
                row + 1,
                col + 1,
                row + 1
            )
            .as_bytes(),
        );
    }

    pub fn capture(&self, name: &str) {
        let root = Path::new("/private/tmp/herdr-core-tui-build");
        std::fs::write(root.join(format!("{name}.txt")), self.text()).unwrap();
        std::fs::write(
            root.join(format!("{name}.ansi")),
            self.output.lock().unwrap().clone(),
        )
        .unwrap();
    }

    pub fn choose_when_ready(&mut self, query: &str) {
        self.wait_text(query);
        self.send(b"/");
        self.send(query.as_bytes());
        self.send(b"\r");
    }

    pub fn choose_terminal(&mut self, query: &str) {
        self.wait_text(query);
        self.send(query.as_bytes());
        self.send(b"\r");
    }

    pub fn close_workspace(&mut self) {
        self.prefix(b"D");
        self.wait_text("Close workspace?");
        self.send(b"\r");
    }

    pub fn print_marker(&mut self, marker: &str) {
        let encoded: String = marker.bytes().map(|byte| format!("\\{byte:03o}")).collect();
        self.send(format!("printf '{encoded}\\n'\r").as_bytes());
    }

    pub fn prefix_and_print(&mut self, action: u8, marker: &str) {
        let encoded: String = marker.bytes().map(|byte| format!("\\{byte:03o}")).collect();
        self.send(format!("\x02{}printf '{encoded}\\n'\r", action as char).as_bytes());
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let output = self.output.lock().unwrap();
        self.master
            .as_ref()
            .unwrap()
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        self.resizes.push((output.len(), cols, rows));
        self.cols = cols;
        self.rows = rows;
    }

    pub fn detach(mut self) {
        self.prefix(b"q");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut confirmed = false;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "TUI detach failed: {status:?}\n{}",
                    self.text()
                );
                break;
            }
            if !confirmed && self.text().contains("Detach with unfinished changes?") {
                self.send(b"\r");
                confirmed = true;
            }
            assert!(
                Instant::now() < deadline,
                "TUI did not detach:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TuiPty {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        drop(self.writer.take());
        drop(self.master.take());
        if self.reader.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}

pub fn layout(client: &Client, owner: Option<&str>) -> Document {
    let reply = client
        .request("doc.get", json!({"name": DOCUMENT_NAME, "on": owner}))
        .unwrap();
    let value = reply.get("value").expect("doc.get value");
    if value.is_null() {
        Document::default()
    } else {
        serde_json::from_value(value.clone()).unwrap()
    }
}

pub fn wait_layout(
    client: &Client,
    owner: Option<&str>,
    predicate: impl Fn(&Document) -> bool,
) -> Document {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let document = layout(client, owner);
        if predicate(&document) {
            document.validate().unwrap();
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "layout was not saved as expected: {document:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn terminal(client: &Client, address: &str) -> Value {
    client
        .request("terminal.get", json!({"terminal": address}))
        .unwrap()
}

pub fn terminal_text(client: &Client, address: &str) -> String {
    client
        .request("terminal.read", json!({"terminal": address}))
        .unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

pub fn wait_terminal_text(client: &Client, address: &str, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = terminal_text(client, address);
        if text.contains(marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{address} did not execute marker {marker:?}: {text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
