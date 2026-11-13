use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Direction;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::layout::{Node, PaneId, TileLayout};

pub const DOCUMENT_SCHEMA: u32 = 1;

pub fn new_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{now:x}-{:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn schema() -> u32 {
    DOCUMENT_SCHEMA
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    #[serde(default = "schema")]
    pub schema: u32,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            schema: DOCUMENT_SCHEMA,
            workspaces: Vec::new(),
            extra: Map::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub folder: String,
    #[serde(skip)]
    pub machine: Option<String>,
    #[serde(default)]
    pub tabs: Vec<Tab>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Workspace {
    pub fn new(
        name: impl Into<String>,
        folder: impl Into<String>,
        machine: Option<String>,
    ) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            folder: folder.into(),
            machine,
            tabs: Vec::new(),
            extra: Map::new(),
        }
    }

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub name: String,
    pub layout: LayoutNode,
    pub panes: Vec<Pane>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Tab {
    pub fn new(name: impl Into<String>, terminal: impl Into<String>) -> Self {
        Self::with_pane(name, Pane::new(terminal))
    }

    pub fn with_pane(name: impl Into<String>, pane: Pane) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            layout: LayoutNode::pane(pane.id.clone()),
            panes: vec![pane],
            extra: Map::new(),
        }
    }

    pub fn pane(&self, id: &str) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    pub fn tile_layout(&self, focused: Option<&str>) -> Result<ViewLayout, ModelError> {
        let panes: HashMap<PaneId, String> = self
            .panes
            .iter()
            .enumerate()
            .map(|(index, pane)| (PaneId::from_raw(index as u32 + 1), pane.id.clone()))
            .collect();
        let pane_ids: HashMap<&str, PaneId> = panes
            .iter()
            .map(|(id, name)| (name.as_str(), *id))
            .collect();
        let root = self.layout.tile_node(&pane_ids)?;
        let first = self
            .panes
            .first()
            .and_then(|pane| pane_ids.get(pane.id.as_str()))
            .copied();
        let focus = focused
            .and_then(|id| pane_ids.get(id))
            .copied()
            .or(first)
            .ok_or_else(|| ModelError(format!("tab {} has no panes", self.id)))?;
        Ok(ViewLayout {
            layout: TileLayout::from_saved(root, focus),
            panes,
        })
    }
}

pub struct ViewLayout {
    pub layout: TileLayout,
    pub panes: HashMap<PaneId, String>,
}

impl ViewLayout {
    pub fn pane_id(&self, stable_id: &str) -> Option<PaneId> {
        self.panes
            .iter()
            .find_map(|(id, name)| (name == stable_id).then_some(*id))
    }

