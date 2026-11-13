pub mod build_info;
pub mod config;
#[path = "detect/agent.rs"]
pub mod detect;
#[path = "client/frame.rs"]
pub mod frame;
#[path = "kitty_graphics/model.rs"]
pub mod graphics_model;
pub mod input;
pub mod kitty_graphics;
pub mod layout;
pub mod palette;
#[path = "pane/input.rs"]
pub mod pane_input;
#[path = "platform/capabilities.rs"]
pub mod platform;
pub mod popup_size;
pub mod public_ids;
pub mod raw_input;
pub mod render_prof;
#[path = "sound_kind.rs"]
pub mod sound;
pub mod terminal_notify;
pub mod terminal_theme;

pub use ghostty_vt as ghostty;
pub use ghostty_vt::pane_graphics_files;

#[path = "core_client/connection.rs"]
pub mod connection;
#[path = "core_client/edit_queue.rs"]
pub mod edit_queue;
#[path = "core_client/image_staging.rs"]
pub mod image_staging;
#[path = "core_client/inventory.rs"]
pub mod inventory;
#[path = "core_client/machine.rs"]
pub mod machine;
#[path = "core_client/model.rs"]
pub mod model;
#[path = "core_client/paint.rs"]
pub mod paint;
#[path = "core_client/persistence.rs"]
pub mod persistence;
#[path = "core_client/protocol.rs"]
pub mod protocol;
#[path = "core_client/render.rs"]
pub mod render;
#[path = "core_client/surfaces.rs"]
pub mod surfaces;
#[path = "core_client/terminal.rs"]
pub mod terminal;
#[path = "core_client/worktrees.rs"]
pub mod worktrees;

#[cfg(test)]
extern crate self as herdr_core_tui;

#[cfg(test)]
#[path = "../tests/core_support/mod.rs"]
pub(crate) mod test_support;
