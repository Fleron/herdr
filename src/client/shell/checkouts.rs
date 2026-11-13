use super::*;
use crate::client::ui_model::ClientShellAvailableCheckout;
use std::sync::Arc;

#[derive(Clone)]
pub(super) enum SidebarTreeEntry {
    Workspace(WorkspaceEntry),
    Checkout {
        checkout: Arc<ClientShellAvailableCheckout>,
        indented: bool,
        last_child: bool,
        group_key: Option<String>,
    },
}

#[derive(Clone, Copy)]
enum Node {
    Workspace(usize),
    Checkout(usize),
}

impl Node {
    fn worktree(
        self,
        snapshot: &ClientShellSnapshot,
    ) -> Option<&crate::client::ui_model::ClientShellWorktree> {
        match self {
            Self::Workspace(index) => snapshot.workspaces.get(index)?.worktree.as_ref(),
            Self::Checkout(index) => snapshot.available_checkouts.get(index)?.worktree.as_ref(),
        }
    }

    fn entry(
        self,
        snapshot: &ClientShellSnapshot,
        indented: bool,
        last_child: bool,
        group_key: Option<String>,
    ) -> SidebarTreeEntry {
        match self {
            Self::Workspace(index) => SidebarTreeEntry::Workspace(WorkspaceEntry {
                index,
                indented,
                last_child,
            }),
            Self::Checkout(index) => SidebarTreeEntry::Checkout {
                checkout: Arc::clone(&snapshot.available_checkouts[index]),
                indented,
                last_child,
                group_key,
            },
        }
    }
}

pub(super) fn tree_entries(
    snapshot: &ClientShellSnapshot,
    collapsed: &HashSet<String>,
) -> Vec<SidebarTreeEntry> {
    let nodes = (0..snapshot.workspaces.len())
        .map(Node::Workspace)
        .chain((0..snapshot.available_checkouts.len()).map(Node::Checkout))
        .collect::<Vec<_>>();
    let mut members = HashMap::<&str, Vec<Node>>::new();
    for node in &nodes {
        if let Some(tree) = node.worktree(snapshot) {
            members.entry(&tree.key).or_default().push(*node);
        }
    }
    let grouped = members
        .iter()
        .filter(|(_, nodes)| {
            nodes.iter().any(|node| {
                node.worktree(snapshot)
                    .is_some_and(|tree| tree.is_linked_worktree)
            }) && nodes.iter().any(|node| {
                node.worktree(snapshot)
                    .is_some_and(|tree| !tree.is_linked_worktree)
            })
        })
        .map(|(key, _)| *key)
        .collect::<HashSet<_>>();
    let mut emitted = HashSet::new();
    let mut entries = Vec::new();
    for node in nodes {
        let Some(tree) = node
            .worktree(snapshot)
            .filter(|tree| grouped.contains(tree.key.as_str()))
        else {
            entries.push(node.entry(snapshot, false, false, None));
            continue;
        };
        if !emitted.insert(tree.key.as_str()) {
            continue;
        }
        let group = &members[tree.key.as_str()];
        for parent in group.iter().copied().filter(|node| {
            node.worktree(snapshot)
                .is_some_and(|tree| !tree.is_linked_worktree)
        }) {
            entries.push(parent.entry(snapshot, false, false, Some(tree.key.clone())));
        }
        if collapsed.contains(&tree.key) {
            continue;
        }
        let children = group
            .iter()
            .copied()
            .filter(|node| {
                node.worktree(snapshot)
                    .is_some_and(|tree| tree.is_linked_worktree)
            })
            .collect::<Vec<_>>();
        for (index, child) in children.iter().enumerate() {
            entries.push(child.entry(snapshot, true, index + 1 == children.len(), None));
        }
    }
    entries
}

pub(super) fn parent_group_key(snapshot: &ClientShellSnapshot, index: usize) -> Option<String> {
    let tree = snapshot.workspaces.get(index)?.worktree.as_ref()?;
    if tree.is_linked_worktree {
        return None;
    }
    let linked = snapshot
        .workspaces
        .iter()
        .filter_map(|workspace| workspace.worktree.as_ref())
        .chain(
            snapshot
                .available_checkouts
                .iter()
                .filter_map(|checkout| checkout.worktree.as_ref()),
        )
        .any(|candidate| candidate.key == tree.key && candidate.is_linked_worktree);
    linked.then(|| tree.key.clone())
}

pub(super) fn checkout_rows(
    checkout: &ClientShellAvailableCheckout,
    indented: bool,
    config: &SpacesSidebarConfig,
) -> Vec<Vec<crate::client::ui::ResolvedToken>> {
    let label = if indented {
        checkout
            .branch
            .as_deref()
            .map(|branch| branch.strip_prefix("worktree/").unwrap_or(branch))
            .unwrap_or(&checkout.label)
    } else {
        &checkout.label
    };
    crate::client::ui::sidebar_space_rows(
        config,
        crate::client::ui::SpaceTokenContext {
            workspace: label,
            branch: checkout.branch.as_deref(),
            state_text: status_text(checkout.agent_status),
            ahead_behind: checkout.git_ahead_behind,
            tokens: &HashMap::new(),
            suppress_git_details: indented,
        },
    )
}

impl ClientShellState {
    pub(super) fn open_available_checkout(
        &mut self,
        checkout: Arc<ClientShellAvailableCheckout>,
        outcome: &mut ClientShellInput,
    ) {
        match checkout.terminals.as_slice() {
            [] => outcome.actions.push(ClientShellAction::Core(
                crate::client::ui_action::CoreAction::OpenCheckout(checkout.target()),
            )),
            [terminal] => outcome.actions.push(ClientShellAction::AttachTerminal {
                target: terminal.target.clone(),
            }),
            terminals => self
                .open_core_terminal_picker(terminals.to_vec(), format!("open {}", checkout.label)),
        }
        outcome.repaint = true;
    }

    pub(super) fn handle_available_checkout_click(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if !matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
        ) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        let Some(hit) = self
            .hits
            .available_checkouts
            .iter()
            .find(|hit| super::contains(hit.rect, point))
            .cloned()
        else {
            return false;
        };
        if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
            self.open_checkout_context_menu(hit.checkout, mouse.column, mouse.row);
        } else if let Some((rect, key)) = hit
            .group_toggle
            .filter(|(rect, _)| super::contains(*rect, point))
        {
            let _ = rect;
            let groups = if hit.endpoint_id.is_local() {
                &mut self.collapsed_groups
            } else {
                self.remote_collapsed_groups
                    .entry(hit.endpoint_id)
                    .or_default()
            };
            if !groups.remove(&key) {
                groups.insert(key);
            }
            self.persist_chrome_preferences(outcome);
        } else {
            self.open_available_checkout(hit.checkout, outcome);
        }
        outcome.repaint = true;
        true
    }
}
