use std::time::{Duration, Instant};

use crate::client::ui_model::ClientShellTabStatusSegment;
use crate::config::{
    self, Config, TabBarRightEntryConfig, WindowTitlePart, WindowTitleTemplate, WindowTitleToken,
};

use super::Backend;

pub(super) struct Presentation {
    entries: Vec<TabBarRightEntryConfig>,
    pub separator: String,
    pub worktree_directory: String,
    window_title: Option<WindowTitleTemplate>,
    due: Instant,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            separator: " ".into(),
            worktree_directory: "~/.herdr/worktrees".into(),
            window_title: WindowTitleTemplate::parse(&config::default_window_title())
                .ok()
                .flatten(),
            due: Instant::now(),
        }
    }
}

impl Backend {
    pub(crate) fn set_presentation_config(&mut self, config: &Config) {
        self.presentation.entries = config
            .ui
            .tab_bar_right
            .iter()
            .take(config::MAX_TAB_BAR_RIGHT_ENTRIES)
            .cloned()
            .collect();
        self.presentation.separator =
            config::sanitize_separator(&config.ui.tab_bar_right_separator);
        self.presentation.worktree_directory = config.worktrees.directory.clone();
        self.presentation.window_title = WindowTitleTemplate::parse(&config.ui.window_title)
            .ok()
            .flatten();
        self.presentation.due = Instant::now();
        self.revision += 1;
    }

    pub(super) fn presentation_tick(&mut self) -> bool {
        if Instant::now() < self.presentation.due {
            return false;
        }
        self.presentation.due = Instant::now() + Duration::from_secs(1);
        self.presentation
            .entries
            .iter()
            .any(|entry| matches!(entry, TabBarRightEntryConfig::Datetime { .. }))
    }

    pub(super) fn tab_bar_status(
        &self,
        owner: Option<&str>,
        zoomed: bool,
    ) -> Vec<ClientShellTabStatusSegment> {
        let metadata = self.metadata.get(&owner.map(str::to_owned));
        self.presentation
            .entries
            .iter()
            .filter_map(|entry| {
                let text = match entry {
                    TabBarRightEntryConfig::Zoom => {
                        return zoomed.then(|| ClientShellTabStatusSegment {
                            text: "ZOOM".into(),
                            accent: true,
                        });
                    }
                    TabBarRightEntryConfig::Text { text } => config::sanitize_literal_text(text),
                    TabBarRightEntryConfig::Hostname => metadata
                        .and_then(|metadata| metadata.hostname.as_deref())
                        .and_then(config::sanitize_status_text),
                    TabBarRightEntryConfig::Datetime { format } => {
                        let clock = metadata?.clock.as_ref()?;
                        let offset = time::UtcOffset::from_whole_seconds(clock.offset).ok()?;
                        let elapsed = i64::try_from(clock.sampled.elapsed().as_secs()).ok()?;
                        let datetime = time::OffsetDateTime::from_unix_timestamp(
                            clock.timestamp.checked_add(elapsed)?,
                        )
                        .ok()?
                        .to_offset(offset);
                        let format = config::parse_tab_bar_datetime_format(format).ok()?;
                        datetime
                            .format(&format)
                            .ok()
                            .as_deref()
                            .and_then(config::sanitize_status_text)
                    }
                    TabBarRightEntryConfig::Command { .. } => None,
                }?;
                Some(ClientShellTabStatusSegment {
                    text,
                    accent: false,
                })
            })
            .collect()
    }

    pub(crate) fn window_title(&self, owner: Option<&str>) -> Option<String> {
        let template = self.presentation.window_title.as_ref()?;
        let workspace = self.active_workspace_for(owner);
        let tab = workspace.and_then(|workspace| {
            self.view
                .tabs
                .get(&workspace.id)
                .and_then(|id| workspace.tab(id))
                .or_else(|| workspace.tabs.first())
        });
        let pane = tab.and_then(|tab| {
            self.view
                .panes
                .get(&tab.id)
                .and_then(|id| tab.pane(id))
                .or_else(|| tab.panes.first())
        });
        let terminal_title = pane
            .and_then(|pane| {
                self.inventory.get(&herdr_core_tui::model::terminal_address(
                    owner,
                    &pane.terminal,
                ))
            })
            .and_then(|record| record["title"].as_str())
            .and_then(crate::client::terminal_title::stripped_terminal_title);
        let hostname = self
            .metadata
            .get(&owner.map(str::to_owned))
            .and_then(|metadata| metadata.hostname.as_deref());
        let mut title = String::new();
        for part in template.parts() {
            let text = match part {
                WindowTitlePart::Literal(text) => Some(text.as_str()),
                WindowTitlePart::Token(WindowTitleToken::Hostname) => hostname,
                WindowTitlePart::Token(WindowTitleToken::Workspace) => {
                    workspace.map(|workspace| workspace.name.as_str())
                }
                WindowTitlePart::Token(WindowTitleToken::Tab) => tab.map(|tab| tab.name.as_str()),
                WindowTitlePart::Token(WindowTitleToken::Pane) => {
                    pane.and_then(|pane| pane.name.as_deref())
                }
                WindowTitlePart::Token(WindowTitleToken::TerminalTitle) => {
                    terminal_title.as_deref()
                }
            };
            if let Some(text) = text {
                title.push_str(text);
            }
        }
        config::sanitize_window_title_text(&title)
    }
}
