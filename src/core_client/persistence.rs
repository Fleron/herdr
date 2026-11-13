use std::fmt;
use std::io;

use serde_json::{json, Value};

use crate::connection::{self, Client};
use crate::model::{Document, ModelError, Operation};

pub const DOCUMENT_NAME: &str = "herdr-tui";

#[derive(Debug)]
pub enum Error {
    Core(connection::Error),
    Model(ModelError),
    InvalidReply(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(error) => error.fmt(f),
            Self::Model(error) => error.fmt(f),
            Self::InvalidReply(message) => write!(f, "invalid core document reply: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(error) => Some(error),
            Self::Model(error) => Some(error),
            Self::InvalidReply(_) => None,
        }
    }
}

impl From<connection::Error> for Error {
    fn from(error: connection::Error) -> Self {
        Self::Core(error)
    }
}

impl From<ModelError> for Error {
    fn from(error: ModelError) -> Self {
        Self::Model(error)
    }
}

impl From<Error> for io::Error {
    fn from(error: Error) -> Self {
        Self::other(error)
    }
}

pub struct LayoutStore {
    on: Option<String>,
    version: u64,
    document: Document,
    pending: Vec<Operation>,
}

impl LayoutStore {
    pub fn empty(on: Option<String>) -> Self {
        Self {
            on,
            version: 0,
            document: Document::default(),
            pending: Vec::new(),
        }
    }

    pub fn load(client: &Client, on: Option<String>) -> Result<Self, Error> {
        let mut store = Self::empty(on);
        store.refresh(client)?;
        Ok(store)
    }

