//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

#[cfg(unix)]
#[path = "clipboard_command.rs"]
mod clipboard_command;
#[cfg(unix)]
pub use clipboard_command::ClipboardCommand;
#[path = "clipboard_image.rs"]
mod clipboard_image;
pub use clipboard_image::ClipboardImage;
#[path = "limited_read.rs"]
mod limited_read;
pub(crate) use limited_read::{read_limited_reader, LimitedRead};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Native adapters construct only the event classifications their OS can report.
#[allow(dead_code)]
pub(crate) enum HostPowerEvent {
    Suspend,
    AutomaticResume,
    InteractiveResume,
    UnclassifiedResume,
}

pub(crate) struct HostPowerMonitor {
    events: tokio::sync::mpsc::UnboundedReceiver<HostPowerEvent>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HostPowerMonitor {
    pub(crate) fn start(enabled: bool) -> Self {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let task = if enabled {
            monitor_host_power(sender)
        } else {
            None
        };
        Self { events, task }
    }

    pub(crate) fn try_recv(&mut self) -> Option<HostPowerEvent> {
        self.events.try_recv().ok()
    }

    pub(crate) async fn recv(&mut self) -> HostPowerEvent {
        match self.events.recv().await {
            Some(event) => event,
            None => std::future::pending().await,
        }
    }
}

impl Drop for HostPowerMonitor {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn monitor_host_power(
    _sender: tokio::sync::mpsc::UnboundedSender<HostPowerEvent>,
) -> Option<tokio::task::JoinHandle<()>> {
    None
}

pub(crate) struct HostShutdownMonitor {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HostShutdownMonitor {
    pub(crate) fn start(
        requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
        intent: HostShutdownIntentCell,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let task = monitor_host_shutdown(requested, intent, wake);
        Self { task }
    }
}

impl Drop for HostShutdownMonitor {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A restart or shutdown the host announced but has not committed to yet.
///
/// Live handoff carries it so a replacement server still recognizes the
/// commit point. The value is captured when the old server builds its handoff
/// manifest; announcements or cancellations that arrive after that and before
/// the replacement starts its monitor reach only the old server. Each handoff
/// phase may take up to 30 seconds, so that window can last tens of seconds.
/// Only macOS reports an intent; the timestamp is in its boot-wide monotonic
/// clock, which is shared by the old and new server processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HostShutdownIntent {
    announced_at_ns: u64,
}

/// The pending host shutdown intent shared by the server and its monitor.
// Windows has no live handoff and no announcement to record, so nothing reads it there.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Default)]
pub(crate) struct HostShutdownIntentCell(
    std::sync::Arc<std::sync::Mutex<Option<HostShutdownIntent>>>,
);

// Windows has no live handoff and no announcement to record, so nothing reads it there.
#[cfg_attr(windows, allow(dead_code))]
impl HostShutdownIntentCell {
    pub(crate) fn get(&self) -> Option<HostShutdownIntent> {
        *self.lock()
    }

    pub(crate) fn set(&self, intent: Option<HostShutdownIntent>) {
        *self.lock() = intent;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<HostShutdownIntent>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn monitor_host_shutdown(
    _requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _intent: HostShutdownIntentCell,
    _wake: impl Fn() + Send + Sync + 'static,
) -> Option<tokio::task::JoinHandle<()>> {
    None
}

#[cfg(not(windows))]
pub(crate) fn host_shutdown_in_progress() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundJob {
    pub process_group_id: u32,
    pub processes: Vec<ForegroundProcess>,
}

/// A request from outside the process to stop the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerQuitSignal {
    #[cfg(unix)]
    Interrupt,
    #[cfg(unix)]
    Terminate,
    #[cfg(not(unix))]
    ConsoleControl,
}

impl std::fmt::Display for ServerQuitSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(unix)]
            Self::Interrupt => "SIGINT",
            #[cfg(unix)]
            Self::Terminate => "SIGTERM",
            #[cfg(not(unix))]
            Self::ConsoleControl => "console control event",
        })
    }
}

#[cfg(not(unix))]
pub(crate) fn spawn_server_signal_monitor(
    on_quit: impl Fn(ServerQuitSignal) + Send + Sync + 'static,
) {
    if let Err(err) = ctrlc::set_handler(move || on_quit(ServerQuitSignal::ConsoleControl)) {
        tracing::warn!(%err, "failed to install server stop handler");
    }
}

