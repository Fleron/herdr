use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use herdr_core_tui::connection::{Client, Display, Subscription};
use herdr_core_tui::model::ViewState;
use herdr_core_tui::surfaces::{Input as TerminalInput, Streams};

use crate::client::core_backend::{Backend, LaunchOutcome};
use crate::client::endpoint::ClientEndpointId;
use crate::client::shell::ClientShellEndpointError;
use crate::client::{ClientShellAction, ClientShellConfig, ClientShellInput, ClientShellState};
use crate::config::Config;

#[cfg(test)]
mod benchmark;
mod clipboard_images;
mod commands;
#[path = "platform/core_context.rs"]
mod context;
#[path = "core_client/host.rs"]
mod host;
mod input;
mod notifications;
mod output;
mod runtime;
mod surface;

const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
const INPUT_BATCH: usize = 64;

struct RequestContext {
    boot_id: String,
    owner: Option<String>,
}

struct Reconnect {
    receiver: mpsc::Receiver<io::Result<(Client, Subscription)>>,
}

struct App {
    backend: Backend,
    shell: ClientShellState,
    streams: Streams,
    config: Config,
    output: output::Output,
    surface: surface::SurfaceBuilder,
    geometry: surface::Geometry,
    installed_owner: Option<ClientEndpointId>,
    requests: HashMap<String, RequestContext>,
    held_input: HashMap<String, VecDeque<crate::client::frame::ClientPaneInputEvent>>,
    blocked_held_input: HashSet<String>,
    pending_focus: Option<String>,
    retained: HashSet<String>,
    wake: mpsc::SyncSender<()>,
    reconnect: Option<Reconnect>,
    reconnect_due: Option<Instant>,
    reconnect_backoff: Duration,
    ownership_error: Option<String>,
    display: Display,
    size: (u16, u16),
    running: bool,
    dirty: bool,
    layout_dirty: bool,
    surface_dirty: bool,
    hits_dirty: bool,
    view_dirty: bool,
    command_namespace: String,
    detached_children: Vec<std::process::Child>,
    notifications: notifications::NotificationTracker,
    prefix_input_source: crate::platform::RealPrefixInputSource,
    mouse_mode: host::MouseMode,
    keyboard_enhanced: bool,
    keyboard_report_all: bool,
    clipboard_images: clipboard_images::Bridge,
}

