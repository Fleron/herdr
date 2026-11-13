use super::*;

impl ClientShellConfig {
    pub(crate) fn with_keybinding_source(mut self, source: ClientShellKeybindingSource) -> Self {
        self.keybinding_source = source;
        self.keybinds.keybinds.custom_commands.clear();
        self
    }
    pub(crate) fn with_local_endpoint(self, socket_path: &std::path::Path) -> Self {
        self.with_preferences_path(preferences::path_for_local_endpoint(socket_path))
    }
    pub(crate) fn with_process_endpoint_preferences(self, socket_path: &std::path::Path) -> Self {
        self.with_endpoint_preferences(
            socket_path,
            std::env::var(crate::client::connection_diagnostics::REMOTE_PREFERENCES_ENV_VAR)
                .ok()
                .as_deref(),
        )
    }
    pub(crate) fn with_endpoint_preferences(
        self,
        socket_path: &std::path::Path,
        remote_identity: Option<&str>,
    ) -> Self {
        let identity = remote_identity
            .and_then(|value| serde_json::from_str::<(String, String)>(value).ok())
            .filter(|(target, session)| !target.is_empty() && !session.is_empty());
        match identity {
            Some((target, session)) => {
                self.with_preferences_path(preferences::path_for_remote_endpoint(&target, &session))
            }
            None => self.with_local_endpoint(socket_path),
        }
    }
    pub(crate) fn initial_surface_size(&self, cols: u16, rows: u16) -> ClientSurfaceSize {
        let sidebar_collapsed = self
            .preferences
            .sidebar_collapsed
            .unwrap_or(self.sidebar_start_collapsed);
        let (min_width, max_width) =
            crate::config::validated_sidebar_bounds(self.sidebar_min_width, self.sidebar_max_width)
                .unwrap_or((18, 36));
        let sidebar_width = self
            .preferences
            .sidebar_width
            .unwrap_or(self.sidebar_width)
            .clamp(min_width, max_width);
        let surface = self
            .layout(cols, rows, sidebar_collapsed, 0, sidebar_width)
            .pane_surface;
        ClientSurfaceSize {
            cols: surface.width.max(1),
            rows: surface.height.max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_surface_size_uses_persisted_endpoint_chrome() {
        let path = std::env::temp_dir().join(format!(
            "herdr-initial-shell-preferences-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        preferences::store(
            &path,
            preferences::ClientChromePreferences {
                sidebar_width: Some(31),
                sidebar_collapsed: Some(true),
                ..preferences::ClientChromePreferences::default()
            },
        )
        .expect("persist endpoint chrome");
        let config =
            ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
        let initial = config.initial_surface_size(100, 30);
        let state = ClientShellState::new(config);
        assert_eq!(initial, state.surface_size(100, 30));
        std::fs::remove_file(path).expect("remove endpoint chrome");
    }
}
