use std::io;
use std::thread;
use std::time::{Duration, Instant};

use herdr_core_tui::connection::Subscription;
use serde_json::Value;

use super::{Backend, Change};

pub(super) enum JobResult {
    MachineRetry {
        generation: u64,
        machine_id: String,
        result: Result<Value, String>,
    },
    Background {
        generation: u64,
        completion: Option<String>,
        result: Result<(), String>,
    },
    Launch {
        generation: u64,
        id: String,
        result: Result<super::launch::RunRecord, super::launch::LaunchError>,
    },
    Worktree {
        generation: u64,
        owner: Option<String>,
        id: String,
        result: Result<super::worktrees::WorktreeJob, String>,
    },
    RemoveCheckout {
        operation: String,
        generation: u64,
        owner: Option<String>,
        folder: String,
        completion: Option<String>,
        result: Result<(), String>,
    },
    Finished {
        generation: u64,
        address: String,
        result: Result<Value, String>,
    },
    Stop {
        generation: u64,
        result: Result<(), String>,
    },
    Profiles {
        generation: u64,
        epoch: u64,
        result: Result<Vec<Value>, String>,
    },
    Snapshot {
        generation: u64,
        epoch: u64,
        result: Result<Subscription, String>,
    },
    Metadata {
        generation: u64,
        owner: Option<String>,
        result: super::metadata::Metadata,
    },
}

impl Backend {
    pub(super) fn request_profiles(&mut self) {
        self.profiles_due = Instant::now() + Duration::from_secs(2);
        if self.profiles_in_flight || !self.connected() {
            return;
        }
        self.profiles_in_flight = true;
        let client = self.client.clone();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        let generation = self.generation;
        let epoch = self.profiles_epoch;
        thread::spawn(move || {
            let result = client
                .request("machine.list", serde_json::json!({}))
                .map_err(io::Error::from)
                .and_then(|reply| {
                    reply
                        .get("machines")
                        .and_then(Value::as_array)
                        .cloned()
                        .ok_or_else(|| io::Error::other("core machine list has no machines array"))
                })
                .map_err(|error| error.to_string());
            let _ = sender.send(JobResult::Profiles {
                generation,
                epoch,
                result,
            });
            let _ = wake.try_send(());
        });
    }

    pub(super) fn request_snapshot(&mut self) {
        if self.snapshot_in_flight || !self.connected() {
            return;
        }
        self.snapshot_in_flight = true;
        let client = self.client.clone();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        let generation = self.generation;
        let epoch = self.profiles_epoch;
        thread::spawn(move || {
            let result = client.subscribe().map_err(|error| error.to_string());
            let _ = sender.send(JobResult::Snapshot {
                generation,
                epoch,
                result,
            });
            let _ = wake.try_send(());
        });
    }

