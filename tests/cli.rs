#![cfg(all(unix, not(target_os = "macos")))]

#[path = "legacy_binary/mod.rs"]
mod legacy_binary;

pub mod support;

#[path = "cli/mod.rs"]
mod cases;
