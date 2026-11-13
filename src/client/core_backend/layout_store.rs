use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use herdr_core_tui::connection::Client;
use herdr_core_tui::edit_queue::EditQueue;
use herdr_core_tui::model::{Document, ModelError, Operation};
use herdr_core_tui::persistence::{self, LayoutStore};

enum Command {
    Apply(u64, Box<Operation>),
    Refresh,
    Client(Client),
    Disconnected,
    Discard,
    Stop,
}

pub(super) struct Store {
    pub document: Document,
    pub loaded: bool,
    pub pending: bool,
    pub error: Option<String>,
    queue: EditQueue,
    next: u64,
    discarded_through: u64,
    discard_watermark: Arc<AtomicU64>,
    sender: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

pub(super) struct Update {
    pub owner: Option<String>,
    document: Option<Document>,
    acknowledged: u64,
    discarded_through: u64,
    pending: bool,
    error: Option<String>,
}

impl Store {
    pub fn start(
        client: Client,
        owner: Option<String>,
        sender: mpsc::Sender<Update>,
        wake: mpsc::SyncSender<()>,
    ) -> std::io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let discard_watermark = Arc::new(AtomicU64::new(0));
        let worker_discard = discard_watermark.clone();
        let thread = thread::Builder::new()
            .name("herdr-tui-layout".into())
            .spawn(move || {
                let mut client = client;
                let mut store = None;
                let mut queue = EditQueue::default();
                let mut discarded_through = 0;
                let mut reload_after_discard = false;
                let mut retry = false;
                let mut initial = true;
                let mut connected = true;
                loop {
                    let command = if initial {
                        initial = false;
                        Some(Command::Refresh)
                    } else if retry {
                        match receiver.recv_timeout(Duration::from_millis(100)) {
                            Ok(command) => Some(command),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    } else {
                        match receiver.recv() {
                            Ok(command) => Some(command),
                            Err(_) => break,
                        }
                    };
                    if matches!(command, Some(Command::Stop)) {
                        break;
                    }
                    if discard_local(
                        &worker_discard,
                        &mut queue,
                        &mut store,
                        &mut discarded_through,
                    ) {
                        reload_after_discard = true;
                    }
                    let result = match command {
                        Some(Command::Client(next)) => {
                            client = next;
                            connected = true;
                            reload_after_discard = false;
                            refresh(&mut store, &client, owner.clone())
                        }
                        Some(Command::Refresh) => {
                            if !connected {
                                Ok(())
                            } else {
                                reload_after_discard = false;
                                refresh(&mut store, &client, owner.clone())
                            }
                        }
                        Some(Command::Disconnected) => {
                            connected = false;
                            Ok(())
                        }
                        Some(Command::Discard) => Ok(()),
                        Some(Command::Apply(sequence, operation)) => queue
                            .push(sequence, *operation)
                            .map(|_| ())
                            .map_err(|error| {
                                persistence::Error::Model(ModelError(error.to_string()))
                            }),
                        None if reload_after_discard && connected => {
                            reload_after_discard = false;
                            refresh(&mut store, &client, owner.clone())
                        }
                        None => Ok(()),
                        Some(Command::Stop) => unreachable!(),
                    };
                    if discard_local(
                        &worker_discard,
                        &mut queue,
                        &mut store,
                        &mut discarded_through,
                    ) {
                        reload_after_discard = true;
                    }
                    let result = result.and_then(|_| {
                        if !connected {
                            return Ok(());
                        }
                        match store.as_mut() {
                            Some(store) => queue.apply_prefix(|operation| {
                                store.apply(operation.clone()).map(|_| ())
                            }),
                            None if queue.pending() => {
                                Err(persistence::Error::InvalidReply("layout has not loaded"))
                            }
                            None => Ok(()),
                        }
                    });
                    let mut error = result.err().map(|error| error.to_string());
                    if discard_local(
                        &worker_discard,
                        &mut queue,
                        &mut store,
                        &mut discarded_through,
                    ) {
                        reload_after_discard = true;
                        error = None;
                    }
                    retry = connected && reload_after_discard;
                    if connected && error.is_none() {
                        if let Some(store) = store.as_mut() {
                            match store.save(&client) {
                                Ok(saved) => retry = !saved,
                                Err(failed) => error = Some(failed.to_string()),
                            }
                        }
                    }
                    if discard_local(
                        &worker_discard,
                        &mut queue,
                        &mut store,
                        &mut discarded_through,
                    ) {
                        reload_after_discard = true;
                        retry = connected;
                        error = None;
                    }
                    let update = Update {
                        owner: owner.clone(),
                        document: store.as_ref().map(|store| store.document().clone()),
                        acknowledged: queue.acknowledged(),
                        discarded_through,
                        pending: queue.pending()
                            || store.as_ref().is_some_and(LayoutStore::pending),
                        error,
                    };
                    if sender.send(update).is_err() {
                        break;
                    }
                    let _ = wake.try_send(());
                }
            })?;
        Ok(Self {
            document: Document::default(),
            loaded: false,
            pending: false,
            error: None,
            queue: EditQueue::default(),
            next: 0,
            discarded_through: 0,
            discard_watermark,
            sender: commands,
            thread: Some(thread),
        })
    }

