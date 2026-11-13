use std::collections::BTreeSet;
use std::io;

use herdr_core_tui::model::Operation;
use herdr_core_tui::worktrees::{Worktree, Worktrees};

use crate::client::ui_action::FrontendCommand;
use crate::client::ui_domain as schema;

use super::{Backend, Change, Completion, LaunchOutcome, Placement};

pub(super) enum WorktreeJob {
    Listed {
        source: schema::WorktreeSourceInfo,
        worktrees: Vec<schema::WorktreeInfo>,
    },
    Created {
        tree: schema::WorktreeInfo,
        label: Option<String>,
        focus: bool,
    },
    Opened {
        tree: schema::WorktreeInfo,
        label: Option<String>,
        focus: bool,
    },
    Removed {
        workspace: String,
        path: String,
    },
}

pub(super) enum CheckoutRemoval {
    Running,
    AwaitingLayout(CheckoutCleanup),
}

#[derive(Clone)]
pub(super) struct CheckoutCleanup {
    pub generation: u64,
    pub owner: Option<String>,
    pub folder: String,
    pub completion: Option<String>,
}

impl Backend {
    pub(crate) fn remove_checkout(
        &mut self,
        target: &crate::client::ui_model::CheckoutTarget,
        completion: Option<String>,
    ) -> io::Result<super::Action> {
        let owner = (target.machine_id != self.local_id()).then(|| target.machine_id.clone());
        if !self.owner_present(owner.as_deref()) {
            return Err(io::Error::other(
                "the checkout's machine is no longer configured",
            ));
        }
        if !self.stores.get(&owner).is_some_and(|store| store.loaded) {
            return Err(io::Error::other(
                "wait for this machine's saved layouts to load before removing a checkout",
            ));
        }
        let expected = target.repo_key.clone().ok_or_else(|| {
            io::Error::other("this folder has no verified worktree repository identity")
        })?;
        let folder = target.folder.clone();
        let referenced = self
            .document
            .workspaces
            .iter()
            .filter(|workspace| workspace.machine == owner)
            .filter(|workspace| {
                self.canonical_folder(owner.as_deref(), &workspace.folder) != folder
            })
            .map(|workspace| workspace.folder.clone())
            .collect::<Vec<_>>();
        let expected_owner = owner.clone().unwrap_or_else(|| self.local_id().to_owned());
        let client = self.client.clone();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        let generation = self.generation;
        let operation = herdr_core_tui::model::new_id();
        self.checkout_removals
            .insert(operation.clone(), CheckoutRemoval::Running);
        std::thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let canonical = herdr_core_tui::machine::resolve_folder(
                    &client,
                    owner.as_deref(),
                    None,
                    &folder,
                )?;
                if canonical != folder {
                    return Err(io::Error::other(
                        "the checkout path changed; refresh and select it again",
                    ));
                }
                let metadata = super::metadata::discover(
                    &client,
                    owner.as_deref(),
                    BTreeSet::from([folder.clone()]),
                );
                let repository =
                    metadata.repositories.get(&folder).ok_or_else(|| {
                        io::Error::other(metadata.errors.get(&folder).cloned().unwrap_or_else(
                            || "the worktree repository could not be verified".into(),
                        ))
                    })?;
                if repository.key != expected {
                    return Err(io::Error::other(
                        "the checkout now belongs to a different repository; no files were removed",
                    ));
                }
                let terminals =
                    super::safety::owner_inventory(&client, owner.as_deref(), &expected_owner)?;
                Worktrees::new(&client, owner.as_deref()).remove(
                    &repository.primary,
                    &folder,
                    &terminals,
                    &referenced,
                )?;
                Ok(())
            })()
            .map_err(|error| error.to_string());
            let _ = sender.send(super::jobs::JobResult::RemoveCheckout {
                operation,
                generation,
                owner,
                folder,
                completion,
                result,
            });
            let _ = wake.try_send(());
        });
        Ok(super::Action::Pending)
    }

    pub(super) fn checkout_removal_result(
        &mut self,
        operation: String,
        cleanup: CheckoutCleanup,
        result: Result<(), String>,
        change: &mut Change,
    ) -> io::Result<()> {
        if !self.checkout_removals.contains_key(&operation) {
            return Ok(());
        }
        if cleanup.generation != self.generation {
            let message = match result {
                Ok(()) => "Checkout removal completed after the core connection changed; its saved views were left unchanged. Refresh checkouts and close any view of the removed folder.".into(),
                Err(error) => format!("Checkout removal could not be confirmed after the core connection changed: {error}. Refresh checkouts before trying again."),
            };
            self.abandon_checkout_cleanup(&operation, cleanup, message, change);
            return Ok(());
        }
        if result.is_ok() {
            if let Some(message) = self.checkout_cleanup_unavailable(&cleanup) {
                self.abandon_checkout_cleanup(&operation, cleanup, message, change);
                return Ok(());
            }
            if !self
                .stores
                .get(&cleanup.owner)
                .is_some_and(|store| store.loaded)
            {
                if let Some(store) = self.stores.get(&cleanup.owner) {
                    store.refresh();
                }
                self.checkout_removals
                    .insert(operation, CheckoutRemoval::AwaitingLayout(cleanup));
                change.mutation = true;
                return Ok(());
            }
        }
        let completed = self.checkout_removed(
            cleanup.owner,
            cleanup.folder,
            cleanup.completion,
            result,
            change,
        );
        self.checkout_removals.remove(&operation);
        change.mutation = true;
        completed
    }

    pub(super) fn resume_checkout_removals(&mut self, change: &mut Change) -> io::Result<()> {
        let ready = self
            .checkout_removals
            .iter()
            .filter_map(|(operation, removal)| match removal {
                CheckoutRemoval::AwaitingLayout(cleanup) => Some((operation, cleanup)),
                CheckoutRemoval::Running => None,
            })
            .filter(|(_, cleanup)| {
                cleanup.generation != self.generation
                    || self.checkout_cleanup_unavailable(cleanup).is_some()
                    || self
                        .stores
                        .get(&cleanup.owner)
                        .is_some_and(|store| store.loaded)
            })
            .map(|(operation, cleanup)| (operation.clone(), cleanup.clone()))
            .collect::<Vec<_>>();
        for (operation, cleanup) in ready {
            self.checkout_removal_result(operation, cleanup, Ok(()), change)?;
        }
        Ok(())
    }

    fn checkout_cleanup_unavailable(&self, cleanup: &CheckoutCleanup) -> Option<String> {
        if !self.owner_present(cleanup.owner.as_deref()) {
            return Some("Checkout removed, but its machine is no longer configured; saved views were left unchanged. Refresh checkouts before closing any remaining view.".into());
        }
        let store = self.stores.get(&cleanup.owner)?;
        if store.loaded {
            return None;
        }
        store.error.as_deref().map(|error| {
            format!("Checkout removed, but its saved layout could not be reloaded: {error}. Refresh checkouts and close any remaining view of the removed folder.")
        })
    }

    fn abandon_checkout_cleanup(
        &mut self,
        operation: &str,
        cleanup: CheckoutCleanup,
        message: String,
        change: &mut Change,
    ) {
        self.checkout_removals.remove(operation);
        change.mutation = true;
        if let Some(id) = cleanup.completion {
            change.completions.push(Completion {
                id,
                result: Err(message),
            });
        } else {
            change.notices.push(message);
        }
        self.metadata_due = std::time::Instant::now();
        self.request_refresh();
    }

    pub(super) fn checkout_removed(
        &mut self,
        owner: Option<String>,
        folder: String,
        completion: Option<String>,
        result: Result<(), String>,
        change: &mut Change,
    ) -> io::Result<()> {
        let result = match result {
            Ok(()) => {
                let workspaces = self
                    .stores
                    .get(&owner)
                    .into_iter()
                    .flat_map(|store| store.document.workspaces.iter())
                    .filter(|workspace| {
                        workspace.machine == owner
                            && self.canonical_folder(owner.as_deref(), &workspace.folder) == folder
                    })
                    .map(|workspace| workspace.id.clone())
                    .collect::<Vec<_>>();
                let mut error = None;
                for workspace in workspaces {
                    if let Err(cause) =
                        self.apply_operation(owner.clone(), Operation::CloseWorkspace { workspace })
                    {
                        error = Some(cause.to_string());
                        break;
                    }
                }
                change.layout = true;
                self.view.repair(&self.document);
                match error {
                    Some(error) => Err(format!(
                        "checkout removed, but its saved view could not be closed: {error}"
                    )),
                    None => Ok(()),
                }
            }
            Err(error) => Err(error),
        };
        self.metadata_due = std::time::Instant::now();
        if let Some(id) = completion {
            change.completions.push(Completion {
                id,
                result: result.map(|_| schema::ResponseResult::Ok {}),
            });
        } else if let Err(error) = result {
            change.notices.push(error);
        }
        Ok(())
    }

    pub(super) fn worktree_command(
        &mut self,
        owner: Option<String>,
        id: String,
        command: FrontendCommand,
    ) -> io::Result<()> {
        let (workspace_id, explicit_cwd) = match &command {
            FrontendCommand::WorktreeList(params) => {
                (params.workspace_id.as_deref(), params.cwd.as_deref())
            }
            FrontendCommand::WorktreeCreate(params) => {
                (params.workspace_id.as_deref(), params.cwd.as_deref())
            }
            FrontendCommand::WorktreeOpen(params) => {
                (params.workspace_id.as_deref(), params.cwd.as_deref())
            }
            FrontendCommand::WorktreeRemove(params) => (Some(params.workspace_id.as_str()), None),
            _ => return Err(io::Error::other("this action is not a worktree operation")),
        };
        let workspace = match workspace_id {
            Some(id) => Some(self.workspace_target(owner.as_deref(), Some(id))?),
            None if explicit_cwd.is_none() => self.active_workspace_for(owner.as_deref()).cloned(),
            None => None,
        };
        let cwd = explicit_cwd
            .map(str::to_owned)
            .or_else(|| workspace.as_ref().map(|workspace| workspace.folder.clone()))
            .ok_or_else(|| {
                io::Error::other(
                    "choose a repository folder or workspace before using worktree actions",
                )
            })?;
        let mut referenced = self.document.workspaces.iter().filter(|workspace| workspace.machine == owner)
            .filter(|workspace| !matches!(&command, FrontendCommand::WorktreeRemove(params) if params.workspace_id == workspace.id))
            .map(|workspace| workspace.folder.clone()).collect::<Vec<_>>();
        referenced.sort();
        referenced.dedup();
        let open = self
            .document
            .workspaces
            .iter()
            .filter(|workspace| workspace.machine == owner)
            .map(|workspace| (workspace.folder.clone(), workspace.id.clone()))
            .collect::<Vec<_>>();
        let expected_id = owner.clone().unwrap_or_else(|| self.local_id().to_owned());
        let worktree_directory = self.presentation.worktree_directory.clone();
        let source_workspace_id = workspace.map(|workspace| workspace.id);
        let client = self.client.clone();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let result = (|| -> io::Result<WorktreeJob> {
                let cwd = herdr_core_tui::machine::resolve_folder(&client, owner.as_deref(), None, &cwd)?;
                let mut metadata = super::metadata::discover(&client, owner.as_deref(), BTreeSet::from([cwd.clone()]));
                let repository = metadata.repositories.remove(&cwd).ok_or_else(|| io::Error::other(
                    metadata.errors.remove(&cwd).unwrap_or_else(|| "repository metadata was not returned".into())))?;
                let git = Worktrees::new(&client, owner.as_deref());
                let source = schema::WorktreeSourceInfo { repo_key: repository.key.clone(), repo_name: repository.label.clone(),
                    repo_root: repository.primary.clone(), source_checkout_path: repository.root.clone(), source_workspace_id };
                let info = |tree: Worktree| {
                    let open_workspace_id = open.iter().find(|(path, _)| *path == tree.path).map(|(_, id)| id.clone());
                    let label = tree.branch.clone().unwrap_or_else(|| tree.path.rsplit('/').next().unwrap_or(&tree.path).to_owned());
                    let detached = !tree.bare && tree.branch.is_none();
                    schema::WorktreeInfo { is_linked_worktree: tree.path != repository.primary, path: tree.path, branch: tree.branch,
                        is_bare: tree.bare, is_detached: detached, is_prunable: tree.prunable,
                        open_workspace_id, label }
                };
                match command {
                    FrontendCommand::WorktreeList(_) => Ok(WorktreeJob::Listed { source,
                        worktrees: git.list(&repository.root)?.into_iter().map(info).collect() }),
                    FrontendCommand::WorktreeCreate(params) => {
                        let branch = params.branch.unwrap_or_else(|| {
                            let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                                .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64).unwrap_or(0);
                            crate::worktree_names::generated_branch_slug(seed)
                        }).trim().to_owned();
                        if branch.is_empty() { return Err(io::Error::other("branch is required")); }
                        let destination = params.path.unwrap_or_else(|| format!("{}/{}/{}", worktree_directory.trim_end_matches('/'), repository.label, crate::worktree_names::branch_to_path_slug(&branch)));
                        let tree = git.create_with_options(&repository.root, &branch, params.base.as_deref(), Some(&destination))?;
                        Ok(WorktreeJob::Created { tree: info(tree), label: params.label, focus: params.focus })
                    }
                    FrontendCommand::WorktreeOpen(params) => {
                        let tree = if let Some(path) = params.path {
                            let path = herdr_core_tui::machine::resolve_folder(&client, owner.as_deref(), Some(&repository.root), &path)?;
                            git.open(&repository.root, &path)?
                        } else if let Some(branch) = params.branch {
                            let mut trees = git.list(&repository.root)?.into_iter().filter(|tree| tree.branch.as_deref() == Some(branch.as_str()));
                            let tree = trees.next().ok_or_else(|| io::Error::other("this branch has no checked-out worktree"))?;
                            if trees.next().is_some() { return Err(io::Error::other("more than one worktree uses this branch; select an explicit path")); }
                            git.open(&repository.root, &tree.path)?
                        } else { git.open(&repository.root, &repository.root)? };
                        Ok(WorktreeJob::Opened { tree: info(tree), label: params.label, focus: params.focus })
                    }
                    FrontendCommand::WorktreeRemove(params) => {
                        if params.force { return Err(io::Error::other("force removal is not supported here; dirty or in-use checkouts are preserved")); }
                        let terminals = super::safety::owner_inventory(&client, owner.as_deref(), &expected_id)?;
                        git.remove(&repository.primary, &cwd, &terminals, &referenced)?;
                        Ok(WorktreeJob::Removed { workspace: params.workspace_id, path: cwd })
                    }
                    _ => unreachable!("validated worktree command"),
                }
            })().map_err(|error| error.to_string());
            let _ = sender.send(super::jobs::JobResult::Worktree {
                generation,
                owner,
                id,
                result,
            });
            let _ = wake.try_send(());
        });
        Ok(())
    }

    pub(super) fn worktree_result(
        &mut self,
        owner: Option<String>,
        id: String,
        result: Result<WorktreeJob, String>,
        change: &mut Change,
    ) -> io::Result<()> {
        let result = match result {
            Ok(WorktreeJob::Listed { source, worktrees }) => {
                Ok(schema::ResponseResult::WorktreeList { source, worktrees })
            }
            Ok(WorktreeJob::Removed { workspace, path }) => {
                let result = self.apply_operation(owner, Operation::CloseWorkspace { workspace: workspace.clone() })
                    .map(|_| schema::ResponseResult::WorktreeRemoved { workspace_id: workspace, path, forced: false })
                    .map_err(|error| format!("the checkout was removed, but its saved view could not be closed: {error}"));
                self.view.repair(&self.document);
                self.metadata_due = std::time::Instant::now();
                change.layout = true;
                result
            }
            Ok(WorktreeJob::Created { tree, label, focus }) => {
                self.start_worktree_view(owner, id, tree, label, focus, true, change)?;
                return Ok(());
            }
            Ok(WorktreeJob::Opened { tree, label, focus }) => {
                self.start_worktree_view(owner, id, tree, label, focus, false, change)?;
                return Ok(());
            }
            Err(error) => Err(error),
        };
        change.completions.push(Completion { id, result });
        Ok(())
    }

    fn start_worktree_view(
        &mut self,
        owner: Option<String>,
        id: String,
        mut tree: schema::WorktreeInfo,
        label: Option<String>,
        focus: bool,
        created: bool,
        change: &mut Change,
    ) -> io::Result<()> {
        let existing = self
            .document
            .workspaces
            .iter()
            .find(|workspace| workspace.machine == owner && workspace.folder == tree.path)
            .cloned();
        if !created {
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
                if focus {
                    self.view.focus_pane(&self.document, &pane.id);
                    self.view.active_owner = owner.clone();
                    self.remember_focus();
                }
                tree.open_workspace_id = Some(workspace.id.clone());
                change.completions.push(Completion {
                    id,
                    result: Ok(schema::ResponseResult::WorktreeOpened {
                        workspace: self.workspace_info(owner.as_deref(), &workspace.id)?,
                        tab: self.tab_info(owner.as_deref(), &tab.id)?,
                        root_pane: self.pane_info(owner.as_deref(), &pane.id)?,
                        worktree: tree,
                        already_open: true,
                    }),
                });
                change.layout = true;
                return Ok(());
            }
        }
        let custom_name = label.is_some();
        let placement = match existing {
            Some(workspace) => Placement::Tab {
                workspace: workspace.id,
                name: "1".into(),
                custom_name: false,
            },
            None => Placement::Workspace {
                owner,
                name: label.unwrap_or_else(|| tree.label.clone()),
                custom_name,
            },
        };
        match self.begin_launch(
            placement,
            tree.path.clone(),
            Default::default(),
            focus,
            Some(id.clone()),
        ) {
            Ok(token) => {
                tree.open_workspace_id = Some(token.workspace.clone());
                let kind = if created {
                    super::launch::CompletionKind::WorktreeCreated(tree)
                } else {
                    super::launch::CompletionKind::WorktreeOpened {
                        worktree: tree,
                        already_open: false,
                    }
                };
                self.launch_completion(&token.id, kind);
                change.launches.push(LaunchOutcome::Reserved { token });
            }
            Err(error) => change.completions.push(Completion {
                id,
                result: Err(format!(
                    "checkout is available, but its terminal could not be opened: {error}"
                )),
            }),
        }
        self.metadata_due = std::time::Instant::now();
        Ok(())
    }
}
