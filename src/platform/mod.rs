#[cfg(all(test, target_os = "macos"))]
macro_rules! legacy_macos_input_tests {
    () => {
        #[path = "macos/input_tests.rs"]
        mod input_tests;
    };
}

#[cfg(unix)]
pub(crate) mod ssh_agent;
mod shared;
pub(crate) use shared::*;
