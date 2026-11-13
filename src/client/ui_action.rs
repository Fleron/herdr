use crate::client::ui_domain as schema;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FrontendCommand {
    CommandInvoke(schema::CommandInvokeParams),
    IntegrationInstall(schema::IntegrationInstallParams),
    IntegrationList(schema::EmptyParams),
    LayoutSetSplitRatio {
        params: schema::LayoutSetSplitRatioParams,
        split_id: Option<String>,
    },
    PaneClear(schema::PaneTarget),
    PaneClose(schema::PaneTarget),
    PaneCopyMotion(schema::PaneCopyMotionParams),
    PaneCopySearch(schema::PaneCopySearchParams),
    PaneEditScrollback(schema::PaneTarget),
    PaneFocus(schema::PaneTarget),
    PaneFocusDirection(schema::PaneFocusDirectionParams),
    PaneInputSet(schema::PaneInputSetParams),
    PaneLinkActivate(schema::PaneLinkActivateParams),
    PaneLinkResolve(schema::PaneLinkActivateParams),
    PaneRename(schema::PaneRenameParams),
    PaneResize(schema::PaneResizeParams),
    PaneScroll(schema::PaneScrollParams),
    PaneSelectionRead(schema::PaneSelectionReadParams),
    PaneSplit(schema::PaneSplitParams),
    PaneSwap(schema::PaneSwapParams),
    PaneZoom(schema::PaneZoomParams),
    ProductAnnouncementDismiss(schema::ProductAnnouncementDismissParams),
    ReleaseNotesDismiss(schema::ReleaseNotesDismissParams),
    ServerReloadConfig(schema::EmptyParams),
    TabClose(schema::TabTarget),
    TabCreate(schema::TabCreateParams),
    TabFocus(schema::TabTarget),
    TabMove(schema::TabMoveParams),
    TabRename(schema::TabRenameParams),
    WorkspaceClose(schema::WorkspaceCloseParams),
    WorkspaceCreate(schema::WorkspaceCreateParams),
    WorkspaceFocus(schema::WorkspaceTarget),
    WorkspaceMove(schema::WorkspaceMoveParams),
    WorkspaceMoveBlock(schema::WorkspaceMoveBlockParams),
    WorkspaceRename(schema::WorkspaceRenameParams),
    WorktreeCreate(schema::WorktreeCreateParams),
    WorktreeList(schema::WorktreeListParams),
    WorktreeOpen(schema::WorktreeOpenParams),
    WorktreeRemove(schema::WorktreeRemoveParams),
}

impl FrontendCommand {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::CommandInvoke(_) => "command.invoke",
            Self::IntegrationInstall(_) => "integration.install",
            Self::IntegrationList(_) => "integration.list",
            Self::LayoutSetSplitRatio { .. } => "layout.set_split_ratio",
            Self::PaneClear(_) => "pane.clear",
            Self::PaneClose(_) => "pane.close",
            Self::PaneCopyMotion(_) => "pane.copy_motion",
            Self::PaneCopySearch(_) => "pane.copy_search",
            Self::PaneEditScrollback(_) => "pane.edit_scrollback",
            Self::PaneFocus(_) => "pane.focus",
            Self::PaneFocusDirection(_) => "pane.focus_direction",
            Self::PaneInputSet(_) => "pane.input.set",
            Self::PaneLinkActivate(_) => "pane.link.activate",
            Self::PaneLinkResolve(_) => "pane.link.resolve",
            Self::PaneRename(_) => "pane.rename",
            Self::PaneResize(_) => "pane.resize",
            Self::PaneScroll(_) => "pane.scroll",
            Self::PaneSelectionRead(_) => "pane.selection.read",
            Self::PaneSplit(_) => "pane.split",
            Self::PaneSwap(_) => "pane.swap",
            Self::PaneZoom(_) => "pane.zoom",
            Self::ProductAnnouncementDismiss(_) => "product_announcement.dismiss",
            Self::ReleaseNotesDismiss(_) => "release_notes.dismiss",
            Self::ServerReloadConfig(_) => "server.reload_config",
            Self::TabClose(_) => "tab.close",
            Self::TabCreate(_) => "tab.create",
            Self::TabFocus(_) => "tab.focus",
            Self::TabMove(_) => "tab.move",
            Self::TabRename(_) => "tab.rename",
            Self::WorkspaceClose(_) => "workspace.close",
            Self::WorkspaceCreate(_) => "workspace.create",
            Self::WorkspaceFocus(_) => "workspace.focus",
            Self::WorkspaceMove(_) => "workspace.move",
            Self::WorkspaceMoveBlock(_) => "workspace.move_block",
            Self::WorkspaceRename(_) => "workspace.rename",
            Self::WorktreeCreate(_) => "worktree.create",
            Self::WorktreeList(_) => "worktree.list",
            Self::WorktreeOpen(_) => "worktree.open",
            Self::WorktreeRemove(_) => "worktree.remove",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrontendRequest {
    pub id: String,
    pub method: FrontendCommand,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UiRequest {
    PaneInput {
        pane_id: String,
        events: Vec<crate::client::frame::ClientPaneInputEvent>,
    },
    PopupInput {
        terminal_id: String,
        events: Vec<crate::client::frame::ClientPaneInputEvent>,
    },
    Focus {
        focused: bool,
    },
    HostTheme {
        update: crate::client::frame::ClientHostThemeUpdate,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoreAction {
    Retry,
    DiscardHeldInput,
    DiscardPendingLayouts,
    StopTerminal(super::ui_model::RuntimeTerminalTarget),
    OpenCheckout(super::ui_model::CheckoutTarget),
    BeginCheckoutWorktree {
        target: super::ui_model::CheckoutTarget,
        create: bool,
    },
    RemoveCheckout(super::ui_model::CheckoutTarget),
}
