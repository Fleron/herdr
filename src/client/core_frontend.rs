macro_rules! core_shell_extensions {
    (host; $($item:item)*) => {$($item)*};
    ($($item:item)*) => {$($item)*};
}

macro_rules! legacy_shell_extensions {
    ($($item:item)*) => {};
}

#[path = "clipboard_capture.rs"]
pub(crate) mod clipboard_capture;

#[path = "connection_diagnostics.rs"]
pub(crate) mod connection_diagnostics;

#[path = "host_capabilities.rs"]
pub(crate) mod host_capabilities;
#[path = "host_queries.rs"]
pub(crate) mod host_queries;
#[path = "terminal_text.rs"]
pub(crate) mod terminal_text;
#[path = "terminal_title.rs"]
pub(crate) mod terminal_title;
#[path = "theme.rs"]
pub(crate) mod theme;
#[path = "ui.rs"]
pub(crate) mod ui;

#[path = "ui_domain.rs"]
pub(crate) mod ui_domain;
#[path = "ui_state.rs"]
pub(crate) mod ui_state;
pub(crate) use herdr_core_tui::frame;
#[path = "endpoint_model.rs"]
pub(crate) mod endpoint;
#[path = "ui_action.rs"]
pub(crate) mod ui_action;
#[path = "ui_model.rs"]
pub(crate) mod ui_model;
macro_rules! legacy_shell_characterization_tests {
    ($path:literal) => {};
}
#[path = "core_backend/mod.rs"]
pub(crate) mod core_backend;
#[path = "shell.rs"]
pub(crate) mod shell;

pub(crate) use shell::{
    ClientEndpointFocusTarget, ClientShellAction, ClientShellConfig, ClientShellInput,
    ClientShellState,
};

#[path = "frame_output.rs"]
pub(crate) mod frame_output;
#[path = "image_files.rs"]
pub(crate) mod image_files;
