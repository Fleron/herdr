use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

pub use super::protocol::Error;
use super::protocol::{self, EventLines, Frame};

type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Closed(Value),
    Lost,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Display {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub cursor: Option<String>,
    pub cell: Option<[u32; 2]>,
}

#[derive(Clone, Debug, Default)]
pub struct AttachOptions {
    pub terminal: String,
    pub cols: u16,
    pub rows: u16,
    pub observe: bool,
    pub history: usize,
    pub resumed: bool,
    pub display: Display,
    pub caller: Option<String>,
}

impl AttachOptions {
    pub fn new(terminal: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            terminal: terminal.into(),
            cols,
            rows,
            ..Self::default()
        }
    }
}

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Pending {
    sender: mpsc::Sender<Result<Response>>,
    stream: bool,
}

#[derive(Default)]
struct Routing {
    pending: HashMap<u64, Pending>,
    channels: HashMap<u32, mpsc::Sender<StreamEvent>>,
    error: Option<Error>,
}

struct Inner {
    writer: Mutex<ChildStdin>,
    process: Mutex<Process>,
    routing: Mutex<Routing>,
    waker: Mutex<Option<mpsc::SyncSender<()>>>,
    hello: Value,
    next_id: AtomicU64,
}

#[derive(Clone)]
pub struct Client(Arc<Inner>);

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("version", &self.hello().get("version"))
            .field("connected", &self.error().is_none())
            .finish()
    }
}

struct OpenedStream {
    id: u32,
    receiver: mpsc::Receiver<StreamEvent>,
}

struct Response {
    value: Value,
    stream: Option<OpenedStream>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

impl Inner {
    fn wake(&self) {
        if let Some(waker) = locked(&self.waker).as_ref() {
            let _ = waker.try_send(());
        }
    }

    fn fail(&self, error: Error) {
        let mut routing = locked(&self.routing);
        if routing.error.is_some() {
            return;
        }
        routing.error = Some(error.clone());
        for (_, request) in routing.pending.drain() {
            let _ = request.sender.send(Err(error.clone()));
        }
        for (_, stream) in routing.channels.drain() {
            let _ = stream.send(StreamEvent::Lost);
        }
        drop(routing);
        self.wake();
    }

    fn send(&self, channel: u32, kind: u8, payload: &[u8]) -> Result<()> {
        if payload.len() > protocol::MAX_FRAME {
            return Err(Error::Protocol(
                "frame exceeds the core protocol's 16 MiB limit".into(),
            ));
        }
        self.write(|writer| Frame::write(writer, channel, kind, payload))
    }

    fn send_stream(&self, channel: u32, kind: u8, bytes: &[u8]) -> Result<()> {
        self.write(|writer| Frame::write_stream(writer, channel, kind, bytes))
    }

    fn write(&self, write: impl FnOnce(&mut ChildStdin) -> io::Result<()>) -> Result<()> {
        if let Some(error) = locked(&self.routing).error.clone() {
            return Err(error);
        }
        let result = write(&mut locked(&self.writer));
        if let Err(error) = result {
            let error = Error::from(error);
            self.fail(error.clone());
            return Err(error);
        }
        Ok(())
    }
}

impl Client {
    pub fn connect() -> Result<Self> {
        Self::connect_with_binary(std::env::var_os("HC_BIN").unwrap_or_else(|| "hc".into()))
    }

    pub fn connect_with_binary(binary: impl AsRef<OsStr>) -> Result<Self> {
        Self::connect_command(&mut Command::new(binary))
    }

