use std::io;

use serde_json::{json, Value};

use super::{jobs::JobResult, Backend};

pub(crate) struct MachineRetryOutcome {
    pub machine_id: String,
    pub error: Option<String>,
}

impl Backend {
    pub(crate) fn retry_machine(&mut self, machine_id: &str) -> io::Result<()> {
        if !self.owner_present(Some(machine_id)) {
            return Err(io::Error::other("the core machine is no longer configured"));
        }
        if !self.connected() {
            return Err(io::Error::other("the local core connection is unavailable"));
        }
        if !self.machine_retries_in_flight.insert(machine_id.to_owned()) {
            return Ok(());
        }
        let client = self.client.clone();
        let generation = self.generation;
        let machine_id = machine_id.to_owned();
        let sender = self.job_sender.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let result = (|| -> io::Result<Value> {
                let listing = client.request("machine.list", json!({}))?;
                let profiles = listing["machines"].as_array().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "core machine list has no machines array")
                })?;
                let profile = profiles.iter().find(|profile| profile["id"] == machine_id)
                    .ok_or_else(|| io::Error::other("the selected core machine is no longer configured"))?;
                let name = profile["name"].as_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "the selected core machine has no name")
                })?;
                let connected = client.request("machine.connect", json!({"machine": name}))?;
                if connected["id"] != machine_id {
                    return Err(io::Error::other("the machine name changed owners during retry; refresh and select the intended machine again"));
                }
                if connected["name"].as_str().is_none() {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "core machine connection result has no name"));
                }
                if connected["state"].as_str().is_none() {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "core machine connection result has no state"));
                }
                Ok(connected)
            })().map_err(|error| error.to_string());
            let _ = sender.send(JobResult::MachineRetry {
                generation,
                machine_id,
                result,
            });
            let _ = wake.try_send(());
        });
        Ok(())
    }
}

pub(super) fn diagnostic(profile: &Value) -> Option<String> {
    let why = profile["why"].as_str().filter(|why| !why.is_empty());
    let name = profile["name"].as_str()?;
    let guidance = match profile["state"].as_str()? {
        "connected" | "connecting" | "retrying" => return None,
        "needs_login" => {
            format!("Authentication required. Run `hc machine login {name}`, then retry.")
        }
        "needs_update" => {
            format!("Core update required. Run `hc machine update {name}`, then retry.")
        }
        "unreachable" => {
            return Some(
                why.unwrap_or("Core could not reach this machine.")
                    .to_owned(),
            )
        }
        state => format!("Core reports machine state: {state}."),
    };
    Some(match why {
        Some(why) => format!("{guidance}\n\n{why}"),
        None => guidance,
    })
}
