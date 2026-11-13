#[cfg(test)]
use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::shell::notification_policy::COMPLETION_RECHECK_INTERVAL;

    fn notification() -> ClientVisibleNotification {
        ClientVisibleNotification {
            runtime_target: None,
            endpoint_id: ClientEndpointId::Local,
            event: SemanticNotification {
                kind: SemanticNotificationKind::Custom,
                title: "notice".into(),
                body: None,
                sound: None,
                agent: None,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                position: None,
            },
            deadline: std::time::Instant::now(),
        }
    }

    #[test]
    fn mobile_notification_is_a_bottom_banner_with_released_title() {
        let palette = crate::client::theme::client_palette_from_config(&Config::default());
        let mut notification = notification();
        notification.event.kind = SemanticNotificationKind::NeedsAttention;
        notification.event.title = "pi needs attention".into();
        notification.event.body = Some("workspace · tab 1".into());
        let area = Rect::new(0, 0, 44, 20);
        let mut buffer = Buffer::empty(area);
        for cell in &mut buffer.content {
            cell.set_symbol("X");
        }
        let rect =
            render_mobile_notification_banner(&mut buffer, area, &notification, true, &palette);
        assert_eq!(rect, Rect::new(0, 18, 44, 1));
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("pi waiting"));
        assert!(text.contains("workspace · tab 1"));
        assert!(buffer.content[18 * 44..19 * 44]
            .iter()
            .all(|cell| cell.symbol() != "X"));
    }

    #[test]
    fn unavailable_notification_target_stays_visible_and_reports_the_machine() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.visible_notification = Some(ClientVisibleNotification {
            runtime_target: None,
            endpoint_id: ClientEndpointId::Local,
            event: SemanticNotification {
                kind: SemanticNotificationKind::NeedsAttention,
                title: "agent needs attention".into(),
                body: None,
                sound: None,
                agent: Some("agent".into()),
                workspace_id: Some("workspace".into()),
                tab_id: Some("tab".into()),
                pane_id: Some("pane".into()),
                position: None,
            },
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(5),
        });
        let mut outcome = ClientShellInput::default();

        state.focus_visible_notification(&mut outcome);

        assert!(state.visible_notification.is_some());
        assert!(state
            .visible_endpoint_notice
            .as_ref()
            .is_some_and(|notice| notice.body.contains("Local is unavailable")));
        assert!(outcome.repaint);
    }

    #[test]
    fn immediate_finished_evidence_for_acknowledged_idle_never_toasts_or_sounds() {
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.toast_delivery = crate::config::ToastDelivery::Herdr;
        config.toast_delay_seconds = 0;
        let mut state = ClientShellState::new(config);
        let mut snapshot = super::super::test_support::snapshot();
        snapshot
            .agents
            .push(crate::client::ui_model::ClientShellAgent {
                agent_id: "pane_1".into(),
                view: Some(crate::client::ui_model::AgentViewTarget {
                    pane_id: "pane_1".into(),
                    workspace_id: "ws_1".into(),
                    tab_id: "tab_1".into(),
                }),
                runtime_target: None,
                cwd: None,
                name: Some("agent".into()),
                display_agent: None,
                agent: Some("agent".into()),
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: crate::client::ui_domain::AgentStatus::Idle,
                state_change_seq: 2,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: true,
            });
        state.set_snapshot(Box::new(snapshot));

        let (effects, repaint) = state.receive_notification(
            &ClientEndpointId::Local,
            SemanticNotification {
                kind: SemanticNotificationKind::Finished,
                title: "agent finished".into(),
                body: None,
                sound: Some(SemanticNotificationSound::Done),
                agent: Some("agent".into()),
                workspace_id: Some("ws_1".into()),
                tab_id: Some("tab_1".into()),
                pane_id: Some("pane_1".into()),
                position: None,
            },
            std::time::Instant::now(),
        );

        assert!(effects.is_empty());
        assert!(!repaint);
        assert!(state.visible_notification.is_none());
    }

    #[test]
    fn finished_hint_waits_for_the_projected_completion_snapshot() {
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.toast_delivery = crate::config::ToastDelivery::Terminal;
        config.toast_delay_seconds = 0;
        let mut state = ClientShellState::new(config);
        let mut snapshot = super::super::test_support::snapshot();
        snapshot
            .agents
            .push(crate::client::ui_model::ClientShellAgent {
                agent_id: "pane_1".into(),
                view: Some(crate::client::ui_model::AgentViewTarget {
                    pane_id: "pane_1".into(),
                    workspace_id: "ws_1".into(),
                    tab_id: "tab_1".into(),
                }),
                runtime_target: None,
                cwd: None,
                name: Some("agent".into()),
                display_agent: None,
                agent: Some("agent".into()),
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: crate::client::ui_domain::AgentStatus::Working,
                state_change_seq: 1,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: false,
            });
        state.set_snapshot(Box::new(snapshot.clone()));
        let now = std::time::Instant::now();
        let event = SemanticNotification {
            kind: SemanticNotificationKind::Finished,
            title: "agent finished".into(),
            body: None,
            sound: None,
            agent: Some("agent".into()),
            workspace_id: Some("ws_1".into()),
            tab_id: Some("background-tab".into()),
            pane_id: Some("pane_1".into()),
            position: None,
        };

        let (effects, _) = state.receive_notification(&ClientEndpointId::Local, event, now);
        assert!(effects.is_empty());
        assert_eq!(state.pending_notifications.len(), 1);

        snapshot.revision = snapshot.revision.saturating_add(1);
        snapshot.agents[0].agent_status = crate::client::ui_domain::AgentStatus::Idle;
        snapshot.agents[0].state_change_seq = 2;
        state.set_snapshot(Box::new(snapshot));
        let (effects, _) = state.tick_notifications(now + COMPLETION_RECHECK_INTERVAL);
        assert!(matches!(
            effects.as_slice(),
            [ClientShellNotificationEffect::Terminal { title, .. }] if title == "agent finished"
        ));
        assert!(state.pending_notifications.is_empty());
    }

    #[test]
    fn retiring_one_endpoint_preserves_other_endpoint_notifications() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let mut local = notification();
        local.event.title = "local".into();
        let remote_id = ClientEndpointId::Ssh(
            crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        );
        let mut remote = notification();
        remote.endpoint_id = remote_id.clone();
        remote.event.title = "remote".into();
        state.visible_notification = Some(local);
        state.queued_notifications.push_back(remote);

        state.retire_endpoint_notifications(&ClientEndpointId::Local);

        assert_eq!(
            state
                .visible_notification
                .as_ref()
                .map(|notification| (&notification.endpoint_id, notification.event.title.as_str())),
            Some((&remote_id, "remote"))
        );
    }

    #[test]
    fn concurrent_endpoint_notifications_are_queued_in_arrival_order() {
        let mut config = Config::default();
        config.ui.toast.delivery = crate::config::ToastDelivery::Herdr;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        let now = std::time::Instant::now();
        for title in ["first", "second"] {
            state.receive_notification(
                &ClientEndpointId::Local,
                SemanticNotification {
                    kind: SemanticNotificationKind::Custom,
                    title: title.into(),
                    body: None,
                    sound: None,
                    agent: None,
                    workspace_id: None,
                    tab_id: None,
                    pane_id: Some(title.into()),
                    position: None,
                },
                now,
            );
        }

        assert_eq!(
            state
                .visible_notification
                .as_ref()
                .map(|notification| notification.event.title.as_str()),
            Some("first")
        );
        assert_eq!(state.queued_notifications.len(), 1);

        state.tick_notifications(now + std::time::Duration::from_secs(5));

        assert_eq!(
            state
                .visible_notification
                .as_ref()
                .map(|notification| notification.event.title.as_str()),
            Some("second")
        );
        assert!(state.queued_notifications.is_empty());
    }

    #[test]
    fn notification_rect_stays_inside_short_nonzero_area() {
        let palette = crate::client::theme::client_palette_from_config(&Config::default());
        for height in [1, 2] {
            let area = Rect::new(3, 4, 8, height);
            for position in [
                crate::config::ToastHerdrPosition::TopRight,
                crate::config::ToastHerdrPosition::BottomRight,
            ] {
                let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 10));
                let rect = render_visible_notification(
                    &mut buffer,
                    area,
                    &notification(),
                    position,
                    1,
                    &palette,
                );
                assert!(rect.y >= area.y);
                assert!(rect.bottom() <= area.bottom());
            }
        }
    }
}
