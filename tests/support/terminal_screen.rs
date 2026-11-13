use std::{mem::size_of, ptr};

// Reuse the generated C API; this test observer needs only terminal parsing and formatting.
use ghostty_vt::ffi;

struct Screen {
    terminal: ffi::GhosttyTerminal,
    formatter: ffi::GhosttyFormatter,
}

impl Drop for Screen {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_formatter_free(self.formatter);
            ffi::ghostty_terminal_free(self.terminal);
        }
    }
}

pub fn text(output: &[u8], cols: u16, rows: u16) -> String {
    text_with_resizes(output, cols, rows, &[])
}

pub fn text_with_resizes(
    output: &[u8],
    cols: u16,
    rows: u16,
    resizes: &[(usize, u16, u16)],
) -> String {
    let mut screen = Screen {
        terminal: ptr::null_mut(),
        formatter: ptr::null_mut(),
    };
    let options = ffi::GhosttyFormatterTerminalOptions {
        size: size_of::<ffi::GhosttyFormatterTerminalOptions>(),
        emit: ffi::GhosttyFormatterFormat_GHOSTTY_FORMATTER_FORMAT_PLAIN,
        trim: true,
        extra: ffi::GhosttyFormatterTerminalExtra {
            size: size_of::<ffi::GhosttyFormatterTerminalExtra>(),
            screen: ffi::GhosttyFormatterScreenExtra {
                size: size_of::<ffi::GhosttyFormatterScreenExtra>(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        assert_eq!(
            ffi::ghostty_terminal_new(ptr::null(), &mut screen.terminal, cols, rows),
            ffi::GhosttyResult_GHOSTTY_SUCCESS
        );
        let mut start = 0;
        for &(offset, cols, rows) in resizes {
            assert!(start <= offset && offset <= output.len());
            let chunk = &output[start..offset];
            ffi::ghostty_terminal_vt_write(screen.terminal, chunk.as_ptr(), chunk.len());
            assert_eq!(
                ffi::ghostty_terminal_resize(screen.terminal, cols, rows, 1, 1),
                ffi::GhosttyResult_GHOSTTY_SUCCESS
            );
            start = offset;
        }
        let chunk = &output[start..];
        ffi::ghostty_terminal_vt_write(screen.terminal, chunk.as_ptr(), chunk.len());
        assert_eq!(
            ffi::ghostty_formatter_terminal_new(
                ptr::null(),
                &mut screen.formatter,
                screen.terminal,
                options,
            ),
            ffi::GhosttyResult_GHOSTTY_SUCCESS
        );
        let mut len = 0;
        let result =
            ffi::ghostty_formatter_format_buf(screen.formatter, ptr::null_mut(), 0, &mut len);
        assert!(matches!(
            result,
            ffi::GhosttyResult_GHOSTTY_SUCCESS | ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE
        ));
        let mut bytes = vec![0; len];
        assert_eq!(
            ffi::ghostty_formatter_format_buf(
                screen.formatter,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut len
            ),
            ffi::GhosttyResult_GHOSTTY_SUCCESS
        );
        String::from_utf8_lossy(&bytes[..len]).into_owned()
    }
}

#[test]
fn screen_text_reconstructs_partial_redraws() {
    let output = b"REMOTE_SURVIVED\x1b[1;8HSTILL_SELECTED";
    assert!(!output
        .windows(b"REMOTE_STILL_SELECTED".len())
        .any(|part| part == b"REMOTE_STILL_SELECTED"));
    assert!(text(output, 80, 24).contains("REMOTE_STILL_SELECTED"));
}

#[test]
fn screen_text_preserves_geometry_before_resizing() {
    let before = b"\x1b[1;1H0123456789abcdef\x1b[2;1HXY";
    let after = b"\x1b[3;1HMENU";
    let output = [before.as_slice(), after.as_slice()].concat();
    let actual = text_with_resizes(&output, 10, 4, &[(before.len(), 20, 4)]);
    assert!(actual.contains("XYcdef"), "{actual}");
    assert_ne!(actual, text(&output, 20, 4));
}
