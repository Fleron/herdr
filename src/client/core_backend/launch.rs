use std::collections::HashMap;
use std::io;

use herdr_core_tui::model::{
    local_terminal, new_id, terminal_address, Axis, LayoutNode, Operation, Pane, Tab, Workspace,
};
use serde_json::{json, Value};

use super::{Backend, Change};

#[derive(Clone, Debug)]
pub(crate) enum Placement {
    Workspace {
        owner: Option<String>,
        name: String,
        custom_name: bool,
    },
    Tab {
        workspace: String,
        name: String,
        custom_name: bool,
    },
    Split {
        workspace: String,
        tab: String,
        target: String,
        axis: Axis,
        ratio: Option<f32>,
        passthrough: bool,
    },
    Overlay {
        workspace: String,
        tab: String,
        target: String,
        previous_zoomed: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct LaunchToken {
    pub id: String,
    pub pane: String,
    pub workspace: String,
    pub tab: String,
    pub focus: bool,
}

#[derive(Debug)]
pub(crate) enum LaunchOutcome {
    Reserved {
        token: LaunchToken,
    },
    Placed {
        token: LaunchToken,
        input_may_retarget: bool,
    },
    Failed {
        token: LaunchToken,
        error: String,
        uncertain: bool,
    },
}

pub(super) struct PendingLaunch {
    pub token: LaunchToken,
    placement: Placement,
    owner: Option<String>,
    focus: bool,
    completion: Option<String>,
    record: Option<Value>,
    error: Option<String>,
    completion_kind: CompletionKind,
    request: Option<LaunchRequest>,
    retryable: bool,
    input_may_retarget: bool,
}

#[derive(Clone)]
enum LaunchRequest {
    Program(ProgramRequest),
    Attach { address: String },
}

#[derive(Clone)]
struct ProgramRequest {
    folder: String,
    argv: Vec<String>,
    env: HashMap<String, String>,
    shell: Option<super::settings::ShellOverride>,
    keep: bool,
    checkout: Option<String>,
}

pub(super) enum CompletionKind {
    Plain,
    WorktreeCreated(crate::client::ui_domain::WorktreeInfo),
    WorktreeOpened {
        worktree: crate::client::ui_domain::WorktreeInfo,
        already_open: bool,
    },
}

pub(super) struct RunRecord {
    folder: String,
    record: Value,
}

pub(super) struct LaunchError {
    message: String,
    uncertain: bool,
}

impl Backend {
    pub(crate) fn attach_runtime(
        &mut self,
        target: &crate::client::ui_model::RuntimeTerminalTarget,
    ) -> io::Result<super::Action> {
        let owner = (target.machine_id != self.local_id()).then(|| target.machine_id.clone());
        let local = local_terminal(None, &target.terminal).map_err(io::Error::other)?;
        if let Some(pane) = self
            .document
            .workspaces
            .iter()
            .filter(|workspace| workspace.machine == owner)
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .find(|pane| pane.terminal == local)
            .map(|pane| pane.id.clone())
        {
            self.relinquish_launch_focus();
            self.view.focus_pane(&self.document, &pane);
            self.view.active_owner = owner;
            self.remember_focus();
            self.revision += 1;
            return Ok(super::Action::Complete(Box::new(
                crate::client::ui_domain::ResponseResult::Ok {},
            )));
        }
        let placement = match self.active_workspace_for(owner.as_deref()) {
            Some(workspace) => Placement::Tab {
                workspace: workspace.id.clone(),
                name: (workspace.tabs.len() + 1).to_string(),
                custom_name: false,
            },
            None => Placement::Workspace {
                owner: owner.clone(),
                name: String::new(),
                custom_name: false,
            },
        };
        let (_, token) = self.reserve_launch(placement, true, None)?;
        let address = terminal_address(owner.as_deref(), local);
        self.launches
            .get_mut(&token.id)
            .expect("reserved attachment")
            .request = Some(LaunchRequest::Attach { address });
        self.spawn_launch_request(&token.id)?;
        Ok(super::Action::Launch(token))
    }

    pub(crate) fn stop_runtime(
        &mut self,
        target: &crate::client::ui_model::RuntimeTerminalTarget,
    ) -> io::Result<()> {
        let owner = (target.machine_id != self.local_id()).then(|| target.machine_id.clone());
        if !self.owner_present(owner.as_deref()) {
            return Err(io::Error::other(
                "the terminal's core machine is no longer configured",
            ));
        }
        let local = local_terminal(None, &target.terminal).map_err(io::Error::other)?;
        let address = terminal_address(owner.as_deref(), local);
        let client = self.client.clone();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let record = selected_terminal(&client, &address)?;
                if record["state"] != "live" {
                    return Err(io::Error::other(
                        "this terminal program has already finished",
                    ));
                }
                client.request("terminal.kill", json!({"terminal": address}))?;
                Ok(())
            })()
            .map_err(|error| error.to_string());
            let _ = sender.send(super::jobs::JobResult::Stop { generation, result });
            let _ = wake.try_send(());
        });
        Ok(())
    }

    pub(crate) fn begin_launch(
        &mut self,
        placement: Placement,
        folder: String,
        env: HashMap<String, String>,
        focus: bool,
        completion: Option<String>,
    ) -> io::Result<LaunchToken> {
        let program = ProgramRequest {
            folder,
            argv: Vec::new(),
            env,
            shell: self.defaults.shell.clone(),
            keep: true,
            checkout: None,
        };
        self.begin_program_launch(placement, program, focus, completion)
    }

    pub(crate) fn run_background_command(
        &mut self,
        owner: Option<&str>,
        folder: String,
        argv: Vec<String>,
        env: HashMap<String, String>,
        completion: Option<String>,
    ) -> io::Result<super::Action> {
        if !self.owner_present(owner) {
            return Err(io::Error::other(
                "the command's core machine is no longer configured",
            ));
        }
        if argv.is_empty() {
            return Err(io::Error::other("a background command must name a program"));
        }
        let owner = owner.map(str::to_owned);
        let client = self.client.clone();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        let generation = self.generation;
        let command_id = new_id();
        std::thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let folder = herdr_core_tui::machine::resolve_folder(&client, owner.as_deref(), None, &folder)?;
                client.request("terminal.run", json!({"argv": argv, "cwd": folder, "on": owner, "env": env,
                    "no_parent": true, "keep": false, "labels": {"herdr.tui.command": command_id}}))
                    .map_err(|error| io::Error::other(format!("background command start could not be confirmed: {error}; refresh running terminals before invoking it again")))?;
                Ok(())
            })().map_err(|error| error.to_string());
            let _ = sender.send(super::jobs::JobResult::Background {
                generation,
                completion,
                result,
            });
            let _ = wake.try_send(());
        });
        Ok(super::Action::Pending)
    }

    pub(crate) fn begin_overlay_command(
        &mut self,
        owner: Option<&str>,
        pane: Option<&str>,
        folder: String,
        argv: Vec<String>,
        env: HashMap<String, String>,
        completion: Option<String>,
    ) -> io::Result<LaunchToken> {
        let (workspace, tab, target) = self.pane_target(owner, pane)?;
        let previous_zoomed = self.view.zoomed.contains(&tab.id);
        self.begin_command_launch(
            Placement::Overlay {
                workspace: workspace.id,
                tab: tab.id,
                target,
                previous_zoomed,
            },
            folder,
            argv,
            env,
            true,
            completion,
        )
    }

    pub(crate) fn begin_command_launch(
        &mut self,
        placement: Placement,
        folder: String,
        argv: Vec<String>,
        env: HashMap<String, String>,
        focus: bool,
        completion: Option<String>,
    ) -> io::Result<LaunchToken> {
        self.begin_program_launch(
            placement,
            ProgramRequest {
                folder,
                argv,
                env,
                shell: None,
                keep: false,
                checkout: None,
            },
            focus,
            completion,
        )
    }

    pub(crate) fn open_checkout(
        &mut self,
        target: &crate::client::ui_model::CheckoutTarget,
    ) -> io::Result<super::Action> {
        let owner = (target.machine_id != self.local_id()).then(|| target.machine_id.clone());
        let existing = self
            .document
            .workspaces
            .iter()
            .find(|workspace| {
                workspace.machine == owner
                    && self.canonical_folder(owner.as_deref(), &workspace.folder) == target.folder
            })
            .cloned();
        if let Some(workspace) = existing
            .as_ref()
            .filter(|workspace| !workspace.tabs.is_empty())
        {
            let tab = self
                .view
                .tabs
                .get(&workspace.id)
                .and_then(|id| workspace.tab(id))
                .unwrap_or(&workspace.tabs[0]);
            let pane = self
                .view
                .panes
                .get(&tab.id)
                .and_then(|id| tab.pane(id))
                .unwrap_or(&tab.panes[0]);
            self.focus_pane(owner.as_deref(), &pane.id)?;
            return Ok(super::Action::Complete(Box::new(
                crate::client::ui_domain::ResponseResult::Ok {},
            )));
        }
        let placement = match existing {
            Some(workspace) => Placement::Tab {
                workspace: workspace.id,
                name: "1".into(),
                custom_name: false,
            },
            None => Placement::Workspace {
                owner,
                name: target.label.clone(),
                custom_name: false,
            },
        };
        let program = ProgramRequest {
            folder: target.folder.clone(),
            argv: Vec::new(),
            env: HashMap::new(),
            shell: self.defaults.shell.clone(),
            keep: true,
            checkout: target.repo_key.clone(),
        };
        self.begin_program_launch(placement, program, true, None)
            .map(super::Action::Launch)
    }

    fn begin_program_launch(
        &mut self,
        placement: Placement,
        program: ProgramRequest,
        focus: bool,
        completion: Option<String>,
    ) -> io::Result<LaunchToken> {
        let (_, token) = self.reserve_launch(placement, focus, completion)?;
        self.launches
            .get_mut(&token.id)
            .expect("reserved program")
            .request = Some(LaunchRequest::Program(program));
        self.spawn_launch_request(&token.id)?;
        Ok(token)
    }

    fn spawn_launch_request(&mut self, id: &str) -> io::Result<()> {
        let pending = self
            .launches
            .get_mut(id)
            .ok_or_else(|| io::Error::other("launch is no longer pending"))?;
        let request = pending
            .request
            .clone()
            .ok_or_else(|| io::Error::other("launch has no pending request"))?;
        pending.retryable = false;
        let owner = pending.owner.clone();
        let client = self.client.clone();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        let generation = self.generation;
        let id = id.to_owned();
        std::thread::spawn(move || {
            let result = match request {
                LaunchRequest::Program(program) => (|| {
                    let folder = herdr_core_tui::machine::resolve_folder(
                        &client,
                        owner.as_deref(),
                        None,
                        &program.folder,
                    )
                    .map_err(|error| LaunchError {
                        message: error.to_string(),
                        uncertain: false,
                    })?;
                    if let Some(expected) = program.checkout {
                        let metadata = super::metadata::discover(
                            &client,
                            owner.as_deref(),
                            std::collections::BTreeSet::from([folder.clone()]),
                        );
                        let actual =
                            metadata
                                .repositories
                                .get(&folder)
                                .ok_or_else(|| LaunchError {
                                    message: metadata.errors.get(&folder).cloned().unwrap_or_else(
                                        || "the checkout repository could not be verified".into(),
                                    ),
                                    uncertain: false,
                                })?;
                        if actual.key != expected {
                            return Err(LaunchError { message: "the checkout now belongs to a different repository; refresh and select it again".into(), uncertain: false });
                        }
                        herdr_core_tui::worktrees::Worktrees::new(&client, owner.as_deref())
                            .open(&actual.root, &folder)
                            .map_err(|error| LaunchError {
                                message: error.to_string(),
                                uncertain: false,
                            })?;
                    }
                    let argv = match program.shell {
                        Some(shell) => {
                            shell
                                .argv(&client, owner.as_deref())
                                .map_err(|error| LaunchError {
                                    message: error.to_string(),
                                    uncertain: false,
                                })?
                        }
                        None => program.argv,
                    };
                    let mut record = client.request("terminal.run", json!({
                        "argv": argv, "cwd": folder, "on": owner, "env": program.env,
                        "no_parent": true, "keep": program.keep, "labels": {"herdr.tui.launch": id}
                    })).map_err(|error| LaunchError { message: error.to_string(), uncertain: true })?;
                    let address = record["address"].as_str().ok_or_else(|| LaunchError {
                        message: "terminal launch reply has no address".into(),
                        uncertain: true,
                    })?;
                    let local = address
                        .split_once('/')
                        .map_or(address, |(_, local)| local)
                        .to_owned();
                    record["address"] = Value::String(terminal_address(owner.as_deref(), &local));
                    Ok(RunRecord { folder, record })
                })(),
                LaunchRequest::Attach { address } => (|| -> io::Result<RunRecord> {
                    let record = selected_terminal(&client, &address)?;
                    if record["state"] != "live" {
                        return Err(io::Error::other(
                            "the selected terminal program has finished",
                        ));
                    }
                    let folder = record["cwd"]
                        .as_str()
                        .ok_or_else(|| {
                            io::Error::other("selected terminal has no working directory")
                        })?
                        .to_owned();
                    Ok(RunRecord { folder, record })
                })()
                .map_err(|error| LaunchError {
                    message: error.to_string(),
                    uncertain: false,
                }),
            };
            let _ = sender.send(super::jobs::JobResult::Launch {
                generation,
                id,
                result,
            });
            let _ = wake.try_send(());
        });
        Ok(())
    }

    fn reserve_launch(
        &mut self,
        placement: Placement,
        focus: bool,
        completion: Option<String>,
    ) -> io::Result<(Option<String>, LaunchToken)> {
        let owner = match &placement {
            Placement::Workspace { owner, .. } => owner.clone(),
            Placement::Tab { workspace, .. }
            | Placement::Split { workspace, .. }
            | Placement::Overlay { workspace, .. } => self
                .document
                .workspace(workspace)
                .ok_or_else(|| io::Error::other("the target workspace no longer exists"))?
                .machine
                .clone(),
        };
        if !self.owner_present(owner.as_deref()) {
            return Err(io::Error::other(
                "the target core machine is no longer configured",
            ));
        }
        if !self.stores.get(&owner).is_some_and(|store| store.loaded) {
            return Err(io::Error::other(
                "the target machine's saved layout has not loaded",
            ));
        }
        if !self.launches.is_empty() {
            return Err(io::Error::other(
                "a terminal launch is pending; resolve or discard it before starting another",
            ));
        }
        let token = LaunchToken {
            id: new_id(),
            pane: new_id(),
            focus,
            workspace: match &placement {
                Placement::Workspace { .. } => new_id(),
                Placement::Tab { workspace, .. }
                | Placement::Split { workspace, .. }
                | Placement::Overlay { workspace, .. } => workspace.clone(),
            },
            tab: match &placement {
                Placement::Split { tab, .. } | Placement::Overlay { tab, .. } => tab.clone(),
                _ => new_id(),
            },
        };
        let pending = PendingLaunch {
            token: token.clone(),
            placement,
            owner: owner.clone(),
            focus,
            completion,
            record: None,
            error: None,
            completion_kind: CompletionKind::Plain,
            request: None,
            retryable: false,
            input_may_retarget: false,
        };
        self.launches.insert(token.id.clone(), pending);
        Ok((owner, token))
    }

    pub(crate) fn pending_launch(&self) -> Option<&LaunchToken> {
        self.launches.values().next().map(|launch| &launch.token)
    }

    pub(crate) fn relinquish_launch_focus(&mut self) {
        for launch in self.launches.values_mut() {
            launch.focus = false;
            launch.token.focus = false;
        }
    }

    pub(super) fn launch_completion(&mut self, token: &str, kind: CompletionKind) {
        if let Some(launch) = self.launches.get_mut(token) {
            launch.completion_kind = kind;
        }
    }

    pub(crate) fn retry_pending_launches(&mut self) {
        let retry = self
            .launches
            .iter()
            .filter(|(_, launch)| launch.retryable)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for launch in self.launches.values_mut() {
            launch.error = None;
        }
        for id in retry {
            if let Err(error) = self.spawn_launch_request(&id) {
                if let Some(launch) = self.launches.get_mut(&id) {
                    launch.error = Some(error.to_string());
                }
            }
        }
        self.request_refresh();
    }

    pub(crate) fn discard_pending_launches(&mut self) {
        self.launches.clear();
    }

    pub(super) fn invalidate_pending_input(&mut self) {
        for launch in self.launches.values_mut() {
            launch.input_may_retarget = true;
        }
    }

    pub(super) fn protect_restarted_launches(&mut self, addresses: &[String]) {
        for address in addresses {
            let Some(record) = self.inventory.get(address) else {
                continue;
            };
            let Some(id) = record
                .pointer("/labels/herdr.tui.launch")
                .and_then(Value::as_str)
            else {
                continue;
            };
            if let Some(launch) = self.launches.get_mut(id) {
                if super::projection::record_owner(record) == launch.owner.as_deref() {
                    launch.input_may_retarget = true;
                }
            }
        }
    }

    pub(super) fn launch_result(
        &mut self,
        id: &str,
        result: Result<RunRecord, LaunchError>,
        change: &mut Change,
    ) -> io::Result<()> {
        let Some(launch) = self.launches.get_mut(id) else {
            return Ok(());
        };
        match result {
            Ok(result) => {
                let mut record = result.record;
                if record["cwd"].as_str().is_none() {
                    record["cwd"] = Value::String(result.folder);
                }
                launch.record = Some(record);
                launch.error = None;
            }
            Err(error) => {
                launch.error = Some(error.message.clone());
                change.launches.push(LaunchOutcome::Failed {
                    token: launch.token.clone(),
                    error: error.message.clone(),
                    uncertain: error.uncertain,
                });
                launch.retryable = !error.uncertain;
                if let Some(id) = launch.completion.take() {
                    change.completions.push(super::Completion {
                        id,
                        result: Err(error.message),
                    });
                }
            }
        }
        self.reconcile_launches(change)
    }

    pub(super) fn reconcile_launches(&mut self, change: &mut Change) -> io::Result<()> {
        let ids: Vec<_> = self.launches.keys().cloned().collect();
        for id in ids {
            let launch = &self.launches[&id];
            let mut matches = self.inventory.records().filter(|record| {
                record
                    .pointer("/labels/herdr.tui.launch")
                    .and_then(Value::as_str)
                    == Some(id.as_str())
                    && super::projection::record_owner(record) == launch.owner.as_deref()
            });
            let observed = matches.next().cloned();
            let duplicate = matches.next().is_some();
            drop(matches);
            if duplicate {
                if launch.error.as_deref()
                    != Some("more than one program claims this pending launch")
                {
                    change.launches.push(LaunchOutcome::Failed {
                        token: launch.token.clone(),
                        error: "more than one program claims this pending launch".into(),
                        uncertain: true,
                    });
                    self.launches.get_mut(&id).expect("pending launch").error =
                        Some("more than one program claims this pending launch".into());
                }
                continue;
            }
            let record = if launch.input_may_retarget {
                observed.or_else(|| launch.record.clone())
            } else {
                launch.record.clone().or(observed)
            };
            let Some(record) = record else { continue };
            if launch.error.is_some() && launch.record.is_some() {
                continue;
            }
            let launch = self.launches.remove(&id).expect("pending launch");
            match self.place_launch(&launch, record.clone()) {
                Ok(()) => {
                    if let Some(id) = launch.completion.clone() {
                        change.completions.push(super::Completion {
                            id,
                            result: self
                                .launch_response(&launch)
                                .map_err(|error| error.to_string()),
                        });
                    }
                    change.launches.push(LaunchOutcome::Placed {
                        token: launch.token,
                        input_may_retarget: launch.input_may_retarget,
                    });
                    change.layout = true;
                    change.inventory = true;
                }
                Err(error) => {
                    let message = format!(
                        "program remains available in core, but its view could not be saved: {error}"
                    );
                    change.launches.push(LaunchOutcome::Failed {
                        token: launch.token.clone(),
                        error: message.clone(),
                        uncertain: false,
                    });
                    let mut launch = launch;
                    launch.record = Some(record);
                    launch.error = Some(message);
                    self.launches.insert(id, launch);
                }
            }
        }
        Ok(())
    }

    fn place_launch(&mut self, launch: &PendingLaunch, record: Value) -> io::Result<()> {
        let address = record["address"]
            .as_str()
            .ok_or_else(|| io::Error::other("launched program has no address"))?
            .to_owned();
        let local = local_terminal(launch.owner.as_deref(), &address).map_err(io::Error::other)?;
        let mut pane = Pane::new(local);
        pane.id = launch.token.pane.clone();
        let operation = match &launch.placement {
            Placement::Workspace {
                owner,
                name,
                custom_name,
            } => {
                let folder = record["cwd"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("launched program has no working directory"))?;
                let name = if name.is_empty() {
                    super::actions::folder_label(folder)
                } else {
                    name.clone()
                };
                let mut workspace = Workspace::new(name, folder, owner.clone());
                workspace.id = launch.token.workspace.clone();
                workspace
                    .extra
                    .insert("custom_name".into(), Value::Bool(*custom_name));
                let mut tab = Tab::with_pane("1", pane);
                tab.id = launch.token.tab.clone();
                workspace.tabs.push(tab);
                Operation::AddWorkspace(workspace)
            }
            Placement::Tab {
                workspace,
                name,
                custom_name,
            } => {
                let mut tab = Tab::with_pane(name, pane);
                tab.extra
                    .insert("custom_name".into(), Value::Bool(*custom_name));
                tab.id = launch.token.tab.clone();
                Operation::AddTab {
                    workspace: workspace.clone(),
                    tab,
                }
            }
            Placement::Overlay {
                workspace,
                tab,
                target,
                previous_zoomed,
            } => {
                pane.extra.insert(
                    "herdr.tui.overlay".into(),
                    json!({"focus": target, "zoomed": previous_zoomed}),
                );
                let expected = self
                    .document
                    .workspace(workspace)
                    .and_then(|workspace| workspace.tab(tab))
                    .ok_or_else(|| io::Error::other("the command target tab no longer exists"))?
                    .layout
                    .clone();
                let mut layout = expected.clone();
                if !insert_pane(
                    &mut layout,
                    target,
                    &pane.id,
                    &launch.token.id,
                    Axis::Horizontal,
                    0.5,
                ) {
                    return Err(io::Error::other("the command target pane no longer exists"));
                }
                Operation::InsertPane {
                    workspace: workspace.clone(),
                    tab: tab.clone(),
                    pane,
                    expected,
                    layout,
                }
            }
            Placement::Split {
                workspace,
                tab,
                target,
                axis,
                ratio,
                passthrough,
            } => {
                pane.extra
                    .insert("right_click_passthrough".into(), Value::Bool(*passthrough));
                let expected = self
                    .document
                    .workspace(workspace)
                    .and_then(|workspace| workspace.tab(tab))
                    .ok_or_else(|| io::Error::other("the split target tab no longer exists"))?
                    .layout
                    .clone();
                let mut layout = expected.clone();
                if !insert_pane(
                    &mut layout,
                    target,
                    &pane.id,
                    &launch.token.id,
                    *axis,
                    ratio.unwrap_or(0.5),
                ) {
                    return Err(io::Error::other("the split target pane no longer exists"));
                }
                Operation::InsertPane {
                    workspace: workspace.clone(),
                    tab: tab.clone(),
                    pane,
                    expected,
                    layout,
                }
            }
        };
        self.apply_operation(launch.owner.clone(), operation)?;
        self.observe_terminal(record)?;
        if launch.focus {
            self.view.focus_pane(&self.document, &launch.token.pane);
            self.view.active_owner = launch.owner.clone();
            if matches!(launch.placement, Placement::Overlay { .. }) {
                self.view.zoomed.insert(launch.token.tab.clone());
            }
        }
        self.remember_focus();
        self.revision += 1;
        Ok(())
    }

    fn launch_response(
        &self,
        launch: &PendingLaunch,
    ) -> io::Result<crate::client::ui_domain::ResponseResult> {
        use crate::client::ui_domain::ResponseResult;
        match &launch.completion_kind {
            CompletionKind::Plain => Ok(ResponseResult::Ok {}),
            CompletionKind::WorktreeCreated(worktree) => Ok(ResponseResult::WorktreeCreated {
                workspace: self.workspace_info(launch.owner.as_deref(), &launch.token.workspace)?,
                tab: self.tab_info(launch.owner.as_deref(), &launch.token.tab)?,
                root_pane: self.pane_info(launch.owner.as_deref(), &launch.token.pane)?,
                worktree: worktree.clone(),
            }),
            CompletionKind::WorktreeOpened {
                worktree,
                already_open,
            } => Ok(ResponseResult::WorktreeOpened {
                workspace: self.workspace_info(launch.owner.as_deref(), &launch.token.workspace)?,
                tab: self.tab_info(launch.owner.as_deref(), &launch.token.tab)?,
                root_pane: self.pane_info(launch.owner.as_deref(), &launch.token.pane)?,
                worktree: worktree.clone(),
                already_open: *already_open,
            }),
        }
    }
}

fn selected_terminal(
    client: &herdr_core_tui::connection::Client,
    address: &str,
) -> io::Result<Value> {
    let listing = herdr_core_tui::inventory::TerminalListing::parse(
        &client.request("terminal.list", json!({"all": true}))?,
    )?;
    listing.terminals.into_iter().find(|terminal| terminal.record["address"].as_str() == Some(address))
        .map(|terminal| terminal.record).ok_or_else(|| io::Error::other("the selected terminal could not be verified in the current core inventory; refresh and select it again"))
}

fn insert_pane(
    layout: &mut LayoutNode,
    target: &str,
    pane: &str,
    split: &str,
    axis: Axis,
    ratio: f32,
) -> bool {
    match layout {
        LayoutNode::Pane { id, .. } if id == target => {
            let old = layout.clone();
            let first = old;
            let second = LayoutNode::pane(pane.to_owned());
            *layout = LayoutNode::Split {
                id: split.into(),
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
                extra: Default::default(),
            };
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { first, second, .. } => {
            insert_pane(first, target, pane, split, axis, ratio)
                || insert_pane(second, target, pane, split, axis, ratio)
        }
    }
}
