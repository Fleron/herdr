mod actions;
mod info;
mod jobs;
mod launch;
mod layout_store;
mod lifecycle;
mod machines;
mod metadata;
mod presentation;
mod projection;
mod safety;
mod settings;
#[cfg(test)]
mod tests;
mod worktrees;

pub(crate) use actions::Action;
pub(crate) use launch::{LaunchOutcome, LaunchToken, Placement};
pub(crate) use machines::MachineRetryOutcome;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::mpsc;
use std::time::Instant;

use herdr_core_tui::connection::{Client, Subscription};
use herdr_core_tui::inventory::Inventory;
use herdr_core_tui::model::{terminal_address, Document, Operation, ViewState};
use serde_json::Value;

use jobs::JobResult;
use layout_store::Store;

#[derive(Default)]
pub(crate) struct Change {
    pub connection: bool,
    pub inventory: bool,
    pub layout: bool,
    pub profiles: bool,
    pub metadata: bool,
    pub presentation: bool,
    pub mutation: bool,
    pub restarted: Vec<String>,
    pub notices: Vec<String>,
    pub launches: Vec<LaunchOutcome>,
    pub completions: Vec<Completion>,
    pub machine_retries: Vec<MachineRetryOutcome>,
}

pub(crate) struct Completion {
    pub id: String,
    pub result: Result<crate::client::ui_domain::ResponseResult, String>,
}

pub(crate) struct Backend {
    client: Client,
    subscription: Subscription,
    inventory: Inventory,
    stores: BTreeMap<Option<String>, Store>,
    document: Document,
    view: ViewState,
    profiles: Vec<Value>,
    profiles_loaded: bool,
    profiles_in_flight: bool,
    profiles_due: Instant,
    profiles_epoch: u64,
    machine_retries_in_flight: BTreeSet<String>,
    snapshot_in_flight: bool,
    generation: u64,
    connected: bool,
    layout_error: Option<String>,
    revision: u64,
    commands: Vec<crate::client::ui_model::ClientShellCommand>,
    metadata: BTreeMap<Option<String>, metadata::Metadata>,
    metadata_in_flight: BTreeSet<Option<String>>,
    metadata_folders: BTreeMap<Option<String>, BTreeSet<String>>,
    metadata_watch: BTreeMap<Option<String>, BTreeSet<String>>,
    metadata_due: Instant,
    jobs: mpsc::Receiver<JobResult>,
    job_sender: mpsc::Sender<JobResult>,
    layout_updates: mpsc::Receiver<layout_store::Update>,
    layout_sender: mpsc::Sender<layout_store::Update>,
    wake: mpsc::SyncSender<()>,
    launches: BTreeMap<String, launch::PendingLaunch>,
    checkout_removals: BTreeMap<String, worktrees::CheckoutRemoval>,
    defaults: settings::TerminalDefaults,
    presentation: presentation::Presentation,
    finished_in_flight: BTreeSet<String>,
    finished_due: Instant,
}

impl Backend {
    pub(crate) fn connect(view: ViewState, wake: mpsc::SyncSender<()>) -> io::Result<Self> {
        Self::from_client(Client::connect()?, view, wake)
    }

