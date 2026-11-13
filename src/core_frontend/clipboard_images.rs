use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use herdr_core_tui::connection::Client;

use super::{App, TerminalInput};
use crate::client::frame::{ClientClipboardImageTarget, ClientPaneInputEvent};
use crate::raw_input::RawInputEvent;

mod worker;

#[derive(Clone, PartialEq, Eq)]
struct Target {
    pane: String,
    owner: String,
    address: String,
    pid: u64,
    foreground_pid: Option<u64>,
    generation: u64,
    local_core: String,
}

impl Target {
    fn foreground_matches(&self, reported: Option<u64>) -> bool {
        // Core can omit foreground facts; only a captured report adds this guard.
        self.foreground_pid.is_none_or(|pid| reported == Some(pid))
    }
}

struct Blocked {
    target: Target,
    events: VecDeque<ClientPaneInputEvent>,
    held: bool,
}

struct Active {
    id: String,
    target: Target,
    trigger: ClientPaneInputEvent,
    cancelled: Arc<AtomicBool>,
    discarded: bool,
}

pub(super) struct Bridge {
    commands: mpsc::Sender<worker::Command>,
    events: mpsc::Receiver<worker::Event>,
    thread: Option<JoinHandle<()>>,
    active: Option<Active>,
    blocked: HashMap<String, Blocked>,
    order: VecDeque<String>,
    paths: HashMap<String, String>,
    last_client: Option<Client>,
    transport: Option<Client>,
    closing: bool,
}

impl Bridge {
    pub(super) fn new(wake: mpsc::SyncSender<()>) -> io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let (sender, events) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("herdr-image-paste".into())
            .spawn(move || worker::run(receiver, sender, wake))?;
        Ok(Self {
            commands,
            events,
            thread: Some(thread),
            active: None,
            blocked: HashMap::new(),
            order: VecDeque::new(),
            paths: HashMap::new(),
            last_client: None,
            transport: None,
            closing: false,
        })
    }

    pub(super) fn pending_count(&self) -> usize {
        self.blocked
            .values()
            .map(|blocked| blocked.events.len())
            .sum::<usize>()
            + usize::from(self.active.as_ref().is_some_and(|active| !active.discarded))
    }

    pub(super) fn discard(&mut self) {
        if let Some(active) = &mut self.active {
            active.discarded = true;
            active.cancelled.store(true, Ordering::Release);
        }
        self.blocked.clear();
        self.order.clear();
    }

    fn block(&mut self, target: Target, event: ClientPaneInputEvent) {
        let pane = target.pane.clone();
        if !self.blocked.contains_key(&pane) {
            self.order.push_back(pane.clone());
        }
        self.blocked
            .entry(pane)
            .or_insert_with(|| Blocked {
                target,
                events: VecDeque::new(),
                held: false,
            })
            .events
            .push_back(event);
    }

    fn start(
        &mut self,
        target: Target,
        trigger: ClientPaneInputEvent,
        client: Client,
        image: crate::platform::ClipboardImage,
    ) -> io::Result<()> {
        let id = herdr_core_tui::model::new_id();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.commands
            .send(worker::Command::Upload {
                id: id.clone(),
                target: target.clone(),
                image,
                cancelled: cancelled.clone(),
            })
            .map_err(|_| io::Error::other("the clipboard image worker stopped"))?;
        self.last_client = Some(client.clone());
        self.active = Some(Active {
            id,
            target,
            trigger,
            cancelled,
            discarded: false,
        });
        Ok(())
    }

    fn finish(&mut self, client: Client) -> io::Result<()> {
        if self.thread.is_none() {
            return Ok(());
        }
        self.closing = true;
        if let Some(active) = &self.active {
            active.cancelled.store(true, Ordering::Release);
        }
        let (reply, receiver) = mpsc::channel();
        self.commands
            .send(worker::Command::Finish {
                client: client.clone(),
                reply,
            })
            .map_err(|_| io::Error::other("the clipboard image cleanup worker stopped"))?;
        let result = match receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.drain_progress();
                if let Some(transport) = &self.transport {
                    let _ = transport.disconnect();
                }
                match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(result) => result,
                    Err(_) => {
                        Err("Image cleanup did not finish after disconnecting this viewer".into())
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("The clipboard image cleanup worker stopped".into())
            }
        };
        self.drain_progress();
        if result.is_err() {
            if let Some(transport) = &self.transport {
                let _ = transport.disconnect();
            }
        }
        let _ = self.commands.send(worker::Command::Stop);
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        } else {
            drop(self.thread.take());
        }
        match result {
            Ok(()) => {
                self.paths.clear();
                Ok(())
            }
            Err(error) => Err(io::Error::other(format!(
                "{error}. Staging paths that may need cleanup: {}",
                if self.paths.is_empty() {
                    "setup was not confirmed".into()
                } else {
                    self.paths.values().cloned().collect::<Vec<_>>().join(", ")
                }
            ))),
        }
    }

    fn drain_progress(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                worker::Event::Transport(client) => {
                    self.transport = Some(client);
                }
                worker::Event::Prepared { id, path } => {
                    self.paths.insert(id, path);
                }
                worker::Event::Removed(id) => {
                    self.paths.remove(&id);
                }
                _ => {}
            }
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if self.thread.is_some() {
            if let Some(client) = self.last_client.clone() {
                if let Err(error) = self.finish(client) {
                    tracing::warn!(error = %error, "clipboard image cleanup failed");
                }
            } else {
                let _ = self.commands.send(worker::Command::Stop);
                drop(self.thread.take());
            }
        }
    }
}