    pub fn connect_command(command: &mut Command) -> Result<Self> {
        let mut process = Process(
            command
                .arg("connect")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
        );
        let mut writer = process
            .0
            .stdin
            .take()
            .ok_or_else(|| Error::Protocol("hc connect has no input pipe".into()))?;
        let stdout = process
            .0
            .stdout
            .take()
            .ok_or_else(|| Error::Protocol("hc connect has no output pipe".into()))?;
        let mut reader = BufReader::new(stdout);
        serde_json::to_writer(
            &mut writer,
            &json!({
                "jsonrpc": "2.0", "id": 0, "method": "hello",
                "params": {"generation": 1, "framed": true,
                    "features": ["automatic_input", "resize_notifications"]}
            }),
        )?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(Error::lost("hc connect ended before its hello reply"));
        }
        let reply: Value = serde_json::from_str(&line)?;
        if reply.get("id").and_then(Value::as_u64) != Some(0) {
            return Err(Error::Protocol(
                "core hello reply has the wrong request id".into(),
            ));
        }
        let hello = protocol::rpc_result(reply)?;
        validate_hello(&hello)?;
        let inner = Arc::new(Inner {
            writer: Mutex::new(writer),
            process: Mutex::new(process),
            routing: Mutex::new(Routing::default()),
            waker: Mutex::new(None),
            hello,
            next_id: AtomicU64::new(1),
        });
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("hc-connection".into())
            .spawn(move || read_connection(&mut reader, weak))?;
        Ok(Self(inner))
    }

    pub fn hello(&self) -> &Value {
        &self.0.hello
    }

    pub fn supports(&self, feature: &str) -> bool {
        advertised(self.hello(), "features", feature)
    }

    pub fn supports_method(&self, method: &str) -> bool {
        advertised(self.hello(), "methods", method)
    }

    pub fn error(&self) -> Option<Error> {
        locked(&self.0.routing).error.clone()
    }

    pub fn set_waker(&self, waker: mpsc::SyncSender<()>) {
        *locked(&self.0.waker) = Some(waker);
        self.0.wake();
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        if matches!(method, "terminal.attach" | "events.subscribe") {
            return Err(Error::Protocol(format!(
                "use the dedicated client method for {method}"
            )));
        }
        Ok(self.call(method, params, false)?.value)
    }

    fn call(&self, method: &str, params: Value, stream: bool) -> Result<Response> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))?;
        let (sender, receiver) = mpsc::channel();
        {
            let mut routing = locked(&self.0.routing);
            if let Some(error) = &routing.error {
                return Err(error.clone());
            }
            routing.pending.insert(id, Pending { sender, stream });
        }
        if let Err(error) = self.0.send(0, protocol::DATA, &payload) {
            locked(&self.0.routing).pending.remove(&id);
            return Err(error);
        }
        receiver.recv().map_err(|_| {
            self.error()
                .unwrap_or_else(|| Error::lost("core reply channel closed"))
        })?
    }

    pub fn subscribe(&self) -> Result<Subscription> {
        let response = self.call("events.subscribe", json!({}), true)?;
        let stream = response
            .stream
            .ok_or_else(|| Error::Protocol("core subscription has no stream channel".into()))?;
        Ok(Subscription {
            snapshot: response.value,
            channel: Channel::new(self.clone(), stream),
            lines: EventLines::default(),
        })
    }

    pub fn attach(&self, options: &AttachOptions) -> Result<Attachment> {
        if options.cols == 0 || options.rows == 0 {
            return Err(Error::Protocol(
                "terminal dimensions must be nonzero".into(),
            ));
        }
        let response = self.call(
            "terminal.attach",
            json!({
                "terminal": options.terminal,
                "cols": options.cols, "rows": options.rows,
                "observe": options.observe, "history": options.history,
                "resumed": options.resumed, "display": options.display,
                "caller": options.caller,
                "resize_notifications": options.observe && self.supports("resize_notifications")
            }),
            true,
        )?;
        let stream = response
            .stream
            .ok_or_else(|| Error::Protocol("core attach has no stream channel".into()))?;
        Ok(Attachment {
            channel: stream.id,
            terminal: response
                .value
                .get("terminal")
                .cloned()
                .unwrap_or(Value::Null),
            automatic_input: self.supports("automatic_input")
                && response
                    .value
                    .get("automatic_input")
                    .and_then(Value::as_bool)
                    == Some(true),
            resize_notifications: self.supports("resize_notifications")
                && response
                    .value
                    .get("resize_notifications")
                    .and_then(Value::as_bool)
                    == Some(true),
            observe: options.observe,
            stream: Channel::new(self.clone(), stream),
        })
    }

    pub fn disconnect(&self) -> Result<()> {
        self.0.fail(Error::lost("core connection was disconnected"));
        let mut process = locked(&self.0.process);
        if process.0.try_wait()?.is_none() {
            if let Err(error) = process.0.kill() {
                if process.0.try_wait()?.is_none() {
                    return Err(error.into());
                }
            }
        }
        process.0.wait()?;
        Ok(())
    }
}

