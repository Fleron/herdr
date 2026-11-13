use std::io::{self, Write};

use crate::client::frame::render_ansi::{self, BlitEncoder};
use crate::client::frame_output::{write_composed_frame, ComposedFrame};
use crate::client::image_files::FileTransport;
use crate::config::{Config, HostCursorModeConfig};

pub(super) struct Output {
    encoder: BlitEncoder,
    files: FileTransport,
    repaint: bool,
    draw_cursor: bool,
    mouse_capture: bool,
    title: Option<String>,
}

impl Output {
    pub fn new(config: &Config) -> Self {
        Self {
            encoder: BlitEncoder::new(),
            files: FileTransport::from_environment(),
            repaint: true,
            draw_cursor: draw_cursor(config.ui.host_cursor),
            mouse_capture: config.ui.mouse_capture,
            title: None,
        }
    }

    pub fn configure(&mut self, config: &Config) {
        self.draw_cursor = draw_cursor(config.ui.host_cursor);
        self.mouse_capture = config.ui.mouse_capture;
        self.repaint = true;
    }

    pub fn invalidate(&mut self) {
        self.repaint = true;
    }

    pub fn graphics_cleanup(&mut self, bytes: &[u8]) -> io::Result<()> {
        if !bytes.is_empty() {
            let mut stdout = io::stdout().lock();
            stdout.write_all(bytes)?;
            stdout.flush()?;
        }
        Ok(())
    }

    pub fn present(&mut self, frame: ComposedFrame, title: Option<&str>) -> io::Result<()> {
        let ComposedFrame {
            mut frame,
            graphics,
        } = frame;
        if self.mouse_capture {
            frame.hyperlinks.clear();
        }
        if self.draw_cursor {
            frame = render_ansi::frame_with_drawn_cursor(frame);
        }
        let encoded = if self.draw_cursor {
            self.encoder
                .encode_with_suppressed_visible_cursor(&frame, self.repaint)
        } else {
            self.encoder.encode(&frame, self.repaint)
        };
        let mut stdout = io::stdout().lock();
        let title = title.and_then(crate::config::sanitize_window_title_text);
        let changed_title = title
            .as_ref()
            .filter(|title| self.title.as_ref() != Some(title));
        let result = (|| {
            write_composed_frame(&mut stdout, &encoded.bytes, &graphics, &mut self.files)?;
            if let Some(title) = changed_title {
                crate::terminal_effects::write_window_title(&mut stdout, Some(title))?;
            }
            stdout.flush()
        })();
        if let Err(error) = result {
            self.repaint = true;
            return Err(error);
        }
        self.encoder.commit(frame, encoded);
        if let Some(title) = title {
            self.title = Some(title);
        }
        self.repaint = false;
        Ok(())
    }
}

fn draw_cursor(mode: HostCursorModeConfig) -> bool {
    match mode {
        HostCursorModeConfig::Auto => crate::platform::should_draw_host_cursor_by_default(),
        HostCursorModeConfig::Native => false,
        HostCursorModeConfig::Drawn => true,
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = crate::client::frame_output::clear_received_kitty_graphics(io::stdout());
    }
}
