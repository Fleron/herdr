use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TerminalTextPoint {
    pub row: u32,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalTextMatch {
    pub start: TerminalTextPoint,
    pub end: TerminalTextPoint,
    pub source_fingerprint: u64,
    pub scan_cols: u16,
    pub scan_screen: crate::ghostty::ActiveScreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalSearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalSearchWindow {
    pub matches: Vec<TerminalTextMatch>,
    pub current: Option<usize>,
    pub current_global: Option<usize>,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalWordMotion {
    NextStart,
    PreviousStart,
    NextEnd,
    NextBigStart,
    PreviousBigStart,
    NextBigEnd,
}

const COPY_MODE_WORD_SEPARATORS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextClass {
    Whitespace,
    Separator,
    Word,
}

#[derive(Debug)]
struct TextAtom {
    point: Option<TerminalTextPoint>,
    end_col: u16,
    class: TextClass,
}

#[derive(Debug)]
struct TextSpan {
    byte_start: usize,
    byte_end: usize,
    start: TerminalTextPoint,
    end: TerminalTextPoint,
}

#[derive(Debug, Default)]
struct LogicalTextLine {
    text: String,
    spans: Vec<TextSpan>,
}

#[derive(Debug)]
pub(crate) struct RetainedTextBuffer {
    cols: u16,
    lines: Vec<LogicalTextLine>,
    atoms: Vec<TextAtom>,
}

impl RetainedTextBuffer {
    #[cfg(test)]
    pub(crate) fn new(cols: u16, rows: Vec<crate::ghostty::ScreenTextRow>) -> Self {
        Self::build(cols, rows, 0, true, true)
    }

    pub(crate) fn new_search(
        cols: u16,
        rows: Vec<crate::ghostty::ScreenTextRow>,
        row_offset: u32,
    ) -> Self {
        Self::build(cols, rows, row_offset, true, false)
    }

    pub(crate) fn new_words(
        cols: u16,
        rows: Vec<crate::ghostty::ScreenTextRow>,
        row_offset: u32,
    ) -> Self {
        Self::build(cols, rows, row_offset, false, true)
    }

    fn build(
        cols: u16,
        rows: Vec<crate::ghostty::ScreenTextRow>,
        row_offset: u32,
        build_lines: bool,
        build_atoms: bool,
    ) -> Self {
        let mut lines = Vec::new();
        let mut line = LogicalTextLine::default();
        let mut atoms: Vec<TextAtom> = Vec::new();

        for (row_idx, row) in rows.into_iter().enumerate() {
            let Some(row_idx) = u32::try_from(row_idx).ok() else {
                break;
            };
            let row_idx = row_offset.saturating_add(row_idx);
            for (col, cell) in row.cells.into_iter().enumerate() {
                let Ok(col) = u16::try_from(col) else {
                    break;
                };
                if cell.wide == crate::ghostty::CellWide::SpacerTail {
                    continue;
                }
                if cell.wide == crate::ghostty::CellWide::SpacerHead {
                    if build_atoms {
                        atoms.push(TextAtom {
                            point: Some(TerminalTextPoint { row: row_idx, col }),
                            end_col: col,
                            class: atoms
                                .last()
                                .map_or(TextClass::Whitespace, |atom| atom.class),
                        });
                    }
                    continue;
                }
                let width = if cell.wide == crate::ghostty::CellWide::Wide {
                    2
                } else {
                    1
                };
                let text = terminal_cell_text(&cell.graphemes);
                let start = TerminalTextPoint { row: row_idx, col };
                let end = TerminalTextPoint {
                    row: row_idx,
                    col: col.saturating_add(width - 1),
                };
                if build_lines {
                    let byte_start = line.text.len();
                    line.text.push_str(&text);
                    let byte_end = line.text.len();
                    line.spans.push(TextSpan {
                        byte_start,
                        byte_end,
                        start,
                        end,
                    });
                }
                if build_atoms {
                    atoms.push(TextAtom {
                        point: Some(start),
                        end_col: end.col,
                        class: text_class(&text),
                    });
                }
            }

            if row.soft_wrapped {
                continue;
            }

            if build_lines {
                let trimmed_len = line.text.trim_end().len();
                while line
                    .spans
                    .last()
                    .is_some_and(|span| span.byte_start >= trimmed_len)
                {
                    line.spans.pop();
                }
                line.text.truncate(trimmed_len);
                lines.push(std::mem::take(&mut line));
            }
            if build_atoms {
                atoms.push(TextAtom {
                    point: None,
                    end_col: 0,
                    class: TextClass::Whitespace,
                });
            }
        }

        if build_lines && (!line.text.is_empty() || !line.spans.is_empty()) {
            lines.push(line);
        }

        Self { cols, lines, atoms }
    }

    pub(crate) fn search_window(
        &self,
        query: &str,
        case_sensitive: bool,
        active_screen: crate::ghostty::ActiveScreen,
        direction: TerminalSearchDirection,
        cursor: TerminalTextPoint,
        previous: Option<(TerminalTextPoint, TerminalTextPoint)>,
        limit: usize,
    ) -> TerminalSearchWindow {
        if query.is_empty() || limit == 0 {
            return TerminalSearchWindow {
                matches: Vec::new(),
                current: None,
                current_global: None,
                total: 0,
            };
        }
        let Ok(regex) = regex::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(!case_sensitive)
            .build()
        else {
            return TerminalSearchWindow {
                matches: Vec::new(),
                current: None,
                current_global: None,
                total: 0,
            };
        };
        let to_match = |line: &LogicalTextLine, found: regex::Match<'_>| {
            let start_index = line
                .spans
                .binary_search_by_key(&found.start(), |span| span.byte_start)
                .ok()?;
            let end_index = line
                .spans
                .binary_search_by_key(&found.end(), |span| span.byte_end)
                .ok()?;
            let start_span = &line.spans[start_index];
            let end_span = &line.spans[end_index];
            Some(TerminalTextMatch {
                start: start_span.start,
                end: end_span.end,
                source_fingerprint: text_fingerprint(found.as_str()),
                scan_cols: self.cols,
                scan_screen: active_screen,
            })
        };

        let origin = match direction {
            TerminalSearchDirection::Forward => previous.map_or(cursor, |(_, end)| end),
            TerminalSearchDirection::Backward => previous.map_or(cursor, |(start, _)| start),
        };
        let mut total = 0usize;
        let mut target = None;
        for line in &self.lines {
            for found in regex.find_iter(&line.text) {
                let Some(text_match) = to_match(line, found) else {
                    continue;
                };
                match direction {
                    TerminalSearchDirection::Forward
                        if target.is_none() && text_match.start > origin =>
                    {
                        target = Some(total);
                    }
                    TerminalSearchDirection::Backward if text_match.end < origin => {
                        target = Some(total);
                    }
                    _ => {}
                }
                total = total.saturating_add(1);
            }
        }
        if total == 0 {
            return TerminalSearchWindow {
                matches: Vec::new(),
                current: None,
                current_global: None,
                total: 0,
            };
        }
        let target = target.unwrap_or(match direction {
            TerminalSearchDirection::Forward => 0,
            TerminalSearchDirection::Backward => total - 1,
        });
        let retained = limit.min(total);
        let start = target
            .saturating_sub(retained / 2)
            .min(total.saturating_sub(retained));
        let end = start.saturating_add(retained);
        let mut index = 0usize;
        let mut matches = Vec::with_capacity(retained);
        for line in &self.lines {
            for found in regex.find_iter(&line.text) {
                let Some(text_match) = to_match(line, found) else {
                    continue;
                };
                if index >= start && index < end {
                    matches.push(text_match);
                }
                index = index.saturating_add(1);
                if index >= end {
                    break;
                }
            }
            if index >= end {
                break;
            }
        }
        TerminalSearchWindow {
            matches,
            current: Some(target - start),
            current_global: Some(target),
            total,
        }
    }

    pub(crate) fn word_motion(
        &self,
        row: u32,
        col: u16,
        motion: TerminalWordMotion,
    ) -> Option<TerminalTextPoint> {
        let current = self.atoms.iter().position(|atom| {
            atom.point
                .is_some_and(|point| point.row == row && col >= point.col && col <= atom.end_col)
        })?;
        match motion {
            TerminalWordMotion::NextStart => self.next_word_start(current),
            TerminalWordMotion::PreviousStart => self.previous_word_start(current),
            TerminalWordMotion::NextEnd => self.next_word_end(current),
            TerminalWordMotion::NextBigStart => self.next_big_word_start(current),
            TerminalWordMotion::PreviousBigStart => self.previous_big_word_start(current),
            TerminalWordMotion::NextBigEnd => self.next_big_word_end(current),
        }
    }

    fn next_word_start(&self, current: usize) -> Option<TerminalTextPoint> {
        let current_class = self.atoms.get(current)?.class;
        let mut next = current.saturating_add(1);
        if current_class != TextClass::Whitespace {
            while self
                .atoms
                .get(next)
                .is_some_and(|atom| atom.class == current_class)
            {
                next += 1;
            }
        }
        while self
            .atoms
            .get(next)
            .is_some_and(|atom| atom.class == TextClass::Whitespace)
        {
            next += 1;
        }
        self.next_point(next)
    }

    fn previous_word_start(&self, current: usize) -> Option<TerminalTextPoint> {
        let mut previous = current.checked_sub(1)?;
        while self
            .atoms
            .get(previous)
            .is_some_and(|atom| atom.class == TextClass::Whitespace)
        {
            previous = previous.checked_sub(1)?;
        }
        let class = self.atoms.get(previous)?.class;
        while previous > 0
            && self
                .atoms
                .get(previous - 1)
                .is_some_and(|atom| atom.class == class)
        {
            previous -= 1;
        }
        self.previous_point(previous)
    }

    fn next_word_end(&self, current: usize) -> Option<TerminalTextPoint> {
        let mut next = current.saturating_add(1);
        while self
            .atoms
            .get(next)
            .is_some_and(|atom| atom.class == TextClass::Whitespace)
        {
            next += 1;
        }
        let class = self.atoms.get(next)?.class;
        while self
            .atoms
            .get(next + 1)
            .is_some_and(|atom| atom.class == class)
        {
            next += 1;
        }
        self.previous_point(next)
    }

    fn next_big_word_start(&self, current: usize) -> Option<TerminalTextPoint> {
        let mut next = current.saturating_add(1);
        if self
            .atoms
            .get(current)
            .is_some_and(|atom| atom.class != TextClass::Whitespace)
        {
            while self
                .atoms
                .get(next)
                .is_some_and(|atom| atom.class != TextClass::Whitespace)
            {
                next += 1;
            }
        }
        while self
            .atoms
            .get(next)
            .is_some_and(|atom| atom.class == TextClass::Whitespace)
        {
            next += 1;
        }
        self.next_point(next)
    }

    fn previous_big_word_start(&self, current: usize) -> Option<TerminalTextPoint> {
        let mut previous = current.checked_sub(1)?;
        while self
            .atoms
            .get(previous)
            .is_some_and(|atom| atom.class == TextClass::Whitespace)
        {
            previous = previous.checked_sub(1)?;
        }
        while previous > 0
            && self
                .atoms
                .get(previous - 1)
                .is_some_and(|atom| atom.class != TextClass::Whitespace)
        {
            previous -= 1;
        }
        self.previous_point(previous)
    }

    fn next_big_word_end(&self, current: usize) -> Option<TerminalTextPoint> {
        let mut next = current.saturating_add(1);
        while self
            .atoms
            .get(next)
            .is_some_and(|atom| atom.class == TextClass::Whitespace)
        {
            next += 1;
        }
        self.atoms.get(next)?;
        while self
            .atoms
            .get(next + 1)
            .is_some_and(|atom| atom.class != TextClass::Whitespace)
        {
            next += 1;
        }
        self.previous_point(next)
    }

    fn next_point(&self, mut index: usize) -> Option<TerminalTextPoint> {
        while let Some(atom) = self.atoms.get(index) {
            if let Some(point) = atom.point {
                return Some(point);
            }
            index += 1;
        }
        None
    }

    fn previous_point(&self, mut index: usize) -> Option<TerminalTextPoint> {
        loop {
            if let Some(point) = self.atoms.get(index)?.point {
                return Some(point);
            }
            index = index.checked_sub(1)?;
        }
    }

    pub(crate) fn point_is_final_atom(&self, point: TerminalTextPoint) -> bool {
        // Word motion targets are atom start points, so compare against the
        // final atom's start point. Comparing against `end_col` would never
        // match a wide glyph, whose end column is one past its start.
        self.atoms
            .iter()
            .rev()
            .find(|atom| atom.point.is_some())
            .is_some_and(|atom| atom.point == Some(point))
    }
}

fn terminal_cell_text(graphemes: &[u32]) -> String {
    if graphemes.is_empty()
        || graphemes.first().copied() == Some(crate::ghostty::KITTY_UNICODE_PLACEHOLDER)
    {
        return " ".to_string();
    }
    graphemes
        .iter()
        .map(|codepoint| char::from_u32(*codepoint).unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn text_class(text: &str) -> TextClass {
    let Some(ch) = text.chars().next() else {
        return TextClass::Whitespace;
    };
    if ch.is_whitespace() {
        TextClass::Whitespace
    } else if ch.is_ascii() && COPY_MODE_WORD_SEPARATORS.contains(ch) {
        TextClass::Separator
    } else {
        TextClass::Word
    }
}

fn text_fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn word_motion_target(
    terminal: &crate::ghostty::Terminal,
    row: u32,
    col: u16,
    motion: TerminalWordMotion,
) -> Option<TerminalTextPoint> {
    let cols = terminal.cols().ok()?;
    let total_rows = terminal.total_rows().ok()?;
    let row = usize::try_from(row).ok()?;
    if row >= total_rows {
        return None;
    }

    let mut window_rows = 64usize;
    loop {
        let (start_row, end_row) = match motion {
            TerminalWordMotion::PreviousStart => {
                (row.saturating_sub(window_rows.saturating_sub(1)), row + 1)
            }
            TerminalWordMotion::NextStart | TerminalWordMotion::NextEnd => {
                (row, row.saturating_add(window_rows).min(total_rows))
            }
            TerminalWordMotion::PreviousBigStart => {
                (row.saturating_sub(window_rows.saturating_sub(1)), row + 1)
            }
            TerminalWordMotion::NextBigStart | TerminalWordMotion::NextBigEnd => {
                (row, row.saturating_add(window_rows).min(total_rows))
            }
        };
        let rows = terminal.screen_text_rows_range(start_row, end_row).ok()?;
        let starts_in_continuation = rows
            .first()
            .is_some_and(|row| row.wrap_continuation && start_row > 0);
        let ends_in_continuation = rows
            .last()
            .is_some_and(|row| row.soft_wrapped && end_row < total_rows);
        let buffer = RetainedTextBuffer::new_words(cols, rows, u32::try_from(start_row).ok()?);
        let target = buffer.word_motion(u32::try_from(row).ok()?, col, motion);
        let needs_more_history = (motion == TerminalWordMotion::PreviousStart
            || motion == TerminalWordMotion::PreviousBigStart)
            && target
                .is_some_and(|target| starts_in_continuation && target.row == start_row as u32);
        let needs_more_future = (motion == TerminalWordMotion::NextEnd
            || motion == TerminalWordMotion::NextBigEnd)
            && ends_in_continuation
            && target.is_some_and(|target| buffer.point_is_final_atom(target));
        if target.is_some() && !needs_more_history && !needs_more_future {
            return target;
        }

        let reached_edge = match motion {
            TerminalWordMotion::PreviousStart => start_row == 0,
            TerminalWordMotion::NextStart | TerminalWordMotion::NextEnd => end_row == total_rows,
            TerminalWordMotion::PreviousBigStart => start_row == 0,
            TerminalWordMotion::NextBigStart | TerminalWordMotion::NextBigEnd => {
                end_row == total_rows
            }
        };
        if reached_edge {
            return target;
        }
        window_rows = window_rows.saturating_mul(2).min(total_rows);
    }
}

pub(crate) fn paragraph_motion_target(
    terminal: &crate::ghostty::Terminal,
    row: u32,
    direction: i8,
) -> Option<TerminalTextPoint> {
    let total_rows = terminal.total_rows().ok()?;
    let current = usize::try_from(row).ok()?;
    if current >= total_rows || direction == 0 {
        return None;
    }
    let mut edge = if direction < 0 { current } else { current + 1 };
    while if direction < 0 {
        edge > 0
    } else {
        edge < total_rows
    } {
        let (start, end) = if direction < 0 {
            (edge.saturating_sub(64), edge)
        } else {
            (edge, edge.saturating_add(64).min(total_rows))
        };
        let rows = terminal.screen_text_rows_range(start, end).ok()?;
        let blank = |row: &crate::ghostty::ScreenTextRow| {
            row.cells.iter().all(|cell| {
                terminal_cell_text(&cell.graphemes)
                    .chars()
                    .all(char::is_whitespace)
            })
        };
        let found = if direction < 0 {
            rows.iter().rposition(blank)
        } else {
            rows.iter().position(blank)
        };
        if let Some(index) = found {
            return Some(TerminalTextPoint {
                row: u32::try_from(start + index).ok()?,
                col: 0,
            });
        }
        edge = if direction < 0 { start } else { end };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_cell(text: &str) -> crate::ghostty::ScreenTextCell {
        crate::ghostty::ScreenTextCell {
            wide: crate::ghostty::CellWide::Narrow,
            graphemes: text.chars().map(u32::from).collect(),
        }
    }

    fn wide_text_cells(text: &str) -> [crate::ghostty::ScreenTextCell; 2] {
        [
            crate::ghostty::ScreenTextCell {
                wide: crate::ghostty::CellWide::Wide,
                graphemes: text.chars().map(u32::from).collect(),
            },
            crate::ghostty::ScreenTextCell {
                wide: crate::ghostty::CellWide::SpacerTail,
                graphemes: Vec::new(),
            },
        ]
    }

    fn text_row(
        cells: impl IntoIterator<Item = crate::ghostty::ScreenTextCell>,
        soft_wrapped: bool,
    ) -> crate::ghostty::ScreenTextRow {
        crate::ghostty::ScreenTextRow {
            cells: cells.into_iter().collect(),
            soft_wrapped,
            wrap_continuation: false,
        }
    }

    fn search_primary(
        buffer: &RetainedTextBuffer,
        query: &str,
        case_sensitive: bool,
    ) -> Vec<TerminalTextMatch> {
        buffer
            .search_window(
                query,
                case_sensitive,
                crate::ghostty::ActiveScreen::Primary,
                TerminalSearchDirection::Forward,
                TerminalTextPoint { row: 0, col: 0 },
                None,
                usize::MAX,
            )
            .matches
    }

    #[test]
    fn retained_text_search_crosses_soft_wraps_but_not_hard_lines() {
        let buffer = RetainedTextBuffer::new(
            5,
            vec![
                text_row("abcde".chars().map(|ch| text_cell(&ch.to_string())), true),
                text_row("fgh  ".chars().map(|ch| text_cell(&ch.to_string())), false),
                text_row("abc  ".chars().map(|ch| text_cell(&ch.to_string())), false),
            ],
        );

        let matches = search_primary(&buffer, "def", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, TerminalTextPoint { row: 0, col: 3 });
        assert_eq!(matches[0].end, TerminalTextPoint { row: 1, col: 0 });
        assert!(search_primary(&buffer, "hab", true).is_empty());
    }

    #[test]
    fn retained_text_search_maps_wide_and_combining_graphemes_to_cells() {
        let mut cells = vec![text_cell("A")];
        cells.extend(wide_text_cells("界"));
        cells.push(text_cell("e\u{301}"));
        cells.push(text_cell("Z"));
        let buffer = RetainedTextBuffer::new(5, vec![text_row(cells, false)]);

        let matches = search_primary(&buffer, "界e\u{301}", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, TerminalTextPoint { row: 0, col: 1 });
        assert_eq!(matches[0].end, TerminalTextPoint { row: 0, col: 3 });
        assert!(search_primary(&buffer, "\u{301}", true).is_empty());
    }

    #[test]
    fn retained_text_search_skips_wide_spacer_heads_at_soft_wraps() {
        let mut first = "abcd"
            .chars()
            .map(|ch| text_cell(&ch.to_string()))
            .collect::<Vec<_>>();
        first.push(crate::ghostty::ScreenTextCell {
            wide: crate::ghostty::CellWide::SpacerHead,
            graphemes: Vec::new(),
        });
        let mut second = wide_text_cells("界").to_vec();
        second.extend("xyz".chars().map(|ch| text_cell(&ch.to_string())));
        let buffer =
            RetainedTextBuffer::new(5, vec![text_row(first, true), text_row(second, false)]);

        let matches = search_primary(&buffer, "d界", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, TerminalTextPoint { row: 0, col: 3 });
        assert_eq!(matches[0].end, TerminalTextPoint { row: 1, col: 1 });
    }

    #[test]
    fn retained_text_word_motion_does_not_split_at_a_wide_spacer_head() {
        let mut first = "abcd"
            .chars()
            .map(|ch| text_cell(&ch.to_string()))
            .collect::<Vec<_>>();
        first.push(crate::ghostty::ScreenTextCell {
            wide: crate::ghostty::CellWide::SpacerHead,
            graphemes: Vec::new(),
        });
        let mut second = wide_text_cells("界").to_vec();
        second.extend("xyz".chars().map(|ch| text_cell(&ch.to_string())));
        let buffer =
            RetainedTextBuffer::new(5, vec![text_row(first, true), text_row(second, false)]);

        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextStart),
            None
        );
        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextEnd),
            Some(TerminalTextPoint { row: 1, col: 4 })
        );
    }

    #[test]
    fn retained_text_search_is_literal_and_unicode_case_aware() {
        let buffer = RetainedTextBuffer::new(
            12,
            vec![text_row(
                "CAFÉ a.b    ".chars().map(|ch| text_cell(&ch.to_string())),
                false,
            )],
        );

        assert_eq!(search_primary(&buffer, "café", false).len(), 1);
        assert!(search_primary(&buffer, "café", true).is_empty());
        assert_eq!(search_primary(&buffer, "a.b", true).len(), 1);
        assert!(search_primary(&buffer, "a?b", true).is_empty());
    }

    #[test]
    fn retained_text_word_motions_use_tmux_separators_across_rows() {
        let buffer = RetainedTextBuffer::new(
            6,
            vec![
                text_row("a_b.c ".chars().map(|ch| text_cell(&ch.to_string())), false),
                text_row("—d    ".chars().map(|ch| text_cell(&ch.to_string())), false),
            ],
        );

        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextStart),
            Some(TerminalTextPoint { row: 0, col: 3 })
        );
        assert_eq!(
            buffer.word_motion(0, 3, TerminalWordMotion::NextStart),
            Some(TerminalTextPoint { row: 0, col: 4 })
        );
        assert_eq!(
            buffer.word_motion(0, 4, TerminalWordMotion::NextStart),
            Some(TerminalTextPoint { row: 1, col: 0 })
        );
        assert_eq!(
            buffer.word_motion(1, 1, TerminalWordMotion::PreviousStart),
            Some(TerminalTextPoint { row: 1, col: 0 })
        );
    }

    #[test]
    fn retained_text_big_word_motions_treat_only_whitespace_as_separators() {
        let buffer = RetainedTextBuffer::new(
            20,
            vec![text_row(
                "foo.bar baz qux/quux"
                    .chars()
                    .map(|ch| text_cell(&ch.to_string())),
                false,
            )],
        );

        // `W` skips punctuation-separated segments and lands on the next
        // whitespace-delimited run.
        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextBigStart),
            Some(TerminalTextPoint { row: 0, col: 8 })
        );
        assert_eq!(
            buffer.word_motion(0, 8, TerminalWordMotion::NextBigStart),
            Some(TerminalTextPoint { row: 0, col: 12 })
        );
        // `E` lands on the last character of the current/next run.
        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextBigEnd),
            Some(TerminalTextPoint { row: 0, col: 6 })
        );
        assert_eq!(
            buffer.word_motion(0, 6, TerminalWordMotion::NextBigEnd),
            Some(TerminalTextPoint { row: 0, col: 10 })
        );
        assert_eq!(
            buffer.word_motion(0, 12, TerminalWordMotion::NextBigEnd),
            Some(TerminalTextPoint { row: 0, col: 19 })
        );
        // `B` returns to the beginning of the previous run.
        assert_eq!(
            buffer.word_motion(0, 19, TerminalWordMotion::PreviousBigStart),
            Some(TerminalTextPoint { row: 0, col: 12 })
        );
        assert_eq!(
            buffer.word_motion(0, 12, TerminalWordMotion::PreviousBigStart),
            Some(TerminalTextPoint { row: 0, col: 8 })
        );
        assert_eq!(
            buffer.word_motion(0, 8, TerminalWordMotion::PreviousBigStart),
            Some(TerminalTextPoint { row: 0, col: 0 })
        );

        // Lowercase motions keep their punctuation-aware behavior.
        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextStart),
            Some(TerminalTextPoint { row: 0, col: 3 })
        );
        assert_eq!(
            buffer.word_motion(0, 3, TerminalWordMotion::NextStart),
            Some(TerminalTextPoint { row: 0, col: 4 })
        );
        assert_eq!(
            buffer.word_motion(0, 4, TerminalWordMotion::PreviousStart),
            Some(TerminalTextPoint { row: 0, col: 3 })
        );
    }

    #[test]
    fn retained_text_big_word_motions_cross_rows_and_blank_lines() {
        let buffer = RetainedTextBuffer::new(
            6,
            vec![
                text_row("a.b-c ".chars().map(|ch| text_cell(&ch.to_string())), false),
                text_row("      ".chars().map(|ch| text_cell(&ch.to_string())), false),
                text_row("d_e   ".chars().map(|ch| text_cell(&ch.to_string())), false),
            ],
        );

        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextBigStart),
            Some(TerminalTextPoint { row: 2, col: 0 })
        );
        assert_eq!(
            buffer.word_motion(2, 0, TerminalWordMotion::PreviousBigStart),
            Some(TerminalTextPoint { row: 0, col: 0 })
        );
        assert_eq!(
            buffer.word_motion(0, 0, TerminalWordMotion::NextBigEnd),
            Some(TerminalTextPoint { row: 0, col: 4 })
        );
        assert_eq!(
            buffer.word_motion(0, 4, TerminalWordMotion::NextBigEnd),
            Some(TerminalTextPoint { row: 2, col: 2 })
        );
    }

    #[test]
    fn paragraph_motion_reaches_separators_beyond_one_thousand_retained_rows() {
        let mut terminal = crate::ghostty::Terminal::new(8, 3, 64 * 1024 * 1024).unwrap();
        terminal.write(b"\r\n");
        for _ in 0..1105 {
            terminal.write(b"x\r\n");
        }
        let last = u32::try_from(terminal.total_rows().unwrap() - 1).unwrap();
        assert!(last > 1000);
        let previous = paragraph_motion_target(&terminal, last, -1).unwrap();
        assert!(last - previous.row > 1000);
        let next = paragraph_motion_target(&terminal, previous.row, 1).unwrap();
        assert_eq!(next.row, last);
    }
}