pub(crate) fn run() -> io::Result<()> {
    let loaded = Config::load();
    let view = load_view()?;
    let (wake, notifications) = mpsc::sync_channel(1);
    let backend = Backend::connect(view, wake.clone())?;
    let display = Display::default();
    let caller = context::caller(backend.client());
    let streams = Streams::new(
        backend.client().clone(),
        display.clone(),
        caller,
        loaded.config.advanced.scrollback_limit_bytes,
        wake.clone(),
    );
    let shell_config = ClientShellConfig::from_config(&loaded.config)
        .with_core_profile(backend.local_id())
        .with_startup_onboarding(loaded.config.should_show_onboarding())
        .with_startup_config_diagnostic(
            (!loaded.diagnostics.is_empty()).then(|| loaded.diagnostics.join("\n")),
        );
    let mut shell = ClientShellState::new(shell_config);
    let (input_sender, inputs) = mpsc::channel();
    let _host = host::Host::enter(loaded.config.ui.mouse_capture, input_sender, wake.clone())?;
    shell.set_host_reports_key_releases(_host.probe().escape_disambiguation);
    shell.set_host_erase_byte(crate::platform::terminal_erase_byte());
    let size = crossterm::terminal::size()?;
    let output = output::Output::new(&loaded.config);
    let clipboard_images = clipboard_images::Bridge::new(wake.clone())?;
    let mut app = App {
        backend,
        shell,
        streams,
        config: loaded.config,
        output,
        surface: surface::SurfaceBuilder::default(),
        geometry: surface::Geometry::default(),
        installed_owner: None,
        requests: HashMap::new(),
        held_input: HashMap::new(),
        blocked_held_input: HashSet::new(),
        pending_focus: None,
        retained: HashSet::new(),
        wake,
        reconnect: None,
        reconnect_due: None,
        reconnect_backoff: Duration::from_millis(200),
        ownership_error: None,
        display,
        size,
        running: true,
        dirty: true,
        layout_dirty: true,
        surface_dirty: true,
        hits_dirty: true,
        view_dirty: false,
        command_namespace: "0".into(),
        detached_children: Vec::new(),
        notifications: notifications::NotificationTracker::default(),
        prefix_input_source: crate::platform::RealPrefixInputSource::default(),
        mouse_mode: _host.mouse_mode(),
        keyboard_enhanced: _host.probe().escape_disambiguation,
        keyboard_report_all: false,
        clipboard_images,
    };
    app.configure_commands();
    app.sync_shell();
    crate::client::host_queries::query_host_terminal_appearance();
    crate::client::host_queries::query_host_terminal_theme();
    crate::client::host_queries::query_host_cell_size();
    let mut paint_due = Instant::now();
    let result = (|| -> io::Result<()> {
        while app.running {
            crate::platform::pump_input_source_runloop();
            app.poll_backend()?;
            for _ in 0..INPUT_BATCH {
                let input = match inputs.try_recv() {
                    Ok(input) => input,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        app.running = false;
                        break;
                    }
                };
                match input {
                    host::Input::Events(events) => {
                        for event in events {
                            if app.layout_dirty || app.surface_dirty {
                                app.refresh_surface()?;
                            }
                            app.host_event(&event)?;
                            if matches!(event, crate::raw_input::RawInputEvent::Mouse(_)) {
                                app.prepare_mouse_input()?;
                            }
                            let outcome = app.shell.handle_raw_events(vec![event]);
                            app.handle_input(outcome)?;
                        }
                    }
                    host::Input::Resize => {
                        app.size = crossterm::terminal::size()?;
                        app.layout_dirty = true;
                        app.hits_dirty = true;
                        app.output.invalidate();
                        app.dirty = true;
                    }
                    host::Input::Mouse { bytes, pixels } => {
                        app.mouse_input(&bytes, pixels)?;
                    }
                    host::Input::Exit => app.running = false,
                    host::Input::Error(error) => return Err(error),
                }
            }
            if app.layout_dirty {
                app.reconcile()?;
            }
            let update = app.streams.poll()?;
            app.dirty |= update.changed;
            app.surface_dirty |= update.changed;
            app.sync_host_keyboard()?;
            if surface::SurfaceBuilder::prepare_visible(&app.geometry, &mut app.streams)? {
                app.layout_dirty = true;
            }
            if app.layout_dirty {
                app.reconcile()?;
            }
            for bytes in update.clipboard {
                if let Err(error) = host::copy(&bytes) {
                    app.notice(format!("Clipboard unavailable: {error}"));
                }
            }
            for message in update.notices {
                app.notice(message);
            }
            for count in update.bells {
                crate::terminal_effects::write_terminal_bells(&mut io::stdout(), count)?;
            }
            app.flush_held_input()?;
            app.poll_clipboard_images()?;
            app.detached_children
                .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
            crate::render_prof::flush_if_due();
            let now = Instant::now();
            app.shell.tick_popup_pending(now);
            app.dirty |= app.shell.tick_endpoint_error(now);
            app.dirty |= app.shell.tick_copy_feedback(now);
            app.dirty |= app.shell.tick_workspace_highlight(now);
            let input = app.shell.tick_selection_autoscroll(now);
            app.handle_input(input)?;
            let (effects, repaint) = app.shell.tick_notifications(now);
            app.dirty |= repaint;
            notifications::emit(effects, &app.config.ui.sound);
            if app.dirty && now >= paint_due {
                app.present()?;
                app.dirty = false;
                paint_due = now + FRAME_INTERVAL;
            }
            if app.view_dirty {
                save_view(app.backend.view())?;
                app.view_dirty = false;
            }
            if !app.running {
                break;
            }
            let mut delay = app
                .shell
                .timer_delay(Instant::now())
                .min(Duration::from_millis(250));
            if app.dirty {
                delay = delay.min(paint_due.saturating_duration_since(Instant::now()));
            }
            if let Some(due) = app.reconnect_due {
                delay = delay.min(due.saturating_duration_since(Instant::now()));
            }
            let _ = notifications.recv_timeout(delay);
        }
        Ok(())
    })();
    let cleanup = app.finish_clipboard_images();
    let saved = save_view(app.backend.view());
    let mut failures = [result, cleanup, saved].into_iter().filter_map(Result::err);
    let Some(first) = failures.next() else {
        return Ok(());
    };
    let remaining = failures.map(|error| error.to_string()).collect::<Vec<_>>();
    if remaining.is_empty() {
        Err(first)
    } else {
        Err(io::Error::new(
            first.kind(),
            format!("{first}; {}", remaining.join("; ")),
        ))
    }
}

