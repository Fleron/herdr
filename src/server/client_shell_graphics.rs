use crate::app::state::AppState;
use crate::kitty_graphics::model::SurfaceGraphicsSource;
use crate::kitty_graphics::surface::{
    asset_key_from_descriptor, image_signature_from_asset, MAX_SURFACE_GRAPHICS_PLACEMENTS,
};
use crate::kitty_graphics::{
    terminal_image_needs_data, HostCellSize, HostPlacement, HostSourceKey, ImageSignature,
};
use crate::terminal::TerminalRuntimeRegistry;
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet};

const HEADLESS_GRAPHICS_TRANSACTION_BUDGET: usize =
    crate::protocol::MAX_GRAPHICS_FRAME_SIZE - crate::protocol::MAX_FRAME_SIZE;

use crate::kitty_graphics::surface::{DeliveryCache, SourceFiles};
use crate::protocol::{ClientShellPopupSurface, SurfaceGraphicsScene, SurfaceGraphicsTarget};

pub(crate) fn collect_retained(
    app: &crate::app::App,
    panes: &[crate::protocol::PaneSurfacePane],
    target: crate::ui::TabSurfaceTarget,
    cell_size: crate::kitty_graphics::HostCellSize,
    delivered: &DeliveryCache,
    client_id: u64,
) -> Option<(SurfaceGraphicsScene, DeliveryCache, SourceFiles)> {
    let rect = |rect: crate::protocol::SurfaceRect| {
        ratatui::layout::Rect::new(rect.x, rect.y, rect.width, rect.height)
    };
    let pane_infos = panes
        .iter()
        .map(|pane| {
            let (workspace_index, id) = app.parse_pane_id(&pane.pane_id)?;
            if workspace_index != target.workspace_index {
                return None;
            }
            Some(crate::layout::PaneInfo {
                id,
                rect: rect(pane.rect),
                inner_rect: rect(pane.inner_rect),
                scrollbar_rect: pane.scrollbar_rect.map(rect),
                borders: ratatui::widgets::Borders::NONE,
                is_focused: pane.focused,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    // Image clipping uses the retained content rectangles, not borders or split handles.
    Some(collect(
        app,
        &pane_infos,
        &[],
        None,
        Some(target),
        cell_size,
        delivered,
        client_id,
    ))
}

pub(crate) fn collect(
    app: &crate::app::App,
    pane_infos: &[crate::layout::PaneInfo],
    split_borders: &[crate::layout::SplitBorder],
    popup: Option<&ClientShellPopupSurface>,
    target: Option<crate::ui::TabSurfaceTarget>,
    cell_size: crate::kitty_graphics::HostCellSize,
    delivered: &DeliveryCache,
    client_id: u64,
) -> (SurfaceGraphicsScene, DeliveryCache, SourceFiles) {
    let popup_content_size = popup.map(|popup| (popup.frame.width, popup.frame.height));
    collect_scene(
        app,
        crate::ui::TabSurfaceView {
            target,
            pane_infos,
            split_borders,
        },
        popup_content_size,
        cell_size,
        delivered,
        client_id,
    )
}

fn collect_visible_placements(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
    delivered_images: &HashMap<HostSourceKey, ImageSignature>,
) -> Vec<HostPlacement> {
    let Some(target) = surface.target else {
        tracing::debug!("collect_visible_placements: no tab surface target");
        return Vec::new();
    };
    let ws_idx = target.workspace_index;
    if app
        .workspaces
        .get(ws_idx)
        .and_then(|workspace| workspace.tabs.get(target.tab_index))
        .is_none()
    {
        tracing::debug!(
            ws_idx,
            tab_idx = target.tab_index,
            "collect_visible_placements: no target tab"
        );
        return Vec::new();
    }

    tracing::debug!(
        ws_idx,
        terminal_runtimes_len = terminal_runtimes.len(),
        pane_infos_len = surface.pane_infos.len(),
        "collect_visible_placements: starting iteration"
    );
    let mut placements = Vec::new();
    for info in surface.pane_infos {
        let runtime = match app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            Some(rt) => rt,
            None => {
                tracing::debug!(pane_id = ?info.id, "collect_visible_placements: runtime not found");
                continue;
            }
        };
        let mut requested_images = HashSet::new();
        let pane_placements = runtime.kitty_image_placements_with_data_filter(|descriptor| {
            if descriptor.source_file {
                return false;
            }
            terminal_image_needs_data(info.id, descriptor, delivered_images, &mut requested_images)
        });
        if pane_placements.is_empty() {
            continue;
        }
        let scrollback_offset = runtime
            .scroll_metrics()
            .map(|m| m.offset_from_bottom as u32)
            .unwrap_or(0);
        for placement in pane_placements {
            placements.push(HostPlacement {
                raw_data: None,
                pane_id: info.id,
                host_image_id: None,
                area: info.inner_rect,
                cell_size,
                source_key: HostSourceKey::Terminal {
                    pane_id: info.id,
                    image_id: placement.image_id,
                },
                placement,
                scrollback_offset,
            });
        }
    }
    tracing::debug!(
        placements_len = placements.len(),
        "collect_visible_placements: done"
    );
    placements
}

pub(crate) fn collect_scene(
    app: &crate::app::App,
    surface: crate::ui::TabSurfaceView<'_>,
    popup_content_size: Option<(u16, u16)>,
    cell_size: HostCellSize,
    delivered: &DeliveryCache,
    _client_id: u64,
) -> (SurfaceGraphicsScene, DeliveryCache, SourceFiles) {
    if !cell_size.is_known() {
        return (
            SurfaceGraphicsScene::default(),
            DeliveryCache::default(),
            SourceFiles::default(),
        );
    }
    let workspace_index = surface.target.map(|target| target.workspace_index);
    let mut targets = HashMap::new();
    let mut public_panes = HashMap::new();
    if let Some(workspace_index) = workspace_index {
        for pane in surface.pane_infos {
            if let Some(public_id) = app.public_pane_id(workspace_index, pane.id) {
                public_panes.insert(public_id.clone(), pane.id);
                targets.insert(pane.id, SurfaceGraphicsTarget::Pane { pane_id: public_id });
            }
        }
    }
    let popup_target = app.state.popup_pane.as_ref().map(|popup| {
        (
            popup.pane_id,
            SurfaceGraphicsTarget::Popup {
                terminal_id: popup.terminal_id.to_string(),
            },
        )
    });
    if let Some((pane_id, target)) = popup_target.as_ref() {
        targets.insert(*pane_id, target.clone());
    }

    // Reconstruct the small source-signature index expected by the collector.
    // This avoids copying already-delivered payloads while Ghostty remains the
    // authoritative image store.
    let mut delivered_terminal_images = HashMap::new();
    for key in &delivered.assets {
        let signature = image_signature_from_asset(key);
        match &key.source {
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane { pane_id },
                image_id,
            } => {
                if let Some(pane_id) = public_panes.get(pane_id) {
                    delivered_terminal_images.insert(
                        HostSourceKey::Terminal {
                            pane_id: *pane_id,
                            image_id: *image_id,
                        },
                        signature,
                    );
                }
            }
            // Frozen for old-server compatibility; current servers no longer collect layers.
            SurfaceGraphicsSource::PaneLayer { .. } => {}
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Popup { .. },
                ..
            } => {}
        }
    }
    let mut host_placements = collect_visible_placements(
        &app.state,
        &app.terminal_runtimes,
        surface,
        cell_size,
        &delivered_terminal_images,
    );

    if let (Some(popup), Some((width, height)), Some((_, target))) = (
        app.state.popup_pane.as_ref(),
        popup_content_size,
        popup_target.as_ref(),
    ) {
        if let Some(runtime) = app.terminal_runtimes.get(&popup.terminal_id) {
            let mut requested = HashSet::new();
            for placement in runtime.kitty_image_placements_with_data_filter(|descriptor| {
                let key = asset_key_from_descriptor(
                    SurfaceGraphicsSource::Terminal {
                        target: target.clone(),
                        image_id: descriptor.image_id,
                    },
                    descriptor,
                );
                !descriptor.source_file && !delivered.assets.contains(&key) && requested.insert(key)
            }) {
                host_placements.push(HostPlacement {
                    raw_data: None,
                    pane_id: popup.pane_id,
                    host_image_id: None,
                    area: Rect::new(0, 0, width, height),
                    cell_size,
                    source_key: HostSourceKey::Terminal {
                        pane_id: popup.pane_id,
                        image_id: placement.image_id,
                    },
                    placement,
                    scrollback_offset: runtime
                        .scroll_metrics()
                        .map(|metrics| metrics.offset_from_bottom as u32)
                        .unwrap_or(0),
                });
            }
        }
    }

    crate::kitty_graphics::surface::build_scene(
        host_placements,
        &targets,
        delivered,
        MAX_SURFACE_GRAPHICS_PLACEMENTS,
        HEADLESS_GRAPHICS_TRANSACTION_BUDGET,
        |visible| {
            crate::kitty_graphics::surface::retained_assets(
                delivered,
                visible,
                |public_id, image_ids| {
                    let fingerprints = workspace_index
                        .and_then(|index| public_panes.get(public_id).map(|id| (index, *id)))
                        .and_then(|(index, id)| {
                            app.state.runtime_for_pane_in_workspace(
                                &app.terminal_runtimes,
                                index,
                                id,
                            )
                        })
                        .map(|runtime| runtime.kitty_image_fingerprints(image_ids))
                        .unwrap_or_else(|| vec![None; image_ids.len()]);
                    Ok::<_, std::convert::Infallible>(fingerprints)
                },
            )
        },
    )
    .unwrap_or_else(|never| match never {})
}