fn advertised(hello: &Value, field: &str, value: &str) -> bool {
    hello
        .get(field)
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|item| item.as_str() == Some(value)))
}

fn validate_hello(hello: &Value) -> Result<()> {
    if hello
        .pointer("/core/id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(Error::Protocol(
            "core hello has no permanent core id".into(),
        ));
    }
    if hello.get("generation").and_then(Value::as_u64) != Some(1) {
        return Err(Error::Protocol(format!(
            "this TUI requires core protocol generation 1; core announced {}",
            hello.get("generation").unwrap_or(&Value::Null)
        )));
    }
    if hello.get("framed").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Protocol(
            "core did not enable framed terminal streams".into(),
        ));
    }
    for method in [
        "terminal.run",
        "terminal.list",
        "terminal.attach",
        "events.subscribe",
        "doc.get",
        "doc.set",
    ] {
        if !advertised(hello, "methods", method) {
            return Err(Error::Protocol(format!(
                "core does not provide required method {method}"
            )));
        }
    }
    Ok(())
}

fn read_connection(reader: &mut impl io::Read, weak: Weak<Inner>) {
    loop {
        let frame = match Frame::read(reader) {
            Ok(Some(frame)) => frame,
            result => {
                if let Some(inner) = weak.upgrade() {
                    inner.fail(match result {
                        Err(error) => error.into(),
                        _ => {
                            Error::lost("hc connect closed; terminal programs may still be running")
                        }
                    });
                }
                return;
            }
        };
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if let Err(error) = dispatch(&inner, frame) {
            inner.fail(error);
            return;
        }
        inner.wake();
    }
}

fn dispatch(inner: &Inner, frame: Frame) -> Result<()> {
    if frame.channel == 0 {
        if frame.kind != protocol::DATA {
            return Ok(());
        }
        let message: Value = serde_json::from_slice(&frame.payload)?;
        let Some(id) = protocol::response_id(&message)? else {
            return Ok(());
        };
        let pending = locked(&inner.routing).pending.remove(&id);
        let Some(pending) = pending else {
            return Ok(());
        };
        let response = protocol::rpc_result(message).and_then(|value| {
            let stream = if pending.stream {
                let id = value
                    .get("channel")
                    .and_then(Value::as_u64)
                    .and_then(|id| u32::try_from(id).ok())
                    .filter(|id| *id != 0)
                    .ok_or_else(|| {
                        Error::Protocol("core returned an invalid stream channel".into())
                    })?;
                let (sender, receiver) = mpsc::channel();
                let mut routing = locked(&inner.routing);
                if let Some(error) = &routing.error {
                    return Err(error.clone());
                }
                if routing.channels.contains_key(&id) {
                    return Err(Error::Protocol(
                        "core reused an active stream channel".into(),
                    ));
                }
                // The receiver must exist before the reader accepts the stream's first paint.
                routing.channels.insert(id, sender);
                Some(OpenedStream { id, receiver })
            } else {
                None
            };
            Ok(Response { value, stream })
        });
        if let Err(mpsc::SendError(Ok(response))) = pending.sender.send(response) {
            if let Some(stream) = response.stream {
                locked(&inner.routing).channels.remove(&stream.id);
                let _ = inner.send(stream.id, protocol::CLOSE, &[]);
            }
        }
        return Ok(());
    }
    let event = match frame.kind {
        protocol::DATA => StreamEvent::Data(frame.payload),
        protocol::CLOSE => StreamEvent::Closed(if frame.payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&frame.payload)?
        }),
        protocol::RESIZE if frame.payload.len() == 4 => StreamEvent::Resize {
            cols: u16::from_le_bytes([frame.payload[0], frame.payload[1]]),
            rows: u16::from_le_bytes([frame.payload[2], frame.payload[3]]),
        },
        protocol::RESIZE => {
            return Err(Error::Protocol(
                "core resize frame must contain four bytes".into(),
            ))
        }
        _ => return Ok(()),
    };
    let mut routing = locked(&inner.routing);
    let sender = if matches!(event, StreamEvent::Closed(_)) {
        routing.channels.remove(&frame.channel)
    } else {
        routing.channels.get(&frame.channel).cloned()
    };
    if let Some(sender) = sender {
        if sender.send(event).is_err() {
            routing.channels.remove(&frame.channel);
        }
    }
    Ok(())
}

