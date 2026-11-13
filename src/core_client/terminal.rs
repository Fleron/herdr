use std::io;
use std::sync::mpsc::{self, Receiver};

use ghostty_vt::{CellWide, Dirty, RenderCursor, RenderState, RowCells, RowIterator};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: usize,
    pub col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
}

impl Selection {
    pub(crate) fn ordered(self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub(crate) fn contains(self, point: Point) -> bool {
        let (start, end) = self.ordered();
        point >= start && point <= end
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollMetrics {
    pub offset_from_bottom: usize,
    pub max_offset_from_bottom: usize,
    pub viewport_rows: usize,
}

type HyperlinkCell = ((u16, u16), String, String);

pub struct TerminalView {
    terminal: ghostty_vt::Terminal,
    render_state: RenderState,
    row_iterator: RowIterator,
    row_cells: RowCells,
    key_encoder: ghostty_vt::KeyEncoder,
    replies: Receiver<Vec<u8>>,
    buffer: Buffer,
    cursor: Option<RenderCursor>,
    viewport_offset: usize,
    pending: bool,
    max_scrollback_bytes: usize,
    initial_colors: ghostty_vt::RenderColors,
    blank_style: Style,
    grapheme_bytes: Vec<u8>,
    symbol: String,
    cell_size: (u32, u32),
    scroll: ScrollMetrics,
    mouse_reporting: bool,
    sgr_pixel_mouse: bool,
    alternate_screen: bool,
    hyperlink_rows: Vec<Vec<HyperlinkCell>>,
    hyperlinks: Vec<HyperlinkCell>,
}

impl TerminalView {
    pub fn new(cols: u16, rows: u16, max_scrollback_bytes: usize) -> io::Result<Self> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let mut terminal = ghostty_vt::Terminal::new(cols, rows, max_scrollback_bytes)
            .map_err(io::Error::other)?;
        terminal
            .enable_inline_kitty_graphics()
            .map_err(io::Error::other)?;
        let (reply_tx, replies) = mpsc::channel();
        terminal
            .set_write_pty_callback(move |bytes| {
                let _ = reply_tx.send(bytes.to_vec());
            })
            .map_err(io::Error::other)?;
        let mut render_state = RenderState::new().map_err(io::Error::other)?;
        render_state.update(&terminal).map_err(io::Error::other)?;
        let initial_colors = render_state.colors().map_err(io::Error::other)?;
        Ok(Self {
            terminal,
            render_state,
            row_iterator: RowIterator::new().map_err(io::Error::other)?,
            row_cells: RowCells::new().map_err(io::Error::other)?,
            key_encoder: ghostty_vt::KeyEncoder::new().map_err(io::Error::other)?,
            replies,
            buffer: Buffer::empty(Rect::new(0, 0, cols, rows)),
            cursor: None,
            viewport_offset: 0,
            pending: true,
            max_scrollback_bytes,
            initial_colors,
            blank_style: Style::default(),
            grapheme_bytes: Vec::new(),
            symbol: String::new(),
            cell_size: (1, 1),
            scroll: ScrollMetrics::default(),
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen: false,
            hyperlink_rows: vec![Vec::new(); usize::from(rows)],
            hyperlinks: Vec::new(),
        })
    }

    pub fn terminal(&self) -> &ghostty_vt::Terminal {
        &self.terminal
    }

    pub fn size(&self) -> (u16, u16) {
        (self.buffer.area.width, self.buffer.area.height)
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.terminal.write(bytes);
            // Core events own CWD; the viewer does not retain the parser's duplicate reports.
            drop(self.terminal.take_pwd_changes());
            self.pending = true;
        }
    }

    pub fn reset(&mut self) -> io::Result<()> {
        let (cols, rows) = self.size();
        let mut view = Self::new(cols, rows, self.max_scrollback_bytes)?;
        view.set_cell_size(self.cell_size.0, self.cell_size.1)?;
        *self = view;
        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<bool> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.size() == (cols, rows) {
            return Ok(false);
        }
        self.terminal
            .resize(cols, rows, self.cell_size.0, self.cell_size.1)
            .map_err(io::Error::other)?;
        self.buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        self.hyperlink_rows.resize_with(usize::from(rows), Vec::new);
        for row in &mut self.hyperlink_rows {
            row.clear();
        }
        self.hyperlinks.clear();
        self.render_state
            .set_dirty(Dirty::Full)
            .map_err(io::Error::other)?;
        self.pending = true;
        Ok(true)
    }

    pub fn set_cell_size(&mut self, width_px: u32, height_px: u32) -> io::Result<bool> {
        let cell_size = (width_px.max(1), height_px.max(1));
        if self.cell_size == cell_size {
            return Ok(false);
        }
        let (cols, rows) = self.size();
        self.terminal
            .resize(cols, rows, cell_size.0, cell_size.1)
            .map_err(io::Error::other)?;
        self.cell_size = cell_size;
        self.pending = true;
        Ok(true)
    }

    pub fn prepare(&mut self) -> io::Result<bool> {
        if !self.pending
            || self
                .terminal
                .mode_get(ghostty_vt::MODE_SYNCHRONIZED_OUTPUT)
                .map_err(io::Error::other)?
        {
            return Ok(false);
        }
        self.render_state
            .update(&self.terminal)
            .map_err(io::Error::other)?;
        let previous_cursor = self.cursor;
        let previous_presentation = (
            self.scroll,
            self.mouse_reporting,
            self.sgr_pixel_mouse,
            self.alternate_screen,
        );
        self.cursor = Some(self.render_state.cursor().map_err(io::Error::other)?);
        let scrollbar = self.terminal.scrollbar().map_err(io::Error::other)?;
        self.viewport_offset = scrollbar.offset;
        self.scroll = ScrollMetrics {
            offset_from_bottom: scrollbar
                .total
                .saturating_sub(scrollbar.offset + scrollbar.len),
            max_offset_from_bottom: scrollbar.total.saturating_sub(scrollbar.len),
            viewport_rows: scrollbar.len,
        };
        self.mouse_reporting = self
            .terminal
            .mouse_tracking_enabled()
            .map_err(io::Error::other)?;
        self.sgr_pixel_mouse = self
            .terminal
            .mode_get(ghostty_vt::MODE_MOUSE_SGR_PIXELS)
            .map_err(io::Error::other)?;
        self.alternate_screen = self.terminal.active_screen().map_err(io::Error::other)?
            == ghostty_vt::ActiveScreen::Alternate;
        let dirty = self.render_state.dirty().map_err(io::Error::other)?;
        if dirty != Dirty::Clean {
            let colors = self.render_state.colors().map_err(io::Error::other)?;
            let default_fg = if colors.foreground == self.initial_colors.foreground {
                Color::Reset
            } else {
                color(colors.foreground)
            };
            let default_bg = if colors.background == self.initial_colors.background {
                Color::Reset
            } else {
                color(colors.background)
            };
            self.blank_style = Style::default().fg(default_fg).bg(default_bg);
            let mut rows = self
                .render_state
                .populate_row_iterator(&mut self.row_iterator)
                .map_err(io::Error::other)?;
            let mut next_row = 0;
            loop {
                let y = match dirty {
                    Dirty::Partial => rows.next_dirty(),
                    _ if next_row < self.buffer.area.height && rows.next() => {
                        let y = next_row;
                        next_row += 1;
                        Some(y)
                    }
                    _ => None,
                };
                let Some(y) = y.filter(|y| *y < self.buffer.area.height) else {
                    break;
                };
                let mut cells = rows
                    .populate_cells(&mut self.row_cells)
                    .map_err(io::Error::other)?;
                let hyperlinks = &mut self.hyperlink_rows[usize::from(y)];
                hyperlinks.clear();
                let mut x = 0;
                while x < self.buffer.area.width && cells.next() {
                    let output = &mut self.buffer[(x, y)];
                    output.reset();
                    if cells.is_default_blank().map_err(io::Error::other)? {
                        output.set_symbol(" ").set_style(self.blank_style);
                    } else {
                        let basic = cells.basic_data().map_err(io::Error::other)?;
                        symbol_into(
                            &cells,
                            basic.wide,
                            &mut self.grapheme_bytes,
                            &mut self.symbol,
                        )?;
                        output.set_symbol(&self.symbol).set_style(cell_style(
                            &cells,
                            basic.style,
                            &colors,
                            &self.initial_colors,
                            default_fg,
                            default_bg,
                        )?);
                        if basic.has_hyperlink {
                            if let Some(uri) = self
                                .terminal
                                .viewport_hyperlink_uri(x, u32::from(y))
                                .map_err(io::Error::other)?
                            {
                                hyperlinks.push(((x, y), self.symbol.clone(), uri));
                            }
                        }
                    }
                    x += 1;
                }
                while x < self.buffer.area.width {
                    self.buffer[(x, y)].reset();
                    self.buffer[(x, y)].set_style(self.blank_style);
                    x += 1;
                }
                rows.clear_dirty().map_err(io::Error::other)?;
            }
            self.hyperlinks.clear();
            for row in &self.hyperlink_rows {
                self.hyperlinks.extend_from_slice(row);
            }
        }
        self.render_state.clean().map_err(io::Error::other)?;
        self.pending = false;
        Ok(dirty != Dirty::Clean
            || self.cursor != previous_cursor
            || previous_presentation
                != (
                    self.scroll,
                    self.mouse_reporting,
                    self.sgr_pixel_mouse,
                    self.alternate_screen,
                ))
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn presentation_pending(&self) -> bool {
        self.pending
    }

    pub fn blank_style(&self) -> Style {
        self.blank_style
    }

    pub fn cursor(&self) -> Option<(u16, u16)> {
        let cursor = self.cursor.filter(|cursor| cursor.visible)?;
        let point = cursor.viewport?;
        (point.x < self.buffer.area.width && point.y < self.buffer.area.height)
            .then_some((point.x, point.y))
    }

    pub fn cursor_state(&self) -> Option<RenderCursor> {
        self.cursor
    }

    pub fn scroll_metrics(&self) -> ScrollMetrics {
        self.scroll
    }

    pub fn mouse_reporting(&self) -> bool {
        self.mouse_reporting
    }

    pub fn sgr_pixel_mouse(&self) -> bool {
        self.sgr_pixel_mouse
    }

    pub fn alternate_screen_active(&self) -> bool {
        self.alternate_screen
    }

    pub fn cell_size(&self) -> (u32, u32) {
        self.cell_size
    }

    pub fn pixel_size(&self) -> (u32, u32) {
        (
            u32::from(self.buffer.area.width).saturating_mul(self.cell_size.0),
            u32::from(self.buffer.area.height).saturating_mul(self.cell_size.1),
        )
    }

    pub fn hyperlinks(&self) -> &[HyperlinkCell] {
        &self.hyperlinks
    }

    pub fn keyboard_negotiated_nothing(&self) -> io::Result<bool> {
        Ok(self
            .terminal
            .kitty_keyboard_flags()
            .map_err(io::Error::other)?
            == 0
            && !self
                .terminal
                .modify_other_keys_enabled()
                .map_err(io::Error::other)?)
    }

    pub fn reports_all_keys(&self) -> io::Result<bool> {
        self.terminal
            .kitty_keyboard_flags()
            .map(|flags| flags & 0b1000 != 0)
            .map_err(io::Error::other)
    }

    pub fn encode_event(&mut self, event: &ghostty_vt::KeyEvent) -> io::Result<Vec<u8>> {
        self.key_encoder.set_from_terminal(&self.terminal);
        self.key_encoder.set_macos_option_as_alt(true);
        self.key_encoder.encode(event).map_err(io::Error::other)
    }

    pub fn encode_paste(&self, text: &str) -> io::Result<Vec<u8>> {
        if self
            .terminal
            .mode_get(ghostty_vt::MODE_BRACKETED_PASTE)
            .map_err(io::Error::other)?
        {
            Ok([b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~"].concat())
        } else {
            Ok(text.as_bytes().to_vec())
        }
    }

    pub fn take_replies(&self) -> Vec<Vec<u8>> {
        self.replies.try_iter().collect()
    }

    pub fn take_clipboard_writes(&mut self) -> Vec<Vec<u8>> {
        self.terminal.take_clipboard_writes()
    }

    pub fn take_bell_count(&mut self) -> u16 {
        self.terminal.take_bell_count()
    }

    pub fn scroll(&mut self, lines: isize) {
        self.terminal.scroll_viewport_delta(lines);
        self.pending = true;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.terminal.scroll_viewport_bottom();
        self.pending = true;
    }

    pub fn scroll_to_offset(&mut self, offset_from_bottom: u64) -> io::Result<()> {
        let scroll = self.terminal.scrollbar().map_err(io::Error::other)?;
        let max = scroll.total.saturating_sub(scroll.len);
        let offset = usize::try_from(offset_from_bottom).unwrap_or(usize::MAX);
        self.terminal
            .scroll_viewport_row(max.saturating_sub(offset.min(max)));
        self.pending = true;
        Ok(())
    }

    pub fn clear_screen(&mut self) -> io::Result<()> {
        if self.terminal.clear_screen() {
            self.render_state
                .set_dirty(Dirty::Full)
                .map_err(io::Error::other)?;
            self.pending = true;
        }
        Ok(())
    }

    pub fn point_at(&self, col: u16, row: u16) -> Point {
        Point {
            row: self.viewport_offset + usize::from(row.min(self.buffer.area.height - 1)),
            col: col.min(self.buffer.area.width - 1),
        }
    }

    pub fn selected_text(&self, selection: Selection) -> io::Result<String> {
        let (start, end) = selection.ordered();
        let start_row = u32::try_from(start.row).map_err(io::Error::other)?;
        let end_row = u32::try_from(end.row).map_err(io::Error::other)?;
        self.terminal
            .read_text_screen((start.col, start_row), (end.col, end_row), false)
            .map_err(io::Error::other)
    }
}

fn color(color: ghostty_vt::RgbColor) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

fn cell_style(
    cell: &ghostty_vt::RowCellIter<'_>,
    style: ghostty_vt::CellStyle,
    colors: &ghostty_vt::RenderColors,
    initial: &ghostty_vt::RenderColors,
    default_fg: Color,
    default_bg: Color,
) -> io::Result<Style> {
    let cell_color = |value| match value {
        ghostty_vt::CellColor::Palette(index) => {
            if colors.palette[usize::from(index)] == initial.palette[usize::from(index)] {
                Color::Indexed(index)
            } else {
                color(colors.palette[usize::from(index)])
            }
        }
        ghostty_vt::CellColor::Rgb(value) => color(value),
    };
    let mut fg = style.fg_color.map(cell_color).unwrap_or(default_fg);
    let mut bg = cell
        .content_bg_color()
        .map_err(io::Error::other)?
        .or(style.bg_color)
        .map(cell_color)
        .unwrap_or(default_bg);
    if style.invisible {
        fg = bg;
    }
    if style.inverse {
        if fg == Color::Reset {
            fg = color(colors.foreground);
        }
        if bg == Color::Reset {
            bg = color(colors.background);
        }
        std::mem::swap(&mut fg, &mut bg);
    }
    let mut modifiers = Modifier::empty();
    for (enabled, modifier) in [
        (style.bold, Modifier::BOLD),
        (style.italic, Modifier::ITALIC),
        (style.faint, Modifier::DIM),
        (style.blink, Modifier::SLOW_BLINK),
        (style.underlined, Modifier::UNDERLINED),
        (style.strikethrough, Modifier::CROSSED_OUT),
    ] {
        if enabled {
            modifiers |= modifier;
        }
    }
    let mut result = Style::default().fg(fg).bg(bg).add_modifier(modifiers);
    if let Some(underline) = style.underline_color {
        result = result.underline_color(cell_color(underline));
    }
    Ok(result)
}

fn symbol_into(
    cells: &ghostty_vt::RowCellIter<'_>,
    wide: CellWide,
    bytes: &mut Vec<u8>,
    symbol: &mut String,
) -> io::Result<()> {
    symbol.clear();
    match wide {
        CellWide::SpacerTail => return Ok(()),
        CellWide::SpacerHead => symbol.push(' '),
        CellWide::Narrow | CellWide::Wide => {
            cells
                .grapheme_text_into(bytes, symbol)
                .map_err(io::Error::other)?;
            if symbol.is_empty() {
                symbol.push(' ');
            }
        }
    }
    let expected_width = if wide == CellWide::Wide { 2 } else { 1 };
    let actual_width = symbol.width();
    let mut chars = symbol.chars();
    let halfwidth_voiced = chars
        .next()
        .is_some_and(|c| ('\u{ff66}'..='\u{ff9d}').contains(&c))
        && matches!(chars.next(), Some('\u{ff9e}' | '\u{ff9f}'))
        && chars.next().is_none();
    if actual_width != expected_width
        && !(wide == CellWide::Narrow && actual_width == 2)
        && !(wide == CellWide::Wide && halfwidth_voiced)
    {
        symbol.clear();
        symbol.push_str(if wide == CellWide::Wide { "  " } else { " " });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HISTORY_BYTES: usize = 1_000_000;

    fn view(cols: u16, rows: u16) -> TerminalView {
        TerminalView::new(cols, rows, HISTORY_BYTES).expect("terminal view")
    }

    fn row(view: &TerminalView, y: u16) -> String {
        (0..view.size().0)
            .map(|x| view.buffer()[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn terminal_resize_and_reconnect_reset_preserve_host_pixel_calibration_for_inline_images() {
        use base64::Engine as _;

        let mut view = view(12, 4);
        assert!(view.set_cell_size(8, 16).expect("cell size"));
        assert!(!view.set_cell_size(8, 16).expect("unchanged cell size"));
        view.resize(20, 8).expect("terminal size");
        view.reset().expect("reconnect reset");
        let pixels = [255, 0, 0, 255].repeat(16 * 16);
        let payload = base64::engine::general_purpose::STANDARD.encode(pixels);
        view.feed(format!("\x1b_Ga=T,f=32,t=d,i=7,p=3,s=16,v=16,q=2;{payload}\x1b\\").as_bytes());
        let placements = view.terminal().kitty_image_placements().expect("images");
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].render.grid_cols, 2);
        assert_eq!(placements[0].render.grid_rows, 1);
    }

    #[test]
    fn presentation_metadata_tracks_links_scrollback_and_terminal_modes() {
        let mut view = view(12, 3);
        view.set_cell_size(8, 16).unwrap();
        view.feed(b"\x1b]8;;https://example.com/path\x1b\\go now\x1b]8;;\x1b\\ plain");
        view.prepare().unwrap();
        assert_eq!(
            view.hyperlinks()
                .iter()
                .map(|(_, symbol, _)| symbol.as_str())
                .collect::<String>(),
            "go now"
        );
        assert!(view
            .hyperlinks()
            .iter()
            .all(|(_, _, uri)| uri == "https://example.com/path"));
        let links = view.hyperlinks().to_vec();
        assert!(!view.prepare().unwrap());
        assert_eq!(view.hyperlinks(), links);
        view.feed(b"\r\x1b[2Kplain");
        view.prepare().unwrap();
        assert!(view.hyperlinks().is_empty());
        for _ in 0..8 {
            view.feed(b"\r\nhistory");
        }
        view.prepare().unwrap();
        let bottom = view.scroll_metrics();
        assert!(bottom.max_offset_from_bottom > 0);
        assert_eq!(bottom.offset_from_bottom, 0);
        assert_eq!(bottom.viewport_rows, 3);
        view.scroll(-2);
        view.prepare().unwrap();
        assert_eq!(view.scroll_metrics().offset_from_bottom, 2);
        view.feed(b"\x1b[?1000h\x1b[?1016h\x1b[?1049h");
        view.prepare().unwrap();
        assert!(view.mouse_reporting());
        assert!(view.sgr_pixel_mouse());
        assert!(view.alternate_screen_active());
        assert_eq!(view.pixel_size(), (96, 48));
        view.resize(14, 4).unwrap();
        view.prepare().unwrap();
        assert_eq!(view.pixel_size(), (112, 64));
        view.feed(b"\x1b[?1000l\x1b[?1016l\x1b[?1049l");
        view.prepare().unwrap();
        assert!(!view.mouse_reporting());
        assert!(!view.sgr_pixel_mouse());
        assert!(!view.alternate_screen_active());
    }

    #[test]
    fn live_inline_graphics_survive_presentation_and_delete_without_touching_text() {
        let mut view = view(12, 3);
        view.feed(b"before\r\n\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=2,r=1,q=2;/wAA/w==\x1b\\after");
        view.prepare().expect("paint");
        let placements = view.terminal().kitty_image_placements().expect("images");
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].image_id, 7);
        assert_eq!(placements[0].data, [255, 0, 0, 255]);
        assert_eq!(row(&view, 0), "before");
        assert!(row(&view, 2).contains("after") || row(&view, 1).contains("after"));
        assert!(!view.prepare().expect("idle paint"));
        assert_eq!(
            view.terminal()
                .kitty_image_placements()
                .expect("retained images"),
            placements
        );
        view.feed(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        view.prepare().expect("delete paint");
        assert!(view
            .terminal()
            .kitty_image_placements()
            .expect("deleted images")
            .is_empty());
        assert_eq!(row(&view, 0), "before");
    }

    #[test]
    fn split_utf8_and_escape_sequences_paint_the_actual_terminal() {
        let mut view = view(12, 3);
        view.feed(b"\x1b[31");
        view.feed(b"mA\xe7\x95");
        view.feed(b"\x8c\x1b[0m");
        view.prepare().expect("paint");
        assert_eq!(row(&view, 0), "A界");
        assert_eq!(view.buffer()[(0, 0)].fg, ratatui::style::Color::Indexed(1));
        assert_eq!(view.buffer()[(1, 0)].symbol(), "界");
        assert_eq!(view.buffer()[(2, 0)].symbol(), "");
        assert_eq!(view.cursor(), Some((3, 0)));
    }

    #[test]
    fn alternate_screen_resize_and_cursor_visibility_follow_program_output() {
        let mut view = view(12, 3);
        view.feed(b"shell\x1b[?1049h\x1b[2J\x1b[Heditor\x1b[?25l");
        view.prepare().expect("paint alternate screen");
        assert_eq!(row(&view, 0), "editor");
        assert_eq!(view.cursor(), None);
        assert!(view.resize(20, 5).expect("resize"));
        view.feed(b"\x1b[?1049l\x1b[?25h");
        view.prepare().expect("paint primary screen");
        assert!(row(&view, 0).starts_with("shell"));
        assert_eq!(view.size(), (20, 5));
        assert!(view.cursor().is_some());
    }

    #[test]
    fn idle_and_hidden_views_do_not_repaint() {
        let mut view = view(12, 3);
        assert!(view.prepare().expect("initial paint"));
        assert!(!view.prepare().expect("idle"));
        view.feed(b"hidden");
        assert_eq!(row(&view, 0), "");
        assert!(view.prepare().expect("became visible"));
        assert_eq!(row(&view, 0), "hidden");
        assert!(!view.prepare().expect("idle again"));
    }

    #[test]
    fn synchronized_output_keeps_the_last_complete_paint() {
        let mut view = view(12, 3);
        view.feed(b"before");
        view.prepare().expect("initial paint");
        view.feed(b"\x1b[?2026h\x1b[Hafter ");
        assert!(!view.prepare().expect("held paint"));
        assert_eq!(row(&view, 0), "before");
        view.feed(b"\x1b[?2026l");
        assert!(view.prepare().expect("complete paint"));
        assert_eq!(row(&view, 0), "after");
    }

    #[test]
    fn scrollback_selection_reads_graphemes_and_unwraps_soft_lines() {
        let mut view = view(8, 2);
        view.feed("ab界efghij\r\nnext\r\nlast".as_bytes());
        view.scroll(-20);
        view.prepare().expect("paint history");
        let selected = Selection {
            anchor: view.point_at(0, 0),
            head: view.point_at(1, 1),
        };
        assert_eq!(view.selected_text(selected).expect("copy"), "ab界efghij");
        view.scroll_to_bottom();
        view.prepare().expect("paint bottom");
        assert_eq!(row(&view, 1), "last");
    }

    #[test]
    fn generated_replies_remain_separate_from_paste_and_keys() {
        let mut view = view(12, 3);
        view.feed(b"hello\x1b[6n");
        assert_eq!(view.take_replies().concat(), b"\x1b[1;6R");
        assert!(view.take_replies().is_empty());
        assert_eq!(view.encode_paste("a\nb").expect("plain paste"), b"a\nb");
        view.feed(b"\x1b[?2004h");
        assert_eq!(
            view.encode_paste("a\nb").expect("bracketed paste"),
            b"\x1b[200~a\nb\x1b[201~"
        );
        let mut key = ghostty_vt::KeyEvent::new().expect("key");
        key.set_key(ghostty_vt::KEY_A);
        key.set_action(ghostty_vt::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS);
        key.set_unshifted_codepoint(u32::from('a'));
        key.set_utf8("a");
        key.set_mods(ghostty_vt::MOD_CTRL);
        assert_eq!(view.encode_event(&key).expect("control key"), b"\x01");
        assert!(view.take_replies().is_empty());
    }
}
