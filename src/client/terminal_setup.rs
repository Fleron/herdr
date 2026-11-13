//! Terminal setup and restoration for the rendered client.

use std::io::{self, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(windows)]
use std::sync::{Mutex, MutexGuard};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
#[cfg(not(windows))]
use crossterm::event::{PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;
use crossterm::terminal::{DisableLineWrap, EnableLineWrap};

use super::frame_output::clear_received_kitty_graphics;
use super::host_capabilities::query_host_capabilities;
pub(super) use crate::input::host::{
    write_host_color_scheme_report_mode, write_terminal_restore_postlude,
};

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

/// Sets up the terminal for client mode (raw mode, optional mouse, keyboard enhancements).
///
/// Returns a guard that restores the terminal when dropped.
pub(super) fn setup_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(true, mouse_capture)
}

/// Sets up a direct attach terminal.
///
/// Direct attach forwards stdin to the attached PTY. When configured, mouse
/// capture lets wheel events drive the attached viewport or reach child
/// programs that requested mouse input.
pub(super) fn setup_direct_attach_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(false, mouse_capture)
}

pub(super) fn setup_terminal_with_capabilities(
    enable_client_protocols: bool,
    mouse_capture: bool,
) -> io::Result<TerminalGuard> {
    ratatui::init();
    let mut terminal_guard = TerminalGuard {
        host_escape_disambiguation_active: false,
        host_sgr_pixel_mouse: None,
        buffered_host_input: Vec::new(),
        reset_keyboard_enhancements: false,
        reset_modify_other_keys: false,
        modify_other_keys_sequence: None,
        reset_host_color_scheme_reports: false,
        restore_claimed: Arc::new(AtomicBool::new(false)),
        restored: false,
        #[cfg(windows)]
        restore_windows_input_mode: Arc::new(WindowsInputModeRestore::default()),
    };
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    let host_color_scheme_reports =
        should_enable_host_color_scheme_reports(enable_client_protocols);
    #[cfg(windows)]
    let windows_ssh_session = is_ssh_session();
    #[cfg(windows)]
    let mut windows_virtual_terminal_input = if windows_ssh_session {
        enable_windows_virtual_terminal_input(
            &terminal_guard.restore_claimed,
            &terminal_guard.restore_windows_input_mode,
        )
    } else {
        WindowsVirtualTerminalInputSetup::default()
    };

    let (host_probe, buffered_host_input) = if enable_client_protocols {
        terminal_guard.reset_keyboard_enhancements = true;
        push_keyboard_enhancement_flags()?;
        let (probe, buffered_input) = query_host_capabilities(true);
        set_mouse_capture(mouse_capture, false)?;
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        if host_color_scheme_reports {
            terminal_guard.reset_host_color_scheme_reports = true;
            write_host_color_scheme_report_mode(&mut io::stdout(), true)?;
        }
        (probe, buffered_input)
    } else {
        if should_enable_host_color_scheme_reports(true) {
            write_host_color_scheme_report_mode(&mut io::stdout(), false)?;
        }
        let (probe, buffered_input) = query_host_capabilities(false);
        set_mouse_capture(mouse_capture, false)?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        (probe, buffered_input)
    };

    #[cfg(windows)]
    if enable_client_protocols && !windows_ssh_session {
        windows_virtual_terminal_input = enable_windows_virtual_terminal_input(
            &terminal_guard.restore_claimed,
            &terminal_guard.restore_windows_input_mode,
        );
    }

    #[cfg(windows)]
    if enable_client_protocols
        && windows_virtual_terminal_input.active
        && windows_win32_input_mode_enabled()
    {
        enable_windows_win32_input_mode(&mut io::stdout())?;
    }

    let modify_other_keys_mode = enable_client_protocols
        .then(crate::input::host_modify_other_keys_mode)
        .flatten();
    if let Some(mode) = modify_other_keys_mode {
        terminal_guard.reset_modify_other_keys = true;
        terminal_guard.modify_other_keys_sequence = Some(mode.set_sequence());
        io::stdout().write_all(mode.set_sequence())?;
        io::stdout().flush()?;
    }

    execute!(io::stdout(), DisableLineWrap)?;

    terminal_guard.host_escape_disambiguation_active = host_probe.escape_disambiguation;
    terminal_guard.host_sgr_pixel_mouse = host_probe.sgr_pixel_mouse;
    terminal_guard.buffered_host_input = buffered_host_input;
    Ok(terminal_guard)
}

