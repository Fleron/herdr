#![cfg(unix)]

mod core_support;
#[path = "core_support/remote.rs"]
mod remote_fixture;
#[path = "core_support/pty.rs"]
mod tui_pty;

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use core_support::wait_until;
use herdr_core_tui::model::{local_terminal, Operation, Tab, Workspace};
use herdr_core_tui::persistence::LayoutStore;
use remote_fixture::{execute, RemoteFixture};
use serde_json::json;
use tui_pty::{terminal, wait_terminal_text, TuiPty};

fn focus_remote(tui: &mut TuiPty, marker: &str) {
    tui.wait_screen(|screen| {
        screen
            .lines()
            .any(|line| line.contains("box") && line.contains('●'))
    });
    tui.click_text("box");
    tui.wait_text(marker);
    tui.click_text(marker);
}

#[test]
fn local_image_file_drop_keeps_the_original_native_input_path() {
    let session = core_support::CoreSession::new();
    let client = session.client();
    let address = client.request("terminal.run", json!({
        "argv":["/bin/sh","-c","stty -echo; printf 'LOCAL_NATIVE_READY\\n'; IFS= read -r path; printf 'LOCAL_PATH:%s\\n' \"$path\"; exec sleep 60"],"no_parent":true,
    })).unwrap()["address"].as_str().unwrap().to_owned();
    let mut workspace = Workspace::new("local native", session.root.to_str().unwrap(), None);
    workspace.tabs.push(Tab::new("local image", &address));
    let mut store = LayoutStore::load(&client, None).unwrap();
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(&client).unwrap();
    let source = session.root.join("native image.png");
    std::fs::write(&source, b"local image").unwrap();
    let mut tui = TuiPty::spawn(&session, Path::new(env!("CARGO_BIN_EXE_herdr")));
    tui.wait_text("LOCAL_NATIVE_READY");
    tui.click_text("LOCAL_NATIVE_READY");
    tui.send(format!("\x1b[200~'{}'\x1b[201~\r", source.display()).as_bytes());
    wait_terminal_text(
        &client,
        &address,
        &format!("LOCAL_PATH:'{}'", source.display()),
    );
    assert!(!tui_pty::terminal_text(&client, &address).contains("herdr-clipboard-image-"));
    tui.detach();
    assert_eq!(std::fs::read(&source).unwrap(), b"local image");
    assert_eq!(terminal(&client, &address)["state"], "live");
    session.finish();
}

#[test]
fn remote_image_drop_uploads_large_binary_content_before_queued_enter_and_cleans_on_detach() {
    let fixture = RemoteFixture::new();
    let client = fixture.client();
    let machines = client.request("machine.list", json!({})).unwrap();
    let owner = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["name"] == "box")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let script = "stty -echo; printf 'IMAGE_BRIDGE_READY\\n'; while IFS= read -r path; do printf 'IMAGE_PATH:%s\\n' \"$path\"; count=$(wc -c < \"$path\" | tr -d ' '); printf 'IMAGE_BYTES:%s\\n' \"$count\"; done";
    let address = client
        .request(
            "terminal.run",
            json!({
                "on": owner, "argv":["/bin/sh","-c",script], "no_parent":true,
                "cwd": fixture.remote.root,
            }),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let local_id = local_terminal(Some("box"), &address).unwrap().to_owned();
    let address = format!("{owner}/{local_id}");
    let pid = terminal(client, &address)["pid"].clone();
    let mut store = LayoutStore::load(client, Some(owner.clone())).unwrap();
    let mut workspace = Workspace::new(
        "image remote",
        fixture.remote.root.to_str().unwrap(),
        Some(owner.clone()),
    );
    workspace.tabs.push(Tab::new("image receiver", local_id));
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(client).unwrap();
    let source = fixture.local.root.join("a dropped image.png");
    let bytes: Vec<u8> = (0..9 * 1024 * 1024 + 17)
        .map(|index| (index % 251) as u8)
        .collect();
    std::fs::write(&source, &bytes).unwrap();
    let checksum = std::process::Command::new("cksum")
        .arg(&source)
        .output()
        .unwrap();
    assert!(checksum.status.success());
    let checksum = String::from_utf8(checksum.stdout)
        .unwrap()
        .split_whitespace()
        .take(2)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut tui = TuiPty::spawn(&fixture.local, Path::new(env!("CARGO_BIN_EXE_herdr")));
    focus_remote(&mut tui, "IMAGE_BRIDGE_READY");
    tui.send(format!("\x1b[200~'{}'\x1b[201~\r", source.display()).as_bytes());
    wait_terminal_text(client, &address, &format!("IMAGE_BYTES:{}", bytes.len()));
    tui.wait_text(&format!("IMAGE_BYTES:{}", bytes.len()));
    let text = tui_pty::terminal_text(client, &address);
    let staged = text
        .lines()
        .find_map(|line| line.strip_prefix("IMAGE_PATH:"))
        .unwrap()
        .to_owned();
    assert_ne!(staged, source.to_str().unwrap());
    let actual = String::from_utf8(execute(client, Some(&owner), &["cksum", &staged])).unwrap();
    assert_eq!(
        actual.split_whitespace().take(2).collect::<Vec<_>>(),
        checksum
    );
    let parent = Path::new(&staged).parent().unwrap().to_str().unwrap();
    let modes = String::from_utf8(execute(
        client,
        Some(&owner),
        &["ls", "-ld", &staged, parent],
    ))
    .unwrap();
    assert!(
        modes.lines().any(|line| line.starts_with("-rw-------")),
        "{modes}"
    );
    assert!(
        modes.lines().any(|line| line.starts_with("drwx------")),
        "{modes}"
    );
    tui.detach();
    execute(
        client,
        Some(&owner),
        &[
            "sh",
            "-ec",
            "test ! -e \"$1\" && test ! -d \"$2\"",
            "image-cleanup",
            &staged,
            parent,
        ],
    );
    assert_eq!(terminal(client, &address)["state"], "live");
    assert_eq!(terminal(client, &address)["pid"], pid);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    fixture.finish();
}

