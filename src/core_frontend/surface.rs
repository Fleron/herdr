use std::collections::HashMap;
use std::io;

use herdr_core_tui::inventory::Inventory;
use herdr_core_tui::model::{terminal_address, Document, ViewState};
use herdr_core_tui::render::paint_terminal;
use herdr_core_tui::surfaces::{PaneSpec, Streams};
use ratatui::buffer::Buffer;
use ratatui::layout::{Direction, Rect};
use ratatui::style::Style;
use serde_json::Value;

use crate::client::frame::{CursorState, FrameData, PaneSurfaceScrollMetrics};
use crate::client::ui::panes::{
    apply_pane_chrome, pane_inner_rect, render_pane_borders, split_hit_rect,
    terminal_inner_rect_for, zoomed_pane_borders,
};
use crate::client::ui::render_pane_scrollbar_buffer;
use crate::client::ui_model::{
    ClientShellAgent, ClientShellSnapshot, PaneSurfaceFrame, PaneSurfacePane, PaneSurfaceSplit,
    PaneSurfaceSplitDirection,
};
use crate::config::Config;
use crate::kitty_graphics::model::SurfaceGraphicsTarget;
use crate::kitty_graphics::surface::{collect_terminal_scene, DeliveryCache, TerminalPane};
use crate::kitty_graphics::HostCellSize;
use crate::layout::{PaneId, PaneInfo, SplitBorder};
use crate::palette::Palette;

#[derive(Default)]
pub(super) struct Geometry {
    pub specs: Vec<PaneSpec>,
    pub panes: Vec<PaneInfo>,
    pub splits: Vec<SplitBorder>,
    pub split_ids: HashMap<Vec<bool>, String>,
    pub ids: HashMap<PaneId, String>,
    pub area: Rect,
    records: HashMap<String, Value>,
}

#[derive(Default)]
pub(super) struct SurfaceBuilder {
    revision: u64,
    delivered: DeliveryCache,
    delivery_epoch: Option<u64>,
}

impl SurfaceBuilder {
    pub(super) fn set_delivery_epoch(&mut self, epoch: u64) {
        if self.delivery_epoch != Some(epoch) {
            self.delivered = DeliveryCache::default();
            self.delivery_epoch = Some(epoch);
        }
    }

    pub(super) fn prepare_visible(geometry: &Geometry, streams: &mut Streams) -> io::Result<bool> {
        let mut reflow = false;
        for spec in &geometry.specs {
            let Some(pane) = streams.pane_mut(&spec.id) else {
                continue;
            };
            let before = (
                pane.view.alternate_screen_active(),
                pane.view.scroll_metrics().max_offset_from_bottom > 0,
            );
            pane.view.prepare()?;
            reflow |= before
                != (
                    pane.view.alternate_screen_active(),
                    pane.view.scroll_metrics().max_offset_from_bottom > 0,
                );
        }
        Ok(reflow)
    }