    pub fn on(&self) -> Option<&str> {
        self.on.as_deref()
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn apply(&mut self, operation: Operation) -> Result<bool, Error> {
        if let Operation::AddWorkspace(workspace) = &operation {
            if workspace.machine.as_deref() != self.on() {
                return Err(ModelError(
                    "workspace belongs to a different layout machine".to_string(),
                )
                .into());
            }
        }
        let changed = self.document.apply(&operation)?;
        if changed {
            self.pending.push(operation);
        }
        Ok(changed)
    }

    pub fn refresh(&mut self, client: &Client) -> Result<(), Error> {
        let (document, version) = self.fetch(client)?;
        self.rebase(document, version)
    }

    pub fn discard_pending(&mut self, client: &Client) -> Result<(), Error> {
        let (mut document, version) = self.fetch(client)?;
        self.set_machine(&mut document);
        self.document = document;
        self.version = version;
        self.pending.clear();
        Ok(())
    }

    pub fn save(&mut self, client: &Client) -> Result<bool, Error> {
        if !self.pending() {
            return Ok(true);
        }
        let value = serde_json::to_value(&self.document)
            .map_err(|error| ModelError(format!("cannot encode TUI layout: {error}")))?;
        let result = client.request(
            "doc.set",
            json!({
                "name": DOCUMENT_NAME,
                "value": value,
                "version": self.version,
                "on": self.on,
            }),
        );
        match result {
            Ok(reply) => {
                let version = reply
                    .get("version")
                    .and_then(Value::as_u64)
                    .ok_or(Error::InvalidReply("missing version"))?;
                if version <= self.version {
                    return Err(Error::InvalidReply("version did not advance"));
                }
                self.version = version;
                self.pending.clear();
                Ok(true)
            }
            Err(error) if error.kind() == Some("conflict") => {
                self.refresh(client)?;
                Ok(!self.pending())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn fetch(&self, client: &Client) -> Result<(Document, u64), Error> {
        let reply = client.request("doc.get", json!({"name": DOCUMENT_NAME, "on": self.on}))?;
        let value = reply
            .get("value")
            .ok_or(Error::InvalidReply("missing value"))?;
        let version = reply
            .get("version")
            .and_then(Value::as_u64)
            .ok_or(Error::InvalidReply("missing version"))?;
        Ok((Document::from_value(value.clone())?, version))
    }

    fn set_machine(&self, document: &mut Document) {
        for workspace in &mut document.workspaces {
            workspace.machine = self.on.clone();
        }
    }

    fn rebase(&mut self, mut baseline: Document, version: u64) -> Result<(), Error> {
        self.set_machine(&mut baseline);
        let mut document = baseline.clone();
        for operation in &self.pending {
            document.apply(operation)?;
        }
        if document == baseline {
            self.pending.clear();
        }
        self.document = document;
        self.version = version;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Tab, Workspace};

    #[test]
    fn conflicting_clients_preserve_both_sets_of_changes_and_pending_save() {
        let mut workspace = Workspace::new("repo", "/repo", None);
        workspace.tabs.push(Tab::new("agent", "t1"));
        let id = workspace.id.clone();
        let original = Document {
            workspaces: vec![workspace],
            ..Document::default()
        };
        let mut store = LayoutStore::empty(None);
        store.rebase(original.clone(), 1).unwrap();
        let mine = Tab::new("tests", "t2");
        store
            .apply(Operation::AddTab {
                workspace: id.clone(),
                tab: mine.clone(),
            })
            .unwrap();
        let mut remote = original;
        let theirs = Tab::new("server", "t3");
        remote
            .apply(&Operation::AddTab {
                workspace: id.clone(),
                tab: theirs.clone(),
            })
            .unwrap();
        store.rebase(remote, 2).unwrap();
        assert_eq!(store.document().workspace(&id).unwrap().tabs.len(), 3);
        assert!(store
            .document()
            .workspace(&id)
            .unwrap()
            .tab(&mine.id)
            .is_some());
        assert!(store
            .document()
            .workspace(&id)
            .unwrap()
            .tab(&theirs.id)
            .is_some());
        assert!(store.pending());
        assert_eq!(store.version(), 2);
    }

    #[test]
    fn failed_replay_preserves_local_edits_and_owner_context() {
        let on = Some("box".to_string());
        let mut store = LayoutStore::empty(on.clone());
        let mut workspace = Workspace::new("repo", "/repo", on);
        workspace.tabs.push(Tab::new("agent", "t1"));
        let id = workspace.id.clone();
        store.apply(Operation::AddWorkspace(workspace)).unwrap();
        let mine = store.document().clone();
        let mut baseline = mine.clone();
        baseline.workspaces[0].machine = None;
        store.rebase(baseline, 1).unwrap();
        assert!(!store.pending());
        store
            .apply(Operation::RenameWorkspace {
                workspace: id.clone(),
                name: "renamed".to_string(),
            })
            .unwrap();
        let edited = store.document().clone();
        assert!(store.rebase(Document::default(), 2).is_err());
        assert_eq!(store.document(), &edited);
        assert_eq!(store.on(), Some("box"));
        assert!(store.pending());
        assert_eq!(store.version(), 1);
        assert_eq!(
            store.document().workspace(&id).unwrap().tabs[0].panes[0].terminal,
            "t1"
        );
    }

    #[test]
    #[cfg(unix)]
    fn real_core_conflict_replays_both_clients_and_failed_save_keeps_edits() {
        let session = crate::test_support::CoreSession::new();
        let first_client = session.client();
        let second_client = session.client();
        let _first_events = first_client.subscribe().unwrap();
        let _second_events = second_client.subscribe().unwrap();
        let mut first = LayoutStore::load(&first_client, None).unwrap();
        let mut second = LayoutStore::load(&second_client, None).unwrap();
        let first_workspace = Workspace::new("first", "/first", None);
        let second_workspace = Workspace::new("second", "/second", None);
        let first_id = first_workspace.id.clone();
        let second_id = second_workspace.id.clone();
        first
            .apply(Operation::AddWorkspace(first_workspace))
            .unwrap();
        second
            .apply(Operation::AddWorkspace(second_workspace))
            .unwrap();
        assert!(first.save(&first_client).unwrap());
        assert!(!second.save(&second_client).unwrap());
        assert!(second.pending());
        assert!(second.document().workspace(&first_id).is_some());
        assert!(second.document().workspace(&second_id).is_some());
        assert!(second.save(&second_client).unwrap());
        first.refresh(&first_client).unwrap();
        assert_eq!(first.document().workspaces.len(), 2);
        let mut too_large = Workspace::new("unsaved", "/large", None);
        too_large.extra.insert(
            "payload".to_string(),
            Value::String("x".repeat(1024 * 1024)),
        );
        let unsaved_id = too_large.id.clone();
        first.apply(Operation::AddWorkspace(too_large)).unwrap();
        assert!(first.save(&first_client).is_err());
        assert!(first.pending());
        assert!(first.document().workspace(&unsaved_id).is_some());
        second.refresh(&second_client).unwrap();
        assert!(second.document().workspace(&unsaved_id).is_none());
        drop(first_client);
        drop(second_client);
        session.finish();
    }
}
