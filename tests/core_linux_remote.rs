#![cfg(target_os = "macos")]

mod core_support;
#[path = "core_support/pty.rs"]
mod tui_pty;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use core_support::{wait_until, CoreSession};
use herdr_core_tui::connection::Client;
use herdr_core_tui::model::Operation;
use herdr_core_tui::persistence::{LayoutStore, DOCUMENT_NAME};
use herdr_core_tui::worktrees::{Error as WorktreeError, Worktrees};
use serde_json::{json, Value};
use tui_pty::{layout, terminal, wait_layout, wait_terminal_text, TuiPty};

struct LinuxRemote {
    local: CoreSession,
    client: Option<Client>,
    container: String,
}

fn docker(args: &[&str]) -> Output {
    Command::new("docker").args(args).output().unwrap()
}

fn successful(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "command failed: {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl LinuxRemote {
    fn new() -> Self {
        let mut fixture = Self {
            local: CoreSession::new(),
            client: None,
            container: format!(
                "herdr-linux-qualification-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
        };
        successful(docker(&[
            "run",
            "--detach",
            "--name",
            &fixture.container,
            "--env",
            "HC_SESSION=linux-qualification",
            "--env",
            "SHELL=/bin/sh",
            "--env",
            "TERM=xterm-256color",
            "ubuntu:24.04",
            "sleep",
            "infinity",
        ]));
        successful(docker(&[
            "exec", &fixture.container, "sh", "-ec",
            "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl git && curl --fail --silent --show-error --location https://releases.herdr.dev/core/install.sh --output /tmp/install-herdr-core.sh && HC_VERSION=0.1.20 HC_INSTALL_DIR=/usr/local/bin sh /tmp/install-herdr-core.sh && test \"$(herdr-core --version)\" = 'hc 0.1.20'",
        ]));
        let endpoint = String::from_utf8(successful(docker(&[
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])))
        .unwrap();
        let connector = fixture.local.root.join("linux-core-connector");
        std::fs::write(
            &connector,
            format!(
                "#!/bin/sh\nexec docker --host {} exec -i {} /usr/local/bin/herdr-core connect\n",
                quote(endpoint.trim()),
                quote(&fixture.container)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&connector, std::fs::Permissions::from_mode(0o700)).unwrap();
        fixture.client = Some(fixture.local.client());
        fixture
            .client()
            .request(
                "machine.add",
                json!({
                    "name": "linux", "via": quote(connector.to_str().unwrap()),
                    "session": "linux-qualification", "temporary": true,
                }),
            )
            .unwrap();
        wait_until(|| {
            fixture.client().request("machine.list", json!({})).unwrap()["machines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|machine| machine["name"] == "linux" && machine["state"] == "connected")
        });
        fixture
    }

    fn client(&self) -> &Client {
        self.client.as_ref().unwrap()
    }
}

impl Drop for LinuxRemote {
    fn drop(&mut self) {
        drop(self.client.take());
        if let Err(error) = self.local.shutdown() {
            eprintln!("isolated local core cleanup failed: {error}");
        }
        let output = docker(&["rm", "--force", &self.container]);
        if !output.status.success() {
            eprintln!(
                "Linux fixture cleanup failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn execute(client: &Client, argv: &[&str]) -> Vec<u8> {
    let result = client
        .request("machine.exec", json!({"on": "linux", "argv": argv}))
        .unwrap();
    let stderr = STANDARD.decode(result["stderr"].as_str().unwrap()).unwrap();
    assert_eq!(result["code"], 0, "{}", String::from_utf8_lossy(&stderr));
    assert!(result["signal"].is_null());
    STANDARD.decode(result["stdout"].as_str().unwrap()).unwrap()
}

fn desktop(client: &Client, on: Option<&str>) -> Value {
    client
        .request("doc.get", json!({"name": "herdr-app", "on": on}))
        .unwrap()
}

fn seed_desktop(client: &Client, on: Option<&str>, sentinel: &str) -> Value {
    client
        .request(
            "doc.set",
            json!({
                "name": "herdr-app", "on": on, "version": 0, "value": {"sentinel": sentinel},
            }),
        )
        .unwrap();
    desktop(client, on)
}

#[test]
#[ignore = "manual macOS-to-Linux qualification requires Docker and release download access"]
fn macos_tui_uses_linux_core_and_preserves_remote_programs_and_worktrees() {
    let fixture = LinuxRemote::new();
    let client = fixture.client();
    assert_eq!(execute(client, &["uname", "-s"]), b"Linux\n");
    assert_eq!(
        execute(client, &["herdr-core", "--version"]),
        b"hc 0.1.20\n"
    );
    let machines = client.request("machine.list", json!({})).unwrap();
    let owner = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["name"] == "linux")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let local_desktop = seed_desktop(client, None, "local-desktop");
    let remote_desktop = seed_desktop(client, Some(owner), "linux-desktop");
    let local_layout = client
        .request("doc.get", json!({"name": DOCUMENT_NAME}))
        .unwrap();
    let repository = "/tmp/remote repository";
    execute(client, &["sh", "-ec", "mkdir -p \"$1\"; git -C \"$1\" init --initial-branch=main; git -C \"$1\" -c user.name=Qualification -c user.email=qualification@example.invalid commit --allow-empty -m initial", "sh", repository]);
    let trees = Worktrees::new(client, Some(owner));
    let tree = trees.create(repository, "linux-feature").unwrap();
    assert_eq!(trees.open(repository, &tree.path).unwrap(), tree);

    let agent=client.request("terminal.run",json!({"on":owner,"argv":["/bin/sh","-c","printf '\\033]7501;state=blocked:app=codex:kind=question\\007REMOTE_AGENT_BEFORE_ATTACH\\n'; exec cat"],"cwd":tree.path,"name":"remote-agent-before-layout","no_parent":true})).unwrap()["address"].as_str().unwrap().to_owned();
    wait_until(|| terminal(client, &agent)["status"]["state"] == "blocked");
    let binary = Path::new(env!("CARGO_BIN_EXE_herdr"));
    let mut tui = TuiPty::spawn(&fixture.local, binary);
    tui.wait_screen(|screen| {
        screen.contains("machines")
            && screen.contains("linux")
            && screen.contains("remote repository")
            && screen.contains("linux-feature")
            && screen.contains("agents")
            && screen.contains("codex")
    });
    tui.capture("regular-linux-sidebar-before-attachment");
    tui.click_text("linux");
    tui.create_workspace(&tree.path);
    let saved = wait_layout(client, Some(owner), |document| {
        document.workspaces.len() == 1
    });
    assert_eq!(saved.workspaces[0].folder, tree.path);
    let local_address = &saved.workspaces[0].tabs[0].panes[0].terminal;
    assert!(!local_address.contains('/'));
    let address = format!("{owner}/{local_address}");
    tui.send(b"printf 'REMOTE_UNAME_%s\\n' \"$(uname -s)\"\r");
    wait_terminal_text(client, &address, "REMOTE_UNAME_Linux");
    tui.wait_text("REMOTE_UNAME_Linux");
    assert_eq!(terminal(client, &address)["cwd"], tree.path);
    let pid = terminal(client, &address)["pid"].as_u64().unwrap();
    assert!(layout(client, None).workspaces.is_empty());
    assert_eq!(desktop(client, None), local_desktop);
    assert_eq!(desktop(client, Some(owner)), remote_desktop);
    tui.detach();
    let view: Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .local
                .root
                .join("state/herdr-dev/core-tui-view.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(view["active_owner"], owner, "{view}");
    assert_eq!(terminal(client, &address)["state"], "live");
    assert_eq!(terminal(client, &address)["pid"].as_u64(), Some(pid));

    let mut reopened = TuiPty::spawn(&fixture.local, binary);
    reopened.wait_text("REMOTE_UNAME_Linux");
    reopened.print_marker("LINUX_PROGRAM_REOPENED");
    reopened.wait_text("LINUX_PROGRAM_REOPENED");
    wait_terminal_text(client, &address, "LINUX_PROGRAM_REOPENED");
    assert_eq!(terminal(client, &address)["pid"].as_u64(), Some(pid));
    assert_eq!(layout(client, Some(owner)), saved);
    assert!(matches!(
        trees.remove(
            repository,
            &tree.path,
            &[],
            &[saved.workspaces[0].folder.clone()]
        ),
        Err(WorktreeError::InUse(_))
    ));
    reopened.detach();
    client
        .request("terminal.kill", json!({"terminal": address}))
        .unwrap();
    wait_until(|| terminal(client, &address)["state"] == "finished");
    client
        .request("terminal.kill", json!({"terminal": agent}))
        .unwrap();
    wait_until(|| terminal(client, &agent)["state"] == "finished");
    let mut store = LayoutStore::load(client, Some(owner.into())).unwrap();
    store
        .apply(Operation::CloseWorkspace {
            workspace: saved.workspaces[0].id.clone(),
        })
        .unwrap();
    store.save(client).unwrap();
    let dirty = format!("{}/untracked.txt", tree.path);
    execute(
        client,
        &["sh", "-ec", "printf dirty > \"$1\"", "sh", &dirty],
    );
    assert!(matches!(
        trees.remove(repository, &tree.path, &[], &[]),
        Err(WorktreeError::Dirty(_))
    ));
    execute(client, &["rm", "--", &dirty]);
    trees.remove(repository, &tree.path, &[], &[]).unwrap();
    assert!(!trees
        .list(repository)
        .unwrap()
        .iter()
        .any(|entry| entry.path == tree.path));
    assert!(layout(client, None).workspaces.is_empty());
    assert_eq!(desktop(client, None), local_desktop);
    assert_eq!(desktop(client, Some(owner)), remote_desktop);
    assert_eq!(
        client
            .request("doc.get", json!({"name": DOCUMENT_NAME}))
            .unwrap(),
        local_layout
    );
    let image_bytes: Vec<u8> = (0..9 * 1024 * 1024 + 17)
        .map(|index| (index % 251) as u8)
        .collect();
    let source = fixture.local.root.join("linux-image.png");
    std::fs::write(&source, &image_bytes).unwrap();
    let local_checksum = Command::new("cksum").arg(&source).output().unwrap();
    assert!(local_checksum.status.success());
    let local_checksum = String::from_utf8(local_checksum.stdout).unwrap();
    let mut image =
        herdr_core_tui::image_staging::StagedImage::prepare(client, Some(owner), "png").unwrap();
    image
        .upload(
            client,
            &image_bytes,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    let checksum = String::from_utf8(execute(client, &["cksum", image.path()])).unwrap();
    assert_eq!(
        checksum.split_whitespace().take(2).collect::<Vec<_>>(),
        local_checksum
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
    );
    let parent = Path::new(image.path()).parent().unwrap().to_str().unwrap();
    let modes = String::from_utf8(execute(client, &["ls", "-ld", image.path(), parent])).unwrap();
    assert!(
        modes.lines().any(|line| line.starts_with("-rw-------")),
        "{modes}"
    );
    assert!(
        modes.lines().any(|line| line.starts_with("drwx------")),
        "{modes}"
    );
    image.remove(client).unwrap();
    execute(
        client,
        &[
            "sh",
            "-ec",
            "test ! -e \"$1\" && test ! -d \"$2\"",
            "image-cleanup",
            image.path(),
            parent,
        ],
    );
    eprintln!("qualified macOS TUI -> Linux core 0.1.20: owner={owner}, retained pid={pid}; shell, documents and worktree safety passed");
}
