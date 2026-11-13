use super::*;
use ratatui::layout::Rect;
use serde_json::json;

fn setup() -> (Document, String, String, String) {
    let mut workspace = Workspace::new("repo", "/repo", None);
    let tab = Tab::new("shell", "t1");
    let ids = (
        workspace.id.clone(),
        tab.id.clone(),
        tab.panes[0].id.clone(),
    );
    workspace.tabs.push(tab);
    (
        Document {
            workspaces: vec![workspace],
            ..Document::default()
        },
        ids.0,
        ids.1,
        ids.2,
    )
}

#[test]
fn split_close_and_restore_keep_terminal_identity_and_geometry() {
    let (mut document, workspace, tab, first) = setup();
    let second = Pane::new("t2");
    let split = Operation::SplitPane {
        workspace: workspace.clone(),
        tab: tab.clone(),
        target: first.clone(),
        pane: second.clone(),
        split: new_id(),
        axis: Axis::Horizontal,
    };
    assert!(document.apply(&split).unwrap());
    assert!(!document.apply(&split).unwrap());
    let restored = Document::from_value(serde_json::to_value(&document).unwrap()).unwrap();
    let layout = restored
        .workspace(&workspace)
        .unwrap()
        .tab(&tab)
        .unwrap()
        .tile_layout(Some(&second.id))
        .unwrap();
    let panes = layout.layout.panes(Rect::new(0, 0, 100, 30));
    assert_eq!(panes.len(), 2);
    assert_eq!(
        layout.stable_id(layout.layout.focused()),
        Some(second.id.as_str())
    );
    assert!(panes.iter().all(|pane| pane.inner_rect.width > 0));
    document
        .apply(&Operation::ClosePane {
            workspace: workspace.clone(),
            tab: tab.clone(),
            pane: first,
        })
        .unwrap();
    let remaining = document.workspace(&workspace).unwrap().tab(&tab).unwrap();
    assert_eq!(remaining.panes, vec![second]);
    let final_pane = remaining.panes[0].id.clone();
    document
        .apply(&Operation::ClosePane {
            workspace: workspace.clone(),
            tab: tab.clone(),
            pane: final_pane,
        })
        .unwrap();
    assert!(document.workspace(&workspace).is_none());
}

#[test]
fn independent_changes_replay_without_erasing_unknown_fields_or_other_tabs() {
    let (mut remote, workspace, tab, _) = setup();
    remote
        .extra
        .insert("future".to_string(), json!({"preserve": true}));
    let mut initial = serde_json::to_value(&remote).unwrap();
    initial["workspaces"][0]["tabs"][0]["layout"]["future_pane_style"] = json!("retain");
    let mut remote = Document::from_value(initial).unwrap();
    let added = Tab::new("tests", "t2");
    remote
        .apply(&Operation::AddTab {
            workspace: workspace.clone(),
            tab: added.clone(),
        })
        .unwrap();
    remote
        .apply(&Operation::RenameTab {
            workspace: workspace.clone(),
            tab: tab.clone(),
            name: "agent".to_string(),
        })
        .unwrap();
    let value = serde_json::to_value(&remote).unwrap();
    assert_eq!(value["future"]["preserve"], true);
    assert_eq!(remote.workspace(&workspace).unwrap().tabs.len(), 2);
    assert_eq!(
        remote
            .workspace(&workspace)
            .unwrap()
            .tab(&tab)
            .unwrap()
            .name,
        "agent"
    );
    assert_eq!(
        remote
            .workspace(&workspace)
            .unwrap()
            .tab(&added.id)
            .unwrap()
            .name,
        "tests"
    );
    let target = remote
        .workspace(&workspace)
        .unwrap()
        .tab(&tab)
        .unwrap()
        .panes[0]
        .id
        .clone();
    remote
        .apply(&Operation::SplitPane {
            workspace,
            tab,
            target,
            pane: Pane::new("t3"),
            split: new_id(),
            axis: Axis::Vertical,
        })
        .unwrap();
    let saved = serde_json::to_value(remote).unwrap();
    assert_eq!(
        saved["workspaces"][0]["tabs"][0]["layout"]["first"]["future_pane_style"],
        "retain"
    );
}