    pub(crate) fn from_client(
        client: Client,
        view: ViewState,
        wake: mpsc::SyncSender<()>,
    ) -> io::Result<Self> {
        let subscription = client.subscribe()?;
        let inventory = Inventory::new(&subscription.snapshot)?;
        let (job_sender, jobs) = mpsc::channel();
        let (layout_sender, layout_updates) = mpsc::channel();
        client.set_waker(wake.clone());
        let mut backend = Self {
            client,
            subscription,
            inventory,
            stores: BTreeMap::new(),
            document: Document::default(),
            view,
            profiles: Vec::new(),
            profiles_loaded: false,
            profiles_in_flight: false,
            profiles_due: Instant::now(),
            profiles_epoch: 0,
            machine_retries_in_flight: BTreeSet::new(),
            snapshot_in_flight: false,
            generation: 0,
            connected: true,
            layout_error: None,
            revision: 0,
            commands: Vec::new(),
            metadata: BTreeMap::new(),
            metadata_in_flight: BTreeSet::new(),
            metadata_folders: BTreeMap::new(),
            metadata_watch: BTreeMap::new(),
            metadata_due: Instant::now(),
            jobs,
            job_sender,
            layout_updates,
            layout_sender,
            wake,
            launches: BTreeMap::new(),
            checkout_removals: BTreeMap::new(),
            defaults: settings::TerminalDefaults::default(),
            presentation: presentation::Presentation::default(),
            finished_in_flight: BTreeSet::new(),
            finished_due: Instant::now(),
        };
        backend.ensure_store(None)?;
        backend.request_profiles();
        Ok(backend)
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn local_id(&self) -> &str {
        self.client
            .hello()
            .pointer("/core/id")
            .and_then(Value::as_str)
            .expect("validated core identity")
    }

    pub(crate) fn document(&self) -> &Document {
        &self.document
    }

    pub(crate) fn view(&self) -> &ViewState {
        &self.view
    }

    #[cfg(test)]
    pub(crate) fn view_mut(&mut self) -> &mut ViewState {
        self.remember_focus();
        self.revision += 1;
        &mut self.view
    }

    pub(crate) fn active_workspace_for(
        &self,
        owner: Option<&str>,
    ) -> Option<&herdr_core_tui::model::Workspace> {
        let key = owner.unwrap_or_else(|| self.local_id());
        self.view
            .workspace
            .as_deref()
            .and_then(|id| self.document.workspace(id))
            .filter(|workspace| workspace.machine.as_deref() == owner)
            .or_else(|| {
                self.view
                    .owner_workspaces
                    .get(key)
                    .and_then(|id| self.document.workspace(id))
                    .filter(|workspace| workspace.machine.as_deref() == owner)
            })
            .or_else(|| {
                self.document
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.machine.as_deref() == owner)
            })
    }

    pub(crate) fn activate_owner(&mut self, owner: Option<&str>) -> io::Result<()> {
        let owner = owner.filter(|id| *id != self.local_id()).map(str::to_owned);
        if !self.owner_present(owner.as_deref()) {
            return Err(io::Error::other("the core machine is no longer configured"));
        }
        self.relinquish_launch_focus();
        self.remember_focus();
        self.view.workspace = self
            .active_workspace_for(owner.as_deref())
            .map(|workspace| workspace.id.clone());
        self.view.active_owner = owner;
        self.revision += 1;
        Ok(())
    }

    fn remember_focus(&mut self) {
        if let Some(workspace) = self
            .view
            .workspace
            .as_deref()
            .and_then(|id| self.document.workspace(id))
        {
            let owner = workspace
                .machine
                .as_deref()
                .unwrap_or_else(|| self.local_id())
                .to_owned();
            self.view
                .owner_workspaces
                .insert(owner, workspace.id.clone());
        }
    }

    pub(crate) fn set_commands(
        &mut self,
        commands: Vec<crate::client::ui_model::ClientShellCommand>,
    ) {
        self.commands = commands;
        self.revision += 1;
    }

    pub(crate) fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    pub(crate) fn machine_views(&self) -> Vec<crate::client::ui_model::CoreMachineView> {
        use crate::client::endpoint::ClientEndpointStatus;
        use crate::client::ui_model::CoreMachineView;
        let mut machines = vec![CoreMachineView {
            machine_id: self.local_id().to_owned(),
            label: "Local".into(),
            local: true,
            status: if self.connected() {
                ClientEndpointStatus::Online
            } else {
                ClientEndpointStatus::Reconnecting
            },
            diagnostic: None,
        }];
        machines.extend(self.profiles.iter().filter_map(|profile| {
            let id = profile["id"].as_str()?;
            let name = profile["name"].as_str()?;
            let state = profile["state"].as_str()?;
            Some(CoreMachineView {
                machine_id: id.to_owned(),
                label: name.to_owned(),
                local: false,
                status: if !self.connected() {
                    ClientEndpointStatus::Reconnecting
                } else {
                    match state {
                        "connected" => ClientEndpointStatus::Online,
                        "connecting" | "retrying" => ClientEndpointStatus::Connecting,
                        _ => ClientEndpointStatus::Attention,
                    }
                },
                diagnostic: machines::diagnostic(profile),
            })
        }));
        machines
    }

