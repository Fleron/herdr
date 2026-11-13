use serde::{Deserialize, Serialize};

pub use crate::client::ui_domain::integrations::IntegrationState;

pub use crate::client::ui_domain::integrations::IntegrationInfo;

pub use crate::client::ui_domain::integrations::IntegrationInstallParams;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntegrationUninstallParams {
    pub target: IntegrationTarget,
}

pub use crate::client::ui_domain::integrations::IntegrationTarget;

pub use crate::client::ui_domain::integrations::IntegrationInstallResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntegrationUninstallResult {
    pub messages: Vec<String>,
}

impl IntegrationTarget {
    pub(crate) const ALL: [Self; 17] = [
        Self::Pi,
        Self::Omp,
        Self::Claude,
        Self::Codex,
        Self::Copilot,
        Self::Devin,
        Self::Droid,
        Self::Kimi,
        Self::Opencode,
        Self::Kilo,
        Self::Hermes,
        Self::Qodercli,
        Self::Qwen,
        Self::Cursor,
        Self::Mastracode,
        Self::AntigravityCli,
        Self::Grok,
    ];
}
