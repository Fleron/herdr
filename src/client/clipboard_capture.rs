use std::path::PathBuf;

use tracing::warn;

#[cfg(windows)]
use crate::client::frame::ClientInputEvent;
use crate::client::frame::MAX_CLIPBOARD_IMAGE_PAYLOAD;

#[cfg(unix)]
pub(crate) fn image_paste_trigger(
    event: &crate::raw_input::RawInputEvent,
    remote: bool,
    key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if !remote {
        return false;
    }
    match event {
        crate::raw_input::RawInputEvent::Paste(text) => text.is_empty(),
        crate::raw_input::RawInputEvent::Key(event) => {
            event.kind == crossterm::event::KeyEventKind::Press
                && key.is_some_and(|key| crate::config::terminal_key_matches_combo(event, key))
        }
        _ => false,
    }
}

#[cfg(windows)]
pub(crate) fn should_bridge_clipboard_image_events(
    events: &[ClientInputEvent],
    is_remote_client: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if !is_remote_client {
        return false;
    }
    if matches!(events, [ClientInputEvent::Paste { text }] if text.is_empty()) {
        return true;
    }

    let Some(remote_image_paste_key) = remote_image_paste_key else {
        return false;
    };
    matches!(
        events,
        [event]
            if matches!(
                event.to_raw_input_event(),
                crate::raw_input::RawInputEvent::Key(key)
                    if key.kind == crossterm::event::KeyEventKind::Press
                        && crate::config::terminal_key_matches_combo(
                            &key,
                            remote_image_paste_key,
                        )
            )
    )
}

#[cfg(unix)]
pub(crate) fn read_image_file_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let (path, extension) = image_path_from_terminal_drop(data, is_remote_client)?;
    read_image_file(path, extension)
}

#[cfg(windows)]
pub(crate) fn read_image_file_from_client_events(
    events: &[ClientInputEvent],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let [ClientInputEvent::Paste { text }] = events else {
        return None;
    };
    let text = normalized_terminal_drop_text(text)?;
    let (path, extension) =
        image_path_from_drop_text(strip_matching_path_quotes(text), is_remote_client)?;
    read_image_file(path, extension)
}

fn read_image_file(
    path: PathBuf,
    extension: &'static str,
) -> Option<crate::platform::ClipboardImage> {
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let file = std::fs::File::open(&path).ok()?;
    let bytes =
        match crate::platform::read_limited_reader(file, MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()? {
            crate::platform::LimitedRead::Complete(bytes) => bytes,
            crate::platform::LimitedRead::Empty => return None,
            crate::platform::LimitedRead::Oversized => {
                warn!(
                    max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                    "local image file drop is too large to bridge"
                );
                return None;
            }
        };

    Some(crate::platform::ClipboardImage { bytes, extension })
}

#[cfg(unix)]
pub(crate) fn image_path_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<(PathBuf, &'static str)> {
    let bytes = bracketed_paste_payload(data).unwrap_or(data);
    let text = std::str::from_utf8(bytes).ok()?;
    let text = normalized_terminal_drop_text(text)?;
    let text = unescape_terminal_drop_path(strip_matching_path_quotes(text));
    image_path_from_drop_text(&text, is_remote_client)
}

fn normalized_terminal_drop_text(text: &str) -> Option<&str> {
    let text = text.trim_end_matches(['\r', '\n']);
    (!text.is_empty() && !text.contains(['\r', '\n'])).then_some(text)
}

fn image_path_from_drop_text(
    text: &str,
    is_remote_client: bool,
) -> Option<(PathBuf, &'static str)> {
    if !is_remote_client {
        return None;
    }
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return None;
    }
    let extension = recognized_image_extension(path.extension()?.to_str()?)?;
    Some((path, extension))
}

#[cfg(unix)]
fn bracketed_paste_payload(data: &[u8]) -> Option<&[u8]> {
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    data.strip_prefix(START)?.strip_suffix(END)
}

fn strip_matching_path_quotes(text: &str) -> &str {
    if text.len() < 2 {
        return text;
    }

    let bytes = text.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) => &text[1..text.len() - 1],
        _ => text,
    }
}

#[cfg(unix)]
fn unescape_terminal_drop_path(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                unescaped.push(escaped);
            } else {
                unescaped.push(ch);
            }
        } else {
            unescaped.push(ch);
        }
    }
    unescaped
}

fn recognized_image_extension(extension: &str) -> Option<&'static str> {
    if extension.eq_ignore_ascii_case("png") {
        Some("png")
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        Some("jpg")
    } else if extension.eq_ignore_ascii_case("gif") {
        Some("gif")
    } else if extension.eq_ignore_ascii_case("webp") {
        Some("webp")
    } else if extension.eq_ignore_ascii_case("bmp") {
        Some("bmp")
    } else {
        None
    }
}
