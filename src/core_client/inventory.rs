use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::protocol::Error;

#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub terminals: BTreeMap<String, Value>,
    pub seq: u64,
    origins: BTreeMap<String, Origin>,
    machine_names: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    retired_aliases: BTreeMap<String, String>,
    unresolved: BTreeSet<String>,
    machines_ready: bool,
    snapshot_required: bool,
}

#[derive(Clone, Debug)]
struct Origin {
    seq: u64,
    owner: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub terminals: bool,
    pub documents: bool,
    pub refresh_machines: bool,
    pub resubscribe: bool,
    pub restarted: Vec<String>,
    pub document: Option<DocumentChange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentChange {
    pub name: String,
    pub machine: Option<String>,
    pub version: u64,
}

#[derive(Clone, Debug)]
pub struct ListedTerminal {
    pub record: Value,
    pub machine_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TerminalListing {
    pub terminals: Vec<ListedTerminal>,
    pub warnings: Vec<String>,
}

impl TerminalListing {
    pub fn parse(response: &Value) -> Result<Self, Error> {
        let machines = match response.get("machines") {
            None => &[][..],
            Some(Value::Array(machines)) => machines.as_slice(),
            Some(_) => return Err(invalid("terminal list captured machines is not an array")),
        };
        let Profiles { names, aliases } = profiles(machines)?;
        let records = response
            .get("terminals")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("terminal list has no terminals array"))?;
        let mut addresses = BTreeSet::new();
        let mut terminals = Vec::new();
        for record in records {
            let (record, owner) = project_record(record, &names, &aliases, true)?;
            let address = record_address(&record)?;
            if address.contains('/') && owner.is_none() {
                return Err(invalid(format!(
                    "terminal list has no captured owner for {address}"
                )));
            }
            if !addresses.insert(address.to_owned()) {
                return Err(invalid(format!("terminal list repeats {address}")));
            }
            terminals.push(ListedTerminal {
                record,
                machine_name: owner.and_then(|id| names.get(&id).cloned()),
            });
        }
        let mut warnings = Vec::new();
        for machine in machines {
            let state = required_string(machine, "state")?;
            if state != "connected" {
                warnings.push(format!("{}: {state}", required_string(machine, "name")?));
            }
        }
        Ok(Self {
            terminals,
            warnings,
        })
    }
}

struct Profiles {
    names: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
}

fn profiles(machines: &[Value]) -> Result<Profiles, Error> {
    let mut names = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    for machine in machines {
        let id = required_string(machine, "id")?;
        let name = required_string(machine, "name")?;
        if names.insert(id.to_owned(), name.to_owned()).is_some()
            || aliases.insert(name.to_owned(), id.to_owned()).is_some()
        {
            return Err(invalid("machine list repeats a permanent id or name"));
        }
    }
    for id in names.keys() {
        if aliases.get(id).is_some_and(|owner| owner != id) {
            return Err(invalid(
                "a machine name shadows another machine's permanent id",
            ));
        }
    }
    Ok(Profiles { names, aliases })
}

impl Inventory {
    pub fn new(snapshot: &Value) -> Result<Self, Error> {
        let seq = sequence(snapshot)?;
        let records = snapshot
            .get("terminals")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("snapshot has no terminal array"))?;
        let mut terminals = BTreeMap::new();
        let mut origins = BTreeMap::new();
        let mut unresolved = BTreeSet::new();
        for record in records {
            let address = record_address(record)?.to_owned();
            if terminals.insert(address.clone(), record.clone()).is_some() {
                return Err(invalid(format!("snapshot repeats terminal {address}")));
            }
            if address.contains('/') {
                unresolved.insert(address.clone());
            }
            origins.insert(address, Origin { seq, owner: None });
        }
        Ok(Self {
            terminals,
            seq,
            origins,
            unresolved,
            ..Self::default()
        })
    }

