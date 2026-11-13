use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};
use crate::cli::workspace_bindings as bindings;

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceCreateParams = crate::client::ui_domain::WorkspaceCreateParams => WorkspaceCreateProjection {
        /// Workspace whose focused pane supplies the `follow` cwd policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source_workspace_id: Option<String> => Field::new(None::<String>),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String> => Field::new(None)
            .input(bindings::optional_text("cwd", "PATH").path().order(0)),
        #[serde(default)]
        pub focus: bool => Field::new(false)
            .input(bindings::boolean("focus", true).order(3))
            .input(bindings::boolean("no-focus", false).order(4)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub label: Option<String> => Field::new(None)
            .input(bindings::optional_text("label", "TEXT").order(1)),
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        pub env: HashMap<String, String> => Field::new(HashMap::new())
            .input(bindings::environment().order(2)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceCloseParams = crate::client::ui_domain::WorkspaceCloseParams => WorkspaceCloseProjection {
        pub workspace_id: String => Field::new(String::new()).input(bindings::workspace_id()),
        #[serde(default, skip_serializing_if = "super::is_false")]
        pub close_group: bool => Field::new(false)
            .input(bindings::boolean("group", true).hidden()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceRenameParams = crate::client::ui_domain::WorkspaceRenameParams => WorkspaceRenameProjection {
        pub workspace_id: String => Field::new(String::new())
            .input(bindings::workspace_id().value_name("WORKSPACE_ID")),
        pub label: String => Field::new(String::new()).input(bindings::label()),
    }
}

pub use crate::client::ui_domain::workspaces::WorkspaceMoveParams;

pub use crate::client::ui_domain::workspaces::WorkspaceMoveBlockParams;

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct WorkspaceReportMetadataParams => WorkspaceMetadataProjection {
        pub workspace_id: String => Field::new(String::new())
            .input(bindings::workspace_id().value_name("WORKSPACE_ID")),
        pub source: String => Field::new(String::new()).input(bindings::source())
            .validate(|source| bindings::validate_source(source)),
        #[schemars(schema_with = "super::common::metadata_token_patch_schema")]
        pub tokens: HashMap<String, Option<String>> => Field::new(HashMap::new())
            .input(bindings::token()).input(bindings::clear_token())
            .validate(bindings::validate_tokens),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64> => Field::new(None).input(bindings::seq()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(min = 1, max = 86_400_000))]
        pub ttl_ms: Option<u64> => Field::new(None).input(bindings::ttl_ms()),
    }
}

pub use crate::client::ui_domain::workspaces::WorkspaceInfo;

pub use crate::client::ui_domain::workspaces::WorkspaceWorktreeInfo;
