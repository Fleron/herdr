// Shared by integration binaries that exercise different core operations.
#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use herdr_core_tui::connection::Client;

pub struct CoreSession {
    pub root: PathBuf,
    pub binary: PathBuf,
    pub session: String,
    closed: bool,
}

impl CoreSession {
    pub fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let binary = core_binary();
        let (root, id) = loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = PathBuf::from(format!("/tmp/hctui-{}-{id}", std::process::id()));
            match std::fs::create_dir(&root) {
                Ok(()) => break (root, id),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create isolated core home: {error}"),
            }
        };
        for directory in ["run", "state", "config"] {
            std::fs::create_dir(root.join(directory)).unwrap();
        }
        Self {
            session: format!("tui-{}-{id}", std::process::id()),
            root,
            binary,
            closed: false,
        }
    }

    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("HC_SESSION", &self.session)
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("HC_BIN", &self.binary)
            .env("HERDR_CORE_BIN", &self.binary)
            .current_dir(&self.root);
        command
    }

    pub fn hc(&self) -> Command {
        self.command(&self.binary)
    }

    pub fn client(&self) -> Client {
        Client::connect_command(&mut self.hc()).expect("connect to isolated real herdr-core")
    }

    pub fn socket(&self) -> PathBuf {
        PathBuf::from(
            std::fs::read_to_string(
                self.root
                    .join("state/herdr-core")
                    .join(&self.session)
                    .join("core.lock"),
            )
            .unwrap(),
        )
    }

    pub fn repository(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir(&path).unwrap();
        self.git(&path, &["init", "--initial-branch=main"]);
        std::fs::write(path.join("tracked.txt"), "fixture\n").unwrap();
        self.git(&path, &["add", "tracked.txt"]);
        self.git(
            &path,
            &[
                "-c",
                "user.name=Core TUI Test",
                "-c",
                "user.email=core-tui@example.invalid",
                "commit",
                "-m",
                "initial fixture",
            ],
        );
        std::fs::canonicalize(path).unwrap()
    }

    pub fn git(&self, cwd: &std::path::Path, arguments: &[&str]) -> Output {
        let output = self
            .command("git")
            .current_dir(cwd)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub fn successful(&self, arguments: &[&str]) -> Output {
        let output = self
            .hc()
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated hc {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn stop(&self) -> std::io::Result<()> {
        let _ = self
            .hc()
            .args(["server", "status"])
            .stdin(Stdio::null())
            .output()?;
        let output = self
            .hc()
            .args(["server", "stop"])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "cannot stop isolated core {}: {}",
                self.root.display(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    pub fn shutdown(&mut self) -> std::io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.stop()?;
        std::fs::remove_dir_all(&self.root)?;
        self.closed = true;
        Ok(())
    }

    pub fn finish(mut self) {
        self.shutdown()
            .expect("stop only the fixture's isolated core");
    }
}

impl Drop for CoreSession {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        match self.stop() {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&self.root);
            }
            Err(error) => eprintln!("isolated core cleanup failed: {error}"),
        }
    }
}

fn core_binary() -> PathBuf {
    let binary = std::env::var_os("HERDR_CORE_BIN").unwrap_or_else(|| OsString::from("hc"));
    let path = PathBuf::from(&binary);
    if path.is_absolute() && path.is_file() {
        return path;
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(&binary))
        .find(|path| path.is_file())
        .expect(
            "install current herdr-core or set HERDR_CORE_BIN before real-core integration tests",
        )
}

pub fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if condition() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "condition did not become true within 10 seconds"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