struct Channel {
    client: Client,
    id: u32,
    receiver: mpsc::Receiver<StreamEvent>,
    closed: AtomicBool,
}

impl Channel {
    fn new(client: Client, stream: OpenedStream) -> Self {
        Self {
            client,
            id: stream.id,
            receiver: stream.receiver,
            closed: AtomicBool::new(false),
        }
    }

    fn received(&self, event: StreamEvent) -> StreamEvent {
        if matches!(event, StreamEvent::Closed(_) | StreamEvent::Lost) {
            self.closed.store(true, Ordering::Relaxed);
        }
        event
    }

    fn try_recv(&self) -> std::result::Result<StreamEvent, mpsc::TryRecvError> {
        self.receiver.try_recv().map(|event| self.received(event))
    }

    fn recv(&self) -> std::result::Result<StreamEvent, mpsc::RecvError> {
        self.receiver.recv().map(|event| self.received(event))
    }

    fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<StreamEvent, mpsc::RecvTimeoutError> {
        self.receiver
            .recv_timeout(timeout)
            .map(|event| self.received(event))
    }

    fn send(&self, kind: u8, payload: &[u8]) -> Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Error::lost("core stream has ended"));
        }
        self.client.0.send(self.id, kind, payload)
    }

    fn send_stream(&self, kind: u8, bytes: &[u8]) -> Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Error::lost("core stream has ended"));
        }
        self.client.0.send_stream(self.id, kind, bytes)
    }

    fn credit(&self, bytes: usize) -> Result<()> {
        let bytes = u32::try_from(bytes).map_err(|_| {
            Error::Protocol("credit does not fit the core protocol's u32 field".into())
        })?;
        if bytes == 0 {
            return Ok(());
        }
        self.send(protocol::CREDIT, &bytes.to_le_bytes())
    }

    fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        self.client.0.send(self.id, protocol::CLOSE, &[])
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        let _ = self.close();
        locked(&self.client.0.routing).channels.remove(&self.id);
    }
}

pub struct Attachment {
    pub channel: u32,
    pub terminal: Value,
    pub automatic_input: bool,
    pub resize_notifications: bool,
    observe: bool,
    stream: Channel,
}

impl Attachment {
    pub fn try_recv(&self) -> std::result::Result<StreamEvent, mpsc::TryRecvError> {
        self.stream.try_recv()
    }