    pub(super) fn apply_job(&mut self, job: JobResult, change: &mut Change) -> io::Result<()> {
        match job {
            JobResult::MachineRetry {
                generation,
                machine_id,
                result,
            } => {
                if generation == self.generation {
                    self.machine_retries_in_flight.remove(&machine_id);
                    self.profiles_epoch += 1;
                    self.profiles_due = Instant::now();
                    change.machine_retries.push(super::MachineRetryOutcome {
                        machine_id,
                        error: match result {
                            Ok(profile) => super::machines::diagnostic(&profile),
                            Err(error) => Some(error),
                        },
                    });
                }
            }
            JobResult::Background {
                generation,
                completion,
                result,
            } => {
                if generation == self.generation {
                    if let Some(id) = completion {
                        change.completions.push(super::Completion {
                            id,
                            result: result
                                .map(|()| crate::client::ui_domain::ResponseResult::Ok {}),
                        });
                    } else if let Err(error) = result {
                        change.notices.push(error);
                    }
                }
            }
            JobResult::Launch {
                generation,
                id,
                result,
            } => {
                // Reconnect retains launch IDs and retry proof, but old success records need fresh inventory confirmation.
                if generation == self.generation || result.is_err() {
                    self.launch_result(&id, result, change)?;
                }
            }
            JobResult::Finished {
                generation,
                address,
                result,
            } => {
                if generation == self.generation {
                    self.finished_result(&address, result, change)?;
                }
            }
            JobResult::Stop { generation, result } => {
                if generation == self.generation {
                    if let Err(error) = result {
                        change.notices.push(error);
                    }
                }
            }
            JobResult::Worktree {
                generation,
                owner,
                id,
                result,
            } => {
                if generation == self.generation {
                    self.worktree_result(owner, id, result, change)?;
                }
            }
            JobResult::RemoveCheckout {
                operation,
                generation,
                owner,
                folder,
                completion,
                result,
            } => {
                self.checkout_removal_result(
                    operation,
                    super::worktrees::CheckoutCleanup {
                        generation,
                        owner,
                        folder,
                        completion,
                    },
                    result,
                    change,
                )?;
            }
            JobResult::Profiles {
                generation,
                epoch,
                result,
            } => {
                if generation != self.generation {
                    return Ok(());
                }
                self.profiles_in_flight = false;
                if epoch != self.profiles_epoch {
                    self.profiles_due = Instant::now();
                    return Ok(());
                }
                let profiles = match result {
                    Ok(profiles) => profiles,
                    Err(error) => {
                        change.notices.push(error);
                        return Ok(());
                    }
                };
                let update = self.inventory.set_machines(&profiles)?;
                change.inventory |= update.terminals;
                change.profiles |= !self.profiles_loaded || self.profiles != profiles;
                let previous: std::collections::BTreeSet<String> = self
                    .profiles
                    .iter()
                    .filter_map(|profile| profile["id"].as_str().map(str::to_owned))
                    .collect();
                self.profiles = profiles;
                self.profiles_loaded = true;
                let ids: std::collections::BTreeSet<String> = self
                    .profiles
                    .iter()
                    .filter_map(|profile| profile["id"].as_str().map(str::to_owned))
                    .collect();
                for id in &ids {
                    let owner = Some(id.clone());
                    self.ensure_store(owner.clone())?;
                    if !previous.contains(id) {
                        self.stores[&owner].connected(self.client.clone());
                    }
                }
                for (owner, store) in &self.stores {
                    if owner.as_ref().is_some_and(|id| !ids.contains(id)) {
                        store.disconnected();
                    }
                }
                if update.resubscribe {
                    self.request_snapshot();
                }
            }
            JobResult::Snapshot {
                generation,
                epoch,
                result,
            } => {
                if generation != self.generation {
                    return Ok(());
                }
                self.snapshot_in_flight = false;
                if epoch != self.profiles_epoch {
                    self.request_snapshot();
                    return Ok(());
                }
                match result {
                    Ok(subscription) => {
                        let mut inventory =
                            herdr_core_tui::inventory::Inventory::new(&subscription.snapshot)?;
                        let update = inventory.set_machines(&self.profiles)?;
                        self.subscription = subscription;
                        self.inventory = inventory;
                        change.inventory = true;
                        if update.resubscribe {
                            self.request_snapshot();
                        }
                    }
                    Err(error) => change.notices.push(error),
                }
            }
            JobResult::Metadata {
                generation,
                owner,
                mut result,
            } => {
                if generation == self.generation {
                    self.metadata_in_flight.remove(&owner);
                    if self.owner_present(owner.as_deref()) {
                        if let Some(previous) = self.metadata.get(&owner) {
                            result.retain_unavailable(previous);
                        }
                        self.metadata_watch
                            .entry(owner.clone())
                            .or_default()
                            .extend(
                                result
                                    .identities
                                    .values()
                                    .map(|identity| identity.primary.clone()),
                            );
                        change.metadata |= self.metadata.get(&owner) != Some(&result);
                        self.metadata.insert(owner, result);
                    }
                }
            }
        }
        Ok(())
    }
}