    pub fn records(&self) -> impl Iterator<Item = &Value> {
        self.terminals
            .iter()
            .filter(|(address, _)| !self.unresolved.contains(*address))
            .map(|(_, record)| record)
    }

    pub fn get(&self, address: &str) -> Option<&Value> {
        let address = self.canonical_address(address);
        self.terminals
            .get(&address)
            .filter(|_| !self.unresolved.contains(&address))
    }

    pub fn live(&self) -> impl Iterator<Item = &Value> {
        self.records()
            .filter(|record| record.get("state").and_then(Value::as_str) == Some("live"))
    }

    pub fn insert_observed(&mut self, record: Value) -> Result<bool, Error> {
        let (record, owner) = project_record(
            &record,
            &self.machine_names,
            &self.aliases,
            self.machines_ready,
        )?;
        let address = record_address(&record)?.to_owned();
        if self.terminals.contains_key(&address) {
            return Ok(false);
        }
        if address.contains('/') && owner.is_none() {
            self.unresolved.insert(address.clone());
        }
        self.origins.insert(
            address.clone(),
            Origin {
                seq: self.seq,
                owner,
            },
        );
        self.terminals.insert(address, record);
        Ok(true)
    }

    pub fn canonical_address(&self, address: &str) -> String {
        project_address(
            address,
            &self.machine_names,
            &self.aliases,
            self.machines_ready,
        )
        .0
    }

    pub fn canonical_record(&self, record: &Value) -> Result<Value, Error> {
        project_record(
            record,
            &self.machine_names,
            &self.aliases,
            self.machines_ready,
        )
        .map(|(record, _)| record)
    }

    pub fn machine_name(&self, id: &str) -> Option<&str> {
        self.machine_names.get(id).map(String::as_str)
    }

    pub fn identities_ready(&self) -> bool {
        self.machines_ready && self.unresolved.is_empty()
    }

    pub fn set_machines(&mut self, machines: &[Value]) -> Result<Change, Error> {
        let Profiles { names, aliases } = profiles(machines)?;
        if self.machines_ready && names == self.machine_names && self.unresolved.is_empty() {
            return Ok(Change::default());
        }
        let rebound = self.snapshot_required
            || aliases.iter().any(|(name, id)| {
                self.aliases
                    .get(name)
                    .or_else(|| self.retired_aliases.get(name))
                    .is_some_and(|old| old != id)
            });
        for (name, id) in &self.aliases {
            if aliases.get(name) != Some(id) {
                self.retired_aliases.insert(name.clone(), id.clone());
            }
        }
        let mut terminals = BTreeMap::new();
        let mut origins = BTreeMap::<String, Origin>::new();
        let mut unresolved = BTreeSet::new();
        let mut conflict = false;
        for (address, record) in &self.terminals {
            let origin = self
                .origins
                .get(address)
                .ok_or_else(|| invalid("terminal has no observed revision"))?;
            let (record, owner) = match &origin.owner {
                Some(owner) if !names.contains_key(owner) => continue,
                Some(owner) => (record.clone(), Some(owner.clone())),
                None if rebound => (record.clone(), None),
                None => project_record(record, &names, &aliases, true)?,
            };
            let projected = record_address(&record)?.to_owned();
            if projected.contains('/') && owner.is_none() {
                unresolved.insert(projected.clone());
            }
            match origins.get(&projected) {
                Some(previous) if previous.seq > origin.seq => continue,
                Some(previous) if previous.seq == origin.seq => {
                    if terminals.get(&projected) != Some(&record) {
                        conflict = true;
                        unresolved.insert(projected.clone());
                    }
                    continue;
                }
                _ => {}
            }
            terminals.insert(projected.clone(), record);
            origins.insert(
                projected,
                Origin {
                    seq: origin.seq,
                    owner,
                },
            );
        }
        self.machine_names = names;
        self.aliases = aliases;
        if conflict {
            self.machines_ready = false;
            self.snapshot_required = true;
            self.unresolved.extend(unresolved);
            return Ok(Change {
                resubscribe: true,
                ..Change::default()
            });
        }
        let changed = terminals != self.terminals;
        self.terminals = terminals;
        self.origins = origins;
        self.unresolved = unresolved;
        self.machines_ready = !rebound;
        self.snapshot_required = rebound;
        Ok(Change {
            terminals: changed,
            resubscribe: rebound || !self.unresolved.is_empty(),
            ..Change::default()
        })
    }