#[cfg(not(unix))]
pub(crate) fn ignore_server_hangup() {}

#[cfg(not(unix))]
pub(crate) fn local_stream_peer_description(_stream: &crate::ipc::LocalStream) -> Option<String> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hangup,
    Terminate,
    Kill,
}

/// Why a pane runtime ended, before application persistence policy is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExitReason {
    Exited,
    Interrupted,
    /// Imported runtimes have no child wait handle in the replacement server.
    #[cfg(unix)]
    Handoff,
    WaitFailed,
}

impl ChildExitReason {
    pub(crate) fn requires_session_checkpoint(self) -> bool {
        match self {
            Self::Interrupted => true,
            #[cfg(unix)]
            Self::Handoff => true,
            _ => false,
        }
    }
}

#[cfg(unix)]
pub(crate) use unix_common::{
    classify_child_exit, poll_fd_readable, read_fd, shared_ssh_control_path,
};

// Tests inject their own probe because the recorded hangup is process-global.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn server_hangup_is_recent() -> bool {
    #[cfg(unix)]
    {
        unix_common::server_hangup_is_recent()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn classify_child_exit(_status: &portable_pty::ExitStatus) -> ChildExitReason {
    ChildExitReason::Exited
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn launch_executable() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}

pub(crate) fn detached_custom_command_process(command: &str) -> std::process::Command {
    let mut process = detached_custom_command_process_platform(command);
    configure_background_command(&mut process);
    process
}

pub(crate) fn pane_custom_command_pty_builder(command: &str) -> portable_pty::CommandBuilder {
    pane_custom_command_pty_builder_platform(command)
}

pub(crate) fn apply_pane_runtime_marker(command: &mut portable_pty::CommandBuilder) {
    apply_pane_runtime_marker_platform(command);
}

pub(crate) fn prepare_paste_text_for_pty(text: String) -> String {
    prepare_paste_text_for_pty_platform(text)
}

pub(crate) fn plugin_runtime_path(path: &std::path::Path) -> std::path::PathBuf {
    plugin_runtime_path_platform(path)
}

pub(crate) fn normalize_cwd_for_launch(path: &std::path::Path) -> std::path::PathBuf {
    normalize_cwd_for_launch_platform(path)
}

#[cfg(not(windows))]
fn normalize_cwd_for_launch_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn plugin_runtime_path_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn prepare_paste_text_for_pty_platform(text: String) -> String {
    text
}

#[cfg(not(windows))]
#[path = "unix_client.rs"]
mod unix_client;
#[cfg(not(windows))]
pub(crate) use unix_client::terminal_title_for_presentation;
#[cfg(not(windows))]
#[path = "background_command.rs"]
mod background_command;
#[cfg(not(windows))]
pub(crate) use background_command::configure_background_command;

#[cfg(not(windows))]
fn apply_pane_runtime_marker_platform(_command: &mut portable_pty::CommandBuilder) {}

#[cfg(windows)]
pub(crate) fn configure_background_command(command: &mut std::process::Command) {
    configure_background_command_platform(command);
}

pub(crate) use herdr_core_tui::platform::capabilities;

/// The byte the host terminal sends for Backspace according to the tty's erase
/// setting (e.g. `^H` for MobaXterm and PuTTY-style terminals), when known.
pub(crate) fn terminal_erase_byte() -> Option<u8> {
    #[cfg(unix)]
    return unix_common::terminal_erase_byte();
    #[cfg(not(unix))]
    None
}

/// Error kind marker for a terminal that momentarily has no usable grid.
///
/// A window drag can sample a zero-sized grid before the terminal settles, so
/// this is distinct from a real terminal failure. Startup still rejects it; the
/// resize watcher treats it as a transient sample instead of a dead terminal.
#[derive(Debug)]
pub(crate) struct ZeroSizedGridError;

impl std::fmt::Display for ZeroSizedGridError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("terminal reported a zero-sized grid")
    }
}

impl std::error::Error for ZeroSizedGridError {}

pub(crate) fn zero_sized_terminal_grid_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, ZeroSizedGridError)
}