pub(super) fn should_enable_host_color_scheme_reports(enable_client_protocols: bool) -> bool {
    enable_client_protocols && !cfg!(windows)
}

/// Guard that restores the terminal when dropped.
pub(super) struct TerminalGuard {
    host_escape_disambiguation_active: bool,
    host_sgr_pixel_mouse: Option<bool>,
    buffered_host_input: Vec<u8>,
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    /// The modifyOtherKeys sequence written at setup, re-sent on refresh.
    modify_other_keys_sequence: Option<&'static [u8]>,
    reset_host_color_scheme_reports: bool,
    restore_claimed: Arc<AtomicBool>,
    restored: bool,
    #[cfg(windows)]
    restore_windows_input_mode: Arc<WindowsInputModeRestore>,
}

pub(super) fn should_draw_host_cursor(mode: crate::config::HostCursorModeConfig) -> bool {
    match mode {
        crate::config::HostCursorModeConfig::Auto => {
            crate::platform::should_draw_host_cursor_by_default()
        }
        crate::config::HostCursorModeConfig::Native => false,
        crate::config::HostCursorModeConfig::Drawn => true,
    }
}

#[cfg(windows)]
#[derive(Default)]
pub(super) struct WindowsVirtualTerminalInputSetup {
    active: bool,
    restore_mode: Option<u32>,
    warning: Option<&'static str>,
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsInputModeRestore {
    mode: Mutex<Option<u32>>,
}

#[cfg(windows)]
impl WindowsInputModeRestore {
    fn lock(&self) -> MutexGuard<'_, Option<u32>> {
        match self.mode.lock() {
            Ok(mode) => mode,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn activate(
        &self,
        restore_claimed: &AtomicBool,
        operation: impl FnOnce() -> WindowsVirtualTerminalInputSetup,
    ) -> WindowsVirtualTerminalInputSetup {
        let mut restore_mode = self.lock();
        if restore_claimed.load(Ordering::Acquire) {
            return WindowsVirtualTerminalInputSetup::default();
        }
        let setup = operation();
        if restore_mode.is_none() {
            *restore_mode = setup.restore_mode;
        }
        setup
    }

    fn take(&self) -> Option<u32> {
        self.lock().take()
    }
}

#[cfg(windows)]
fn enable_windows_virtual_terminal_input(
    restore_claimed: &AtomicBool,
    restore_mode: &WindowsInputModeRestore,
) -> WindowsVirtualTerminalInputSetup {
    let setup = restore_mode.activate(restore_claimed, enable_windows_virtual_terminal_input_inner);
    if let Some(warning) = setup.warning {
        tracing::warn!("{warning}");
    }
    setup
}

#[cfg(windows)]
fn enable_windows_virtual_terminal_input_inner() -> WindowsVirtualTerminalInputSetup {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
        STD_INPUT_HANDLE,
    };

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return WindowsVirtualTerminalInputSetup {
            warning: Some("failed to get Windows console input handle for VT input"),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return WindowsVirtualTerminalInputSetup {
            warning: Some("failed to read Windows console input mode for VT input"),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    let desired = windows_virtual_terminal_input_mode(mode);
    if desired == mode {
        return WindowsVirtualTerminalInputSetup {
            active: true,
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    if unsafe { SetConsoleMode(handle, desired) } == 0 {
        return WindowsVirtualTerminalInputSetup {
            warning: Some("failed to enable Windows virtual terminal input"),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    let mut applied = 0;
    if unsafe { GetConsoleMode(handle, &mut applied) } == 0 {
        let rollback_failed = unsafe { SetConsoleMode(handle, mode) } == 0;
        return WindowsVirtualTerminalInputSetup {
            restore_mode: rollback_failed.then_some(mode),
            warning: Some(if rollback_failed {
                "failed to verify or restore Windows virtual terminal input mode"
            } else {
                "failed to verify Windows virtual terminal input mode"
            }),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }
    if applied & ENABLE_VIRTUAL_TERMINAL_INPUT == 0 {
        let rollback_failed = unsafe { SetConsoleMode(handle, mode) } == 0;
        return WindowsVirtualTerminalInputSetup {
            restore_mode: rollback_failed.then_some(mode),
            warning: Some(if rollback_failed {
                "Windows virtual terminal input bit did not stick and the prior mode could not be restored"
            } else {
                "Windows virtual terminal input bit did not stick"
            }),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    WindowsVirtualTerminalInputSetup {
        active: true,
        restore_mode: Some(mode),
        warning: None,
    }
}

pub(super) fn is_ssh_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

#[cfg(any(windows, test))]
pub(super) fn windows_virtual_terminal_input_mode(mode: u32) -> u32 {
    mode | 0x0200
}

#[cfg(windows)]
fn restore_windows_input_mode_value(mode: u32) {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE};

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return;
    }
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        tracing::warn!("failed to restore Windows console input mode");
    }
}

pub(super) fn effective_mouse_capture(
    server_enabled: bool,
    direct_attach_preference: bool,
) -> bool {
    server_enabled || direct_attach_preference
}

pub(super) fn effective_sgr_pixel_mouse(
    enabled: bool,
    requested: bool,
    pixel_mouse_available: bool,
) -> bool {
    enabled && requested && pixel_mouse_available
}

#[cfg(any(windows, test))]
fn set_windows_native_mouse_capture<W: io::Write>(
    writer: &mut W,
    enabled: bool,
    sgr_pixels: bool,
    set_console_capture: impl FnOnce(bool) -> io::Result<()>,
) -> io::Result<()> {
    if !enabled {
        crate::terminal_modes::clear_host_mouse_reporting(writer)?;
    }
    set_console_capture(enabled)?;
    if enabled {
        crate::terminal_modes::set_windows_mouse_reporting(writer, true, sgr_pixels)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_uses_vt_mouse_reporting() -> bool {
    is_ssh_session() || crate::platform::windows_virtual_terminal_input_active()
}

pub(super) fn set_mouse_capture(enabled: bool, sgr_pixels: bool) -> io::Result<()> {
    #[cfg(windows)]
    if windows_uses_vt_mouse_reporting() {
        if !enabled {
            crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        }
        return crate::terminal_modes::set_windows_mouse_reporting(
            &mut io::stdout(),
            enabled,
            sgr_pixels,
        );
    }
    #[cfg(windows)]
    return set_windows_native_mouse_capture(&mut io::stdout(), enabled, sgr_pixels, |enabled| {
        if enabled {
            execute!(io::stdout(), EnableMouseCapture)
        } else {
            disable_windows_native_mouse_capture()
        }
    });
    #[cfg(not(windows))]
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    #[cfg(not(windows))]
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)?;
        if sgr_pixels {
            io::stdout().write_all(b"\x1b[?1016h")?;
            io::stdout().flush()?;
        }
        Ok(())
    } else {
        match execute!(io::stdout(), DisableMouseCapture) {
            Ok(()) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(windows)]
fn disable_windows_native_mouse_capture() -> io::Result<()> {
    match execute!(io::stdout(), DisableMouseCapture) {
        Ok(()) => Ok(()),
        Err(err) if err.to_string() == "Initial console modes not set" => Ok(()),
        Err(err) => Err(err),
    }
}

fn restore_terminal_state_once(
    restore_claimed: &AtomicBool,
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    #[cfg(windows)] restore_windows_input_mode: &WindowsInputModeRestore,
) -> io::Result<()> {
    if restore_claimed.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    #[cfg(windows)]
    let restore_windows_input_mode = restore_windows_input_mode.take();
    restore_terminal_state(
        reset_keyboard_enhancements,
        reset_modify_other_keys,
        reset_host_color_scheme_reports,
        #[cfg(windows)]
        restore_windows_input_mode,
    )
}

fn restore_terminal_state(
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    #[cfg(windows)] restore_windows_input_mode: Option<u32>,
) -> io::Result<()> {
    let _ = clear_received_kitty_graphics(&mut io::stdout());

    // Reset modifyOtherKeys if we enabled it.
    if reset_modify_other_keys {
        let _ = io::stdout().write_all(b"\x1b[>4;0m");
        let _ = io::stdout().flush();
    }

    if reset_keyboard_enhancements {
        let _ = pop_keyboard_enhancement_flags();
    }

    let _ = execute!(
        io::stdout(),
        EnableLineWrap,
        DisableFocusChange,
        DisableBracketedPaste
    );
    let _ = set_mouse_capture(false, false);
    #[cfg(windows)]
    if let Some(mode) = restore_windows_input_mode {
        restore_windows_input_mode_value(mode);
    }
    #[cfg(windows)]
    if !is_ssh_session() {
        let _ = disable_windows_native_mouse_capture();
    }

    let restore_result = ratatui::try_restore();
    let postlude_result =
        write_terminal_restore_postlude(&mut io::stdout(), reset_host_color_scheme_reports);

    #[cfg(windows)]
    if windows_win32_input_mode_enabled() {
        let _ = disable_windows_win32_input_mode(&mut io::stdout());
    }

    restore_result.and(postlude_result)
}

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
    )
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(io::stdout(), PopKeyboardEnhancementFlags)
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(any(windows, test))]
pub(super) fn windows_win32_input_mode_enabled() -> bool {
    std::env::var("HERDR_WINDOWS_INPUT_PROBE")
        .map(|probe| probe.eq_ignore_ascii_case("win32"))
        .unwrap_or(true)
}

#[cfg(windows)]
fn enable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001h")?;
    writer.flush()
}

#[cfg(windows)]
fn disable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001l")?;
    writer.flush()
}

/// Host input modes to re-assert after the host may have reset them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HostKeyboardProtocolRefresh {
    /// Some when the host confirmed the kitty keyboard protocol at startup;
    /// the value says whether all keys are currently reported as escapes.
    pub(super) kitty_report_all_keys: Option<bool>,
    pub(super) modify_other_keys_sequence: Option<&'static [u8]>,
    /// Re-enable focus reporting (1004). Callers reacting to a focus-in skip
    /// it: that host still reports focus, and re-enabling from a focus report
    /// could loop on a host that answers the enable with another report.
    pub(super) focus_reporting: bool,
}

pub(super) fn write_host_keyboard_protocol_refresh(
    writer: &mut impl io::Write,
    refresh: HostKeyboardProtocolRefresh,
) -> io::Result<()> {
    if let Some(report_all_keys) = refresh.kitty_report_all_keys {
        // Replace Herdr's stack entry rather than pushing, so repeated
        // refreshes never grow the host's keyboard stack. On a host that
        // reset its stack, the pop is a no-op and the push restores the flags.
        crate::terminal_modes::set_host_kitty_keyboard_report_all(writer, report_all_keys)?;
    }
    if let Some(sequence) = refresh.modify_other_keys_sequence {
        writer.write_all(sequence)?;
    }
    writer.write_all(b"\x1b[?2004h")?;
    if refresh.focus_reporting {
        writer.write_all(b"\x1b[?1004h")?;
    }
    writer.flush()
}

impl TerminalGuard {
    pub(super) fn host_escape_disambiguation_active(&self) -> bool {
        self.host_escape_disambiguation_active
    }

    /// Whether the host answered that it supports SGR pixel mouse reports.
    pub(super) fn host_sgr_pixel_mouse(&self) -> Option<bool> {
        self.host_sgr_pixel_mouse
    }

    pub(super) fn take_buffered_host_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffered_host_input)
    }

    /// Re-sends the keyboard and input modes enabled at setup. A host that
    /// reset its terminal state (a reconnect or window reload) forgets them,
    /// which turns Shift+Enter and Ctrl+Enter into plain Enter.
    pub(super) fn refresh_host_keyboard_protocol(
        &self,
        writer: &mut impl io::Write,
        report_all_keys: bool,
        focus_reporting: bool,
    ) -> io::Result<()> {
        // Direct attach leaves these modes to the attached pane.
        if !self.reset_keyboard_enhancements || self.restore_claimed.load(Ordering::Acquire) {
            return Ok(());
        }
        write_host_keyboard_protocol_refresh(
            writer,
            HostKeyboardProtocolRefresh {
                kitty_report_all_keys: self
                    .host_escape_disambiguation_active
                    .then_some(report_all_keys),
                modify_other_keys_sequence: self.modify_other_keys_sequence,
                focus_reporting,
            },
        )
    }

    #[cfg(windows)]
    pub(super) fn recover_windows_virtual_terminal_input(&self) -> io::Result<()> {
        let active = enable_windows_virtual_terminal_input(
            &self.restore_claimed,
            &self.restore_windows_input_mode,
        )
        .active;
        if active && windows_win32_input_mode_enabled() {
            enable_windows_win32_input_mode(&mut io::stdout())?;
        }
        Ok(())
    }

    /// Captures the restoration state for use by the process panic hook.
    pub(super) fn panic_restore(&self) -> impl Fn() + Send + Sync + 'static {
        let restore_claimed = self.restore_claimed.clone();
        let reset_keyboard_enhancements = self.reset_keyboard_enhancements;
        let reset_modify_other_keys = self.reset_modify_other_keys;
        let reset_host_color_scheme_reports = self.reset_host_color_scheme_reports;
        #[cfg(windows)]
        let restore_windows_input_mode = self.restore_windows_input_mode.clone();
        move || {
            let _ = restore_terminal_state_once(
                &restore_claimed,
                reset_keyboard_enhancements,
                reset_modify_other_keys,
                reset_host_color_scheme_reports,
                #[cfg(windows)]
                &restore_windows_input_mode,
            );
        }
    }