    pub(super) fn geometry(
        document: &Document,
        view: &ViewState,
        inventory: &Inventory,
        streams: &Streams,
        size: (u16, u16),
        config: &Config,
    ) -> io::Result<Geometry> {
        let area = Rect::new(0, 0, size.0, size.1);
        let Some(workspace) = view
            .workspace
            .as_deref()
            .and_then(|id| document.workspace(id))
        else {
            return Ok(Geometry {
                area,
                ..Geometry::default()
            });
        };
        let tab = view
            .tabs
            .get(&workspace.id)
            .and_then(|id| workspace.tab(id))
            .or_else(|| workspace.tabs.first());
        let Some(tab) = tab else {
            return Ok(Geometry {
                area,
                ..Geometry::default()
            });
        };
        let focused = view
            .panes
            .get(&tab.id)
            .map(String::as_str)
            .or_else(|| tab.panes.first().map(|pane| pane.id.as_str()));
        let layout = tab.tile_layout(focused).map_err(io::Error::other)?;
        let zoomed = view.zoomed.contains(&tab.id);
        let mut panes = if zoomed {
            let borders = zoomed_pane_borders(
                config.ui.pane_borders,
                config.ui.pane_outer_borders,
                tab.panes.len() > 1,
            );
            vec![PaneInfo {
                id: layout.layout.focused(),
                rect: area,
                inner_rect: pane_inner_rect(area, borders),
                scrollbar_rect: None,
                borders,
                is_focused: true,
            }]
        } else {
            apply_pane_chrome(
                layout.layout.panes(area),
                config.ui.pane_borders,
                config.ui.pane_gaps,
                config.ui.pane_outer_borders,
            )
        };
        let splits = if zoomed {
            Vec::new()
        } else {
            layout.layout.splits(area)
        };
        let split_ids = splits
            .iter()
            .map(|split| {
                let id = tab
                    .layout
                    .split_at(&split.path)
                    .ok_or_else(|| io::Error::other("split geometry has no saved split"))?;
                Ok((split.path.clone(), id.to_owned()))
            })
            .collect::<io::Result<_>>()?;
        let mut specs = Vec::new();
        let mut records = HashMap::new();
        for info in &mut panes {
            let id = layout
                .stable_id(info.id)
                .ok_or_else(|| io::Error::other("pane geometry has no saved view"))?;
            let saved = tab
                .pane(id)
                .ok_or_else(|| io::Error::other("pane geometry refers to a closed view"))?;
            let pane_inner = pane_inner_rect(info.rect, info.borders);
            let stream = streams.pane(id);
            let gutter = config.ui.pane_scrollbars
                && stream.is_none_or(|pane| !pane.view.alternate_screen_active());
            info.inner_rect = terminal_inner_rect_for(pane_inner, gutter);
            info.scrollbar_rect = (info.inner_rect != pane_inner
                && stream
                    .is_some_and(|pane| pane.view.scroll_metrics().max_offset_from_bottom > 0))
            .then_some(Rect::new(
                pane_inner.right().saturating_sub(1),
                pane_inner.y,
                1,
                pane_inner.height,
            ));
            let address = terminal_address(workspace.machine.as_deref(), &saved.terminal);
            let record = inventory.get(&address);
            specs.push(PaneSpec {
                id: id.into(),
                address,
                cols: info.inner_rect.width,
                rows: info.inner_rect.height,
                focused: info.is_focused,
                live: record.is_some_and(|record| record["state"] == "live"),
                completion: record
                    .filter(|record| record["state"] == "finished")
                    .cloned(),
            });
            if let Some(record) = record {
                records.insert(id.into(), record.clone());
            }
        }
        Ok(Geometry {
            specs,
            panes,
            splits,
            split_ids,
            ids: layout.panes,
            area,
            records,
        })
    }

