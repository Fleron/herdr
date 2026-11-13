use super::*;
use crate::client::ui_action::CoreAction;
use crate::client::ui_model::RuntimeTerminalTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientGlobalMenuAction {
    Binding(crate::input::KeybindAction),
    WhatsNew,
    AttachTerminal,
    Retry,
    DiscardHeldInput,
    DiscardPendingLayouts,
    StopTerminal,
}

pub(super) fn focused_runtime_target(
    snapshot: &ClientShellSnapshot,
) -> Option<RuntimeTerminalTarget> {
    let id = snapshot.focused_pane_id.as_deref()?;
    snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == id)?
        .runtime_target
        .clone()
}

pub(super) fn global_menu_attention(snapshot: &ClientShellSnapshot) -> bool {
    snapshot.update_available.is_some() || snapshot.integration_updates_available
}

pub(super) fn global_menu_item_has_badge(
    snapshot: &ClientShellSnapshot,
    action: ClientGlobalMenuAction,
) -> bool {
    (action == ClientGlobalMenuAction::WhatsNew && snapshot.update_available.is_some())
        || (action == ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Settings)
            && snapshot.integration_updates_available)
}

pub(super) fn global_menu_items(
    snapshot: &ClientShellSnapshot,
    stop: bool,
) -> Vec<(&'static str, ClientGlobalMenuAction)> {
    let mut items = vec![
        (
            "settings",
            ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Settings),
        ),
        (
            "keybinds",
            ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Help),
        ),
        (
            "reload config",
            ClientGlobalMenuAction::Binding(crate::input::KeybindAction::ReloadConfig),
        ),
    ];
    if snapshot.core_terminals.is_some() {
        items.push(("attach terminal", ClientGlobalMenuAction::AttachTerminal));
    }
    if let Some(controls) = &snapshot.runtime_controls {
        items.push(("retry connections", ClientGlobalMenuAction::Retry));
        if controls.held_input > 0 || controls.pending_launch {
            items.push((
                "discard held typing",
                ClientGlobalMenuAction::DiscardHeldInput,
            ));
        }
        if controls.pending_layout {
            items.push((
                "discard unsaved layout changes",
                ClientGlobalMenuAction::DiscardPendingLayouts,
            ));
        }
    }
    if stop {
        items.push(("stop terminal", ClientGlobalMenuAction::StopTerminal));
    }
    if snapshot.update_available.is_some() || snapshot.latest_release_notes_available {
        items.push((
            if snapshot.update_available.is_some() {
                "update ready"
            } else {
                "what's new"
            },
            ClientGlobalMenuAction::WhatsNew,
        ));
    }
    items.push((
        "detach",
        ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Detach),
    ));
    items
}

