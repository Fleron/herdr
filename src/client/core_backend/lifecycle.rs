use std::collections::BTreeSet;
use std::io;
use std::time::{Duration, Instant};

use herdr_core_tui::model::{local_terminal, terminal_address, Operation};
use serde_json::{json, Value};

use super::{Backend, Change};

impl Backend {
    pub(super) fn request_finished(&mut self, changed: bool) {
        if !changed && Instant::now() < self.finished_due {
            return;
        }
        self.finished_due = Instant::now() + Duration::from_secs(2);
        let addresses = self
            .document
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace.tabs.iter().flat_map(|tab| {
                    tab.panes
                        .iter()
                        .map(|pane| terminal_address(workspace.machine.as_deref(), &pane.terminal))
                })
            })
            .collect::<BTreeSet<_>>();
        for address in addresses {
            if self.finished_in_flight.contains(&address)
                || self
                    .inventory
                    .get(&address)
                    .is_none_or(|record| record["state"] != "finished")
            {
                continue;
            }
            self.finished_in_flight.insert(address.clone());
            let client = self.client.clone();
            let sender = self.job_sender.clone();
            let wake = self.wake.clone();
            let generation = self.generation;
            std::thread::spawn(move || {
                let result = client
                    .request("terminal.get", json!({"terminal": address}))
                    .map_err(|error| error.to_string());
                let _ = sender.send(super::jobs::JobResult::Finished {
                    generation,
                    address,
                    result,
                });
                let _ = wake.try_send(());
            });
        }
    }

    pub(super) fn finished_result(
        &mut self,
        address: &str,
        result: Result<Value, String>,
        change: &mut Change,
    ) -> io::Result<()> {
        self.finished_in_flight.remove(address);
        let Ok(record) = result else { return Ok(()) };
        if record["state"] != "finished"
            || self
                .inventory
                .get(address)
                .is_none_or(|current| current["state"] != "finished")
        {
            return Ok(());
        }
        let targets = self
            .document
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace.tabs.iter().flat_map(|tab| {
                    tab.panes
                        .iter()
                        .filter(|pane| {
                            terminal_address(workspace.machine.as_deref(), &pane.terminal)
                                == address
                        })
                        .map(|pane| {
                            (
                                workspace.machine.clone(),
                                workspace.id.clone(),
                                tab.id.clone(),
                                pane.id.clone(),
                                pane.terminal.clone(),
                            )
                        })
                })
            })
            .collect::<Vec<_>>();
        for (owner, workspace, tab, pane, terminal) in targets {
            let Some(actual) = record["address"].as_str() else {
                continue;
            };
            if local_terminal(owner.as_deref(), actual).ok() != Some(terminal.as_str())
                && actual.split_once('/').map(|(_, local)| local) != Some(terminal.as_str())
            {
                continue;
            }
            match self.close_pane_view(owner, workspace, tab, pane) {
                Ok(()) => change.layout = true,
                Err(error) => change
                    .notices
                    .push(format!("could not close a finished terminal view: {error}")),
            }
        }
        Ok(())
    }
    pub(super) fn close_pane_view(
        &mut self,
        owner: Option<String>,
        workspace: String,
        tab: String,
        pane: String,
    ) -> io::Result<()> {
        let restore = self
            .document
            .workspace(&workspace)
            .and_then(|workspace| workspace.tab(&tab))
            .and_then(|tab| tab.pane(&pane))
            .and_then(|pane| pane.extra.get("herdr.tui.overlay"))
            .cloned();
        let focused = self.view.panes.get(&tab) == Some(&pane);
        self.apply_operation(
            owner,
            Operation::ClosePane {
                workspace: workspace.clone(),
                tab: tab.clone(),
                pane,
            },
        )?;
        if focused {
            if let Some(restore) = restore {
                if let Some(previous) = restore["focus"].as_str().filter(|previous| {
                    self.document
                        .workspace(&workspace)
                        .and_then(|workspace| workspace.tab(&tab))
                        .is_some_and(|tab| tab.pane(previous).is_some())
                }) {
                    self.view.panes.insert(tab.clone(), previous.into());
                }
                if restore["zoomed"].as_bool() == Some(true) {
                    self.view.zoomed.insert(tab);
                } else {
                    self.view.zoomed.remove(&tab);
                }
                self.remember_focus();
            }
        }
        Ok(())
    }
}