fn slow_image_fixture() -> RemoteFixture {
    RemoteFixture::with_remote_start(|remote| {
        let bin = remote.root.join("slow-bin");
        let temporary = remote.root.join("image-tmp");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(&temporary).unwrap();
        let release = remote.root.join("release-image-write");
        assert!(std::process::Command::new("mkfifo")
            .arg(&release)
            .status()
            .unwrap()
            .success());
        let quote = |value: &Path| format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"));
        let wrapper = bin.join("cat");
        std::fs::write(&wrapper, format!(
            "#!/bin/sh\nif test \"$#\" -eq 0; then\n/bin/cat\n: > {}\nIFS= read -r released < {}\nexit 0\nfi\nexec /bin/cat \"$@\"\n",
            quote(&remote.root.join("image-write-started")), quote(&release),
        )).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = remote
            .hc()
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("TMPDIR", &temporary)
            .args(["server", "start"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    })
}

#[test]
fn a_reported_foreground_change_holds_the_image_and_typing_without_changing_the_terminal_pid() {
    let fixture = slow_image_fixture();
    let client = fixture.client();
    let machines = client.request("machine.list", json!({})).unwrap();
    let owner = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["name"] == "box")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let record = client
        .request(
            "terminal.run",
            json!({
                "on":owner,"argv":["/bin/sh"],"cwd":fixture.remote.root,"no_parent":true,
            }),
        )
        .unwrap();
    let id = local_terminal(Some("box"), record["address"].as_str().unwrap()).unwrap();
    let address = format!("{owner}/{id}");
    let pid = record["pid"].as_u64().unwrap();
    let desktop = client
        .attach(&herdr_core_tui::connection::AttachOptions::new(
            &address, 93, 39,
        ))
        .unwrap();
    wait_until(|| {
        terminal(client, &address)
            .pointer("/foreground/pid")
            .and_then(serde_json::Value::as_u64)
            == Some(pid)
    });
    desktop
        .send_data(b"printf 'FOREGROUND_IMAGE_READY\\n'\r")
        .unwrap();
    let mut workspace = Workspace::new(
        "foreground image",
        fixture.remote.root.to_str().unwrap(),
        Some(owner.clone()),
    );
    workspace.tabs.push(Tab::new("foreground receiver", id));
    let mut store = LayoutStore::load(client, Some(owner)).unwrap();
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(client).unwrap();
    let source = fixture.local.root.join("foreground.png");
    std::fs::write(&source, vec![3; 32 * 1024]).unwrap();
    let mut tui = TuiPty::spawn(&fixture.local, Path::new(env!("CARGO_BIN_EXE_herdr")));
    focus_remote(&mut tui, "FOREGROUND_IMAGE_READY");
    tui.send(
        format!(
            "\x1b[200~{}\x1b[201~\rQUEUED_OLD_FOREGROUND\r",
            source.display()
        )
        .as_bytes(),
    );
    wait_until(|| fixture.remote.root.join("image-write-started").exists());
    let temporary = fixture.remote.root.join("image-tmp");
    let stages = std::fs::read_dir(&temporary)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("herdr-clipboard-image-")
        })
        .collect::<Vec<_>>();
    assert_eq!(stages.len(), 1);
    desktop.send_data(b"sleep 60\r").unwrap();
    wait_until(|| {
        terminal(client, &address)
            .pointer("/foreground/pid")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|foreground| foreground != pid)
    });
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fixture.remote.root.join("release-image-write"))
        .unwrap()
        .write_all(b"released\n")
        .unwrap();
    wait_until(|| !stages[0].exists());
    let text = tui_pty::terminal_text(client, &address);
    assert!(!text.contains("herdr-clipboard-image-"), "{text}");
    assert!(!text.contains("QUEUED_OLD_FOREGROUND"), "{text}");
    tui.action("discard held typing");
    tui.wait_text("Discard held typing?");
    tui.send(b"\r");
    tui.wait_screen(|screen| !screen.contains("Discard held typing?"));
    tui.detach();
    assert_eq!(terminal(client, &address)["state"], "live");
    assert_eq!(terminal(client, &address)["pid"].as_u64(), Some(pid));
    drop(desktop);
    fixture.finish();
}