impl ClientShellState {
    pub(super) fn toggle_global_menu(&mut self) {
        if matches!(self.overlay, Some(ClientShellOverlay::GlobalMenu(_))) {
            self.overlay = None;
        } else {
            let stop_target = self.snapshot.as_deref().and_then(focused_runtime_target);
            let stop_label = stop_target.as_ref().and_then(|target| {
                self.snapshot
                    .as_deref()?
                    .core_terminals
                    .as_ref()?
                    .iter()
                    .find(|terminal| terminal.target == *target)
                    .map(|terminal| terminal.label.clone())
            });
            self.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
                highlighted: 0,
                stop_target,
                stop_label,
            }));
        }
    }

    pub(super) fn move_global_menu_selection(&mut self, delta: isize) {
        let stop = matches!(self.overlay.as_ref(), Some(ClientShellOverlay::GlobalMenu(menu)) if menu.stop_target.is_some());
        let count = self
            .snapshot
            .as_deref()
            .map(|snapshot| global_menu_items(snapshot, stop).len())
            .unwrap_or(0);
        let Some(ClientShellOverlay::GlobalMenu(menu)) = self.overlay.as_mut() else {
            return;
        };
        menu.highlighted =
            (menu.highlighted as isize + delta).clamp(0, count.saturating_sub(1) as isize) as usize;
    }

    pub(super) fn open_reported_core_terminals(&mut self) {
        let terminals = self
            .endpoints
            .iter()
            .filter_map(|endpoint| endpoint.snapshot.as_deref())
            .flat_map(|snapshot| {
                snapshot
                    .core_terminals
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .cloned()
            })
            .collect();
        self.open_core_terminal_picker(terminals, "attach terminal");
    }

    pub(super) fn request_core_action(
        &mut self,
        action: CoreAction,
        outcome: &mut ClientShellInput,
    ) {
        if matches!(
            action,
            CoreAction::Retry
                | CoreAction::OpenCheckout(_)
                | CoreAction::BeginCheckoutWorktree { .. }
        ) {
            outcome.actions.push(ClientShellAction::Core(action));
            outcome.repaint = true;
            return;
        }
        let (title, detail) = match &action {
            CoreAction::Retry | CoreAction::OpenCheckout(_) | CoreAction::BeginCheckoutWorktree{..} => unreachable!(),
            CoreAction::DiscardHeldInput => ("Discard held typing?", "Discard unsent typing and cancel pending terminal placement. Programs already started stay running."),
            CoreAction::DiscardPendingLayouts => ("Discard unsaved layout changes?", "Discard pending arrangement changes. Running terminal programs stay alive."),
            CoreAction::RemoveCheckout(_) => ("Remove worktree?", "Remove this checkout and keep its branch. Dirty, main and in-use checkouts are refused."),
            CoreAction::StopTerminal(_) => ("Stop terminal?", "End this shared terminal program. Other viewers will lose this program too."),
        };
        let detail = if let CoreAction::RemoveCheckout(target) = &action {
            format!(
                "Remove {} ({}) and keep its branch. Dirty, main and in-use checkouts are refused.",
                target.label, target.folder
            )
        } else {
            detail.into()
        };
        self.overlay = Some(ClientShellOverlay::ConfirmRuntime(
            ClientRuntimeConfirmation {
                title: title.into(),
                detail,
                action,
            },
        ));
        outcome.repaint = true;
    }

    pub(super) fn activate_global_menu_item(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let (stop_target, stop_label) = match self.overlay.as_ref() {
            Some(ClientShellOverlay::GlobalMenu(menu)) => {
                (menu.stop_target.clone(), menu.stop_label.clone())
            }
            _ => (None, None),
        };
        let Some(action) = self.snapshot.as_deref().and_then(|snapshot| {
            global_menu_items(snapshot, stop_target.is_some())
                .get(index)
                .map(|(_, action)| *action)
        }) else {
            return;
        };
        if action == ClientGlobalMenuAction::WhatsNew
            && self
                .snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.release_notes.as_ref())
                .is_none()
        {
            return;
        }
        self.overlay = None;
        match action {
            ClientGlobalMenuAction::Binding(binding) => {
                self.record_binding(crate::input::KeybindMatch::Action(binding), outcome)
            }
            ClientGlobalMenuAction::WhatsNew => self.open_release_notes(),
            ClientGlobalMenuAction::AttachTerminal => self.open_reported_core_terminals(),
            ClientGlobalMenuAction::Retry => self.request_core_action(CoreAction::Retry, outcome),
            ClientGlobalMenuAction::DiscardHeldInput => {
                self.request_core_action(CoreAction::DiscardHeldInput, outcome)
            }
            ClientGlobalMenuAction::DiscardPendingLayouts => {
                self.request_core_action(CoreAction::DiscardPendingLayouts, outcome)
            }
            ClientGlobalMenuAction::StopTerminal => {
                if let Some(target) = stop_target {
                    self.request_core_action(CoreAction::StopTerminal(target), outcome);
                    if let (Some(label), Some(ClientShellOverlay::ConfirmRuntime(confirm))) =
                        (stop_label, &mut self.overlay)
                    {
                        confirm.detail=format!("Stop {label}? Other viewers will lose this shared terminal program too.");
                    }
                }
            }
        }
        outcome.repaint = true;
    }
}