impl App {
    fn configure_commands(&mut self) {
        self.backend.set_terminal_defaults(&self.config);
        self.backend.set_presentation_config(&self.config);
        let bindings = self.config.keybinds();
        self.backend.set_commands(Backend::command_manifest(
            &bindings.custom_commands,
            &self.command_namespace,
        ));
    }

    fn notice(&mut self, message: impl Into<String>) {
        self.shell.receive_runtime_error(message.into());
        self.dirty = true;
        self.hits_dirty = true;
    }

    fn request_detach(&mut self) {
        if self.backend.pending_mutation() {
            self.notice(
                "Checkout deletion is still running. Wait for it to finish before detaching.",
            );
            return;
        }
        let held = self.held_input.values().map(VecDeque::len).sum::<usize>()
            + self.streams.pending_input_count()
            + self.clipboard_images.pending_count();
        if self.backend.pending() || self.backend.pending_launch().is_some() || held > 0 {
            self.sync_shell();
            self.notice("Changes or typing are still pending. Open the menu to retry or discard them, then detach.");
            return;
        }
        self.running = false;
    }

    fn endpoint(&self, owner: Option<&str>) -> ClientEndpointId {
        ClientEndpointId::Core {
            machine_id: owner.unwrap_or_else(|| self.backend.local_id()).into(),
            local: owner.is_none(),
        }
    }

    fn sync_shell(&mut self) {
        let machines = self.backend.machine_views();
        self.shell.set_core_catalog(&machines);
        for machine in machines {
            let owner = (!machine.local).then_some(machine.machine_id.as_str());
            let endpoint = self.endpoint(owner);
            let mut snapshot = self.backend.snapshot(owner);
            let notifications = self.notifications.observe(
                &endpoint,
                &machine.label,
                &snapshot,
                machine.status == crate::client::endpoint::ClientEndpointStatus::Online,
            );
            snapshot.runtime_controls = Some(crate::client::ui_model::RuntimeControls {
                held_input: self.held_input.values().map(VecDeque::len).sum::<usize>()
                    + self.streams.pending_input_count()
                    + self.clipboard_images.pending_count(),
                pending_launch: self.backend.pending_launch().is_some(),
                pending_layout: self.backend.pending(),
            });
            self.shell.set_endpoint_snapshot_for_generation(
                &endpoint,
                self.backend.generation(),
                Box::new(snapshot),
            );
            for notification in notifications {
                let (effects, repaint) = self.shell.receive_runtime_notification(
                    &endpoint,
                    notification.event,
                    notification.target,
                    Instant::now(),
                );
                self.dirty |= repaint;
                notifications::emit(effects, &self.config.ui.sound);
            }
        }
        let endpoint = self.endpoint(self.backend.view().active_owner.as_deref());
        if self.installed_owner.as_ref() != Some(&endpoint)
            && self.shell.activate_endpoint_projection(&endpoint)
        {
            self.installed_owner = Some(endpoint);
        }
        self.dirty = true;
        self.hits_dirty = true;
    }

