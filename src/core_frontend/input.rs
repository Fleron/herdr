use super::*;

use crate::client::core_backend::Action;
use crate::client::frame::{ClientMousePosition, ClientPaneInputEvent};
use crate::client::ui_action::{CoreAction, FrontendCommand, FrontendRequest, UiRequest};
use crate::client::ui_domain as domain;
use crate::client::ClientEndpointFocusTarget;
use crate::platform::PrefixInputSource;

impl App {
    pub(super) fn host_event(&mut self, event: &crate::raw_input::RawInputEvent) -> io::Result<()> {
        if let crate::raw_input::RawInputEvent::HostCellSizeReport {
            width_px,
            height_px,
        } = event
        {
            if *width_px > 0 && *height_px > 0 {
                self.display.cell = Some([*width_px, *height_px]);
                self.shell.set_graphics_cell_size(*width_px, *height_px);
                self.mouse_mode
                    .configure(self.config.ui.mouse_capture, true)?;
                self.streams.set_display(self.display.clone())?;
                self.layout_dirty = true;
                self.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) fn handle_input(&mut self, input: ClientShellInput) -> io::Result<()> {
        if input.detach {
            self.request_detach();
        }
        self.dirty |= input.repaint;
        self.hits_dirty |= input.repaint || input.resize;
        self.layout_dirty |= input.resize;
        if input.query_host_appearance {
            crate::client::host_queries::query_host_terminal_appearance();
        }
        if input.query_host_theme {
            crate::client::host_queries::query_host_terminal_theme();
        }
        for action in input.actions {
            self.action(action)?;
        }
        for request in input.requests {
            match request {
                UiRequest::PaneInput { pane_id, events } => {
                    for event in events {
                        if let Err(error) = self.pane_input(&pane_id, event) {
                            self.notice(error.to_string());
                        }
                    }
                }
                UiRequest::PopupInput { .. } => {
                    self.notice("No external tool popup is running");
                }
                UiRequest::Focus { focused } => {
                    if let Err(error) = self.streams.set_outer_focus(focused) {
                        self.notice(format!("Could not update terminal focus: {error}"));
                    }
                }
                UiRequest::HostTheme { update } => {
                    if let crate::client::frame::ClientHostThemeUpdate::DefaultColor {
                        kind,
                        color,
                    } = update
                    {
                        let color = Some(format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b));
                        match kind {
                            crate::client::frame::ClientHostDefaultColorKind::Foreground => {
                                self.display.fg = color
                            }
                            crate::client::frame::ClientHostDefaultColorKind::Background => {
                                self.display.bg = color
                            }
                        }
                        self.streams.set_display(self.display.clone())?;
                    }
                    self.dirty = true;
                }
            }
        }
        for active in self.shell.take_input_source_changes() {
            if active {
                self.prefix_input_source.switch_to_ascii();
            } else {
                self.prefix_input_source.restore();
            }
        }
        for endpoint in self.shell.take_machine_retries() {
            let result = match &endpoint {
                ClientEndpointId::Core {
                    machine_id,
                    local: false,
                } => self.backend.retry_machine(machine_id),
                _ => Err(io::Error::other(
                    "This machine is not a configured core remote",
                )),
            };
            if let Err(error) = result {
                self.dirty |= self
                    .shell
                    .finish_machine_retry(&endpoint, Some(error.to_string()));
            }
        }
        self.sync_host_keyboard()?;
        Ok(())
    }

    pub(super) fn mouse_input(&mut self, bytes: &[u8], pixels: bool) -> io::Result<()> {
        self.prepare_mouse_input()?;
        let input = if pixels {
            let Some([width, height]) = self.display.cell else {
                return Ok(());
            };
            self.shell.handle_pixel_mouse(
                bytes,
                crate::input::mouse::HostGeometry {
                    cols: self.size.0,
                    rows: self.size.1,
                    width_px: u32::from(self.size.0).saturating_mul(width),
                    height_px: u32::from(self.size.1).saturating_mul(height),
                },
            )
        } else {
            self.shell
                .handle_raw_events(crate::raw_input::parse_framed_input(bytes))
        };
        self.handle_input(input)
    }

    pub(super) fn sync_host_keyboard(&mut self) -> io::Result<()> {
        let pane_reports_all = self
            .geometry
            .specs
            .iter()
            .find(|spec| spec.focused)
            .and_then(|spec| self.streams.pane(&spec.id))
            .map(|pane| pane.view.reports_all_keys())
            .transpose()?
            .unwrap_or(false);
        let desired = self.keyboard_enhanced
            && (pane_reports_all || self.shell.host_keyboard_report_all_requested());
        if desired != self.keyboard_report_all {
            crate::input::host::set_host_kitty_keyboard_report_all(&mut io::stdout(), desired)?;
            self.shell.set_host_reports_all_keys(desired);
            self.keyboard_report_all = desired;
        }
        Ok(())
    }

    pub(super) fn action(&mut self, action: ClientShellAction) -> io::Result<()> {
        match action {
            ClientShellAction::Endpoint {
                endpoint_id,
                boot_id,
                request,
            } => {
                let owner = owner(&endpoint_id)?;
                self.dispatch(owner, boot_id, *request)?;
            }
            ClientShellAction::AttachTerminal { target } => {
                let owner = (target.machine_id != self.backend.local_id())
                    .then(|| target.machine_id.clone());
                self.activate(owner.as_deref())?;
                match self.backend.attach_runtime(&target) {
                    Ok(action) => {
                        self.accept_action(action)?;
                    }
                    Err(error) => self.notice(format!("Could not attach terminal: {error}")),
                }
            }
            ClientShellAction::ClipboardWrite(bytes) => match host::copy(&bytes) {
                Ok(()) => {
                    self.dirty |= self.shell.show_copy_feedback(Instant::now());
                }
                Err(error) => self.notice(format!("Clipboard unavailable: {error}")),
            },
            ClientShellAction::OpenSafeWebUrl(url) => {
                if crate::client::terminal_text::safe_web_url(&url).is_some() {
                    match crate::platform::open_url(&url) {
                        Ok(Some(child)) => self.detached_children.push(child),
                        Ok(None) => {}
                        Err(error) => self.notice(format!("Could not open link: {error}")),
                    }
                }
            }
            ClientShellAction::ActivateEndpoint {
                endpoint_id,
                target,
            } => {
                let owner = owner(&endpoint_id)?;
                self.activate(owner.as_deref())?;
                let command = match target {
                    Some(ClientEndpointFocusTarget::Workspace(workspace_id)) => {
                        Some(FrontendCommand::WorkspaceFocus(domain::WorkspaceTarget {
                            workspace_id,
                        }))
                    }
                    Some(ClientEndpointFocusTarget::Tab(tab_id)) => {
                        Some(FrontendCommand::TabFocus(domain::TabTarget { tab_id }))
                    }
                    Some(ClientEndpointFocusTarget::Pane(pane_id)) => {
                        Some(FrontendCommand::PaneFocus(domain::PaneTarget { pane_id }))
                    }
                    Some(ClientEndpointFocusTarget::Agent(target)) => {
                        return self.action(ClientShellAction::AttachTerminal { target });
                    }
                    None => None,
                };
                if let Some(method) = command {
                    let request = FrontendRequest {
                        id: herdr_core_tui::model::new_id(),
                        method,
                    };
                    let boot_id = self.backend.snapshot(owner.as_deref()).boot_id;
                    self.dispatch(owner, boot_id, request)?;
                }
            }
            ClientShellAction::ReplayMouse(events) => {
                let input = self.shell.replay_mouse_events(events);
                self.handle_input(input)?;
            }
            ClientShellAction::Keybind(action) => {
                if action == crate::input::KeybindAction::Detach {
                    self.request_detach();
                } else {
                    self.notice("Create or attach a workspace before using this action");
                }
            }
            ClientShellAction::Core(action) => self.core_action(action)?,
        }
        Ok(())
    }

    fn activate(&mut self, owner: Option<&str>) -> io::Result<()> {
        self.backend.activate_owner(owner)?;
        self.pending_focus = None;
        self.view_dirty = true;
        self.layout_dirty = true;
        self.sync_shell();
        Ok(())
    }

    fn dispatch(
        &mut self,
        owner: Option<String>,
        boot_id: String,
        request: FrontendRequest,
    ) -> io::Result<()> {
        if self.ownership_error.is_some() {
            self.notice("The core identity changed. Reopen the TUI to choose its terminals.");
            return Ok(());
        }
        if self.backend.snapshot(owner.as_deref()).boot_id != boot_id {
            self.notice("The selected machine changed. Select the action again.");
            return Ok(());
        }
        let id = request.id.clone();
        let explicit_focus = matches!(
            request.method,
            FrontendCommand::WorkspaceFocus(_)
                | FrontendCommand::TabFocus(_)
                | FrontendCommand::PaneFocus(_)
                | FrontendCommand::PaneZoom(_)
        );
        if explicit_focus {
            self.pending_focus = None;
        }
        self.requests.insert(
            id.clone(),
            RequestContext {
                boot_id,
                owner: owner.clone(),
            },
        );
        match self.backend.command(owner.as_deref(), request) {
            Ok(Action::Complete(result)) => {
                self.layout_dirty = true;
                self.view_dirty = true;
                self.sync_shell();
                self.complete_request(id, Ok(*result))?;
            }
            Ok(Action::Runtime(FrontendCommand::CommandInvoke(params))) => {
                match self.invoke_custom_command(owner.as_deref(), &params, id.clone()) {
                    Ok(action) => self.accept_action(action)?,
                    Err(error) => self.complete_request(id, Err(endpoint_error(error)))?,
                }
            }
            Ok(Action::Runtime(command)) => match self.runtime_command(owner.as_deref(), command) {
                Ok(Some(result)) => self.complete_request(id, Ok(result))?,
                Ok(None) => {}
                Err(error) => self.complete_request(id, Err(endpoint_error(error)))?,
            },
            Ok(Action::ReloadConfig) => {
                self.reload_config();
                self.complete_request(id, Ok(domain::ResponseResult::Ok {}))?;
            }
            Ok(action) => self.accept_action(action)?,
            Err(error) => self.complete_request(id, Err(endpoint_error(error)))?,
        }
        self.dirty = true;
        Ok(())
    }

    fn accept_action(&mut self, action: Action) -> io::Result<()> {
        match action {
            Action::Launch(token) => {
                if token.focus {
                    self.pending_focus = Some(token.pane.clone());
                    self.held_input.entry(token.pane).or_default();
                }
            }
            Action::Complete(_) => {
                self.sync_shell();
            }
            Action::Pending => {}
            Action::ReloadConfig => self.reload_config(),
            Action::Runtime(_) => {
                return Err(io::Error::other(
                    "terminal action requires its captured request context",
                ));
            }
        }
        self.layout_dirty = true;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn pane_input(
        &mut self,
        pane_id: &str,
        event: ClientPaneInputEvent,
    ) -> io::Result<()> {
        let id = self.pending_focus.as_deref().unwrap_or(pane_id).to_owned();
        if id != pane_id && matches!(event, ClientPaneInputEvent::Mouse { .. }) {
            return Ok(());
        }
        self.targeted_pane_input(&id, event)
    }

    pub(super) fn targeted_pane_input(
        &mut self,
        pane_id: &str,
        event: ClientPaneInputEvent,
    ) -> io::Result<()> {
        let id = pane_id.to_owned();
        let address = self.backend.saved_address(&id);
        if address.is_none() && self.pending_focus.as_deref() != Some(id.as_str()) {
            return Err(io::Error::other("The intended terminal view has closed"));
        }
        if self.blocked_held_input.contains(&id) || self.streams.pane(&id).is_none() {
            self.held_input.entry(id).or_default().push_back(event);
            self.layout_dirty = true;
            return Ok(());
        }
        if self
            .streams
            .pane(&id)
            .map(|pane| pane.spec.address.as_str())
            != address.as_deref()
        {
            return Err(io::Error::other(
                "The terminal view changed. Held input was not sent to another program.",
            ));
        }
        if self.clipboard_pane_input(&id, &event)? {
            return Ok(());
        }
        let input = match event {
            ClientPaneInputEvent::Mouse {
                kind,
                position,
                modifiers,
                lines,
                geometry,
            } => {
                if let (Some(geometry), Some(pane)) = (geometry, self.streams.pane(&id)) {
                    if (geometry.cols, geometry.rows) != pane.view.size()
                        || (geometry.width_px, geometry.height_px) != pane.view.pixel_size()
                    {
                        return Err(io::Error::other(
                            "The pane resized. Try the mouse action again.",
                        ));
                    }
                }
                let position = match position {
                    ClientMousePosition::Cell { column, row } => {
                        crate::input::mouse::Position::Cell { column, row }
                    }
                    ClientMousePosition::Pixels { x, y, .. } => {
                        crate::input::mouse::Position::Pixels { x, y }
                    }
                };
                TerminalInput::Mouse {
                    kind: kind.to_crossterm(),
                    modifiers: crossterm::event::KeyModifiers::from_bits_truncate(modifiers),
                    position,
                    lines,
                }
            }
            event => match event.to_raw_input_event() {
                crate::raw_input::RawInputEvent::Key(key) => TerminalInput::Key(key),
                crate::raw_input::RawInputEvent::Text(text) => {
                    TerminalInput::Bytes(text.into_string().into_bytes())
                }
                crate::raw_input::RawInputEvent::Paste(text) => TerminalInput::Paste(text),
                _ => return Err(io::Error::other("Unsupported terminal input event")),
            },
        };
        self.streams.input(&id, input)?;
        Ok(())
    }

    fn core_action(&mut self, action: CoreAction) -> io::Result<()> {
        match action {
            CoreAction::Retry => {
                self.backend.retry_pending_launches();
                self.backend.request_refresh();
                self.streams.retry();
                for machine in self.backend.machine_views() {
                    if self.backend.connected()
                        && !machine.local
                        && machine.status != crate::client::endpoint::ClientEndpointStatus::Online
                    {
                        if let Err(error) = self.backend.retry_machine(&machine.machine_id) {
                            self.notice(format!("Could not retry {}: {error}", machine.label));
                        }
                    }
                }
                if !self.backend.connected() && self.ownership_error.is_none() {
                    self.reconnect_due = Some(Instant::now());
                }
            }
            CoreAction::DiscardHeldInput => {
                self.held_input.clear();
                self.blocked_held_input.clear();
                self.streams.discard_all_input();
                self.backend.discard_pending_launches();
                self.clipboard_images.discard();
                self.pending_focus = None;
            }
            CoreAction::DiscardPendingLayouts => {
                if let Err(error) = self.backend.discard_pending() {
                    self.notice(format!("Could not discard layout edits: {error}"));
                }
            }
            CoreAction::StopTerminal(target) => {
                if let Err(error) = self.backend.stop_runtime(&target) {
                    self.notice(format!("Could not stop terminal: {error}"));
                }
            }
            CoreAction::OpenCheckout(target) => {
                let owner = (target.machine_id != self.backend.local_id())
                    .then(|| target.machine_id.clone());
                self.activate(owner.as_deref())?;
                match self.backend.open_checkout(&target) {
                    Ok(action) => self.accept_action(action)?,
                    Err(error) => self.notice(format!("Could not open checkout: {error}")),
                }
            }
            CoreAction::BeginCheckoutWorktree { target, create } => {
                let owner = (target.machine_id != self.backend.local_id())
                    .then(|| target.machine_id.clone());
                self.activate(owner.as_deref())?;
                let input = self.shell.begin_checkout_worktree(target, create);
                self.handle_input(input)?;
            }
            CoreAction::RemoveCheckout(target) => {
                match self.backend.remove_checkout(&target, None) {
                    Ok(action) => self.accept_action(action)?,
                    Err(error) => self.notice(format!("Could not remove checkout: {error}")),
                }
            }
        }
        self.layout_dirty = true;
        self.sync_shell();
        Ok(())
    }

    fn reload_config(&mut self) {
        let loaded = Config::load();
        if self.config.ui.mouse_capture != loaded.config.ui.mouse_capture {
            if let Err(error) = self
                .mouse_mode
                .configure(loaded.config.ui.mouse_capture, self.display.cell.is_some())
            {
                self.notice(format!("Could not update mouse capture: {error}"));
            }
        }
        self.config = loaded.config;
        self.shell.reload_client_config();
        self.output.configure(&self.config);
        self.command_namespace = herdr_core_tui::model::new_id();
        self.configure_commands();
        if !loaded.diagnostics.is_empty() {
            self.notice(loaded.diagnostics.join("\n"));
        }
        self.layout_dirty = true;
        self.sync_shell();
    }
}

fn endpoint_error(error: io::Error) -> ClientShellEndpointError {
    let code = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<super::runtime::Error>())
        .map(|error| error.code.to_owned());
    ClientShellEndpointError {
        code,
        message: error.to_string(),
    }
}

fn owner(endpoint: &ClientEndpointId) -> io::Result<Option<String>> {
    match endpoint {
        ClientEndpointId::Core { local: true, .. } => Ok(None),
        ClientEndpointId::Core {
            machine_id,
            local: false,
        } => Ok(Some(machine_id.clone())),
        _ => Err(io::Error::other("This machine uses the old Herdr runtime")),
    }
}
