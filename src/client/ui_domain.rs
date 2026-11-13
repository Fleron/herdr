#[path = "ui_domain/agents.rs"]
pub(crate) mod agents;
pub use agents::*;
#[path = "ui_domain/commands.rs"]
pub(crate) mod commands;
pub use commands::*;
#[path = "ui_domain/common.rs"]
pub(crate) mod common;
pub use common::*;
#[path = "ui_domain/integrations.rs"]
pub(crate) mod integrations;
pub use integrations::*;
#[path = "ui_domain/panes.rs"]
pub(crate) mod panes;
pub use panes::*;
#[path = "ui_domain/response.rs"]
pub(crate) mod response;
pub use response::*;
#[path = "ui_domain/tabs.rs"]
pub(crate) mod tabs;
pub use tabs::*;
#[path = "ui_domain/workspaces.rs"]
pub(crate) mod workspaces;
pub use workspaces::*;
#[path = "ui_domain/worktrees.rs"]
pub(crate) mod worktrees;
pub use worktrees::*;

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}