    fn poll_backend(&mut self) -> io::Result<()> {
        if self.backend.client().error().is_some() && self.backend.connected() {
            self.backend.disconnected();
            self.requests.clear();
            self.block_held_input();
            self.reconnect_due = Some(Instant::now() + self.reconnect_backoff);
            self.notice("Core connection lost");
            self.sync_shell();
        }
        self.poll_reconnect()?;
        let change = self.backend.poll()?;
        let retried_machine = !change.machine_retries.is_empty();
        if change.connection {
            self.requests.clear();
            self.block_held_input();
            if !self.backend.connected()
                && self.reconnect.is_none()
                && self.reconnect_due.is_none()
                && self.ownership_error.is_none()
            {
                self.reconnect_due = Some(Instant::now() + self.reconnect_backoff);
            }
        }
        for address in change.restarted {
            let mut held = self.streams.recovered(&address);
            for (pane, events) in &self.held_input {
                if !events.is_empty()
                    && self.backend.saved_address(pane).as_deref() == Some(&address)
                {
                    self.blocked_held_input.insert(pane.clone());
                    held += events.len();
                }
            }
            if held > 0 {
                self.notice(format!("The program was replaced. {held} held input events were not sent to the replacement."));
            }
        }
        for notice in change.notices {
            self.notice(notice);
        }
        for launch in change.launches {
            match launch {
                LaunchOutcome::Reserved { token } => {
                    if token.focus {
                        self.pending_focus = Some(token.pane);
                    }
                }
                LaunchOutcome::Placed {
                    token,
                    input_may_retarget,
                } => {
                    if input_may_retarget {
                        let held = self.held_input.get(&token.pane).map_or(0, VecDeque::len);
                        if held > 0 {
                            self.blocked_held_input.insert(token.pane.clone());
                            self.notice(format!("Terminal input ownership could not be confirmed. {held} held input events were not sent; discard held input to continue."));
                        }
                    }
                    if self.pending_focus.as_deref() == Some(token.pane.as_str()) {
                        self.pending_focus = None;
                    }
                    self.layout_dirty = true;
                    self.view_dirty = true;
                }
                LaunchOutcome::Failed {
                    token,
                    error,
                    uncertain,
                } => {
                    let held = self.held_input.get(&token.pane).map_or(0, VecDeque::len);
                    self.notice(if held > 0 {
                        format!("{error}. {held} input events are held for the intended terminal.")
                    } else {
                        error
                    });
                    if !uncertain
                        && held == 0
                        && self.pending_focus.as_deref() == Some(token.pane.as_str())
                    {
                        self.pending_focus = None;
                    }
                }
            }
        }
        for completion in change.completions {
            self.complete_request(
                completion.id,
                completion
                    .result
                    .map_err(|message| ClientShellEndpointError {
                        code: None,
                        message,
                    }),
            )?;
        }
        if change.inventory
            || change.layout
            || change.profiles
            || change.metadata
            || change.presentation
            || change.connection
            || retried_machine
            || change.mutation
        {
            self.layout_dirty = true;
            self.sync_shell();
        }
        for retry in change.machine_retries {
            let endpoint = self.endpoint(Some(&retry.machine_id));
            self.dirty |= self.shell.finish_machine_retry(&endpoint, retry.error);
            self.hits_dirty = true;
        }
        Ok(())
    }

    fn poll_reconnect(&mut self) -> io::Result<()> {
        if let Some(job) = &self.reconnect {
            if let Ok(result) = job.receiver.try_recv() {
                self.reconnect = None;
                match result {
                    Ok((client, subscription)) => {
                        if let Err(error) = self.backend.reconnect(client.clone(), subscription) {
                            self.ownership_error = Some(error.to_string());
                            self.reconnect_due = None;
                            self.notice(error.to_string());
                        } else {
                            let held = self.streams.reconnect(client);
                            if held > 0 {
                                self.notice(format!("The connection was interrupted. {held} held input events were not replayed; discard held input to continue."));
                            }
                            self.reconnect_backoff = Duration::from_millis(200);
                            self.reconnect_due = None;
                            self.layout_dirty = true;
                            self.sync_shell();
                        }
                    }
                    Err(error) => {
                        self.reconnect_backoff =
                            (self.reconnect_backoff * 2).min(Duration::from_secs(3));
                        self.reconnect_due = Some(Instant::now() + self.reconnect_backoff);
                        self.notice(format!("Could not reconnect to core: {error}"));
                    }
                }
            }
        }
        if self.ownership_error.is_none()
            && self.reconnect.is_none()
            && self.reconnect_due.is_some_and(|due| due <= Instant::now())
        {
            let (sender, receiver) = mpsc::channel();
            let wake = self.wake.clone();
            thread::spawn(move || {
                let result = (|| -> io::Result<_> {
                    let client = Client::connect()?;
                    let subscription = client.subscribe()?;
                    Ok((client, subscription))
                })();
                let _ = sender.send(result);
                let _ = wake.try_send(());
            });
            self.reconnect = Some(Reconnect { receiver });
        }
        Ok(())
    }