#[test]
fn invalid_documents_and_operations_do_not_corrupt_existing_layout() {
    let (mut document, workspace, tab, first) = setup();
    let before = document.clone();
    let operation = Operation::SplitPane {
        workspace,
        tab,
        target: first.clone(),
        pane: Pane {
            id: first,
            terminal: "t2".to_string(),
            name: None,
            extra: Map::new(),
        },
        split: new_id(),
        axis: Axis::Vertical,
    };
    assert!(!document.apply(&operation).unwrap());
    assert_eq!(document, before);
    let mut invalid = serde_json::to_value(&document).unwrap();
    invalid["workspaces"][0]["tabs"][0]["layout"]["id"] = json!("missing");
    assert!(Document::from_value(invalid).is_err());
    assert!(Document::from_value(json!({"schema": 2})).is_err());
}

#[test]
fn remote_layouts_store_local_ids_and_private_focus_repairs_after_close() {
    let (mut document, workspace, tab, pane) = setup();
    document.workspaces[0].machine = Some("box".to_string());
    let mut view = ViewState::default();
    assert!(view.focus_pane(&document, &pane));
    let saved = serde_json::to_value(&document).unwrap();
    assert!(saved["workspaces"][0].get("machine").is_none());
    assert_eq!(
        saved["workspaces"][0]["tabs"][0]["panes"][0]["terminal"],
        "t1"
    );
    assert_eq!(terminal_address(Some("box"), "t1"), "box/t1");
    assert_eq!(local_terminal(Some("box"), "box/t1").unwrap(), "t1");
    assert!(local_terminal(Some("box"), "other/t1").is_err());
    document
        .apply(&Operation::CloseTab { workspace, tab })
        .unwrap();
    view.repair(&document);
    assert!(view.active_pane(&document).is_none());
    assert!(view.panes.is_empty());
}

#[test]
fn conditional_swap_is_replayable_without_overwriting_another_layout_edit() {
    let (mut document, workspace, tab, first) = setup();
    let second = Pane::new("t2");
    document
        .apply(&Operation::SplitPane {
            workspace: workspace.clone(),
            tab: tab.clone(),
            target: first.clone(),
            pane: second.clone(),
            split: new_id(),
            axis: Axis::Horizontal,
        })
        .unwrap();
    let baseline = document.clone();
    let expected = document
        .workspace(&workspace)
        .unwrap()
        .tab(&tab)
        .unwrap()
        .layout
        .clone();
    let layout = expected.with_swapped_panes(&first, &second.id).unwrap();
    let swap = Operation::SetTabLayout {
        workspace: workspace.clone(),
        tab: tab.clone(),
        expected,
        layout,
    };
    assert!(document.apply(&swap).unwrap());
    assert!(!document.apply(&swap).unwrap());
    let mut concurrent = baseline;
    concurrent
        .apply(&Operation::SplitPane {
            workspace,
            tab,
            target: first,
            pane: Pane::new("t3"),
            split: new_id(),
            axis: Axis::Vertical,
        })
        .unwrap();
    let before = concurrent.clone();
    assert!(concurrent.apply(&swap).is_err());
    assert_eq!(concurrent, before);
}

#[test]
fn moving_tabs_uses_stable_destinations_and_is_idempotent() {
    let (mut document, workspace, first, _) = setup();
    let second = Tab::new("second", "t2");
    let third = Tab::new("third", "t3");
    document
        .apply(&Operation::AddTab {
            workspace: workspace.clone(),
            tab: second.clone(),
        })
        .unwrap();
    document
        .apply(&Operation::AddTab {
            workspace: workspace.clone(),
            tab: third.clone(),
        })
        .unwrap();
    let move_tab = Operation::MoveTab {
        workspace: workspace.clone(),
        tab: third.id.clone(),
        before: Some(first.clone()),
    };
    assert!(document.apply(&move_tab).unwrap());
    assert!(!document.apply(&move_tab).unwrap());
    let order: Vec<_> = document
        .workspace(&workspace)
        .unwrap()
        .tabs
        .iter()
        .map(|tab| tab.id.as_str())
        .collect();
    assert_eq!(
        order,
        vec![third.id.as_str(), first.as_str(), second.id.as_str()]
    );
    let to_end = Operation::MoveTab {
        workspace: workspace.clone(),
        tab: third.id.clone(),
        before: None,
    };
    assert!(document.apply(&to_end).unwrap());
    assert!(!document.apply(&to_end).unwrap());
    assert_eq!(
        document
            .workspace(&workspace)
            .unwrap()
            .tabs
            .last()
            .unwrap()
            .id,
        third.id
    );
}