    pub fn apply(&mut self, notification: &Value) -> Result<Change, Error> {
        let method = notification
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("notification has no method"))?;
        if !matches!(method, "event" | "missed" | "heartbeat") {
            return Ok(Change::default());
        }
        let params = notification
            .get("params")
            .ok_or_else(|| invalid("notification has no params"))?;
        let seq = sequence(params)?;
        if method == "missed" {
            if seq < self.seq {
                return Ok(Change::default());
            }
            let mut replacement = Self::new(params)?;
            let changed = replacement.terminals != self.terminals;
            replacement.machine_names = self.machine_names.clone();
            replacement.aliases = self.aliases.clone();
            *self = replacement;
            return Ok(Change {
                terminals: changed,
                documents: true,
                refresh_machines: true,
                ..Change::default()
            });
        }
        if seq <= self.seq {
            return Ok(Change::default());
        }
        if method == "heartbeat" || seq != self.seq.saturating_add(1) {
            return Ok(Change {
                resubscribe: true,
                ..Change::default()
            });
        }
        let kind = params
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("event has no type"))?;
        let mut change = Change::default();
        if kind == "doc" {
            let data = params
                .get("data")
                .ok_or_else(|| invalid("document event has no data"))?;
            let machine = match params.get("machine") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| invalid("document event has an invalid machine"))?
                        .to_owned(),
                ),
            };
            change.documents = true;
            let canonical_machine = machine.as_deref().and_then(|machine| {
                machine_owner(
                    machine,
                    &self.machine_names,
                    &self.aliases,
                    self.machines_ready,
                )
            });
            let document = DocumentChange {
                name: required_string(data, "name")?.to_owned(),
                machine: canonical_machine,
                version: data
                    .get("version")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| invalid("document event has no version"))?,
            };
            if machine.is_some() && document.machine.is_none() {
                change.refresh_machines = true;
            } else {
                change.document = Some(document);
            }
        } else if matches!(
            kind,
            "created"
                | "restarted"
                | "removed"
                | "lost"
                | "finished"
                | "status"
                | "turn"
                | "title"
                | "labels"
                | "size"
                | "foreground"
                | "keep"
        ) {
            let raw_address = required_string(params, "terminal")?;
            let (address, owner) = project_address(
                raw_address,
                &self.machine_names,
                &self.aliases,
                self.machines_ready,
            );
            if address.contains('/') && owner.is_none() {
                change.refresh_machines = true;
            }
            let data = params
                .get("data")
                .ok_or_else(|| invalid("terminal event has no data"))?;
            let whole = match kind {
                "created" | "restarted" => Some(data),
                "labels" => data.get("record"),
                _ => None,
            };
            if let Some(record) = whole {
                let (record, owner) = project_record(
                    record,
                    &self.machine_names,
                    &self.aliases,
                    self.machines_ready,
                )?;
                if record_address(&record)? != address {
                    return Err(invalid("event terminal differs from its record address"));
                }
                change.terminals = self.terminals.get(&address) != Some(&record);
                self.terminals.insert(address.clone(), record);
                if address.contains('/') && owner.is_none() {
                    self.unresolved.insert(address.clone());
                } else {
                    self.unresolved.remove(&address);
                }
                self.origins.insert(address.clone(), Origin { seq, owner });
                if kind == "restarted" {
                    change.restarted.push(address);
                }
            } else if kind == "removed" {
                change.terminals = self.terminals.remove(&address).is_some();
                self.origins.remove(&address);
                self.unresolved.remove(&address);
            } else if let Some(record) = self.terminals.get_mut(&address) {
                let origin = self
                    .origins
                    .get_mut(&address)
                    .ok_or_else(|| invalid("terminal has no observed revision"))?;
                change.terminals = update_record(record, kind, data)?;
                origin.seq = seq;
            } else {
                return Ok(Change {
                    resubscribe: true,
                    ..change
                });
            }
        }
        self.seq = seq;
        Ok(change)
    }
}