    fn reconcile(&mut self) -> io::Result<()> {
        let size = self.shell.surface_size(self.size.0, self.size.1);
        self.geometry = surface::SurfaceBuilder::geometry(
            self.backend.document(),
            self.backend.view(),
            self.backend.inventory(),
            &self.streams,
            (size.cols, size.rows),
            &self.config,
        )?;
        self.retained = self
            .backend
            .document()
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .map(|pane| pane.id.clone())
            .collect();
        self.streams
            .reconcile(&self.geometry.specs, &self.retained)?;
        self.layout_dirty = false;
        self.surface_dirty = true;
        self.dirty = true;
        Ok(())
    }

    fn present(&mut self) -> io::Result<()> {
        if self.layout_dirty || self.surface_dirty {
            self.refresh_surface()?;
        }
        let cleanup = self.shell.take_pending_graphics_cleanup();
        self.output.graphics_cleanup(&cleanup)?;
        if let Some(frame) = self.shell.compose(self.size.0, self.size.1) {
            let title = self
                .backend
                .window_title(self.backend.view().active_owner.as_deref());
            self.output.present(frame, title.as_deref())?;
        }
        self.hits_dirty = false;
        Ok(())
    }

    fn prepare_mouse_input(&mut self) -> io::Result<()> {
        if self.layout_dirty || self.surface_dirty {
            self.refresh_surface()?;
        }
        if self.hits_dirty {
            self.shell.refresh_hit_map(self.size.0, self.size.1);
            self.hits_dirty = false;
        }
        Ok(())
    }

    fn refresh_surface(&mut self) -> io::Result<()> {
        if self.layout_dirty {
            self.reconcile()?;
        }
        if surface::SurfaceBuilder::prepare_visible(&self.geometry, &mut self.streams)? {
            self.reconcile()?;
        }
        let snapshot = self
            .backend
            .snapshot(self.backend.view().active_owner.as_deref());
        let cell = self.display.cell.unwrap_or([0, 0]);
        self.surface
            .set_delivery_epoch(self.shell.graphics_delivery_epoch());
        let frame = self.surface.build(
            &snapshot,
            &self.geometry,
            &mut self.streams,
            &self.config,
            self.shell.palette(),
            crate::kitty_graphics::HostCellSize {
                width_px: cell[0],
                height_px: cell[1],
            },
        )?;
        self.shell.set_pane_surface(frame);
        self.surface_dirty = false;
        Ok(())
    }

    fn flush_held_input(&mut self) -> io::Result<()> {
        let ready: Vec<_> = self
            .held_input
            .keys()
            .filter(|id| !self.blocked_held_input.contains(*id) && self.streams.pane(id).is_some())
            .cloned()
            .collect();
        for id in ready {
            if let Some(mut events) = self.held_input.remove(&id) {
                while let Some(event) = events.pop_front() {
                    if let Err(error) = self.targeted_pane_input(&id, event.clone()) {
                        events.push_front(event);
                        self.blocked_held_input.insert(id.clone());
                        self.held_input
                            .entry(id.clone())
                            .or_default()
                            .extend(events);
                        self.notice(format!("Could not deliver held input: {error}"));
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn block_held_input(&mut self) {
        self.blocked_held_input.extend(
            self.held_input
                .iter()
                .filter(|(_, events)| !events.is_empty())
                .map(|(id, _)| id.clone()),
        );
    }

    fn complete_request(
        &mut self,
        id: String,
        result: Result<crate::client::ui_domain::ResponseResult, ClientShellEndpointError>,
    ) -> io::Result<()> {
        let Some(context) = self.requests.remove(&id) else {
            return Ok(());
        };
        let current = self.backend.snapshot(context.owner.as_deref());
        if current.boot_id != context.boot_id {
            return Ok(());
        }
        let (repaint, actions) = self
            .shell
            .handle_endpoint_result(&context.boot_id, &id, result);
        self.dirty |= repaint;
        for action in actions {
            self.action(action)?;
        }
        Ok(())
    }
}

fn view_path() -> std::path::PathBuf {
    crate::config::state_dir().join("core-tui-view.json")
}

fn load_view() -> io::Result<ViewState> {
    match fs::read(view_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ViewState::default()),
        Err(error) => Err(error),
    }
}

fn save_view(view: &ViewState) -> io::Result<()> {
    let path = view_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_vec(view).map_err(io::Error::other)?,
    )?;
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests;