    pub(super) fn build(
        &mut self,
        snapshot: &ClientShellSnapshot,
        geometry: &Geometry,
        streams: &mut Streams,
        config: &Config,
        palette: &Palette,
        cell_size: HostCellSize,
    ) -> io::Result<PaneSurfaceFrame> {
        self.revision = self.revision.wrapping_add(1);
        let mut buffer = Buffer::empty(geometry.area);
        let mut cursor = None;
        let mut hyperlinks = Vec::new();
        let mut panes = Vec::new();
        let metadata: HashMap<_, _> = snapshot
            .panes
            .iter()
            .map(|pane| (pane.pane_id.as_str(), pane))
            .collect();
        let agents: HashMap<_, _> = snapshot
            .agents
            .iter()
            .filter_map(|agent| {
                agent
                    .view
                    .as_ref()
                    .map(|view| (view.pane_id.as_str(), agent))
            })
            .collect();
        let labels = |id| {
            geometry.ids.get(&id).and_then(|id| {
                metadata
                    .get(id.as_str())
                    .and_then(|pane| pane.label.clone())
                    .or_else(|| {
                        config
                            .ui
                            .show_agent_labels_on_pane_borders
                            .then(|| {
                                agents
                                    .get(id.as_str())
                                    .and_then(|agent| agent.display_agent.clone())
                            })
                            .flatten()
                    })
            })
        };
        render_pane_borders(
            config.ui.pane_borders,
            config.ui.pane_gaps,
            palette,
            &geometry.panes,
            &geometry.splits,
            labels,
            &mut buffer,
        );
        for info in &geometry.panes {
            let id = geometry
                .ids
                .get(&info.id)
                .ok_or_else(|| io::Error::other("pane surface has no saved view"))?;
            let Some(pane) = streams.pane_mut(id) else {
                continue;
            };
            if cell_size.is_known() {
                pane.view
                    .set_cell_size(cell_size.width_px, cell_size.height_px)?;
            }
            pane.view.prepare()?;
            paint_terminal(
                &pane.view,
                &mut buffer,
                info.inner_rect,
                pane.selection,
                false,
            );
            let scroll = pane.view.scroll_metrics();
            if let Some(track) = info.scrollbar_rect {
                render_pane_scrollbar_buffer(
                    &mut buffer,
                    crate::client::ui_state::ScrollMetrics {
                        offset_from_bottom: scroll.offset_from_bottom,
                        max_offset_from_bottom: scroll.max_offset_from_bottom,
                        viewport_rows: scroll.viewport_rows,
                    },
                    track,
                    palette,
                    info.is_focused,
                );
            }
            for ((x, y), symbol, uri) in pane.view.hyperlinks() {
                if *x < info.inner_rect.width && *y < info.inner_rect.height {
                    hyperlinks.push((
                        (info.inner_rect.x + x, info.inner_rect.y + y),
                        symbol.clone(),
                        uri.clone(),
                    ));
                }
            }
            if info.is_focused && !pane.view.presentation_pending() {
                cursor = terminal_cursor(
                    &pane.view,
                    info.inner_rect,
                    config,
                    agents.get(id.as_str()).copied(),
                );
            }
            let (pixel_width, pixel_height) = if cell_size.is_known() {
                (
                    u32::from(info.inner_rect.width).saturating_mul(cell_size.width_px),
                    u32::from(info.inner_rect.height).saturating_mul(cell_size.height_px),
                )
            } else {
                (0, 0)
            };
            panes.push(PaneSurfacePane {
                pane_id: id.clone(),
                content_revision: if pane.view.presentation_pending() {
                    pane.revision | 1
                } else {
                    pane.revision
                },
                rect: info.rect.into(),
                inner_rect: info.inner_rect.into(),
                scrollbar_rect: info.scrollbar_rect.map(Into::into),
                scroll: Some(PaneSurfaceScrollMetrics {
                    offset_from_bottom: scroll.offset_from_bottom as u64,
                    max_offset_from_bottom: scroll.max_offset_from_bottom as u64,
                    viewport_rows: scroll.viewport_rows as u64,
                }),
                focused: info.is_focused,
                mouse_reporting: pane.view.mouse_reporting(),
                sgr_pixel_mouse: pane.view.sgr_pixel_mouse(),
                alternate_screen_active: pane.view.alternate_screen_active(),
                pixel_width,
                pixel_height,
            });
            let record = geometry.records.get(id.as_str());
            let completion = pane
                .completion
                .as_ref()
                .or_else(|| record.filter(|record| record["state"] == "finished"));
            let message = completion
                .map(completion_message)
                .or_else(|| pane.error.clone());
            if let Some(message) = message.filter(|_| !info.inner_rect.is_empty()) {
                buffer.set_stringn(
                    info.inner_rect.x,
                    info.inner_rect.y,
                    message,
                    usize::from(info.inner_rect.width),
                    Style::default().fg(if pane.error.is_some() {
                        palette.red
                    } else {
                        palette.subtext0
                    }),
                );
            }
        }
        let pane_frames: Vec<_> = geometry.panes.iter().map(|pane| pane.rect).collect();
        let splits = geometry
            .splits
            .iter()
            .filter_map(|split| {
                split_hit_rect(
                    split,
                    config.ui.pane_borders.draws_borders(),
                    config.ui.pane_gaps,
                    &pane_frames,
                )
                .map(|hit_rect| {
                    let split_id = geometry
                        .split_ids
                        .get(&split.path)
                        .ok_or_else(|| io::Error::other("split surface has no saved identity"))?;
                    Ok(PaneSurfaceSplit {
                        split_id: Some(split_id.clone()),
                        direction: match split.direction {
                            Direction::Horizontal => PaneSurfaceSplitDirection::Horizontal,
                            Direction::Vertical => PaneSurfaceSplitDirection::Vertical,
                        },
                        pos: split.pos,
                        area: split.area.into(),
                        hit_rect: hit_rect.into(),
                        path: split.path.clone(),
                    })
                })
            })
            .collect::<io::Result<_>>()?;
        let graphics = if config.kitty_graphics_enabled() {
            let sources: Vec<_> = geometry
                .panes
                .iter()
                .filter_map(|info| {
                    let id = geometry.ids.get(&info.id)?;
                    let pane = streams.pane(id)?;
                    Some(TerminalPane {
                        pane_id: info.id,
                        target: SurfaceGraphicsTarget::Pane {
                            pane_id: id.clone(),
                        },
                        area: info.inner_rect,
                        scrollback_offset: pane.view.scroll_metrics().offset_from_bottom as u32,
                        terminal: pane.view.terminal(),
                    })
                })
                .collect();
            let (scene, delivered, _) =
                collect_terminal_scene(&sources, cell_size, &self.delivered)?;
            self.delivered = delivered;
            scene
        } else {
            self.delivered = DeliveryCache::default();
            Default::default()
        };
        Ok(PaneSurfaceFrame {
            boot_id: snapshot.boot_id.clone(),
            projection_revision: snapshot.revision,
            surface_revision: self.revision,
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, cursor, &hyperlinks),
            panes,
            splits,
            popup: None,
            graphics,
        })
    }
}