fn machine_owner(
    prefix: &str,
    names: &BTreeMap<String, String>,
    aliases: &BTreeMap<String, String>,
    ready: bool,
) -> Option<String> {
    if names.contains_key(prefix) {
        return Some(prefix.to_owned());
    }
    ready.then(|| aliases.get(prefix).cloned()).flatten()
}

fn project_address(
    address: &str,
    names: &BTreeMap<String, String>,
    aliases: &BTreeMap<String, String>,
    ready: bool,
) -> (String, Option<String>) {
    let Some((prefix, local)) = address.split_once('/') else {
        return (address.to_owned(), None);
    };
    match machine_owner(prefix, names, aliases, ready) {
        Some(owner) => (format!("{owner}/{local}"), Some(owner)),
        None => (address.to_owned(), None),
    }
}

fn project_record(
    record: &Value,
    names: &BTreeMap<String, String>,
    aliases: &BTreeMap<String, String>,
    ready: bool,
) -> Result<(Value, Option<String>), Error> {
    let (address, owner) = project_address(record_address(record)?, names, aliases, ready);
    let mut record = record.clone();
    let object = record
        .as_object_mut()
        .ok_or_else(|| invalid("terminal record is not an object"))?;
    object.insert("address".into(), address.into());
    if let Some(Value::String(parent)) = object.get_mut("parent") {
        *parent = project_address(parent, names, aliases, ready).0;
    }
    Ok((record, owner))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Protocol(format!("invalid core event: {}", message.into()))
}

fn sequence(value: &Value) -> Result<u64, Error> {
    value
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("missing sequence number"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid(format!("missing {field}")))
}

fn record_address(record: &Value) -> Result<&str, Error> {
    required_string(record, "state")?;
    required_string(record, "address")
}

fn put(record: &mut Map<String, Value>, key: &str, value: Value) -> bool {
    if record.get(key) == Some(&value) {
        return false;
    }
    record.insert(key.to_owned(), value);
    true
}

fn update_record(record: &mut Value, kind: &str, data: &Value) -> Result<bool, Error> {
    let fields: &[&str] = match kind {
        "lost" | "finished" => &["reason", "exit_code", "signal", "finished_ms"],
        "status" => &["status", "records", "reported"],
        "turn" => &["turn"],
        "title" => &["title"],
        "labels" => &["labels"],
        "size" => &["cols", "rows"],
        "keep" => &["keep"],
        "foreground" => &[],
        _ => return Ok(false),
    };
    let updates: Vec<_> = fields
        .iter()
        .map(|field| {
            data.get(*field)
                .cloned()
                .map(|value| (*field, value))
                .ok_or_else(|| invalid(format!("{kind} event has no {field}")))
        })
        .collect::<Result<_, _>>()?;
    let object = record
        .as_object_mut()
        .ok_or_else(|| invalid("terminal record is not an object"))?;
    let mut changed = false;
    for (field, value) in updates {
        changed |= put(object, field, value);
    }
    if matches!(kind, "lost" | "finished") {
        changed |= put(
            object,
            "state",
            if kind == "lost" { "live" } else { "finished" }.into(),
        );
        if kind == "lost" {
            changed |= put(object, "pid", Value::Null);
        }
        changed |= object.remove("foreground").is_some();
    }
    if kind == "foreground" {
        changed |= put(object, "foreground", data.clone());
    }
    Ok(changed)
}

#[cfg(test)]
#[path = "inventory/tests.rs"]
mod tests;