/// Whether an error only means the terminal has no usable size yet, so a
/// resizing terminal should keep its last known geometry instead of detaching.
pub(crate) fn is_transient_terminal_grid_error(err: &std::io::Error) -> bool {
    err.get_ref()
        .is_some_and(|inner| inner.is::<ZeroSizedGridError>())
}

pub(crate) fn terminal_grid_size() -> std::io::Result<(u16, u16)> {
    #[cfg(unix)]
    let (cols, rows) = unix_common::read_terminal_grid_size()?;
    #[cfg(windows)]
    let (cols, rows) = windows::read_terminal_grid_size()?;
    #[cfg(not(any(unix, windows)))]
    let (cols, rows) = fallback::read_terminal_grid_size()?;

    if cols == 0 || rows == 0 {
        return Err(zero_sized_terminal_grid_error());
    }
    Ok((cols, rows))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    command.spawn().map(|child| child.id())
}

/// Starts the daemon through launchd, so quitting the terminal app that
/// started it does not take the server down (herdr#4920). Over SSH, or when
/// launchd cannot carry the command exactly, it spawns a detached child.
/// Returns the child's pid, or 0 when launchd started it.
#[cfg(target_os = "macos")]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    match macos::spawn_launchd_job(command) {
        Ok(_label) => Ok(0),
        Err(err) => {
            tracing::info!(%err, "starting the server directly instead of through launchd");
            command.spawn().map(|child| child.id())
        }
    }
}

