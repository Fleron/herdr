use std::process::{Command, Stdio};
use std::sync::OnceLock;

const WSL_MARKER_ENV_VARS: &[&str] = &["WSL_DISTRO_NAME", "WSL_INTEROP"];

use super::clipboard;
use crate::platform::{ClipboardCommand, ClipboardImage};

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    running_inside_wsl()
}

pub(crate) fn should_query_host_terminal_palette() -> bool {
    !running_inside_wsl()
}

pub(super) fn running_inside_wsl() -> bool {
    static RUNNING_INSIDE_WSL: OnceLock<bool> = OnceLock::new();
    *RUNNING_INSIDE_WSL.get_or_init(detect_running_inside_wsl)
}

fn detect_running_inside_wsl() -> bool {
    proc_file_indicates_wsl("/proc/sys/kernel/osrelease")
        || proc_file_indicates_wsl("/proc/version")
        || WSL_MARKER_ENV_VARS
            .iter()
            .any(|key| std::env::var_os(key).is_some())
        || std::path::Path::new("/run/WSL").exists()
}

fn proc_file_indicates_wsl(path: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text_indicates_wsl(&text))
        .unwrap_or(false)
}

pub(super) fn text_indicates_wsl(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("microsoft") || text.contains("wsl")
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    let deadline = std::time::Instant::now() + clipboard::TIMEOUT;
    for command in clipboard_commands() {
        if run_clipboard_command_until(&command, bytes, deadline) {
            return true;
        }
    }
    false
}

pub fn read_clipboard_text() -> Option<String> {
    let deadline = std::time::Instant::now() + clipboard::TIMEOUT;
    for command in read_clipboard_text_commands() {
        if let Some(text) = read_clipboard_text_with_command_until(&command, deadline) {
            return Some(text);
        }
    }
    None
}

pub fn open_url(url: &str) -> std::io::Result<Option<std::process::Child>> {
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Some)
}

/// Show a native desktop notification through libnotify's command-line helper.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

pub(super) fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Ok(false);
    }

    let mut cmd = command("notify-send");
    cmd.arg("--app-name").arg("Herdr").arg("--").arg(title);
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        cmd.arg(body);
    }
    run_notification_command(cmd)
}

fn run_notification_command(mut command: Command) -> std::io::Result<bool> {
    let status = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    Ok(status.success())
}

pub(super) fn clipboard_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-copy",
            args: &["--type", "text/plain;charset=utf-8"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-in"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--input"],
        });
    }

    commands
}

pub(super) fn read_clipboard_text_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain;charset=utf-8"],
        });
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-out"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--output"],
        });
    }

    commands
}

pub(super) fn read_clipboard_text_with_command_until(
    command: &ClipboardCommand,
    deadline: std::time::Instant,
) -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;
    let mut process = Command::new(command.program);
    process.args(command.args);
    let bytes = clipboard::run(process, None, MAX_CLIPBOARD_TEXT_BYTES, deadline).ok()?;
    if bytes.is_empty() {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub(super) fn run_clipboard_command_until(
    command: &ClipboardCommand,
    bytes: &[u8],
    deadline: std::time::Instant,
) -> bool {
    let mut process = Command::new(command.program);
    process.args(command.args);
    clipboard::run(process, Some(bytes), 0, deadline).is_ok()
}

pub fn clipboard_text_matches(_bytes: &[u8]) -> Option<bool> {
    None
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    let deadline = std::time::Instant::now() + clipboard::TIMEOUT;
    if running_inside_wsl() {
        if let Some(image) =
            read_wsl_clipboard_image_with_command_until(|program| Command::new(program), deadline)
        {
            return Some(image);
        }
    }

    for (mime, extension) in [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/jpg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("image/bmp", "bmp"),
    ] {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Some(image) = read_validated_clipboard_image_until(
                "wl-paste",
                &["--type", mime],
                extension,
                deadline,
            ) {
                return Some(image);
            }
        }

        if std::env::var_os("DISPLAY").is_some() {
            if let Some(image) = read_validated_clipboard_image_until(
                "xclip",
                &["-selection", "clipboard", "-t", mime, "-o"],
                extension,
                deadline,
            ) {
                return Some(image);
            }
        }
    }

    None
}

pub(super) fn read_wsl_clipboard_image_with_command_until(
    mut command: impl FnMut(&str) -> Command,
    deadline: std::time::Instant,
) -> Option<ClipboardImage> {
    let mut command = command("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-STA",
        "-Command",
        "$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $image=[System.Windows.Forms.Clipboard]::GetImage(); if ($null -eq $image) { exit 1 }; $stream=[System.IO.MemoryStream]::new(); try { $image.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png); [Console]::OpenStandardOutput().Write($stream.GetBuffer(), 0, [int]$stream.Length) } finally { $stream.Dispose(); $image.Dispose() }",
    ]);
    let bytes = read_clipboard_image_until(
        command,
        crate::client::frame::MAX_CLIPBOARD_IMAGE_PAYLOAD,
        deadline,
    )?;
    bytes_match_image_signature("png", &bytes).then_some(ClipboardImage {
        bytes,
        extension: "png",
    })
}

pub(super) fn read_validated_clipboard_image_until(
    program: &str,
    args: &[&str],
    extension: &'static str,
    deadline: std::time::Instant,
) -> Option<ClipboardImage> {
    let mut command = Command::new(program);
    command.args(args);
    let bytes = read_clipboard_image_until(
        command,
        crate::client::frame::MAX_CLIPBOARD_IMAGE_PAYLOAD,
        deadline,
    )?;
    if !bytes_match_image_signature(extension, &bytes) {
        return None;
    }
    Some(ClipboardImage { bytes, extension })
}

pub(super) fn bytes_match_image_signature(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP",
        "bmp" => {
            if bytes.len() < 26 || !bytes.starts_with(b"BM") {
                return false;
            }
            let offset = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
            (26..=bytes.len()).contains(&offset)
        }
        _ => false,
    }
}

pub(super) fn read_clipboard_image_until(
    command: Command,
    max_bytes: usize,
    deadline: std::time::Instant,
) -> Option<Vec<u8>> {
    let bytes = clipboard::run(command, None, max_bytes, deadline).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}