impl App {
    pub(super) fn normalize_image_drop_batch(
        &self,
        mut events: Vec<RawInputEvent>,
    ) -> Vec<RawInputEvent> {
        let Some(ClientClipboardImageTarget::Pane(pane)) = self.shell.clipboard_image_target()
        else {
            return events;
        };
        let remote = self.backend.document().workspaces.iter().any(|workspace| {
            workspace.machine.is_some()
                && workspace.tabs.iter().any(|tab| tab.pane(&pane).is_some())
        });
        if !remote
            || !events
                .first()
                .and_then(plain_vt_character)
                .is_some_and(|character| matches!(character, '/' | '\'' | '"'))
        {
            return events;
        }
        let mut text = String::new();
        let mut end = 0;
        for event in &events {
            let Some(character) = plain_vt_character(event) else {
                break;
            };
            text.push(character);
            end += 1;
        }
        let Some((path, _)) =
            crate::client::clipboard_capture::image_path_from_terminal_drop(text.as_bytes(), true)
        else {
            return events;
        };
        let Ok(metadata) = std::fs::metadata(path) else {
            return events;
        };
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > crate::client::frame::MAX_CLIPBOARD_IMAGE_PAYLOAD as u64
        {
            return events;
        }
        let remainder = events.split_off(end);
        std::iter::once(RawInputEvent::Paste(text))
            .chain(remainder)
            .collect()
    }

