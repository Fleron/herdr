use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{cursor, execute};

use crate::client::host_capabilities::{query_host_capabilities, HostTerminalProbe};
use crate::raw_input::{RawInputByteFramer, RawInputEvent};

pub(super) enum Input {
    Events(Vec<RawInputEvent>),
    Mouse { bytes: Vec<u8>, pixels: bool },
    Resize,
    Exit,
    Error(io::Error),
}

pub(super) struct Host {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    signals: Vec<signal_hook::SigId>,
    probe: HostTerminalProbe,
    mouse_mode: MouseMode,
    keyboard_active: bool,
    color_scheme_reports: bool,
    modify_other_keys: bool,
    line_wrap_disabled: bool,
}

#[derive(Clone)]
pub(super) struct MouseMode {
    supported: bool,
    pixels: Arc<AtomicBool>,
    capture: Arc<AtomicBool>,
}

impl MouseMode {
    pub(super) fn new(probe: HostTerminalProbe) -> Self {
        Self {
            supported: probe.sgr_pixel_mouse == Some(true),
            pixels: Arc::new(AtomicBool::new(false)),
            capture: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn configure(&self, capture: bool, cell_calibrated: bool) -> io::Result<()> {
        self.configure_with_writer(&mut io::stdout().lock(), capture, cell_calibrated)
    }

    fn configure_with_writer(
        &self,
        writer: &mut impl Write,
        capture: bool,
        cell_calibrated: bool,
    ) -> io::Result<()> {
        let pixels = capture && cell_calibrated && self.supported;
        crate::input::host::clear_host_mouse_reporting(writer)?;
        if capture {
            execute!(writer, EnableMouseCapture)?;
            if pixels {
                writer.write_all(b"\x1b[?1016h")?;
            }
        }
        writer.flush()?;
        self.pixels.store(pixels, Ordering::Release);
        self.capture.store(capture, Ordering::Release);
        Ok(())
    }
}

impl Host {
    pub(super) fn enter(
        mouse: bool,
        sender: mpsc::Sender<Input>,
        wake: mpsc::SyncSender<()>,
    ) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut host = Self {
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            signals: Vec::new(),
            probe: HostTerminalProbe::default(),
            mouse_mode: MouseMode::new(HostTerminalProbe::default()),
            keyboard_active: false,
            color_scheme_reports: false,
            modify_other_keys: false,
            line_wrap_disabled: false,
        };
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        crate::input::host::clear_host_mouse_reporting(&mut io::stdout())?;
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
        )?;
        host.keyboard_active = true;
        let (probe, buffered_input) = query_host_capabilities(true);
        host.probe = probe;
        host.mouse_mode = MouseMode::new(probe);
        host.mouse_mode.configure(mouse, false)?;
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        host.color_scheme_reports = true;
        crate::input::host::write_host_color_scheme_report_mode(&mut io::stdout(), true)?;
        if let Some(mode) = crate::input::host_modify_other_keys_mode() {
            host.modify_other_keys = true;
            io::stdout().write_all(mode.set_sequence())?;
            io::stdout().flush()?;
        }
        host.line_wrap_disabled = true;
        execute!(io::stdout(), DisableLineWrap)?;
        let resized = Arc::new(AtomicBool::new(false));
        for signal in [
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGHUP,
        ] {
            host.signals
                .push(signal_hook::flag::register(signal, host.stop.clone())?);
        }
        host.signals.push(signal_hook::flag::register(
            signal_hook::consts::SIGWINCH,
            resized.clone(),
        )?);
        let stop = host.stop.clone();
        let mouse_mode = host.mouse_mode.clone();
        host.thread = Some(
            thread::Builder::new()
                .name("herdr-tui-input".into())
                .spawn(move || {
                    let mut framer = RawInputByteFramer::for_host_input();
                    framer.set_host_escape_disambiguation_active(probe.escape_disambiguation);
                    let mut bytes = [0_u8; 8192];
                    let send = |message| {
                        let sent = sender.send(message).is_ok();
                        let _ = wake.try_send(());
                        sent
                    };
                    let initial = framer.push(&buffered_input);
                    let mut pending_pixels = framer.has_pending_input().then_some(false);
                    if !send_chunks(initial, false, &send) {
                        return;
                    }
                    while !stop.load(Ordering::Relaxed) {
                        if resized.swap(false, Ordering::Relaxed) && !send(Input::Resize) {
                            return;
                        }
                        let timeout = if framer.has_pending_input() {
                            framer
                                .idle_flush_timeout_ms(mouse_mode.capture.load(Ordering::Acquire))
                                .min(100)
                        } else {
                            100
                        };
                        let ready =
                            match crate::platform::poll_fd_readable(libc::STDIN_FILENO, timeout) {
                                Ok(ready) => ready,
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                    continue
                                }
                                Err(error) => {
                                    send(Input::Error(error));
                                    return;
                                }
                            };
                        let pixels = *pending_pixels
                            .get_or_insert_with(|| mouse_mode.pixels.load(Ordering::Acquire));
                        let chunks = if !ready {
                            if framer.has_pending_input() {
                                framer.flush_timeout()
                            } else {
                                Vec::new()
                            }
                        } else {
                            match crate::platform::read_fd(libc::STDIN_FILENO, &mut bytes) {
                                Ok(0) => {
                                    send(Input::Exit);
                                    return;
                                }
                                Ok(count) => framer.push(&bytes[..count]),
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                    continue
                                }
                                Err(error) => {
                                    send(Input::Error(error));
                                    return;
                                }
                            }
                        };
                        if !framer.has_pending_input() {
                            pending_pixels = None;
                        }
                        if !send_chunks(chunks, pixels, &send) {
                            return;
                        }
                    }
                    send(Input::Exit);
                })?,
        );
        Ok(host)
    }

    pub(super) fn probe(&self) -> HostTerminalProbe {
        self.probe
    }

    pub(super) fn mouse_mode(&self) -> MouseMode {
        self.mouse_mode.clone()
    }
}

