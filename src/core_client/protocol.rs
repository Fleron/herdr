use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};

use serde_json::Value;

pub const DATA: u8 = 0;
pub const CREDIT: u8 = 1;
pub const CLOSE: u8 = 2;
pub const RESIZE: u8 = 3;
pub const FOCUS: u8 = 4;
pub const FACTS: u8 = 5;
pub const AUTOMATIC_INPUT: u8 = 6;
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum Error {
    Transport {
        kind: io::ErrorKind,
        message: String,
    },
    Protocol(String),
    Rpc {
        code: i64,
        message: String,
        data: Value,
    },
}

impl Error {
    pub fn kind(&self) -> Option<&str> {
        match self {
            Self::Rpc { data, .. } => data.get("kind").and_then(Value::as_str),
            _ => None,
        }
    }

    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport { .. })
    }

    pub(crate) fn lost(message: impl Into<String>) -> Self {
        Self::Transport {
            kind: io::ErrorKind::BrokenPipe,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { message, .. } | Self::Protocol(message) => f.write_str(message),
            Self::Rpc { message, data, .. } => {
                f.write_str(message)?;
                if let Some(hint) = data.get("hint").and_then(Value::as_str) {
                    if !hint.is_empty() {
                        write!(f, ": {hint}")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Transport {
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Protocol(error.to_string())
    }
}

impl From<Error> for io::Error {
    fn from(error: Error) -> Self {
        let kind = match &error {
            Error::Transport { kind, .. } => *kind,
            Error::Protocol(_) => io::ErrorKind::InvalidData,
            Error::Rpc { .. } => io::ErrorKind::Other,
        };
        Self::new(kind, error)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub channel: u32,
    pub kind: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn read(reader: &mut impl Read) -> io::Result<Option<Self>> {
        let mut header = [0; 9];
        loop {
            match reader.read(&mut header[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        reader.read_exact(&mut header[1..])?;
        let channel = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let size = u32::from_le_bytes([header[5], header[6], header[7], header[8]]) as usize;
        if size > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "core frame exceeds the protocol's 16 MiB limit",
            ));
        }
        let mut payload = vec![0; size];
        reader.read_exact(&mut payload)?;
        Ok(Some(Self {
            channel,
            kind: header[4],
            payload,
        }))
    }

    pub fn write(
        writer: &mut impl Write,
        channel: u32,
        kind: u8,
        payload: &[u8],
    ) -> io::Result<()> {
        if payload.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame exceeds the core protocol's 16 MiB limit",
            ));
        }
        let mut header = [0; 9];
        header[..4].copy_from_slice(&channel.to_le_bytes());
        header[4] = kind;
        header[5..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        writer.write_all(&header)?;
        writer.write_all(payload)?;
        writer.flush()
    }

    pub fn write_stream(
        writer: &mut impl Write,
        channel: u32,
        kind: u8,
        bytes: &[u8],
    ) -> io::Result<()> {
        for chunk in bytes.chunks(MAX_FRAME) {
            Self::write(writer, channel, kind, chunk)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub(crate) struct EventLines {
    pending: Vec<u8>,
    ready: VecDeque<Value>,
}

impl EventLines {
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        while let Some(relative) = self.pending[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let end = consumed + relative;
            let line = &self.pending[consumed..end];
            if !line.iter().all(u8::is_ascii_whitespace) {
                let message: Value = serde_json::from_slice(line)?;
                if message.get("method").and_then(Value::as_str).is_none() {
                    return Err(Error::Protocol("core event has no method".into()));
                }
                self.ready.push_back(message);
            }
            consumed = end + 1;
        }
        self.pending.drain(..consumed);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Value> {
        self.ready.pop_front()
    }
}

pub(crate) fn rpc_result(mut message: Value) -> Result<Value, Error> {
    let object = message
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("core reply is not an object".into()))?;
    if let Some(error) = object.remove("error") {
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::Protocol("core error has no numeric code".into()))?;
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Protocol("core error has no message".into()))?;
        return Err(Error::Rpc {
            code,
            message: text.into(),
            data: error.get("data").cloned().unwrap_or(Value::Null),
        });
    }
    object
        .remove("result")
        .ok_or_else(|| Error::Protocol("core reply has neither result nor error".into()))
}

pub(crate) fn response_id(message: &Value) -> Result<Option<u64>, Error> {
    let object = message
        .as_object()
        .ok_or_else(|| Error::Protocol("core message is not an object".into()))?;
    match object.get("id") {
        Some(id) => id
            .as_u64()
            .map(Some)
            .ok_or_else(|| Error::Protocol("core reply has an invalid request id".into())),
        None if object.get("method").and_then(Value::as_str).is_some() => Ok(None),
        None => Err(Error::Protocol(
            "core message has neither request id nor notification method".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Fragmented(Cursor<Vec<u8>>);

    impl Read for Fragmented {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let size = bytes.len().min(1);
            self.0.read(&mut bytes[..size])
        }
    }

    #[test]
    fn frames_survive_fragmented_reads_and_distinguish_clean_eof() {
        let mut wire = Vec::new();
        Frame::write(&mut wire, 73, DATA, b"\0screen\xff").unwrap();
        Frame::write(&mut wire, 73, CLOSE, b"").unwrap();
        assert_eq!(&wire[..9], &[73, 0, 0, 0, 0, 8, 0, 0, 0]);
        let mut input = Fragmented(Cursor::new(wire));
        assert_eq!(
            Frame::read(&mut input).unwrap(),
            Some(Frame {
                channel: 73,
                kind: DATA,
                payload: b"\0screen\xff".to_vec(),
            })
        );
        assert_eq!(Frame::read(&mut input).unwrap().unwrap().kind, CLOSE);
        assert!(Frame::read(&mut input).unwrap().is_none());
        assert_eq!(
            Frame::read(&mut Cursor::new(vec![1, 0]))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn oversized_frame_is_rejected_before_reading_its_payload() {
        let mut header = vec![0; 9];
        header[5..].copy_from_slice(&((MAX_FRAME + 1) as u32).to_le_bytes());
        assert_eq!(
            Frame::read(&mut Cursor::new(header)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn logical_input_larger_than_one_frame_preserves_every_byte_for_both_input_kinds() {
        let input: Vec<_> = (0..MAX_FRAME + 7)
            .map(|index| (index % 251) as u8)
            .collect();
        for kind in [DATA, AUTOMATIC_INPUT] {
            let mut wire = Vec::new();
            Frame::write_stream(&mut wire, 9, kind, &input).unwrap();
            let mut reader = Cursor::new(wire);
            let first = Frame::read(&mut reader).unwrap().unwrap();
            let second = Frame::read(&mut reader).unwrap().unwrap();
            assert_eq!(
                (first.channel, first.kind, first.payload.len()),
                (9, kind, MAX_FRAME)
            );
            assert_eq!(
                (second.channel, second.kind, second.payload.len()),
                (9, kind, 7)
            );
            assert_eq!([first.payload, second.payload].concat(), input);
            assert!(Frame::read(&mut reader).unwrap().is_none());
        }
    }

    #[test]
    fn notifications_preserve_split_utf8_and_missed_snapshots() {
        let text = "{\"method\":\"event\",\"params\":{\"title\":\"羊\"}}\n";
        let mut lines = EventLines::default();
        for byte in text.as_bytes() {
            lines.push(&[*byte]).unwrap();
        }
        assert_eq!(lines.pop().unwrap()["params"]["title"], "羊");
        lines
            .push(b"\n{\"method\":\"missed\",\"params\":{\"seq\":7,\"terminals\":[]}}\n{\"method\":\"heartbeat\",\"params\":{\"seq\":7}}\n")
            .unwrap();
        assert_eq!(lines.pop().unwrap()["method"], "missed");
        assert_eq!(lines.pop().unwrap()["method"], "heartbeat");
        assert!(lines.pop().is_none());
        assert!(lines.push(b"{broken}\n").is_err());
    }

    #[test]
    fn rpc_errors_preserve_kind_code_and_hint() {
        let error = rpc_result(serde_json::json!({
            "error": {"code": -32000, "message": "changed", "data": {"kind": "conflict", "hint": "read again"}}
        }))
        .unwrap_err();
        assert_eq!(error.kind(), Some("conflict"));
        assert_eq!(error.to_string(), "changed: read again");
        assert!(matches!(error, Error::Rpc { code: -32000, .. }));
        assert_eq!(
            rpc_result(serde_json::json!({"result": null})).unwrap(),
            Value::Null
        );
        assert_eq!(response_id(&serde_json::json!({"id": 0})).unwrap(), Some(0));
        assert_eq!(
            response_id(&serde_json::json!({"method": "event"})).unwrap(),
            None
        );
        assert!(response_id(&serde_json::json!({"id": "0"})).is_err());
        assert!(response_id(&Value::Null).is_err());
        assert!(response_id(&serde_json::json!({"result": 1})).is_err());
    }
}