    pub(super) fn clipboard_pane_input(
        &mut self,
        pane: &str,
        event: &ClientPaneInputEvent,
    ) -> io::Result<bool> {
        if self.clipboard_images.closing {
            return Ok(false);
        }
        if let Some(blocked) = self.clipboard_images.blocked.get_mut(pane) {
            blocked.events.push_back(event.clone());
            return Ok(true);
        }
        if self
            .clipboard_images
            .active
            .as_ref()
            .is_some_and(|active| !active.discarded && active.target.pane == pane)
        {
            let target = self
                .clipboard_images
                .active
                .as_ref()
                .map(|active| active.target.clone())
                .ok_or_else(|| io::Error::other("the pending image target disappeared"))?;
            self.clipboard_images.block(target, event.clone());
            return Ok(true);
        }
        let trigger = match event {
            ClientPaneInputEvent::Key { .. } => {
                crate::client::clipboard_capture::image_paste_trigger(
                    &event.to_raw_input_event(),
                    true,
                    self.config.remote_image_paste_key().ok().flatten(),
                )
            }
            ClientPaneInputEvent::Paste(text) => text.is_empty(),
            _ => false,
        };
        let dropped = match event {
            ClientPaneInputEvent::Paste(text) | ClientPaneInputEvent::TextCommit(text) => {
                Some(text.as_bytes())
            }
            _ => None,
        };
        let is_file = dropped.is_some_and(|bytes| {
            crate::client::clipboard_capture::image_path_from_terminal_drop(bytes, true).is_some()
        });
        if !trigger && !is_file {
            return Ok(false);
        }
        let remote = self.backend.document().workspaces.iter().any(|workspace| {
            workspace.machine.is_some() && workspace.tabs.iter().any(|tab| tab.pane(pane).is_some())
        });
        if !remote {
            return Ok(false);
        }
        let Some(target) = self.clipboard_target(pane) else {
            self.held_input
                .entry(pane.into())
                .or_default()
                .push_back(event.clone());
            self.blocked_held_input.insert(pane.into());
            self.notice("The image's terminal program could not be verified; the paste and following typing are held until discarded.");
            self.sync_shell();
            return Ok(true);
        };
        if self.shell.clipboard_image_target()
            != Some(ClientClipboardImageTarget::Pane(pane.into()))
            || !self.clipboard_target_current(&target)
        {
            self.clipboard_images.block(target, event.clone());
            if let Some(blocked) = self.clipboard_images.blocked.get_mut(pane) {
                blocked.held = true;
            }
            self.notice("The image's original pane is no longer active; the paste and following typing are held until discarded.");
            self.sync_shell();
            return Ok(true);
        }
        if self.clipboard_images.active.is_some() {
            self.clipboard_images.block(target, event.clone());
            return Ok(true);
        }
        let image = if trigger {
            crate::platform::read_clipboard_image()
        } else {
            dropped.and_then(|bytes| {
                crate::client::clipboard_capture::read_image_file_from_terminal_drop(bytes, true)
            })
        };
        let Some(image) = image else { return Ok(false) };
        self.clipboard_images
            .start(target, event.clone(), self.backend.client().clone(), image)?;
        self.dirty = true;
        self.sync_shell();
        Ok(true)
    }

    fn clipboard_target(&self, pane: &str) -> Option<Target> {
        let owner = self
            .backend
            .document()
            .workspaces
            .iter()
            .find(|workspace| workspace.tabs.iter().any(|tab| tab.pane(pane).is_some()))?
            .machine
            .clone()?;
        let address = self.backend.saved_address(pane)?;
        let record = self.backend.inventory().get(&address)?;
        if record["state"] != "live" {
            return None;
        }
        Some(Target {
            pane: pane.into(),
            owner,
            address,
            pid: record["pid"].as_u64()?,
            foreground_pid: record
                .pointer("/foreground/pid")
                .and_then(serde_json::Value::as_u64),
            generation: self.backend.generation(),
            local_core: self.backend.local_id().into(),
        })
    }

    fn clipboard_target_current(&self, target: &Target) -> bool {
        let Some(current) = self.clipboard_target(&target.pane) else {
            return false;
        };
        self.backend.connected()
            && self.backend.view().active_owner.as_deref() == Some(target.owner.as_str())
            && self
                .backend
                .view()
                .active_pane(self.backend.document())
                .is_some_and(|pane| pane.id == target.pane)
            && current.pane == target.pane
            && current.owner == target.owner
            && current.address == target.address
            && current.pid == target.pid
            && current.generation == target.generation
            && current.local_core == target.local_core
            && target.foreground_matches(current.foreground_pid)
    }