fn send_chunks(chunks: Vec<Vec<u8>>, pixels: bool, send: &impl Fn(Input) -> bool) -> bool {
    let mut events = Vec::new();
    for bytes in chunks {
        if crate::raw_input::parse_sgr_mouse_report(&bytes).is_some() {
            if !events.is_empty() && !send(Input::Events(std::mem::take(&mut events))) {
                return false;
            }
            if !send(Input::Mouse { bytes, pixels }) {
                return false;
            }
        } else {
            events.extend(crate::raw_input::parse_framed_input(&bytes));
        }
    }
    events.is_empty() || send(Input::Events(events))
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for signal in self.signals.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if self.modify_other_keys {
            let _ = io::stdout().write_all(b"\x1b[>4;0m");
            let _ = io::stdout().flush();
        }
        if self.keyboard_active {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = crate::input::host::clear_host_mouse_reporting(&mut io::stdout());
        if self.line_wrap_disabled {
            let _ = execute!(io::stdout(), EnableLineWrap);
        }
        let _ = execute!(
            io::stdout(),
            DisableFocusChange,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = crate::input::host::write_terminal_restore_postlude(
            &mut io::stdout(),
            self.color_scheme_reports,
        );
        let _ = disable_raw_mode();
    }
}

pub(super) fn copy(text: &[u8]) -> io::Result<()> {
    if crate::selection::write_osc52_bytes(text) {
        Ok(())
    } else {
        Err(io::Error::other("Could not write to the clipboard"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_mouse_requires_both_host_support_and_calibration_and_resets_on_disable() {
        for supported in [None, Some(false), Some(true)] {
            let mode = MouseMode::new(HostTerminalProbe {
                sgr_pixel_mouse: supported,
                ..HostTerminalProbe::default()
            });
            let mut output = Vec::new();
            mode.configure_with_writer(&mut output, true, false)
                .unwrap();
            assert!(!output.windows(8).any(|bytes| bytes == b"\x1b[?1016h"));
            assert!(!mode.pixels.load(Ordering::Acquire));
            output.clear();
            mode.configure_with_writer(&mut output, true, true).unwrap();
            assert_eq!(
                output.windows(8).any(|bytes| bytes == b"\x1b[?1016h"),
                supported == Some(true)
            );
            assert_eq!(mode.pixels.load(Ordering::Acquire), supported == Some(true));
            output.clear();
            mode.configure_with_writer(&mut output, false, true)
                .unwrap();
            assert!(!mode.pixels.load(Ordering::Acquire));
            assert!(!mode.capture.load(Ordering::Acquire));
            assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?1016l"));
        }
    }

    #[test]
    fn mouse_reports_keep_their_captured_units_and_order_between_keys() {
        let messages = std::cell::RefCell::new(Vec::new());
        let mut framer = RawInputByteFramer::for_host_input();
        let chunks = framer.push(b"a\x1b[<0;11;12Mb");
        assert!(send_chunks(chunks, true, &|input| {
            messages.borrow_mut().push(input);
            true
        }));
        let messages = messages.into_inner();
        assert!(
            matches!(&messages[0], Input::Events(events) if matches!(events.as_slice(), [RawInputEvent::Key(key)] if key.code == crossterm::event::KeyCode::Char('a')))
        );
        assert!(
            matches!(&messages[1], Input::Mouse {bytes, pixels: true} if bytes == b"\x1b[<0;11;12M")
        );
        assert!(
            matches!(&messages[2], Input::Events(events) if matches!(events.as_slice(), [RawInputEvent::Key(key)] if key.code == crossterm::event::KeyCode::Char('b')))
        );
        assert_eq!(messages.len(), 3);
    }
}