    pub fn stable_id(&self, id: PaneId) -> Option<&str> {
        self.panes.get(&id).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub id: String,
    pub terminal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Pane {
    pub fn new(terminal: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            terminal: terminal.into(),
            name: None,
            extra: Map::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    pub fn direction(self) -> Direction {
        match self {
            Self::Horizontal => Direction::Horizontal,
            Self::Vertical => Direction::Vertical,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNode {
    Pane {
        id: String,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    Split {
        id: String,
        axis: Axis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
}

impl LayoutNode {
    pub fn pane(id: String) -> Self {
        Self::Pane {
            id,
            extra: Map::new(),
        }
    }

    pub fn with_swapped_panes(&self, first: &str, second: &str) -> Result<Self, ModelError> {
        let mut layout = self.clone();
        let found = layout.swap_ids(first, second);
        if found != 3 && !(first == second && found == 1) {
            return Err(ModelError("a pane to swap no longer exists".to_string()));
        }
        Ok(layout)
    }

    fn swap_ids(&mut self, first: &str, second: &str) -> u8 {
        match self {
            Self::Pane { id, .. } if id == first => {
                *id = second.to_string();
                1
            }
            Self::Pane { id, .. } if id == second => {
                *id = first.to_string();
                2
            }
            Self::Pane { .. } => 0,
            Self::Split {
                first: left,
                second: right,
                ..
            } => left.swap_ids(first, second) | right.swap_ids(first, second),
        }
    }

    fn tile_node(&self, panes: &HashMap<&str, PaneId>) -> Result<Node, ModelError> {
        match self {
            Self::Pane { id, .. } => panes
                .get(id.as_str())
                .copied()
                .map(Node::Pane)
                .ok_or_else(|| ModelError(format!("layout refers to missing pane {id}"))),
            Self::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => Ok(Node::Split {
                direction: axis.direction(),
                ratio: *ratio,
                first: Box::new(first.tile_node(panes)?),
                second: Box::new(second.tile_node(panes)?),
            }),
        }
    }

    pub fn split_at(&self, path: &[bool]) -> Option<&str> {
        match self {
            Self::Split {
                id, first, second, ..
            } => match path.split_first() {
                None => Some(id),
                Some((false, rest)) => first.split_at(rest),
                Some((true, rest)) => second.split_at(rest),
            },
            Self::Pane { .. } => None,
        }
    }

    fn split(&mut self, target: &str, pane: &str, split: &str, axis: Axis) -> bool {
        match self {
            Self::Pane { id, .. } if id == target => {
                let first = self.clone();
                *self = Self::Split {
                    id: split.to_string(),
                    axis,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(Self::pane(pane.to_string())),
                    extra: Map::new(),
                };
                true
            }
            Self::Split { first, second, .. } => {
                first.split(target, pane, split, axis) || second.split(target, pane, split, axis)
            }
            _ => false,
        }
    }

    fn without(&self, pane: &str) -> Option<Self> {
        match self {
            Self::Pane { id, .. } => (id != pane).then(|| self.clone()),
            Self::Split {
                id,
                axis,
                ratio,
                first,
                second,
                extra,
            } => match (first.without(pane), second.without(pane)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    id: id.clone(),
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                    extra: extra.clone(),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            },
        }
    }

    fn set_ratio(&mut self, target: &str, value: f32) -> bool {
        match self {
            Self::Split { id, ratio, .. } if id == target => {
                *ratio = value;
                true
            }
            Self::Split { first, second, .. } => {
                first.set_ratio(target, value) || second.set_ratio(target, value)
            }
            Self::Pane { .. } => false,
        }
    }

    fn validate(
        &self,
        ids: &mut HashSet<String>,
        panes: &mut HashSet<String>,
    ) -> Result<(), ModelError> {
        match self {
            Self::Pane { id, .. } => {
                if !panes.insert(id.clone()) {
                    return Err(ModelError(format!("pane {id} appears twice in a layout")));
                }
            }
            Self::Split {
                id,
                ratio,
                first,
                second,
                ..
            } => {
                unique_id(ids, id)?;
                if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                    return Err(ModelError(format!("split {id} has an invalid ratio")));
                }
                first.validate(ids, panes)?;
                second.validate(ids, panes)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum Operation {
    AddWorkspace(Workspace),
    AddTab {
        workspace: String,
        tab: Tab,
    },
    SplitPane {
        workspace: String,
        tab: String,
        target: String,
        pane: Pane,
        split: String,
        axis: Axis,
    },
    RenameWorkspace {
        workspace: String,
        name: String,
    },
    RenameTab {
        workspace: String,
        tab: String,
        name: String,
    },
    RenamePane {
        workspace: String,
        tab: String,
        pane: String,
        name: Option<String>,
    },
    CloseWorkspace {
        workspace: String,
    },
    CloseTab {
        workspace: String,
        tab: String,
    },
    ClosePane {
        workspace: String,
        tab: String,
        pane: String,
    },
    SetRatio {
        workspace: String,
        tab: String,
        split: String,
        ratio: f32,
    },
    SetTabLayout {
        workspace: String,
        tab: String,
        expected: LayoutNode,
        layout: LayoutNode,
    },
    MoveTab {
        workspace: String,
        tab: String,
        before: Option<String>,
    },
    MoveWorkspaces {
        workspaces: Vec<String>,
        before: Option<String>,
    },
    SetPaneInput {
        workspace: String,
        tab: String,
        pane: String,
        passthrough: bool,
    },
    InsertPane {
        workspace: String,
        tab: String,
        pane: Pane,
        expected: LayoutNode,
        layout: LayoutNode,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelError(pub String);

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ModelError {}

fn unique_id(ids: &mut HashSet<String>, id: &str) -> Result<(), ModelError> {
    if id.is_empty() || !ids.insert(id.to_string()) {
        return Err(ModelError(format!("empty or duplicate layout id {id:?}")));
    }
    Ok(())
}

impl Document {
    pub fn from_value(value: Value) -> Result<Self, ModelError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        let document: Self = serde_json::from_value(value)
            .map_err(|error| ModelError(format!("invalid TUI layout: {error}")))?;
        document.validate()?;
        Ok(document)
    }

    pub fn workspace(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.schema != DOCUMENT_SCHEMA {
            return Err(ModelError(format!(
                "unsupported TUI layout schema {}",
                self.schema
            )));
        }
        let mut ids = HashSet::new();
        for workspace in &self.workspaces {
            unique_id(&mut ids, &workspace.id)?;
            for tab in &workspace.tabs {
                unique_id(&mut ids, &tab.id)?;
                if tab.panes.is_empty() {
                    return Err(ModelError(format!("tab {} has no panes", tab.id)));
                }
                let mut declared = HashSet::new();
                for pane in &tab.panes {
                    unique_id(&mut ids, &pane.id)?;
                    declared.insert(pane.id.clone());
                    if pane.terminal.is_empty() || pane.terminal.contains('/') {
                        return Err(ModelError(format!(
                            "pane {} needs an owner-local terminal address",
                            pane.id
                        )));
                    }
                }
                let mut rendered = HashSet::new();
                tab.layout.validate(&mut ids, &mut rendered)?;
                if rendered != declared {
                    return Err(ModelError(format!(
                        "tab {} layout does not match its panes",
                        tab.id
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn apply(&mut self, operation: &Operation) -> Result<bool, ModelError> {
        let mut next = self.clone();
        next.apply_inner(operation)?;
        next.validate()?;
        if *self == next {
            return Ok(false);
        }
        *self = next;
        Ok(true)
    }

    fn workspace_mut(&mut self, id: &str) -> Result<&mut Workspace, ModelError> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .ok_or_else(|| ModelError(format!("workspace {id} no longer exists")))
    }

    fn tab_mut(&mut self, workspace: &str, tab: &str) -> Result<&mut Tab, ModelError> {
        self.workspace_mut(workspace)?
            .tabs
            .iter_mut()
            .find(|entry| entry.id == tab)
            .ok_or_else(|| ModelError(format!("tab {tab} no longer exists")))
    }

    fn apply_inner(&mut self, operation: &Operation) -> Result<(), ModelError> {
        match operation {
            Operation::AddWorkspace(workspace) => {
                if self.workspace(&workspace.id).is_none() {
                    self.workspaces.push(workspace.clone());
                }
            }
            Operation::AddTab { workspace, tab } => {
                let workspace = self.workspace_mut(workspace)?;
                if workspace.tab(&tab.id).is_none() {
                    workspace.tabs.push(tab.clone());
                }
            }
            Operation::SplitPane {
                workspace,
                tab,
                target,
                pane,
                split,
                axis,
            } => {
                let tab = self.tab_mut(workspace, tab)?;
                if tab.pane(&pane.id).is_some() {
                    return Ok(());
                }
                if !tab.layout.split(target, &pane.id, split, *axis) {
                    return Err(ModelError(format!("pane {target} no longer exists")));
                }
                tab.panes.push(pane.clone());
            }
            Operation::RenameWorkspace { workspace, name } => {
                let workspace = self.workspace_mut(workspace)?;
                workspace.name = name.clone();
                workspace
                    .extra
                    .insert("custom_name".into(), Value::Bool(true));
            }
            Operation::RenameTab {
                workspace,
                tab,
                name,
            } => {
                let tab = self.tab_mut(workspace, tab)?;
                tab.name = name.clone();
                tab.extra.insert("custom_name".into(), Value::Bool(true));
            }
            Operation::RenamePane {
                workspace,
                tab,
                pane,
                name,
            } => {
                let pane = self
                    .tab_mut(workspace, tab)?
                    .panes
                    .iter_mut()
                    .find(|entry| entry.id == *pane)
                    .ok_or_else(|| ModelError(format!("pane {pane} no longer exists")))?;
                pane.name = name.clone();
            }
            Operation::CloseWorkspace { workspace } => {
                self.workspaces.retain(|entry| entry.id != *workspace)
            }
            Operation::MoveWorkspaces { workspaces, before } => {
                let selected: HashSet<_> = workspaces.iter().map(String::as_str).collect();
                if selected.len() != workspaces.len() {
                    return Err(ModelError("a workspace move repeats an identifier".into()));
                }
                for id in workspaces {
                    if self.workspace(id).is_none() {
                        return Err(ModelError(format!("workspace {id} no longer exists")));
                    }
                }
                if before.as_deref().is_some_and(|id| selected.contains(id)) {
                    return Ok(());
                }
                let mut moved = Vec::new();
                for id in workspaces {
                    let index = self
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.id == *id)
                        .expect("workspace existence checked above");
                    moved.push(self.workspaces.remove(index));
                }
                let index = match before {
                    Some(id) => self
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.id == *id)
                        .ok_or_else(|| ModelError(format!("workspace {id} no longer exists")))?,
                    None => self.workspaces.len(),
                };
                self.workspaces.splice(index..index, moved);
            }
            Operation::SetPaneInput {
                workspace,
                tab,
                pane,
                passthrough,
            } => {
                let pane = self
                    .tab_mut(workspace, tab)?
                    .panes
                    .iter_mut()
                    .find(|entry| entry.id == *pane)
                    .ok_or_else(|| ModelError(format!("pane {pane} no longer exists")))?;
                pane.extra
                    .insert("right_click_passthrough".into(), Value::Bool(*passthrough));
            }
            Operation::InsertPane {
                workspace,
                tab,
                pane,
                expected,
                layout,
            } => {
                let tab = self.tab_mut(workspace, tab)?;
                if tab.pane(&pane.id).is_some() {
                    if tab.layout == *layout {
                        return Ok(());
                    }
                    return Err(ModelError(
                        "the new pane already exists with a different layout".into(),
                    ));
                }
                if tab.layout != *expected {
                    return Err(ModelError(
                        "the target layout changed before this pane could be inserted".into(),
                    ));
                }
                tab.panes.push(pane.clone());
                tab.layout = layout.clone();
            }
            Operation::CloseTab { workspace, tab } => {
                if let Some(workspace) = self
                    .workspaces
                    .iter_mut()
                    .find(|entry| entry.id == *workspace)
                {
                    workspace.tabs.retain(|entry| entry.id != *tab);
                }
            }
            Operation::ClosePane {
                workspace,
                tab,
                pane,
            } => {
                let Some(workspace) = self
                    .workspaces
                    .iter_mut()
                    .find(|entry| entry.id == *workspace)
                else {
                    return Ok(());
                };
                let Some(index) = workspace.tabs.iter().position(|entry| entry.id == *tab) else {
                    return Ok(());
                };
                match workspace.tabs[index].layout.without(pane) {
                    None => {
                        workspace.tabs.remove(index);
                    }
                    Some(layout) => {
                        workspace.tabs[index].layout = layout;
                        workspace.tabs[index]
                            .panes
                            .retain(|entry| entry.id != *pane);
                    }
                }
            }
            Operation::SetRatio {
                workspace,
                tab,
                split,
                ratio,
            } => {
                if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                    return Err(ModelError(
                        "split ratio must be between zero and one".to_string(),
                    ));
                }
                if !self
                    .tab_mut(workspace, tab)?
                    .layout
                    .set_ratio(split, *ratio)
                {
                    return Err(ModelError(format!("split {split} no longer exists")));
                }
            }
            Operation::SetTabLayout {
                workspace,
                tab,
                expected,
                layout,
            } => {
                let tab = self.tab_mut(workspace, tab)?;
                if tab.layout == *layout {
                    return Ok(());
                }
                if tab.layout != *expected {
                    return Err(ModelError(
                        "tab layout changed elsewhere; reload or discard this edit before retrying"
                            .to_string(),
                    ));
                }
                tab.layout = layout.clone();
            }
            Operation::MoveTab {
                workspace,
                tab,
                before,
            } => {
                let workspace = self.workspace_mut(workspace)?;
                let current = workspace
                    .tabs
                    .iter()
                    .position(|entry| entry.id == *tab)
                    .ok_or_else(|| ModelError(format!("tab {tab} no longer exists")))?;
                if before.as_deref() == Some(tab) {
                    return Ok(());
                }
                let destination = match before {
                    Some(before) => workspace
                        .tabs
                        .iter()
                        .position(|entry| entry.id == *before)
                        .ok_or_else(|| {
                            ModelError(format!("destination tab {before} no longer exists"))
                        })?,
                    None => workspace.tabs.len(),
                };
                let moved = workspace.tabs.remove(current);
                workspace.tabs.insert(
                    if current < destination {
                        destination - 1
                    } else {
                        destination
                    },
                    moved,
                );
            }
        }
        if let Operation::ClosePane { workspace, .. } | Operation::CloseTab { workspace, .. } =
            operation
        {
            self.workspaces
                .retain(|entry| entry.id != *workspace || !entry.tabs.is_empty());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ViewState {
    pub workspace: Option<String>,
    #[serde(default)]
    pub active_owner: Option<String>,
    #[serde(default)]
    pub owner_workspaces: BTreeMap<String, String>,
    #[serde(default)]
    pub tabs: BTreeMap<String, String>,
    #[serde(default)]
    pub panes: BTreeMap<String, String>,
    #[serde(default)]
    pub zoomed: BTreeSet<String>,
}

impl ViewState {
    pub fn active_workspace<'a>(&self, document: &'a Document) -> Option<&'a Workspace> {
        self.workspace
            .as_deref()
            .and_then(|id| document.workspace(id))
            .or_else(|| document.workspaces.first())
    }

    pub fn active_tab<'a>(&self, document: &'a Document) -> Option<&'a Tab> {
        let workspace = self.active_workspace(document)?;
        self.tabs
            .get(&workspace.id)
            .and_then(|id| workspace.tab(id))
            .or_else(|| workspace.tabs.first())
    }

    pub fn active_pane<'a>(&self, document: &'a Document) -> Option<&'a Pane> {
        let tab = self.active_tab(document)?;
        self.panes
            .get(&tab.id)
            .and_then(|id| tab.pane(id))
            .or_else(|| tab.panes.first())
    }

    pub fn repair(&mut self, document: &Document) {
        self.workspace = self
            .active_workspace(document)
            .map(|workspace| workspace.id.clone());
        self.tabs.retain(|workspace, tab| {
            document
                .workspace(workspace)
                .is_some_and(|entry| entry.tab(tab).is_some())
        });
        self.panes.retain(|tab, pane| {
            document
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .any(|entry| entry.id == *tab && entry.pane(pane).is_some())
        });
        self.zoomed.retain(|tab| {
            document
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .any(|entry| entry.id == *tab)
        });
        self.owner_workspaces
            .retain(|_, workspace| document.workspace(workspace).is_some());
    }

    pub fn focus_pane(&mut self, document: &Document, pane: &str) -> bool {
        for workspace in &document.workspaces {
            for tab in &workspace.tabs {
                if tab.pane(pane).is_some() {
                    self.workspace = Some(workspace.id.clone());
                    self.tabs.insert(workspace.id.clone(), tab.id.clone());
                    self.panes.insert(tab.id.clone(), pane.to_string());
                    return true;
                }
            }
        }
        false
    }
}

pub fn terminal_address(machine: Option<&str>, local: &str) -> String {
    machine.map_or_else(|| local.to_string(), |machine| format!("{machine}/{local}"))
}

pub fn local_terminal<'a>(machine: Option<&str>, address: &'a str) -> Result<&'a str, ModelError> {
    match (machine, address.split_once('/')) {
        (None, None) => Ok(address),
        (Some(expected), Some((actual, local))) if actual == expected && !local.contains('/') => {
            Ok(local)
        }
        _ => Err(ModelError(format!(
            "terminal {address:?} belongs to a different machine"
        ))),
    }
}

#[cfg(test)]
#[path = "model/tests.rs"]
mod tests;