/// First step of a server process; see `launch_server_daemon_command`.
/// Returns a warning to log once logging is initialized.
#[cfg(target_os = "macos")]
pub(crate) fn leave_launcher_job() -> Option<String> {
    macos::leave_launchd_job()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn leave_launcher_job() -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn prepare_server_process(_handoff_import: bool) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    #[cfg(target_os = "macos")]
    macos::configure_server_daemon_context(command);

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn current_process_is_detached_server_daemon() -> bool {
    unsafe { libc::getsid(0) == libc::getpid() }
}

/// Raised by the SIGWINCH handler, consumed by the host resize watcher.
#[cfg(unix)]
static TERMINAL_RESIZE_SIGNALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn record_terminal_resize_signal(_signal: libc::c_int) {
    TERMINAL_RESIZE_SIGNALLED.store(true, std::sync::atomic::Ordering::Release);
}

/// Records SIGWINCH events that size polling can miss.
#[cfg(unix)]
pub(crate) fn watch_terminal_resize_signal() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction =
        record_terminal_resize_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // Keep blocking stdin and socket reads from failing with EINTR.
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub(crate) fn watch_terminal_resize_signal() {}

/// Returns whether a terminal size change was signalled since the last call.
#[cfg(unix)]
pub(crate) fn take_terminal_resize_signal() -> bool {
    TERMINAL_RESIZE_SIGNALLED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// Windows relies on size polling.
#[cfg(not(unix))]
pub(crate) fn take_terminal_resize_signal() -> bool {
    false
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteSshConfigPaths {
    pub(crate) user_config: Option<std::path::PathBuf>,
    pub(crate) system_config: Option<std::path::PathBuf>,
    pub(crate) multiplexing: bool,
}

pub(crate) const REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED: bool =
    cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(unix)]
#[path = "remote_bridge.rs"]
mod remote_bridge;
#[cfg(all(test, unix))]
#[path = "remote_bridge_tests.rs"]
mod remote_bridge_tests;
#[cfg(unix)]
#[path = "unix_common.rs"]
mod unix_common;
/// Other Unix platforms keep their existing input admission policy.
/// Their _PC_MAX_CANON values need not describe the actual queue capacity.
#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone, Default)]
pub(crate) struct PtyInputGuard;

#[cfg(all(unix, not(target_os = "macos")))]
impl PtyInputGuard {
    pub(crate) fn new(_master: &std::sync::Arc<std::fs::File>) -> Self {
        Self
    }

    pub(crate) fn validate(&self, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
}
#[cfg(unix)]
#[path = "unix_image_files.rs"]
pub(crate) mod unix_image_files;
#[cfg(unix)]
pub(crate) use unix_common::{
    begin_cli_output, end_cli_output, forward_remote_bridge_stdio, ignore_server_hangup,
    local_stream_peer_description, spawn_server_signal_monitor, ChildPipe, RemoteBridgeWake,
};

#[path = "client_state.rs"]
mod client_state;
pub(crate) use client_state::{create_private_state_file, replace_file, sync_parent_directory};

#[cfg(not(unix))]
pub(crate) fn begin_cli_output() {}

#[cfg(not(unix))]
pub(crate) fn end_cli_output() {}

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(windows))]
pub(crate) use process_cwd as pane_process_cwd;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
#[path = "fallback.rs"]
mod fallback;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use fallback::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn available_pane_shell_from_job(child_pid: u32, job: ForegroundJob) -> Option<String> {
    if job.process_group_id != child_pid
        || job.processes.iter().any(|process| process.pid != child_pid)
    {
        return None;
    }
    job.processes
        .into_iter()
        .find(|process| process.pid == child_pid)
        .map(|process| process.name)
        .filter(|name| is_pane_shell_process_name(name))
}

fn normalized_process_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn is_powershell_process_name(name: &str) -> bool {
    matches!(
        normalized_process_name(name).as_str(),
        "pwsh" | "powershell"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn interactive_unix_shell_command(
    argv: &[String],
    shell_name: &str,
    quote_posix_arg: fn(&str) -> String,
) -> Option<String> {
    let quote = if is_powershell_process_name(shell_name) {
        quote_powershell_arg
    } else {
        quote_posix_arg
    };
    let mut parts = argv.iter();
    let mut command = quote(parts.next()?);
    for part in parts {
        command.push(' ');
        command.push_str(&quote(part));
    }
    Some(command)
}

pub(crate) fn quote_powershell_arg(value: &str) -> String {
    if !value.is_empty()
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn quote_windows_command_line_arg(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return value.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
        }
        backslashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn is_pane_shell_process_name(name: &str) -> bool {
    let normalized = normalized_process_name(name);
    matches!(
        normalized.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "elvish"
            | "xonsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_agent_hint(_pid: u32) -> Option<crate::detect::Agent> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        return crate::detect::parse_agent_label(std::str::from_utf8(value).ok()?);
    }
    None
}

#[path = "prefix_input_source.rs"]
mod prefix_input_source;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) use prefix_input_source::{
    pump_input_source_runloop, switch_to_ascii_input_source, InputSourceRestore,
};
pub(crate) use prefix_input_source::{PrefixInputSource, RealPrefixInputSource};

#[cfg(all(test, any(unix, windows)))]
#[test]
fn child_exit_classification_only_checkpoints_interruptions() {
    for code in [0, 1, 130, 255, 0xC0000005] {
        let reason = classify_child_exit(&portable_pty::ExitStatus::with_exit_code(code));
        assert_eq!(reason, ChildExitReason::Exited, "exit code {code:#x}");
        assert!(!reason.requires_session_checkpoint());
    }
    #[cfg(windows)]
    let status = portable_pty::ExitStatus::with_exit_code(0xC000013A);
    #[cfg(not(windows))]
    let status = portable_pty::ExitStatus::with_signal("Terminated: 15");
    assert_eq!(classify_child_exit(&status), ChildExitReason::Interrupted);
    assert!(classify_child_exit(&status).requires_session_checkpoint());
    #[cfg(unix)]
    assert!(ChildExitReason::Handoff.requires_session_checkpoint());
    assert!(!ChildExitReason::WaitFailed.requires_session_checkpoint());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn live_pane_process_group_rejects_processes_outside_the_pane_session() {
        use std::os::unix::process::CommandExt;

        let mut detached = std::process::Command::new("sleep");
        detached.arg("30");
        // SAFETY: setsid is async-signal-safe and touches only the child.
        unsafe {
            detached.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut detached = detached.spawn().expect("spawn detached");
        let token = process_start_token(detached.id()).expect("start token");
        let mut gone = std::process::Command::new("true").spawn().expect("spawn");
        let gone_pid = gone.id();
        gone.wait().expect("reap");

        assert_eq!(
            live_pane_process_group(std::process::id(), detached.id(), token),
            None,
            "a live process in another terminal session"
        );
        assert_eq!(
            live_pane_process_group(gone_pid, detached.id(), token),
            None,
            "a pane shell that is gone"
        );
        let _ = detached.kill();
        let _ = detached.wait();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_pane_process_group_follows_the_agent_process_not_its_job() {
        use std::os::unix::process::CommandExt;

        let shell_pid = std::process::id();
        let mut wrapper = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn wrapper");
        let job = wrapper.id();
        let mut agent = std::process::Command::new("sleep")
            .arg("30")
            .process_group(job as i32)
            .spawn()
            .expect("spawn agent");
        let agent_pid = agent.id();
        let token = process_start_token(agent_pid).expect("agent start token");
        let wrapper_token = process_start_token(job).expect("wrapper start token");

        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            Some(job)
        );
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token + 1),
            None,
            "a reused pid has a different start token"
        );
        unsafe {
            libc::kill(agent_pid as libc::pid_t, libc::SIGSTOP);
        }
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            Some(job)
        );

        unsafe {
            libc::kill(agent_pid as libc::pid_t, libc::SIGKILL);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while live_pane_process_group(shell_pid, agent_pid, token).is_some()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            None,
            "an unreaped agent must not count as alive while its wrapper lives"
        );
        assert_eq!(
            live_pane_process_group(shell_pid, job, wrapper_token),
            Some(job)
        );
        agent.wait().expect("reap agent");
        assert_eq!(live_pane_process_group(shell_pid, agent_pid, token), None);
        let _ = wrapper.kill();
        let _ = wrapper.wait();
    }

    #[test]
    fn terminal_resize_signal_is_recorded_once_per_delivery() {
        watch_terminal_resize_signal();
        assert!(!take_terminal_resize_signal());

        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());
    }

    #[test]
    fn pane_shell_process_names_reject_exec_replacement_programs() {
        for shell in ["bash", "-zsh", "/bin/fish", "pwsh", "powershell.exe"] {
            assert!(is_pane_shell_process_name(shell), "{shell}");
        }
        for program in ["vim", "nvim", "cargo", "test-runner", "opencode"] {
            assert!(!is_pane_shell_process_name(program), "{program}");
        }
    }

    #[test]
    fn detached_custom_command_preserves_unix_login_shell_flag() {
        let cmd = detached_custom_command_process("echo hello");
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-lc"),
                std::ffi::OsStr::new("echo hello")
            ]
        );
    }

    #[test]
    fn pane_custom_command_builder_preserves_unix_shell_flag() {
        let expected: Vec<std::ffi::OsString> =
            vec!["/bin/sh".into(), "-c".into(), "echo hello".into()];
        assert_eq!(
            pane_custom_command_pty_builder("echo hello").get_argv(),
            &expected
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0HERDR_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"HERDR_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"HERDR_AGENT=not-an-agent\0"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interactive_shell_command_quotes_for_posix_and_powershell() {
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "a'b".into(),
            "$HOME".into(),
            "semi;colon".into(),
            "@options".into(),
        ];
        assert_eq!(
            interactive_shell_command(&argv, "bash").as_deref(),
            Some("pi '' 'two words' 'a'\\''b' '$HOME' 'semi;colon' @options")
        );
        assert_eq!(
            interactive_shell_command(&argv, "pwsh").as_deref(),
            Some("pi '' 'two words' 'a''b' '$HOME' 'semi;colon' '@options'")
        );
    }

    #[test]
    fn read_limited_reader_returns_complete_data_under_limit() {
        let input = std::io::Cursor::new(b"image".to_vec());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_returns_empty_for_empty_input() {
        let input = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Empty
        );
    }

    #[test]
    fn read_limited_reader_accepts_data_exactly_at_limit() {
        let input = std::io::Cursor::new(b"four".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Complete(b"four".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_rejects_data_over_limit() {
        let input = std::io::Cursor::new(b"oversized".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Oversized
        );
    }

    #[test]
    fn read_limited_reader_retries_interrupted_reads() {
        struct InterruptedOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.inner.read(buffer)
            }
        }

        let input = InterruptedOnce {
            interrupted: false,
            inner: std::io::Cursor::new(b"image".to_vec()),
        };
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }
}

#[cfg(not(unix))]
pub(crate) fn shared_ssh_control_path(
    _namespace: &std::path::Path,
    _target: &str,
) -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "interactive SSH recovery requires Unix OpenSSH multiplexing",
    ))
}
