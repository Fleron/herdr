#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi(Option<u8>),
    String {
        osc: bool,
        escaped: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct InitialPaint {
    state: State,
    utf8_remaining: u8,
    ready: bool,
}

impl InitialPaint {
    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn push(&mut self, bytes: &[u8]) -> bool {
        if self.ready {
            return true;
        }
        for &byte in bytes {
            if self.utf8_remaining > 0 {
                self.utf8_remaining = self.utf8_remaining.saturating_sub(1);
                if (0x80..=0xbf).contains(&byte) {
                    continue;
                }
                self.utf8_remaining = 0;
            }
            let utf8 = match byte {
                0xc2..=0xdf => 1,
                0xe0..=0xef => 2,
                0xf0..=0xf4 => 3,
                _ => 0,
            };
            if utf8 > 0 {
                self.utf8_remaining = utf8;
                self.state = match self.state {
                    State::Csi(_) => State::Csi(None),
                    State::Escape | State::EscapeIntermediate => State::Ground,
                    State::String { osc, .. } => State::String {
                        osc,
                        escaped: false,
                    },
                    state => state,
                };
                continue;
            }
            if matches!(byte, 0x18 | 0x1a) {
                self.state = State::Ground;
                continue;
            }
            if let State::String { osc, escaped } = self.state {
                self.state = if byte == 0x9c || (osc && byte == 0x07) || (escaped && byte == b'\\')
                {
                    State::Ground
                } else {
                    State::String {
                        osc,
                        escaped: byte == 0x1b,
                    }
                };
                continue;
            }
            self.state = match byte {
                0x1b => State::Escape,
                0x9b => State::Csi(Some(0)),
                0x9d => State::String {
                    osc: true,
                    escaped: false,
                },
                0x90 | 0x98 | 0x9e | 0x9f => State::String {
                    osc: false,
                    escaped: false,
                },
                0x9c => State::Ground,
                0x00..=0x1f | 0x7f => self.state,
                _ => match self.state {
                    State::Ground => State::Ground,
                    State::Escape => match byte {
                        b'[' => State::Csi(Some(0)),
                        b']' => State::String {
                            osc: true,
                            escaped: false,
                        },
                        b'P' | b'X' | b'^' | b'_' => State::String {
                            osc: false,
                            escaped: false,
                        },
                        0x20..=0x2f => State::EscapeIntermediate,
                        _ => State::Ground,
                    },
                    State::EscapeIntermediate if (0x20..=0x2f).contains(&byte) => {
                        State::EscapeIntermediate
                    }
                    State::EscapeIntermediate => State::Ground,
                    State::Csi(matched) if (0x40..=0x7e).contains(&byte) => {
                        // Core appends this CSI after history, screen and keyboard-mode restoration.
                        if matched == Some(5) && matches!(byte, b'h' | b'l') {
                            self.ready = true;
                            return true;
                        }
                        State::Ground
                    }
                    State::Csi(matched) => State::Csi(matched.and_then(|index| {
                        (b"?9001".get(usize::from(index)) == Some(&byte)).then_some(index + 1)
                    })),
                    State::String { .. } => unreachable!(),
                },
            };
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &[u8] = b"\x1b[?9001l";

    #[test]
    fn first_data_and_sync_mode_are_not_readiness() {
        let mut paint = InitialPaint::default();
        assert!(!paint.ready());
        assert!(!paint.push(b"\x18\x1b[!p\x1b[?2026l\x1b[?2004lpartial history"));
        assert!(!paint.push(b"\x1b[?2004h\x1b[>1u"));
        assert!(paint.push(MARKER));
        assert!(paint.ready());
        assert!(paint.push(b""));
    }

    #[test]
    fn complete_and_fragmented_marker_finish_after_modes() {
        for marker in [b"\x1b[?9001h".as_slice(), MARKER] {
            for split in 0..marker.len() {
                let mut paint = InitialPaint::default();
                assert!(!paint.push(&marker[..split]));
                assert!(paint.push(&marker[split..]));
            }
            let mut paint = InitialPaint::default();
            for (index, byte) in marker.iter().enumerate() {
                assert_eq!(paint.push(&[*byte]), index == marker.len() - 1);
            }
        }
    }

    #[test]
    fn strings_never_expose_embedded_marker_sequences() {
        for introducer in *b"]P_^X" {
            let mut paint = InitialPaint::default();
            assert!(!paint.push(&[0x1b, introducer]));
            assert!(!paint.push(b"title or clipboard or graphics \x1b[?9001h"));
            assert!(!paint.push(b"\x9b?9001l"));
            assert!(!paint.push(b"\x1b"));
            assert!(!paint.push(b"\\"));
            assert!(paint.push(MARKER));
        }
    }

    #[test]
    fn bel_ends_only_osc_and_string_terminators_can_be_fragmented() {
        let mut osc = InitialPaint::default();
        assert!(!osc.push(b"\x1b]2;title\x07"));
        assert!(osc.push(MARKER));
        for introducer in *b"P_^X" {
            let mut paint = InitialPaint::default();
            assert!(!paint.push(&[0x1b, introducer]));
            assert!(!paint.push(b"\x07\x1b[?9001h"));
            assert!(!paint.push(b"\x1b\x1b"));
            assert!(!paint.push(b"\\"));
            assert!(paint.push(MARKER));
        }
    }

    #[test]
    fn unicode_continuations_are_not_c1_controls() {
        let mut paint = InitialPaint::default();
        assert!(!paint.push("Û?9001h".as_bytes()));
        assert!(!paint.push(b"\x1b]title "));
        assert!(!paint.push("Ü".as_bytes()));
        assert!(!paint.push(MARKER));
        assert!(!paint.push(b"\x1b\\"));
        assert!(paint.push(MARKER));

        let mut paint = InitialPaint::default();
        assert!(!paint.push(b"\x1b]title \x1b"));
        assert!(!paint.push("Ü".as_bytes()));
        assert!(!paint.push(b"\\"));
        assert!(!paint.push(MARKER));
        assert!(!paint.push(b"\x1b\\"));
        assert!(paint.push(MARKER));
    }

    #[test]
    fn cancelled_sequences_and_noncanonical_modes_do_not_finish_the_paint() {
        for cancel in [0x18, 0x1a] {
            let mut paint = InitialPaint::default();
            assert!(!paint.push(b"\x1b[?900"));
            assert!(!paint.push(&[cancel]));
            assert!(!paint.push(b"1h"));
            assert!(!paint.push(b"\x1b]unfinished title"));
            assert!(!paint.push(&[cancel]));
            assert!(paint.push(MARKER));
        }
        for sequence in [
            b"\x1b[?19001h".as_slice(),
            b"\x1b[?9001;0h",
            b"\x1b[?9001$h",
            b"\x1b[>9001h",
            b"\x1b[?9001m",
        ] {
            let mut paint = InitialPaint::default();
            assert!(!paint.push(sequence));
            assert!(paint.push(MARKER));
        }
    }

    #[test]
    fn large_history_and_incomplete_controls_keep_input_waiting() {
        let mut paint = InitialPaint::default();
        for _ in 0..20_000 {
            assert!(!paint.push(b"history line with colors \x1b[31mred\x1b[0m\r\n"));
        }
        assert!(!paint.push(b"\x1b[?2004h\x1b[>4;2m\x1b[>1u\x1b[?9001"));
        assert!(paint.push(b"l\x1b]7501;status\x1b\\"));
    }

    #[test]
    fn c1_sequences_and_ignored_c0_controls_follow_the_same_boundary() {
        for introducer in [0x90, 0x98, 0x9d, 0x9e, 0x9f] {
            let mut paint = InitialPaint::default();
            assert!(!paint.push(&[introducer]));
            assert!(!paint.push(MARKER));
            assert!(!paint.push(&[0x9c]));
            assert!(paint.push(b"\x9b?9001h"));
        }
        let mut paint = InitialPaint::default();
        assert!(paint.push(b"\x1b[?9\x000\x0701h"));
    }
}