    pub(super) fn poll_clipboard_images(&mut self) -> io::Result<()> {
        if let Some(active) = &self.clipboard_images.active {
            if !self.clipboard_target_current(&active.target) {
                active.cancelled.store(true, Ordering::Release);
            }
        }
        while let Ok(event) = self.clipboard_images.events.try_recv() {
            match event {
                worker::Event::Transport(client) => {
                    self.clipboard_images.transport = Some(client);
                }
                worker::Event::Prepared { id, path } => {
                    self.clipboard_images.paths.insert(id, path);
                }
                worker::Event::Removed(id) => {
                    self.clipboard_images.paths.remove(&id);
                }
                worker::Event::Notice(message) => self.notice(message),
                worker::Event::Completed { id, result } => {
                    let Some(active) = self.clipboard_images.active.take() else {
                        continue;
                    };
                    if active.id != id {
                        return Err(io::Error::other(
                            "the completed image paste belongs to another request",
                        ));
                    }
                    let current = !active.cancelled.load(Ordering::Acquire)
                        && self.clipboard_target_current(&active.target);
                    match result {
                        Ok(path) if current => {
                            let queued = self.clipboard_images.blocked.remove(&active.target.pane);
                            self.clipboard_images
                                .order
                                .retain(|pane| pane != &active.target.pane);
                            if let Err(error) = self
                                .streams
                                .input(&active.target.pane, TerminalInput::Paste(path))
                            {
                                let mut blocked = queued.unwrap_or(Blocked {
                                    target: active.target.clone(),
                                    events: VecDeque::new(),
                                    held: true,
                                });
                                blocked.held = true;
                                self.clipboard_images
                                    .blocked
                                    .insert(active.target.pane.clone(), blocked);
                                self.notice(format!("Image paste delivery is uncertain; queued typing was held: {error}"));
                            } else if let Some(queued) = queued {
                                for event in queued.events {
                                    self.targeted_pane_input(&active.target.pane, event)?;
                                }
                            }
                        }
                        result => {
                            let _ = self
                                .clipboard_images
                                .commands
                                .send(worker::Command::Remove {
                                    id,
                                    client: self.backend.client().clone(),
                                });
                            if !active.discarded {
                                let pane = active.target.pane.clone();
                                let blocked =
                                    self.clipboard_images.blocked.entry(pane).or_insert_with(
                                        || Blocked {
                                            target: active.target.clone(),
                                            events: VecDeque::new(),
                                            held: true,
                                        },
                                    );
                                blocked.events.push_front(active.trigger);
                                blocked.held = true;
                                self.notice(match result {
                                    Err(error) => format!("{error}. The image paste and queued typing are held; discard held typing before continuing."),
                                    Ok(_) => "The image's pane, machine or program changed. The image paste and queued typing are held; discard held typing before continuing.".into(),
                                });
                            }
                        }
                    }
                    self.dirty = true;
                    self.sync_shell();
                }
            }
        }
        while self.clipboard_images.active.is_none() {
            let Some(pane) = self.clipboard_images.order.pop_front() else {
                break;
            };
            let Some(mut blocked) = self.clipboard_images.blocked.remove(&pane) else {
                continue;
            };
            if blocked.held {
                self.clipboard_images.blocked.insert(pane, blocked);
                continue;
            }
            if !self.clipboard_target_current(&blocked.target) {
                blocked.held = true;
                self.clipboard_images.blocked.insert(pane, blocked);
                self.notice("Queued image paste input belongs to a pane or program that changed; discard held typing before continuing.");
                continue;
            }
            for event in blocked.events {
                self.targeted_pane_input(&pane, event)?;
            }
        }
        Ok(())
    }

    pub(super) fn finish_clipboard_images(&mut self) -> io::Result<()> {
        self.clipboard_images.finish(self.backend.client().clone())
    }
}

fn plain_vt_character(event: &RawInputEvent) -> Option<char> {
    let RawInputEvent::Key(key) = event else {
        return None;
    };
    if key.kind != crossterm::event::KeyEventKind::Press || key.has_physical_identity() {
        return None;
    }
    let bytes = key.vt_bytes()?;
    let text = std::str::from_utf8(bytes).ok()?;
    let mut characters = text.chars();
    let character = characters.next()?;
    if character.is_control()
        || characters.next().is_some()
        || key.code != crossterm::event::KeyCode::Char(character)
    {
        return None;
    }
    // Legacy parsing infers Shift from uppercase bytes; encoded key reports are excluded above.
    let modifiers = key.modifiers;
    (modifiers.is_empty()
        || (character.is_ascii_uppercase() && modifiers == crossterm::event::KeyModifiers::SHIFT))
        .then_some(character)
}
