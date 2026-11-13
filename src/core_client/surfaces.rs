use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::mpsc;
use std::thread;

use crate::connection::{AttachOptions, Attachment, Client, Display, StreamEvent};
use crate::input::TerminalKey;
use crate::paint::InitialPaint;
use crate::terminal::TerminalView;
use serde_json::Value;

const FRAME_BATCH: usize = 64;
const PANE_BATCH: usize = 16;

#[derive(Clone, Debug)]
pub struct PaneSpec {
    pub id: String,
    pub address: String,
    pub cols: u16,
    pub rows: u16,
    pub focused: bool,
    pub live: bool,
    pub completion: Option<Value>,
}

#[derive(Clone)]
pub enum Input {
    Bytes(Vec<u8>),
    Key(TerminalKey),
    Paste(String),
    Mouse {
        kind: crossterm::event::MouseEventKind,
        modifiers: crossterm::event::KeyModifiers,
        position: crate::input::mouse::Position,
        lines: u16,
    },
}

impl Input {
    fn encode(&self, view: &mut TerminalView) -> io::Result<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Ok(bytes.clone()),
            Self::Paste(text) => view.encode_paste(text),
            Self::Mouse {
                kind,
                modifiers,
                position,
                lines,
            } => {
                let Some((mut encoder, point)) =
                    crate::pane_input::ghostty_mouse_encoder_for_terminal(
                        view.terminal(),
                        *position,
                    )
                else {
                    return Ok(Vec::new());
                };
                let (column, row) = match position {
                    crate::input::mouse::Position::Cell { column, row } => (*column, *row),
                    crate::input::mouse::Position::Pixels { .. } => (0, 0),
                };
                let event = crate::pane_input::ghostty_mouse_event_from_button_kind(
                    *kind, column, row, *modifiers,
                )
                .or_else(|| {
                    crate::pane_input::ghostty_mouse_event_from_wheel_kind(
                        *kind, column, row, *modifiers,
                    )
                })
                .or_else(|| {
                    crate::pane_input::ghostty_mouse_event_from_motion_kind(
                        *kind, column, row, *modifiers,
                    )
                });
                let Some(mut event) = event else {
                    return Ok(Vec::new());
                };
                event.set_position(point.0, point.1);
                let bytes = encoder.encode(&event).map_err(io::Error::other)?;
                let count = if matches!(
                    kind,
                    crossterm::event::MouseEventKind::ScrollUp
                        | crossterm::event::MouseEventKind::ScrollDown
                        | crossterm::event::MouseEventKind::ScrollLeft
                        | crossterm::event::MouseEventKind::ScrollRight
                ) {
                    usize::from((*lines).max(1))
                } else {
                    1
                };
                Ok(bytes.repeat(count))
            }
            Self::Key(key) => {
                let legacy = view.keyboard_negotiated_nothing()?;
                if legacy && crate::pane_input::legacy_super_chord(key) {
                    return Ok(Vec::new());
                }
                let key = if legacy {
                    crate::pane_input::legacy_shell_key(key.clone())
                } else {
                    key.clone()
                };
                let event = crate::pane_input::ghostty_key_event_from_terminal_key(&key, legacy)
                    .ok_or_else(|| {
                        io::Error::other("this key cannot be encoded for the terminal")
                    })?;
                view.encode_event(&event)
            }
        }
    }
}

pub struct PaneStream {
    pub spec: PaneSpec,
    pub view: TerminalView,
    pub error: Option<String>,
    pub completion: Option<Value>,
    // The frontend reserves the low bit for content that has not been painted yet.
    pub revision: u64,
    pub selection: Option<crate::terminal::Selection>,
    attachment: Option<Attachment>,
    observe: bool,
    connecting: bool,
    attempt: u64,
    paint: InitialPaint,
    inputs: VecDeque<Input>,
    input_blocked: bool,
    resumed: bool,
}

impl PaneStream {
    pub fn input_ready(&self) -> bool {
        self.attachment.is_some()
            && self.completion.is_none()
            && !self.observe
            && self.paint.ready()
            && !self.input_blocked
    }

    pub fn pending_input(&self) -> usize {
        self.inputs.len()
    }

    fn attachment_failed(&mut self, error: impl ToString) {
        self.attachment = None;
        self.connecting = false;
        self.attempt += 1;
        self.error = Some(error.to_string());
    }
}

