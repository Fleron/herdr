mod clipboard_image;
pub(crate) use clipboard_image::ClipboardImage;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod clipboard_command;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use clipboard_command::ClipboardCommand;

#[cfg(target_os = "macos")]
mod limited_read;
#[cfg(target_os = "macos")]
pub(crate) use limited_read::{read_limited_reader, LimitedRead};

#[cfg(target_os = "linux")]
#[path = "linux/clipboard.rs"]
mod clipboard;

#[cfg(target_os = "macos")]
#[path = "macos/client.rs"]
mod host;
#[cfg(target_os = "linux")]
#[path = "linux/client.rs"]
mod host;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[path = "fallback/client.rs"]
mod host;

pub(crate) use host::{
    clipboard_text_matches, open_url, read_clipboard_image, read_clipboard_text,
    should_draw_host_cursor_by_default, should_query_host_terminal_palette,
    show_desktop_notification, write_clipboard,
};

#[cfg(unix)]
pub(crate) mod unix_image_files;

mod unix_client;
pub(crate) use unix_client::terminal_title_for_presentation;
#[cfg(test)]
mod background_command;
#[cfg(test)]
pub(crate) use background_command::configure_background_command;

#[cfg(target_os = "macos")]
#[path = "macos/input_source.rs"]
mod input_source;
#[cfg(target_os = "macos")]
pub(crate) use input_source::{
    pump_input_source_runloop, switch_to_ascii_input_source, InputSourceRestore,
};
mod prefix_input_source;
#[cfg(not(target_os = "macos"))]
pub(crate) use prefix_input_source::{
    pump_input_source_runloop, switch_to_ascii_input_source, InputSourceRestore,
};
pub(crate) use prefix_input_source::{PrefixInputSource, RealPrefixInputSource};

#[cfg(unix)]
pub(crate) use unix_client::{poll_fd_readable, read_fd, terminal_erase_byte};
