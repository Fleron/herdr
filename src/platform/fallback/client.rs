use crate::platform::ClipboardImage;

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    false
}

pub(crate) fn should_query_host_terminal_palette() -> bool {
    false
}

pub fn write_clipboard(_bytes: &[u8]) -> bool {
    false
}

pub fn read_clipboard_text() -> Option<String> {
    None
}

pub fn open_url(_url: &str) -> std::io::Result<Option<std::process::Child>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening URLs is not supported on this platform",
    ))
}

pub fn show_desktop_notification(_title: &str, _body: Option<&str>) -> std::io::Result<bool> {
    Ok(false)
}

pub fn clipboard_text_matches(_bytes: &[u8]) -> Option<bool> {
    None
}

/// Unsupported platform stub.
pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
}