    pub(crate) fn repository(
        &self,
        owner: Option<&str>,
        folder: &str,
    ) -> Option<&metadata::Repository> {
        self.metadata
            .get(&owner.map(str::to_owned))?
            .repositories
            .get(folder)
    }

    pub(crate) fn connected(&self) -> bool {
        self.connected && self.client.error().is_none()
    }

    pub(crate) fn owner_present(&self, owner: Option<&str>) -> bool {
        owner.is_none_or(|id| self.profiles.iter().any(|profile| profile["id"] == id))
    }

    pub(crate) fn layouts_ready(&self) -> bool {
        self.profiles_loaded
            && self.stores.get(&None).is_some_and(|store| store.loaded)
            && self.profiles.iter().all(|profile| {
                profile["id"].as_str().is_some_and(|id| {
                    self.stores
                        .get(&Some(id.to_owned()))
                        .is_some_and(|store| store.loaded)
                })
            })
    }

    pub(crate) fn pending(&self) -> bool {
        self.stores.values().any(|store| store.pending)
    }

    pub(crate) fn pending_mutation(&self) -> bool {
        !self.checkout_removals.is_empty()
    }

    pub(crate) fn saved_address(&self, pane_id: &str) -> Option<String> {
        for workspace in &self.document.workspaces {
            for tab in &workspace.tabs {
                if let Some(pane) = tab.pane(pane_id) {
                    return Some(terminal_address(
                        workspace.machine.as_deref(),
                        &pane.terminal,
                    ));
                }
            }
        }
        None
    }

    pub(crate) fn apply_operation(
        &mut self,
        owner: Option<String>,
        operation: Operation,
    ) -> io::Result<()> {
        if !self.owner_present(owner.as_deref()) {
            return Err(io::Error::other(
                "the layout's core machine is no longer configured",
            ));
        }
        self.ensure_store(owner.clone())?;
        self.stores
            .get_mut(&owner)
            .expect("ensured layout store")
            .apply(operation)?;
        self.revision += 1;
        self.rebuild()
    }

    pub(crate) fn observe_terminal(&mut self, record: Value) -> io::Result<bool> {
        self.inventory.insert_observed(record).map_err(Into::into)
    }

    pub(crate) fn request_refresh(&mut self) {
        self.profiles_due = Instant::now();
        for store in self.stores.values() {
            store.refresh();
        }
        self.request_snapshot();
    }

    pub(crate) fn discard_pending(&mut self) -> io::Result<()> {
        let mut discarded = false;
        for store in self.stores.values_mut() {
            discarded |= store.discard();
        }
        if discarded {
            self.revision += 1;
            self.rebuild()?;
        }
        Ok(())
    }

    pub(crate) fn disconnected(&mut self) {
        self.invalidate_pending_input();
        self.connected = false;
        self.generation += 1;
        self.profiles_in_flight = false;
        self.machine_retries_in_flight.clear();
        self.snapshot_in_flight = false;
        self.metadata_in_flight.clear();
        self.finished_in_flight.clear();
        self.finished_due = Instant::now();
        for store in self.stores.values() {
            store.disconnected();
        }
    }