fn terminal_cursor(
    view: &herdr_core_tui::terminal::TerminalView,
    area: Rect,
    config: &Config,
    agent: Option<&ClientShellAgent>,
) -> Option<CursorState> {
    if view.scroll_metrics().offset_from_bottom > 0 || area.is_empty() {
        return None;
    }
    let reveal = config.experimental.reveal_hidden_cursor_for_cjk_ime
        && (config.experimental.cjk_ime_agents.is_empty()
            || agent
                .and_then(|agent| agent.agent.as_deref())
                .and_then(crate::detect::parse_agent_label)
                .is_some_and(|agent| {
                    config
                        .experimental
                        .cjk_ime_agents
                        .iter()
                        .filter_map(|name| crate::detect::parse_agent_label(name))
                        .any(|configured| configured == agent)
                }));
    let current = view.cursor_state();
    let point = current.and_then(|cursor| cursor.viewport);
    let (x, y) = point.map_or((0, 0), |point| (point.x, point.y));
    if x >= area.width || y >= area.height {
        return None;
    }
    let visible =
        reveal || current.is_some_and(|cursor| cursor.visible && cursor.viewport.is_some());
    let shape = if reveal {
        config.experimental.cjk_ime_cursor_shape.to_decscusr()
    } else {
        let cursor = current?;
        crate::ghostty::decscusr_cursor_shape(cursor.visual_style, cursor.blinking)
    };
    Some(CursorState {
        x: area.x + x,
        y: area.y + y,
        visible,
        shape,
    })
}

fn completion_message(completion: &Value) -> String {
    if let Some(code) = completion["exit_code"].as_i64() {
        format!("Program finished with exit code {code}.")
    } else if let Some(signal) = completion["signal"].as_i64() {
        format!("Program stopped by signal {signal}.")
    } else {
        "Program finished.".into()
    }
}