    pub(super) fn restore(mut self) -> io::Result<()> {
        self.restored = true;
        restore_terminal_state_once(
            &self.restore_claimed,
            self.reset_keyboard_enhancements,
            self.reset_modify_other_keys,
            self.reset_host_color_scheme_reports,
            #[cfg(windows)]
            &self.restore_windows_input_mode,
        )
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.restored {
            let _ = restore_terminal_state_once(
                &self.restore_claimed,
                self.reset_keyboard_enhancements,
                self.reset_modify_other_keys,
                self.reset_host_color_scheme_reports,
                #[cfg(windows)]
                &self.restore_windows_input_mode,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_input_mode_restore_tracks_recovery_after_cleanup_callback_creation() {
        let restore_claimed = Arc::new(AtomicBool::new(false));
        let restore_mode = Arc::new(WindowsInputModeRestore::default());
        let callback_claimed = restore_claimed.clone();
        let callback_mode = restore_mode.clone();
        let cleanup = move || {
            if callback_claimed.swap(true, Ordering::AcqRel) {
                None
            } else {
                callback_mode.take()
            }
        };

        let failed =
            restore_mode.activate(&restore_claimed, WindowsVirtualTerminalInputSetup::default);
        assert!(!failed.active);
        let already_active =
            restore_mode.activate(&restore_claimed, || WindowsVirtualTerminalInputSetup {
                active: true,
                ..WindowsVirtualTerminalInputSetup::default()
            });
        assert!(already_active.active);
        restore_mode.activate(&restore_claimed, || WindowsVirtualTerminalInputSetup {
            active: true,
            restore_mode: Some(152),
            warning: None,
        });
        restore_mode.activate(&restore_claimed, || WindowsVirtualTerminalInputSetup {
            active: true,
            restore_mode: Some(999),
            warning: None,
        });

        assert_eq!(cleanup(), Some(152));
        assert_eq!(cleanup(), None);
        let called = AtomicBool::new(false);
        restore_mode.activate(&restore_claimed, || {
            called.store(true, Ordering::Release);
            WindowsVirtualTerminalInputSetup::default()
        });
        assert!(!called.load(Ordering::Acquire));
    }

    #[derive(Clone, Default)]
    struct SharedOutput(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl io::Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(not(windows))]
    fn refresh_test_guard(
        kitty_confirmed: bool,
        client_protocols: bool,
        modify_other_keys_sequence: Option<&'static [u8]>,
    ) -> TerminalGuard {
        TerminalGuard {
            host_escape_disambiguation_active: kitty_confirmed,
            host_sgr_pixel_mouse: None,
            buffered_host_input: Vec::new(),
            reset_keyboard_enhancements: client_protocols,
            reset_modify_other_keys: modify_other_keys_sequence.is_some(),
            modify_other_keys_sequence,
            reset_host_color_scheme_reports: false,
            restore_claimed: Arc::new(AtomicBool::new(false)),
            // Keep Drop from writing restore sequences to the test's stdout.
            restored: true,
        }
    }

    #[cfg(not(windows))]
    fn refresh_bytes(guard: &TerminalGuard, report_all_keys: bool, focus: bool) -> Vec<u8> {
        let mut output = Vec::new();
        guard
            .refresh_host_keyboard_protocol(&mut output, report_all_keys, focus)
            .unwrap();
        output
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_protocol_refresh_resends_the_setup_modes() {
        // Focus-in on a kitty host: replace Herdr's stack entry, keep paste on.
        let kitty = refresh_test_guard(true, true, None);
        assert_eq!(
            refresh_bytes(&kitty, false, false),
            b"\x1b[<1u\x1b[>7u\x1b[?2004h"
        );
        // Resize while a pane wants all keys, on a modifyOtherKeys host too.
        let both = refresh_test_guard(true, true, Some(b"\x1b[>4;1m"));
        assert_eq!(
            refresh_bytes(&both, true, true),
            b"\x1b[<1u\x1b[>31u\x1b[>4;1m\x1b[?2004h\x1b[?1004h"
        );
        // A host that never confirmed the kitty protocol gets no kitty flags.
        let legacy = refresh_test_guard(false, true, None);
        assert_eq!(
            refresh_bytes(&legacy, false, true),
            b"\x1b[?2004h\x1b[?1004h"
        );
        // Direct attach leaves keyboard modes to the attached pane.
        let direct = refresh_test_guard(true, false, None);
        assert!(refresh_bytes(&direct, false, true).is_empty());
        // Nothing is re-enabled once the terminal is being restored.
        kitty.restore_claimed.store(true, Ordering::Release);
        assert!(refresh_bytes(&kitty, false, true).is_empty());
    }

    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn host_keyboard_protocol_refresh_restores_shift_enter_after_host_reset() {
        use crate::input::{KeyboardProtocol, TerminalKey};
        use crossterm::event::{KeyCode, KeyModifiers};

        let guard = refresh_test_guard(true, true, None);
        let shift_enter = TerminalKey::new(KeyCode::Enter, KeyModifiers::SHIFT);
        // libghostty stands in for the host terminal. `reset` models a host
        // that lost its modes, like a reloaded VS Code window (#4985).
        for reset in [false, true] {
            let (host, _rx) =
                crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                    80, 24, 0, b"", 4096,
                );
            // Herdr's setup push.
            host.test_process_pty_bytes(b"\x1b[>7u");
            if reset {
                host.test_process_pty_bytes(b"\x1bc");
                assert_eq!(host.keyboard_protocol(), KeyboardProtocol::Legacy);
                assert_eq!(host.encode_terminal_key(shift_enter.clone()), b"\r");
            }
            for _ in 0..3 {
                host.test_process_pty_bytes(&refresh_bytes(&guard, false, true));
            }
            assert_eq!(
                host.keyboard_protocol(),
                KeyboardProtocol::Kitty { flags: 7 },
                "reset={reset}"
            );
            assert_eq!(
                host.encode_terminal_key(shift_enter.clone()),
                b"\x1b[13;2u",
                "reset={reset}"
            );
            // Refreshes never grow the stack: Herdr's one exit pop restores
            // the host's own keyboard mode.
            host.test_process_pty_bytes(b"\x1b[<1u");
            assert_eq!(
                host.keyboard_protocol(),
                KeyboardProtocol::Legacy,
                "reset={reset}"
            );
        }
    }

    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn host_keyboard_protocol_refresh_keeps_the_outer_mode_across_report_all_toggles() {
        use crate::input::KeyboardProtocol;

        let guard = refresh_test_guard(true, true, None);
        let (host, _rx) = crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80, 24, 0, b"", 4096,
        );
        // Something outside Herdr (a shell or wrapper) already set its own mode.
        host.test_process_pty_bytes(b"\x1b[>1u");
        // Herdr's setup push.
        host.test_process_pty_bytes(b"\x1b[>7u");
        for round in 0..6 {
            let report_all = round % 2 == 0;
            let mut toggle = Vec::new();
            crate::terminal_modes::set_host_kitty_keyboard_report_all(&mut toggle, report_all)
                .unwrap();
            host.test_process_pty_bytes(&toggle);
            for focus in [false, true, false] {
                host.test_process_pty_bytes(&refresh_bytes(&guard, report_all, focus));
            }
            let flags = if report_all { 31 } else { 7 };
            assert_eq!(
                host.keyboard_protocol(),
                KeyboardProtocol::Kitty { flags },
                "round {round}"
            );
        }
        // Herdr's one exit pop restores exactly the outer mode.
        host.test_process_pty_bytes(b"\x1b[<1u");
        assert_eq!(
            host.keyboard_protocol(),
            KeyboardProtocol::Kitty { flags: 1 }
        );
    }

    #[test]
    fn windows_native_mouse_capture_reasserts_final_encoding_after_native_enable() {
        let mut output = SharedOutput::default();
        for sgr_pixels in [false, false, true, false] {
            let start = output.0.borrow().len();
            let native_output = output.clone();
            let native_boundary = std::cell::Cell::new(0);
            set_windows_native_mouse_capture(&mut output, true, sgr_pixels, |enabled| {
                assert!(enabled);
                // ConPTY enables host SGR during native capture, before our VT requests.
                native_output
                    .0
                    .borrow_mut()
                    .extend_from_slice(b"\x1b[?1003h\x1b[?1006h");
                native_boundary.set(native_output.0.borrow().len());
                Ok(())
            })
            .unwrap();

            let bytes = output.0.borrow();
            let before = std::str::from_utf8(&bytes[start..native_boundary.get()]).unwrap();
            let after = std::str::from_utf8(&bytes[native_boundary.get()..]).unwrap();
            assert!(!before.contains("\x1b[?1016l"));
            for reset in ["\x1b[?1005l", "\x1b[?1006l"] {
                assert!(
                    !after.contains(reset),
                    "mouse format reset after native capture (sgr_pixels={sgr_pixels}): {after:?}"
                );
            }
            assert!(after.contains("\x1b[?1003h\x1b[?1006h"));
            assert_eq!(after.contains("\x1b[?1016h"), sgr_pixels);
            if !sgr_pixels {
                assert!(after.starts_with("\x1b[?1016l"));
            }
        }
    }

    #[test]
    fn windows_native_mouse_capture_disable_clears_reporting() {
        let mut output = Vec::new();

        set_windows_native_mouse_capture(&mut output, false, false, |enabled| {
            assert!(!enabled);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            output,
            b"\x1b[?1006l\x1b[?1016l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?9l"
        );
    }
}