    pub fn apply(&mut self, operation: Operation) -> std::io::Result<()> {
        if !self.loaded {
            return Err(std::io::Error::other(
                "layout is unavailable; refresh before editing it",
            ));
        }
        let mut next = self.document.clone();
        if !next.apply(&operation).map_err(std::io::Error::other)? {
            return Ok(());
        }
        self.next += 1;
        self.queue
            .push(self.next, operation.clone())
            .map_err(std::io::Error::other)?;
        self.document = next;
        self.pending = true;
        if let Err(error) = self
            .sender
            .send(Command::Apply(self.next, Box::new(operation)))
        {
            self.error = Some(format!("could not send unsaved layout edit: {error}"));
            return Err(std::io::Error::other(error));
        }
        Ok(())
    }

    pub fn update(&mut self, update: Update) {
        if update.discarded_through < self.discarded_through {
            return;
        }
        self.error = update.error;
        self.queue.acknowledge(update.acknowledged);
        if let Some(mut document) = update.document {
            self.loaded = true;
            let replay = self
                .queue
                .iter()
                .try_for_each(|(_, operation)| document.apply(operation).map(|_| ()));
            match replay {
                Ok(()) => self.document = document,
                Err(error) => self.error = Some(format!("unsaved layout conflict: {error}")),
            }
        }
        self.pending = update.pending || self.queue.pending();
    }

    pub fn refresh(&self) {
        let _ = self.sender.send(Command::Refresh);
    }

    pub fn connected(&self, client: Client) {
        let _ = self.sender.send(Command::Client(client));
    }

    pub fn disconnected(&self) {
        let _ = self.sender.send(Command::Disconnected);
    }

    pub fn discard(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        self.discarded_through = self.next;
        self.discard_watermark.store(self.next, Ordering::Release);
        self.queue.acknowledge(self.next);
        self.document = Document::default();
        self.loaded = false;
        self.pending = false;
        self.error = None;
        if let Err(error) = self.sender.send(Command::Discard) {
            self.error = Some(format!(
                "local edits discarded; layout worker unavailable: {error}"
            ));
        }
        true
    }

    pub fn stop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn refresh(
    store: &mut Option<LayoutStore>,
    client: &Client,
    owner: Option<String>,
) -> Result<(), persistence::Error> {
    match store {
        Some(store) => store.refresh(client),
        None => LayoutStore::load(client, owner).map(|loaded| *store = Some(loaded)),
    }
}

fn discard_local(
    watermark: &AtomicU64,
    queue: &mut EditQueue,
    store: &mut Option<LayoutStore>,
    discarded_through: &mut u64,
) -> bool {
    let through = watermark.load(Ordering::Acquire);
    if through <= *discarded_through {
        return false;
    }
    // An in-flight CAS may have committed; only a fresh read can restore its saved baseline.
    queue.acknowledge(through);
    *store = None;
    *discarded_through = through;
    true
}
