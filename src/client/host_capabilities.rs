#[cfg(not(windows))]
use std::io::{self, Write as _};
#[cfg(not(windows))]
use std::os::fd::AsRawFd as _;
#[cfg(not(windows))]
use std::time::{Duration, Instant};

#[cfg(not(windows))]
const HOST_CAPABILITY_QUERY_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(not(windows))]
const MAX_BUFFERED_HOST_INPUT: usize = 64 * 1024;

#[cfg(not(windows))]
#[derive(Default)]
struct HostKeyboardProbeResponses {
    flags: Option<u16>,
    sgr_pixel_mouse_status: Option<u8>,
    primary_device_attributes: bool,
}

/// What the startup probe learned about the host terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostTerminalProbe {
    pub(crate) escape_disambiguation: bool,
    /// None when the host did not answer; exact pixel input stays disabled.
    pub(crate) sgr_pixel_mouse: Option<bool>,
}

#[cfg(not(windows))]
pub(crate) fn query_host_capabilities(query_keyboard: bool) -> (HostTerminalProbe, Vec<u8>) {
    const FULL_QUERY: &[u8] = b"\x1b[?1016$p\x1b[?u\x1b[c";
    const PIXEL_MOUSE_QUERY: &[u8] = b"\x1b[?1016$p\x1b[c";

    let mut buffered_input = Vec::new();
    let query = if query_keyboard {
        FULL_QUERY
    } else {
        PIXEL_MOUSE_QUERY
    };
    if let Err(err) = io::stdout()
        .write_all(query)
        .and_then(|()| io::stdout().flush())
    {
        tracing::debug!(%err, "host capability query unavailable");
        return (HostTerminalProbe::default(), buffered_input);
    }

    // Bypass StdinLock's shared buffer so poll and read observe the same bytes.
    let stdin = io::stdin();
    let stdin_fd = stdin.as_raw_fd();
    let deadline = Instant::now() + HOST_CAPABILITY_QUERY_TIMEOUT;
    let mut responses = HostKeyboardProbeResponses::default();
    while !responses.primary_device_attributes && buffered_input.len() < MAX_BUFFERED_HOST_INPUT {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        match crate::platform::poll_fd_readable(stdin_fd, timeout_ms) {
            Ok(true) => {}
            Ok(false) => break,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::debug!(%err, "host keyboard enhancement query read unavailable");
                break;
            }
        }

        let mut scratch = [0u8; 4096];
        let capacity = MAX_BUFFERED_HOST_INPUT - buffered_input.len();
        let read_limit = capacity.min(scratch.len());
        match crate::platform::read_fd(stdin_fd, &mut scratch[..read_limit]) {
            Ok(0) => break,
            Ok(read) => {
                buffered_input.extend_from_slice(&scratch[..read]);
                consume_host_keyboard_probe_responses(&mut buffered_input, &mut responses);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::debug!(%err, "host keyboard enhancement query read failed");
                break;
            }
        }
    }

    (host_terminal_probe(&responses), buffered_input)
}

#[cfg(not(windows))]
fn host_terminal_probe(responses: &HostKeyboardProbeResponses) -> HostTerminalProbe {
    let sgr_pixel_mouse = match responses.sgr_pixel_mouse_status {
        // The query follows an explicit reset, so a host still reporting SET may
        // send pixels where Herdr expects cells. Wrong mouse units beat refusing
        // to start, so keep pixel mouse off and continue.
        Some(status @ (1 | 3)) => {
            tracing::warn!(status, "host SGR pixel mouse mode stays set after reset");
            Some(false)
        }
        Some(2) => Some(true),
        Some(0 | 4) => Some(false),
        _ => None,
    };
    HostTerminalProbe {
        escape_disambiguation: host_escape_disambiguation_confirmed(responses),
        sgr_pixel_mouse,
    }
}

#[cfg(not(windows))]
fn host_escape_disambiguation_confirmed(responses: &HostKeyboardProbeResponses) -> bool {
    responses.primary_device_attributes
        && responses
            .flags
            .is_some_and(|flags| flags & 0b0000_0001 != 0)
}

#[cfg(not(windows))]
fn consume_host_keyboard_probe_responses(
    buffered_input: &mut Vec<u8>,
    responses: &mut HostKeyboardProbeResponses,
) {
    const PASTE_START: &[u8] = b"\x1b[200~";
    const PASTE_END: &[u8] = b"\x1b[201~";

    let mut offset = 0;
    while offset < buffered_input.len() {
        if buffered_input[offset..].starts_with(PASTE_START) {
            let payload_start = offset + PASTE_START.len();
            let Some(relative_end) = buffered_input[payload_start..]
                .windows(PASTE_END.len())
                .position(|bytes| bytes == PASTE_END)
            else {
                break;
            };
            offset = payload_start + relative_end + PASTE_END.len();
            continue;
        }
        if let Some(control_string_end) = host_control_string_end(&buffered_input[offset..]) {
            let Some(control_string_end) = control_string_end else {
                break;
            };
            offset += control_string_end;
            continue;
        }
        if !buffered_input[offset..].starts_with(b"\x1b[?") {
            offset += 1;
            continue;
        }

        let start = offset;
        let mut end = start + 3;
        while end < buffered_input.len()
            && (buffered_input[end].is_ascii_digit() || buffered_input[end] == b';')
        {
            end += 1;
        }
        if end == buffered_input.len() {
            break;
        }
        let body = &buffered_input[start + 3..end];
        // DECRPM: CSI ? 1016 ; Ps $ y.
        if buffered_input[end] == b'$' {
            let Some(&final_byte) = buffered_input.get(end + 1) else {
                break;
            };
            if final_byte == b'y' {
                if let Some(status) = parse_sgr_pixel_mouse_status(body) {
                    responses.sgr_pixel_mouse_status = Some(status);
                    buffered_input.drain(start..=end + 1);
                    continue;
                }
            }
            offset += 1;
            continue;
        }
        let recognized = match buffered_input[end] {
            b'u' if !body.is_empty() && body.iter().all(u8::is_ascii_digit) => {
                std::str::from_utf8(body)
                    .ok()
                    .and_then(|flags| flags.parse().ok())
                    .map(|flags| {
                        if !responses.primary_device_attributes {
                            responses.flags = Some(flags);
                        }
                    })
                    .is_some()
            }
            b'c' if !body.is_empty()
                && body
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || *byte == b';') =>
            {
                responses.primary_device_attributes = true;
                true
            }
            _ => false,
        };
        if recognized {
            buffered_input.drain(start..=end);
        } else {
            offset += 1;
        }
    }
}