    pub(crate) fn reconnect(
        &mut self,
        client: Client,
        subscription: Subscription,
    ) -> io::Result<()> {
        if self.client.hello().pointer("/core/id") != client.hello().pointer("/core/id") {
            return Err(io::Error::other(
                "the local core identity changed; saved views and input were not retargeted",
            ));
        }
        let mut inventory = Inventory::new(&subscription.snapshot)?;
        inventory.set_machines(&self.profiles)?;
        client.set_waker(self.wake.clone());
        self.client = client;
        self.subscription = subscription;
        self.inventory = inventory;
        self.connected = true;
        self.generation += 1;
        self.profiles_in_flight = false;
        self.machine_retries_in_flight.clear();
        self.snapshot_in_flight = false;
        self.metadata_in_flight.clear();
        self.finished_in_flight.clear();
        self.finished_due = Instant::now();
        self.metadata_due = Instant::now();
        self.profiles_due = Instant::now();
        for (owner, store) in &self.stores {
            if self.owner_present(owner.as_deref()) {
                store.connected(self.client.clone());
            }
        }
        Ok(())
    }

    pub(crate) fn poll(&mut self) -> io::Result<Change> {
        let mut change = Change::default();
        for update in self.layout_updates.try_iter().take(64) {
            if let Some(store) = self.stores.get_mut(&update.owner) {
                store.update(update);
                change.layout = true;
            }
        }
        if self.connected {
            for _ in 0..64 {
                let event = match self.subscription.try_event() {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(error) if error.is_transport() => {
                        self.disconnected();
                        change.connection = true;
                        change
                            .notices
                            .push(format!("Core connection lost: {error}"));
                        break;
                    }
                    Err(error) => return Err(error.into()),
                };
                let update = self.inventory.apply(&event)?;
                change.inventory |= update.terminals;
                change.restarted.extend(update.restarted);
                if update.refresh_machines {
                    self.profiles_epoch += 1;
                    self.profiles_due = Instant::now();
                }
                if update.resubscribe {
                    self.request_snapshot();
                }
                if let Some(document) = update.document {
                    if document.name == herdr_core_tui::persistence::DOCUMENT_NAME {
                        if let Some(store) = self.stores.get(&document.machine) {
                            store.refresh();
                        }
                    }
                }
            }
            if Instant::now() >= self.profiles_due {
                self.request_profiles();
            }
        }
        self.protect_restarted_launches(&change.restarted);
        for _ in 0..64 {
            let Ok(result) = self.jobs.try_recv() else {
                break;
            };
            self.apply_job(result, &mut change)?;
        }
        if change.layout || change.profiles {
            self.rebuild()?;
        }
        self.resume_checkout_removals(&mut change)?;
        if self.connected() {
            self.request_metadata(
                change.layout || change.inventory || change.profiles || change.metadata,
            );
            self.request_finished(change.layout || change.inventory);
        }
        change.presentation = self.presentation_tick();
        if change.connection
            || change.layout
            || change.inventory
            || change.profiles
            || change.metadata
            || change.presentation
            || change.mutation
        {
            self.revision += 1;
        }
        self.reconcile_launches(&mut change)?;
        Ok(change)
    }

    fn ensure_store(&mut self, owner: Option<String>) -> io::Result<()> {
        if !self.stores.contains_key(&owner) {
            let store = Store::start(
                self.client.clone(),
                owner.clone(),
                self.layout_sender.clone(),
                self.wake.clone(),
            )?;
            self.stores.insert(owner, store);
        }
        Ok(())
    }

    fn rebuild(&mut self) -> io::Result<()> {
        let mut document = Document::default();
        for (owner, store) in &self.stores {
            if self.owner_present(owner.as_deref()) {
                document
                    .workspaces
                    .extend(store.document.workspaces.iter().cloned());
            }
        }
        if let Err(error) = document.validate() {
            self.layout_error = Some(error.to_string());
            return Err(io::Error::other(error));
        }
        self.layout_error = None;
        self.document = document;
        if self.layouts_ready() {
            let mut retained = Document::default();
            for store in self.stores.values() {
                retained
                    .workspaces
                    .extend(store.document.workspaces.iter().cloned());
            }
            let selected = self
                .active_workspace_for(self.view.active_owner.as_deref())
                .map(|workspace| workspace.id.clone());
            self.view.repair(&retained);
            self.view.workspace = selected;
        }
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        for store in self.stores.values_mut() {
            store.stop();
        }
    }
}