#[cfg(test)]
use crate::core_test_support as core_support;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::mpsc;

    use herdr_core_tui::connection::Display;
    use herdr_core_tui::model::{new_id, Axis, Operation, Pane, Tab, Workspace};
    use herdr_core_tui::persistence::LayoutStore;
    use herdr_core_tui::surfaces::Input;
    use serde_json::json;

    use super::core_support::{wait_until, CoreSession};
    use super::*;
    use crate::client::core_backend::Backend;

    #[test]
    fn original_surface_keeps_real_pane_ids_links_zoom_and_alternate_screen_gutter() {
        let session = CoreSession::new();
        let client = session.client();
        let script = "stty -echo; printf READY; \
            while IFS= read -r line; do \
                case \"$line\" in \
                    links) printf '\\033]8;;https://example.com/surface\\033\\\\LINK\\033]8;;\\033\\\\';; \
                    alt) printf '\\033[?1049hALT_SCREEN';; \
                    normal) printf '\\033[?1049lNORMAL_SCREEN';; \
                esac; \
            done";
        let start = || {
            client
                .request(
                    "terminal.run",
                    json!({
                        "argv": ["/bin/sh", "-c", script], "no_parent": true,
                        "size": {"cols": 40, "rows": 24}
                    }),
                )
                .unwrap()["address"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let mut workspace = Workspace::new("surface", session.root.to_str().unwrap(), None);
        let mut tab = Tab::new("programs", start());
        tab.panes[0].name = Some("first".into());
        let first = tab.panes[0].id.clone();
        let mut second = Pane::new(start());
        second.name = Some("second".into());
        let second_id = second.id.clone();
        let workspace_id = workspace.id.clone();
        let tab_id = tab.id.clone();
        workspace.tabs.push(tab);
        let mut store = LayoutStore::load(&client, None).unwrap();
        store.apply(Operation::AddWorkspace(workspace)).unwrap();
        store
            .apply(Operation::SplitPane {
                workspace: workspace_id.clone(),
                tab: tab_id.clone(),
                target: first.clone(),
                pane: second,
                split: new_id(),
                axis: Axis::Horizontal,
            })
            .unwrap();
        store.save(&client).unwrap();
        let mut view = ViewState {
            workspace: Some(workspace_id.clone()),
            ..ViewState::default()
        };
        view.tabs.insert(workspace_id, tab_id.clone());
        view.panes.insert(tab_id.clone(), first.clone());
        let (wake, _) = mpsc::sync_channel(1);
        let mut backend = Backend::from_client(client.clone(), view, wake.clone()).unwrap();
        wait_until(|| {
            backend.poll().unwrap();
            backend
                .document()
                .workspaces
                .iter()
                .any(|workspace| workspace.tabs.len() == 1)
        });
        let mut streams = Streams::new(
            client,
            Display {
                cell: Some([8, 16]),
                ..Display::default()
            },
            None,
            1024 * 1024,
            wake,
        );
        let config = Config::default();
        let palette = Palette::catppuccin();
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let retained = HashSet::from([first.clone(), second_id.clone()]);
        let mut builder = SurfaceBuilder::default();
        let mut geometry = SurfaceBuilder::geometry(
            backend.document(),
            backend.view(),
            backend.inventory(),
            &streams,
            (80, 24),
            &config,
        )
        .unwrap();
        assert_eq!(geometry.specs.len(), 2);
        assert_eq!(
            geometry.ids.values().cloned().collect::<HashSet<_>>(),
            retained
        );
        assert!(!geometry.splits.is_empty());
        streams.reconcile(&geometry.specs, &retained).unwrap();
        wait_until(|| {
            streams.poll().unwrap();
            geometry
                .specs
                .iter()
                .all(|spec| streams.pane(&spec.id).unwrap().input_ready())
        });
        streams
            .input(&first, Input::Bytes(b"links\n".to_vec()))
            .unwrap();
        wait_until(|| {
            streams.poll().unwrap();
            let pane = streams.pane_mut(&first).unwrap();
            pane.view.prepare().unwrap();
            !pane.view.hyperlinks().is_empty()
        });
        let frame = builder
            .build(
                &backend.snapshot(None),
                &geometry,
                &mut streams,
                &config,
                &palette,
                cell,
            )
            .unwrap();
        assert_eq!(frame.panes.len(), 2);
        assert!(!frame.splits.is_empty());
        for split in &frame.splits {
            assert_eq!(split.split_id.as_ref(), geometry.split_ids.get(&split.path));
            assert!(split.split_id.is_some());
        }
        assert_eq!(frame.frame.cells.len(), 80 * 24);
        assert!(frame
            .frame
            .hyperlinks
            .iter()
            .any(|uri| uri == "https://example.com/surface"));
        assert!(frame
            .panes
            .iter()
            .all(|pane| pane.content_revision.is_multiple_of(2)));
        assert!(frame
            .panes
            .iter()
            .all(|pane| pane.pixel_width > 0 && pane.pixel_height > 0));

        backend.view_mut().zoomed.insert(tab_id);
        geometry = SurfaceBuilder::geometry(
            backend.document(),
            backend.view(),
            backend.inventory(),
            &streams,
            (80, 24),
            &config,
        )
        .unwrap();
        assert_eq!(geometry.specs.len(), 1);
        assert!(geometry.splits.is_empty());
        assert_eq!(geometry.specs[0].id, first);
        assert_eq!(geometry.panes[0].inner_rect.width, 77);
        streams.reconcile(&geometry.specs, &retained).unwrap();
        streams
            .input(&first, Input::Bytes(b"alt\n".to_vec()))
            .unwrap();
        wait_until(|| {
            streams.poll().unwrap();
            let pane = streams.pane_mut(&first).unwrap();
            pane.view.prepare().unwrap();
            pane.view.alternate_screen_active()
        });
        geometry = SurfaceBuilder::geometry(
            backend.document(),
            backend.view(),
            backend.inventory(),
            &streams,
            (80, 24),
            &config,
        )
        .unwrap();
        assert_eq!(geometry.panes[0].inner_rect.width, 78);
        streams.reconcile(&geometry.specs, &retained).unwrap();
        streams
            .input(&first, Input::Bytes(b"normal\n".to_vec()))
            .unwrap();
        wait_until(|| {
            streams.poll().unwrap();
            let pane = streams.pane_mut(&first).unwrap();
            pane.view.prepare().unwrap();
            !pane.view.alternate_screen_active()
        });
        geometry = SurfaceBuilder::geometry(
            backend.document(),
            backend.view(),
            backend.inventory(),
            &streams,
            (80, 24),
            &config,
        )
        .unwrap();
        assert_eq!(geometry.panes[0].inner_rect.width, 77);
        assert!(streams.pane(&second_id).is_some());
        drop(streams);
        drop(backend);
        session.finish();
    }
}