#[test]
fn delayed_image_upload_keeps_other_terminal_input_live_and_detaches_with_bounded_cleanup() {
    let fixture = slow_image_fixture();
    let client = fixture.client();
    let machines = client.request("machine.list", json!({})).unwrap();
    let owner = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["name"] == "box")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let seed = |on: Option<&str>, folder: &Path, label: &str| {
        let script = format!("stty -echo; printf '{label}_READY\\n'; while IFS= read -r line; do printf '{label}_INPUT:%s\\n' \"$line\"; done");
        let record = client
            .request(
                "terminal.run",
                json!({"on":on,"argv":["/bin/sh","-c",script],"cwd":folder,"no_parent":true}),
            )
            .unwrap();
        let raw = record["address"].as_str().unwrap();
        let id = local_terminal(on.map(|_| "box"), raw).unwrap();
        let address = on.map_or_else(|| id.to_owned(), |on| format!("{on}/{id}"));
        let mut workspace = Workspace::new(label, folder.to_str().unwrap(), on.map(str::to_owned));
        workspace.tabs.push(Tab::new(label, id));
        let mut store = LayoutStore::load(client, on.map(str::to_owned)).unwrap();
        store.apply(Operation::AddWorkspace(workspace)).unwrap();
        store.save(client).unwrap();
        address
    };
    let local = seed(None, &fixture.local.root, "LOCAL_IMAGE");
    let remote = seed(Some(&owner), &fixture.remote.root, "REMOTE_IMAGE");
    let local_pid = terminal(client, &local)["pid"].clone();
    let remote_pid = terminal(client, &remote)["pid"].clone();
    let source = fixture.local.root.join("delayed.png");
    std::fs::write(&source, vec![7; 32 * 1024]).unwrap();
    let mut tui = TuiPty::spawn(&fixture.local, Path::new(env!("CARGO_BIN_EXE_herdr")));
    focus_remote(&mut tui, "REMOTE_IMAGE_READY");
    tui.send(
        format!(
            "\x1b[200~{}\x1b[201~\rQUEUED_IMAGE_INPUT\r",
            source.display()
        )
        .as_bytes(),
    );
    wait_until(|| fixture.remote.root.join("image-write-started").exists());
    let temporary = fixture.remote.root.join("image-tmp");
    let stages = std::fs::read_dir(&temporary)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("herdr-clipboard-image-")
        })
        .collect::<Vec<_>>();
    assert_eq!(stages.len(), 1);
    let staged = stages[0].join("image.png");
    assert_eq!(std::fs::metadata(&staged).unwrap().len(), 32 * 1024);
    tui.action("Help");
    tui.wait_text("keybinds");
    tui.send(b"\x1b");
    tui.wait_screen(|screen| !screen.contains("keybinds"));
    tui.click_text("Local");
    tui.wait_text("LOCAL_IMAGE_READY");
    tui.click_text("LOCAL_IMAGE_READY");
    tui.send(b"LOCAL_WHILE_IMAGE_PENDING\r");
    wait_terminal_text(
        client,
        &local,
        "LOCAL_IMAGE_INPUT:LOCAL_WHILE_IMAGE_PENDING",
    );
    tui.action("discard held typing");
    tui.wait_text("Discard held typing?");
    tui.send(b"\r");
    tui.wait_screen(|screen| !screen.contains("Discard held typing?"));
    let started = Instant::now();
    tui.prefix(b"q");
    let status = tui.wait_for_exit();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "detach waited for the blocked owner command"
    );
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(mut release) = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fixture.remote.root.join("release-image-write"))
    {
        let _ = release.write_all(b"released\n");
    }
    assert!(
        status.success(),
        "{}",
        String::from_utf8_lossy(&tui.output_bytes())
    );
    assert!(!staged.exists());
    assert!(!stages[0].exists());
    assert_eq!(terminal(client, &local)["pid"], local_pid);
    assert_eq!(terminal(client, &remote)["pid"], remote_pid);
    assert_eq!(terminal(client, &local)["state"], "live");
    assert_eq!(terminal(client, &remote)["state"], "live");
    assert!(!tui_pty::terminal_text(client, &remote).contains("QUEUED_IMAGE_INPUT"));
    fixture.finish();
}