    pub fn recv(&self) -> std::result::Result<StreamEvent, mpsc::RecvError> {
        self.stream.recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<StreamEvent, mpsc::RecvTimeoutError> {
        self.stream.recv_timeout(timeout)
    }

    fn input(&self, kind: u8, bytes: &[u8]) -> Result<()> {
        if self.observe {
            return Err(Error::Protocol(
                "an observing attachment cannot send input".into(),
            ));
        }
        self.stream.send_stream(kind, bytes)
    }

    pub fn send_data(&self, bytes: &[u8]) -> Result<()> {
        self.input(protocol::DATA, bytes)
    }

    pub fn automatic_input(&self, bytes: &[u8]) -> Result<()> {
        self.input(
            if self.automatic_input {
                protocol::AUTOMATIC_INPUT
            } else {
                protocol::DATA
            },
            bytes,
        )
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        if self.observe {
            return Err(Error::Protocol(
                "an observing attachment cannot resize the terminal".into(),
            ));
        }
        if cols == 0 || rows == 0 {
            return Err(Error::Protocol(
                "terminal dimensions must be nonzero".into(),
            ));
        }
        let mut payload = [0; 4];
        payload[..2].copy_from_slice(&cols.to_le_bytes());
        payload[2..].copy_from_slice(&rows.to_le_bytes());
        self.stream.send(protocol::RESIZE, &payload)
    }

    pub fn focus(&self, focused: bool) -> Result<()> {
        self.stream.send(protocol::FOCUS, &[u8::from(focused)])
    }

    pub fn facts(&self, display: &Display) -> Result<()> {
        self.stream
            .send(protocol::FACTS, &serde_json::to_vec(display)?)
    }

    pub fn credit(&self, bytes: usize) -> Result<()> {
        self.stream.credit(bytes)
    }

    pub fn close(&self) -> Result<()> {
        self.stream.close()
    }
}

pub struct Subscription {
    pub snapshot: Value,
    channel: Channel,
    lines: EventLines,
}

impl Subscription {
    pub fn try_event(&mut self) -> Result<Option<Value>> {
        loop {
            if let Some(event) = self.lines.pop() {
                return Ok(Some(event));
            }
            match self.channel.try_recv() {
                Ok(event) => self.consume(event)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => return Err(self.lost()),
            }
        }
    }

    pub fn recv_event(&mut self) -> Result<Value> {
        loop {
            if let Some(event) = self.lines.pop() {
                return Ok(event);
            }
            let event = self.channel.recv().map_err(|_| self.lost())?;
            self.consume(event)?;
        }
    }

    fn lost(&self) -> Error {
        self.channel.client.error().unwrap_or_else(|| {
            Error::lost("core event stream ended; subscribe again for a fresh snapshot")
        })
    }

    fn consume(&mut self, event: StreamEvent) -> Result<()> {
        match event {
            StreamEvent::Data(bytes) => {
                self.lines.push(&bytes)?;
                self.channel.credit(bytes.len())
            }
            StreamEvent::Closed(reason) => {
                Err(Error::lost(format!("core event stream closed: {reason}")))
            }
            StreamEvent::Lost => Err(self.lost()),
            StreamEvent::Resize { .. } => Err(Error::Protocol(
                "core sent terminal resize on an event stream".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_generation_requires_the_runtime_methods_but_not_new_features() {
        let hello = json!({
            "core": {"id": "owned-core"},
            "generation": 1, "framed": true,
            "methods": ["terminal.run", "terminal.list", "terminal.attach", "events.subscribe", "doc.get", "doc.set"]
        });
        validate_hello(&hello).unwrap();
        assert!(!advertised(&hello, "features", "automatic_input"));
        let mut newer = hello.clone();
        newer["generation"] = json!(2);
        assert!(validate_hello(&newer)
            .unwrap_err()
            .to_string()
            .contains("generation 1"));
        let mut unidentified = hello.clone();
        unidentified["core"] = Value::Null;
        assert!(validate_hello(&unidentified)
            .unwrap_err()
            .to_string()
            .contains("permanent core id"));
        let mut missing = hello;
        missing["methods"] = json!(["terminal.list"]);
        assert!(validate_hello(&missing)
            .unwrap_err()
            .to_string()
            .contains("terminal.run"));
    }
}