#[cfg(not(windows))]
fn parse_sgr_pixel_mouse_status(body: &[u8]) -> Option<u8> {
    let mut fields = body.split(|byte| *byte == b';');
    (fields.next()? == b"1016").then_some(())?;
    let status = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    (fields.next().is_none() && status <= 4).then_some(status)
}

#[cfg(not(windows))]
fn host_control_string_end(bytes: &[u8]) -> Option<Option<usize>> {
    if bytes.first() != Some(&0x1b) {
        return None;
    }

    let allow_bel = if bytes.starts_with(b"\x1b]") {
        true
    } else if bytes
        .get(1)
        .is_some_and(|byte| matches!(*byte, b'P' | b'_' | b'^' | b'X'))
    {
        false
    } else {
        return None;
    };

    for offset in 2..bytes.len() {
        if allow_bel && bytes[offset] == 0x07 {
            return Some(Some(offset + 1));
        }
        if bytes[offset..].starts_with(b"\x1b\\") {
            return Some(Some(offset + 2));
        }
    }
    Some(None)
}

#[cfg(windows)]
pub(crate) fn query_host_capabilities(_query_keyboard: bool) -> (HostTerminalProbe, Vec<u8>) {
    (HostTerminalProbe::default(), Vec::new())
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn host_capability_probe_consumes_fragmented_responses_and_preserves_input() {
        let stream = b"before\x1b[?1016;2$y-middle-\x1b[?7u-\x1b[?1;2cafter";

        for split in 1..stream.len() {
            let mut buffered = Vec::new();
            let mut responses = HostKeyboardProbeResponses::default();
            buffered.extend_from_slice(&stream[..split]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);
            buffered.extend_from_slice(&stream[split..]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(responses.sgr_pixel_mouse_status, Some(2), "split {split}");
            assert_eq!(responses.flags, Some(7), "split {split}");
            assert!(responses.primary_device_attributes, "split {split}");
            assert_eq!(buffered, b"before-middle--after", "split {split}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_capability_probe_accepts_only_usable_sgr_pixel_mouse_states() {
        for status in 0..=4 {
            let mut buffered = format!("\x1b[?1016;{status}$y").into_bytes();
            let mut responses = HostKeyboardProbeResponses::default();

            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(
                host_terminal_probe(&responses).sgr_pixel_mouse,
                Some(status == 2),
                "status {status}"
            );
            assert!(buffered.is_empty());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_probe_keeps_a_pixel_mouse_answer_without_device_attributes() {
        let mut buffered = b"\x1b[?1016;0$y".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();
        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(!responses.primary_device_attributes);
        assert_eq!(host_terminal_probe(&responses).sgr_pixel_mouse, Some(false));
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_typed_input_before_responses() {
        let mut buffered = b"aPtyped\x1b[?1016;2$y\x1b[?7u\x1b[?1;2c".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(host_escape_disambiguation_confirmed(&responses));
        assert_eq!(host_terminal_probe(&responses).sgr_pixel_mouse, Some(true));
        assert_eq!(buffered, b"aPtyped");
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_requires_disambiguation_bit_and_device_attributes() {
        for (flags, expected) in [(0, false), (2, false), (7, true)] {
            let mut buffered = format!("\x1b[?{flags}u\x1b[?1;2c").into_bytes();
            let mut responses = HostKeyboardProbeResponses::default();

            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(host_escape_disambiguation_confirmed(&responses), expected);
            assert!(buffered.is_empty());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_requires_flags_before_device_attributes() {
        let mut buffered = b"\x1b[?1;2c\x1b[?7uinput".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert_eq!(responses.flags, None);
        assert!(responses.primary_device_attributes);
        assert_eq!(buffered, b"input");
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_response_shaped_payloads() {
        let opaque =
            b"\x1b[200~paste \x1b[?1016;2$y \x1b[?1u \x1b[?1;2c\x1b[201~-\x1bPdata \x1b[?7u\x1b\\";
        let mut buffered = [opaque.as_slice(), b"\x1b[?1016;2$y\x1b[?7u\x1b[?1;2c"].concat();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(host_escape_disambiguation_confirmed(&responses));
        assert_eq!(host_terminal_probe(&responses).sgr_pixel_mouse, Some(true));
        assert_eq!(buffered, opaque);
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_malformed_responses() {
        let mut buffered = b"a\x1b[?7;1ub\x1b[?65536uc\x1b[?1016;5$y\x1b[?1006;2$y".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert_eq!(responses.flags, None);
        assert_eq!(responses.sgr_pixel_mouse_status, None);
        assert!(!responses.primary_device_attributes);
        assert_eq!(
            buffered,
            b"a\x1b[?7;1ub\x1b[?65536uc\x1b[?1016;5$y\x1b[?1006;2$y"
        );
    }
}