struct Attached {
    generation: u64,
    pane: String,
    address: String,
    observe: bool,
    attempt: u64,
    requested: (u16, u16),
    result: Result<Attachment, String>,
}

#[derive(Default)]
pub struct Update {
    pub changed: bool,
    pub clipboard: Vec<Vec<u8>>,
    pub bells: Vec<u16>,
    pub notices: Vec<String>,
}

pub struct Streams {
    client: Client,
    display: Display,
    caller: Option<String>,
    scrollback: usize,
    generation: u64,
    next_pane: usize,
    panes: HashMap<String, PaneStream>,
    sender: mpsc::Sender<Attached>,
    receiver: mpsc::Receiver<Attached>,
    wake: mpsc::SyncSender<()>,
    outer_focused: bool,
}

impl Streams {
    pub fn new(
        client: Client,
        display: Display,
        caller: Option<String>,
        scrollback: usize,
        wake: mpsc::SyncSender<()>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            client,
            display,
            caller,
            scrollback,
            generation: 0,
            next_pane: 0,
            panes: HashMap::new(),
            sender,
            receiver,
            wake,
            outer_focused: true,
        }
    }

    pub fn pane(&self, id: &str) -> Option<&PaneStream> {
        self.panes.get(id)
    }

    pub fn pane_mut(&mut self, id: &str) -> Option<&mut PaneStream> {
        self.panes.get_mut(id)
    }

    pub fn pending_input_count(&self) -> usize {
        self.panes.values().map(|pane| pane.inputs.len()).sum()
    }

    pub fn discard_all_input(&mut self) {
        for pane in self.panes.values_mut() {
            pane.inputs.clear();
            pane.input_blocked = false;
        }
    }

    pub fn set_outer_focus(&mut self, focused: bool) -> io::Result<()> {
        self.outer_focused = focused;
        for pane in self.panes.values_mut() {
            if let Some(attachment) = &pane.attachment {
                if let Err(error) = attachment.focus(focused && pane.spec.focused) {
                    pane.attachment_failed(error);
                }
            }
        }
        Ok(())
    }

    pub fn set_display(&mut self, display: Display) -> io::Result<()> {
        self.display = display;
        for pane in self.panes.values_mut() {
            if let Some([width, height]) = self.display.cell {
                pane.view.set_cell_size(width, height)?;
            }
            if let Some(attachment) = &pane.attachment {
                if let Err(error) = attachment.facts(&self.display) {
                    pane.attachment_failed(error);
                }
            }
        }
        Ok(())
    }

    pub fn reconnect(&mut self, client: Client) -> usize {
        self.client = client;
        self.generation += 1;
        let mut held = 0;
        for pane in self.panes.values_mut() {
            if !pane.inputs.is_empty() {
                pane.input_blocked = true;
                held += pane.inputs.len();
            }
            pane.attachment = None;
            pane.connecting = false;
            pane.error = None;
            pane.attempt += 1;
            pane.resumed = true;
        }
        held
    }

    pub fn recovered(&mut self, address: &str) -> usize {
        let mut held = 0;
        for pane in self
            .panes
            .values_mut()
            .filter(|pane| pane.spec.address == address)
        {
            pane.attachment = None;
            pane.connecting = false;
            pane.completion = None;
            pane.error = None;
            pane.attempt += 1;
            if !pane.inputs.is_empty() {
                pane.input_blocked = true;
                held += pane.inputs.len();
            }
        }
        held
    }

    pub fn retry(&mut self) {
        for pane in self.panes.values_mut() {
            pane.error = None;
        }
    }

    pub fn reconcile(&mut self, specs: &[PaneSpec], retained: &HashSet<String>) -> io::Result<()> {
        self.panes.retain(|id, _| retained.contains(id));
        let visible: HashSet<_> = specs.iter().map(|spec| spec.id.as_str()).collect();
        for pane in self.panes.values_mut() {
            if !visible.contains(pane.spec.id.as_str()) && pane.spec.focused {
                if let Some(attachment) = &pane.attachment {
                    if let Err(error) = attachment.focus(false) {
                        pane.attachment_failed(error);
                    }
                }
                pane.spec.focused = false;
            }
        }
        let mut ordered: Vec<_> = specs.to_vec();
        ordered.sort_by_key(|spec| !spec.focused);
        ordered.extend(
            self.panes
                .values()
                .filter(|pane| !visible.contains(pane.spec.id.as_str()))
                .map(|pane| pane.spec.clone()),
        );
        let mut addresses = HashSet::new();
        for spec in ordered {
            if spec.cols == 0 || spec.rows == 0 {
                continue;
            }
            let observe = !addresses.insert(spec.address.clone());
            if !self.panes.contains_key(&spec.id) {
                self.panes.insert(
                    spec.id.clone(),
                    PaneStream {
                        spec: spec.clone(),
                        view: TerminalView::new(spec.cols, spec.rows, self.scrollback)?,
                        error: None,
                        completion: None,
                        revision: 0,
                        selection: None,
                        attachment: None,
                        observe,
                        connecting: false,
                        attempt: 0,
                        paint: InitialPaint::default(),
                        inputs: VecDeque::new(),
                        input_blocked: false,
                        resumed: false,
                    },
                );
            }
            let pane = self.panes.get_mut(&spec.id).expect("pane inserted above");
            if pane.spec.address != spec.address || pane.observe != observe {
                if pane.spec.address != spec.address {
                    pane.selection = None;
                    if !pane.inputs.is_empty() {
                        pane.input_blocked = true;
                        pane.attachment_failed(
                            "The terminal view changed identity. Discard held input before attaching its replacement.",
                        );
                        continue;
                    }
                    pane.completion = None;
                }
                pane.attachment = None;
                pane.connecting = false;
                pane.error = None;
                pane.observe = observe;
                pane.attempt += 1;
            }
            if let Some(attachment) = &pane.attachment {
                let updated = (|| -> io::Result<()> {
                    if !observe && (pane.spec.cols, pane.spec.rows) != (spec.cols, spec.rows) {
                        attachment.resize(spec.cols, spec.rows)?;
                    }
                    if pane.spec.focused != spec.focused {
                        attachment.focus(self.outer_focused && spec.focused)?;
                    }
                    Ok(())
                })();
                if let Err(error) = updated {
                    pane.attachment_failed(error);
                }
            }
            pane.spec = spec.clone();
            if let Some(completion) = &spec.completion {
                pane.completion = Some(completion.clone());
            }
            if pane.attachment.is_none()
                && !pane.connecting
                && pane.completion.is_none()
                && pane.error.is_none()
                && spec.live
                && self.client.error().is_none()
            {
                pane.connecting = true;
                pane.attempt += 1;
                let mut options = AttachOptions::new(&spec.address, spec.cols, spec.rows);
                options.observe = observe;
                options.history = u32::MAX as usize;
                options.display = self.display.clone();
                options.caller = self.caller.clone();
                options.resumed = pane.resumed;
                let client = self.client.clone();
                let sender = self.sender.clone();
                let wake = self.wake.clone();
                let generation = self.generation;
                let id = spec.id.clone();
                let attempt = pane.attempt;
                thread::spawn(move || {
                    let result = client.attach(&options).map_err(|error| error.to_string());
                    let _ = sender.send(Attached {
                        generation,
                        pane: id,
                        address: options.terminal,
                        observe,
                        attempt,
                        requested: (options.cols, options.rows),
                        result,
                    });
                    let _ = wake.try_send(());
                });
            }
        }
        Ok(())
    }

    pub fn input(&mut self, id: &str, input: Input) -> io::Result<()> {
        let pane = self
            .panes
            .get_mut(id)
            .ok_or_else(|| io::Error::other("the intended terminal view is not attached yet"))?;
        if pane.completion.is_some() {
            return Err(io::Error::other("this terminal program has finished"));
        }
        pane.inputs.push_back(input);
        Ok(())
    }

    pub fn discard_input(&mut self, id: &str) {
        if let Some(pane) = self.panes.get_mut(id) {
            pane.inputs.clear();
            pane.input_blocked = false;
        }
    }

    pub fn poll(&mut self) -> io::Result<Update> {
        let mut update = Update::default();
        for _ in 0..FRAME_BATCH {
            let Ok(message) = self.receiver.try_recv() else {
                break;
            };
            if message.generation != self.generation {
                continue;
            }
            let Some(pane) = self.panes.get_mut(&message.pane).filter(|pane| {
                pane.spec.address == message.address
                    && pane.observe == message.observe
                    && pane.attempt == message.attempt
            }) else {
                continue;
            };
            pane.connecting = false;
            match message.result {
                Ok(attachment) => {
                    let cols = dimension(&attachment.terminal, "cols")?;
                    let rows = dimension(&attachment.terminal, "rows")?;
                    pane.view.reset()?;
                    pane.selection = None;
                    pane.view.resize(cols, rows)?;
                    if let Some([width, height]) = self.display.cell {
                        pane.view.set_cell_size(width, height)?;
                    }
                    pane.paint = InitialPaint::default();
                    let configured = (|| -> io::Result<()> {
                        if !pane.observe && message.requested != (pane.spec.cols, pane.spec.rows) {
                            attachment.resize(pane.spec.cols, pane.spec.rows)?;
                        }
                        if !pane.resumed {
                            attachment.focus(self.outer_focused && pane.spec.focused)?;
                        }
                        Ok(())
                    })();
                    match configured {
                        Ok(()) => {
                            pane.attachment = Some(attachment);
                            pane.error = None;
                            pane.resumed = false;
                        }
                        Err(error) => pane.attachment_failed(error),
                    }
                }
                Err(error) => pane.error = Some(error),
            }
            update.changed = true;
        }
        let mut ids: Vec<_> = self.panes.keys().cloned().collect();
        ids.sort();
        let mut remaining = FRAME_BATCH;
        for _ in 0..ids.len() {
            let index = self.next_pane % ids.len();
            self.next_pane = (index + 1) % ids.len();
            let pane = self
                .panes
                .get_mut(&ids[index])
                .expect("pane keys collected above");
            let Some(attachment) = pane.attachment.as_ref() else {
                continue;
            };
            let mut ended = false;
            for _ in 0..PANE_BATCH.min(remaining) {
                let event = match attachment.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => StreamEvent::Lost,
                };
                remaining -= 1;
                match event {
                    StreamEvent::Data(bytes) => {
                        pane.view.feed(&bytes);
                        pane.paint.push(&bytes);
                        pane.revision += 2;
                        for reply in pane.view.take_replies() {
                            if !pane.observe {
                                if let Err(error) = attachment.automatic_input(&reply) {
                                    pane.error =
                                        Some(format!("terminal reply interrupted: {error}"));
                                    ended = true;
                                    break;
                                }
                            }
                        }
                        let clipboard = pane.view.take_clipboard_writes();
                        if pane.spec.focused {
                            update.clipboard.extend(clipboard);
                        }
                        let bells = pane.view.take_bell_count();
                        if bells > 0 && !pane.observe && self.outer_focused {
                            update.bells.push(bells);
                        }
                        if !ended {
                            if let Err(error) = attachment.credit(bytes.len()) {
                                pane.error = Some(error.to_string());
                                ended = true;
                            }
                        }
                    }
                    StreamEvent::Resize { cols, rows } => {
                        pane.paint = InitialPaint::default();
                        pane.view.resize(cols, rows)?;
                        pane.revision += 2;
                    }
                    StreamEvent::Closed(details) => {
                        if matches!(details["reason"].as_str(), Some("exited" | "killed")) {
                            pane.completion = Some(details);
                            pane.error = None;
                        } else {
                            pane.error = Some(format!("terminal stream closed: {details}"));
                        }
                        ended = true;
                    }
                    StreamEvent::Lost => {
                        pane.error = Some("Connection lost; waiting to reattach".into());
                        ended = true;
                    }
                }
                update.changed = true;
                if ended {
                    break;
                }
            }
            if ended {
                pane.attachment = None;
            }
            if remaining == 0 {
                let _ = self.wake.try_send(());
                break;
            }
        }
        for pane in self.panes.values_mut() {
            if !pane.input_ready() {
                continue;
            }
            while let Some(input) = pane.inputs.pop_front() {
                let sent = input.encode(&mut pane.view).and_then(|bytes| {
                    if bytes.is_empty() {
                        return Ok(());
                    }
                    pane.attachment
                        .as_ref()
                        .expect("input_ready requires attachment")
                        .send_data(&bytes)
                        .map_err(io::Error::from)
                });
                if let Err(error) = sent {
                    pane.inputs.push_front(input);
                    pane.input_blocked = true;
                    update.notices.push(format!(
                        "Input delivery is uncertain: {error}. Held input has not been replayed."
                    ));
                    break;
                }
            }
        }
        Ok(update)
    }
}

fn dimension(record: &Value, name: &str) -> io::Result<u16> {
    record
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid terminal {name}"),
            )
        })
}