#[test]
fn unreachable_image_owner_reports_its_owned_path_and_keeps_the_program_running() {
    let fixture = RemoteFixture::new();
    let client = fixture.client();
    let machines = client.request("machine.list", json!({})).unwrap();
    let owner = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["name"] == "box")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let record = client.request("terminal.run", json!({
        "on":owner,"argv":["/bin/sh","-c","stty -echo; printf 'UNREACHABLE_IMAGE_READY\\n'; IFS= read -r path; printf 'IMAGE_PATH:%s\\n' \"$path\"; exec sleep 60"],
        "no_parent":true,"cwd":fixture.remote.root,
    })).unwrap();
    let id = local_terminal(Some("box"), record["address"].as_str().unwrap()).unwrap();
    let address = format!("{owner}/{id}");
    let pid = record["pid"].clone();
    let mut workspace = Workspace::new(
        "image cleanup",
        fixture.remote.root.to_str().unwrap(),
        Some(owner.clone()),
    );
    workspace.tabs.push(Tab::new("image receiver", id));
    let mut store = LayoutStore::load(client, Some(owner.clone())).unwrap();
    store.apply(Operation::AddWorkspace(workspace)).unwrap();
    store.save(client).unwrap();
    let source = fixture.local.root.join("cleanup.png");
    std::fs::write(&source, b"fixture image").unwrap();
    let mut tui = TuiPty::spawn(&fixture.local, Path::new(env!("CARGO_BIN_EXE_herdr")));
    focus_remote(&mut tui, "UNREACHABLE_IMAGE_READY");
    tui.send(format!("\x1b[200~{}\x1b[201~\r", source.display()).as_bytes());
    wait_terminal_text(client, &address, "IMAGE_PATH:");
    let text = tui_pty::terminal_text(client, &address);
    let staged = text
        .lines()
        .find_map(|line| line.strip_prefix("IMAGE_PATH:"))
        .unwrap()
        .to_owned();
    assert!(Path::new(&staged).exists());
    client
        .request("machine.rm", json!({"machine":"box"}))
        .unwrap();
    tui.prefix(b"q");
    let status = tui.wait_for_exit();
    assert!(
        !status.success(),
        "detach must report a failed private-file cleanup"
    );
    wait_until(|| String::from_utf8_lossy(&tui.output_bytes()).contains(&staged));
    let output = String::from_utf8_lossy(&tui.output_bytes()).into_owned();
    assert!(output.contains("cleanup"), "{output}");
    assert!(output.contains(&staged), "{output}");
    assert_eq!(std::fs::read(&staged).unwrap(), b"fixture image");
    let direct = fixture.remote.client();
    assert_eq!(terminal(&direct, id)["state"], "live");
    assert_eq!(terminal(&direct, id)["pid"], pid);
    let directory = Path::new(&staged).parent().unwrap();
    let token = std::fs::read_to_string(directory.join(".owner")).unwrap();
    assert_eq!(
        directory.file_name().unwrap().to_str().unwrap(),
        format!("herdr-clipboard-image-{token}")
    );
    std::fs::remove_file(&staged).unwrap();
    std::fs::remove_file(directory.join(".owner")).unwrap();
    std::fs::remove_dir(directory).unwrap();
    fixture.finish();
}
