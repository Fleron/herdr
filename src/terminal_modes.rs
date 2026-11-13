pub(crate) use crate::input::host::{
    clear_host_mouse_reporting, set_host_kitty_keyboard_report_all,
};

use std::io::{self, Write};

#[cfg(any(windows, test))]
const WINDOWS_MOUSE_REPORTING_ENABLE_SEQUENCE: &[u8] =
    b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
#[cfg(any(windows, test))]
const WINDOWS_MOUSE_REPORTING_DISABLE_SEQUENCE: &[u8] =
    b"\x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

#[cfg(any(windows, test))]
pub(crate) fn set_windows_mouse_reporting<W: Write>(
    writer: &mut W,
    enabled: bool,
    sgr_pixels: bool,
) -> io::Result<()> {
    if enabled {
        // Keep a true-to-false pixel transition in the same buffered reassertion,
        // before the final SGR enable, so refresh never exposes a disabled gap.
        if !sgr_pixels {
            writer.write_all(b"\x1b[?1016l")?;
        }
        writer.write_all(WINDOWS_MOUSE_REPORTING_ENABLE_SEQUENCE)?;
        if sgr_pixels {
            writer.write_all(b"\x1b[?1016h")?;
        }
    } else {
        writer.write_all(WINDOWS_MOUSE_REPORTING_DISABLE_SEQUENCE)?;
    }
    writer.flush()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectHostKeyboardState {
    kitty_flags: Option<u16>,
    modify_other_keys_level: u8,
}

#[cfg(not(windows))]
pub(crate) fn set_direct_host_keyboard_protocol<W: Write>(
    writer: &mut W,
    active: &mut DirectHostKeyboardState,
    next_flags: u16,
    next_modify_other_keys_level: u8,
) -> io::Result<()> {
    let next_kitty_flags = (next_flags != 0).then_some(next_flags);
    if active.kitty_flags == next_kitty_flags
        && active.modify_other_keys_level == next_modify_other_keys_level
    {
        return Ok(());
    }

    if active.kitty_flags != next_kitty_flags {
        if active.kitty_flags.is_some() {
            writer.write_all(b"\x1b[<1u")?;
        }
        if next_flags != 0 {
            write!(writer, "\x1b[>{next_flags}u")?;
        }
    }
    if active.modify_other_keys_level != next_modify_other_keys_level {
        write!(writer, "\x1b[>4;{next_modify_other_keys_level}m")?;
    }
    writer.flush()?;
    *active = DirectHostKeyboardState {
        kitty_flags: next_kitty_flags,
        modify_other_keys_level: next_modify_other_keys_level,
    };
    Ok(())
}

#[cfg(windows)]
pub(crate) fn set_direct_host_keyboard_protocol<W: Write>(
    _writer: &mut W,
    active: &mut DirectHostKeyboardState,
    next_flags: u16,
    next_modify_other_keys_level: u8,
) -> io::Result<()> {
    *active = DirectHostKeyboardState {
        kitty_flags: (next_flags != 0).then_some(next_flags),
        modify_other_keys_level: next_modify_other_keys_level,
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn direct_keyboard_protocol_owns_exactly_one_stack_entry_and_modify_other_keys() {
        let mut output = Vec::new();
        let mut active = DirectHostKeyboardState::default();

        set_direct_host_keyboard_protocol(&mut output, &mut active, 3, 0).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 15, 2).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 0).unwrap();

        assert_eq!(
            output,
            b"\x1b[>3u\x1b[<1u\x1b[>15u\x1b[>4;2m\x1b[<1u\x1b[>4;0m"
        );
        assert_eq!(active, DirectHostKeyboardState::default());
    }

    #[cfg(not(windows))]
    #[test]
    fn direct_modify_other_keys_works_without_kitty_flags() {
        let mut output = Vec::new();
        let mut active = DirectHostKeyboardState::default();

        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 1).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 2).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 0).unwrap();

        assert_eq!(output, b"\x1b[>4;1m\x1b[>4;2m\x1b[>4;0m");
        assert_eq!(active, DirectHostKeyboardState::default());
    }

    #[test]
    fn direct_legacy_keyboard_mode_does_not_pop_the_host_stack() {
        let mut output = Vec::new();
        let mut active = DirectHostKeyboardState::default();

        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 0).unwrap();

        assert!(output.is_empty());
        assert_eq!(active, DirectHostKeyboardState::default());
    }

    #[test]
    fn windows_mouse_reporting_setup_and_teardown_request_required_modes() {
        let mut output = Vec::new();

        set_windows_mouse_reporting(&mut output, true, true).unwrap();
        set_windows_mouse_reporting(&mut output, true, false).unwrap();
        set_windows_mouse_reporting(&mut output, false, false).unwrap();

        assert_eq!(
            output,
            b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1016h\x1b[?1016l\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l"
        );
    }
}
