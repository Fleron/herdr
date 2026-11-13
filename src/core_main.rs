use std::io;

use herdr_core_tui::{build_info, config};
#[cfg(all(test, unix))]
#[path = "../tests/core_support/mod.rs"]
mod core_test_support;
#[cfg(unix)]
use herdr_core_tui::{input, palette, raw_input, terminal_theme};

#[cfg(unix)]
mod agent_view_eval;
#[path = "client/core_frontend.rs"]
#[cfg(unix)]
mod client;
#[cfg(unix)]
mod copy_mode;
#[cfg(unix)]
use herdr_core_tui::kitty_graphics;
#[path = "platform/client.rs"]
#[cfg(unix)]
mod platform;
#[cfg(unix)]
use herdr_core_tui::public_ids;
#[cfg(unix)]
use herdr_core_tui::render_prof;
#[cfg(unix)]
mod selection;
#[cfg(unix)]
mod terminal_text;
#[cfg(unix)]
mod worktree_names;
#[cfg(unix)]
use ghostty_vt as ghostty;
#[cfg(unix)]
#[cfg(test)]
#[path = "process_command.rs"]
mod noninteractive_process;
#[cfg(unix)]
use herdr_core_tui::sound as sound_kind;
#[cfg(unix)]
mod sound;
#[cfg(unix)]
use herdr_core_tui::terminal_notify;
#[cfg(unix)]
use herdr_core_tui::{detect, layout, popup_size};

#[cfg(unix)]
mod core_frontend;
#[cfg(unix)]
mod terminal_effects;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => run(),
        [flag] if flag == "--version" || flag == "-V" => {
            println!("herdr {} (herdr-core client)", build_info::version());
            Ok(())
        }
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{}", include_str!("core_client/HELP.md"));
            Ok(())
        }
        [flag] if flag == "--skill" => {
            println!("{}", include_str!("core_client/SKILL.md"));
            Ok(())
        }
        [flag] if flag == "--default-config" => {
            print!("{}", config::DEFAULT_CONFIG);
            Ok(())
        }
        [command, action] if command == "config" && action == "check" => {
            let loaded = config::Config::load();
            if loaded.diagnostics.is_empty() {
                println!("configuration is valid");
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    loaded.diagnostics.join("\n"),
                ))
            }
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "this fork is a herdr-core TUI; run herdr --help or use hc for runtime commands",
        )),
    }
}

#[cfg(unix)]
fn run() -> io::Result<()> {
    core_frontend::run()
}

#[cfg(not(unix))]
fn run() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this core-backed TUI currently supports macOS; Windows client support is not qualified",
    ))
}
