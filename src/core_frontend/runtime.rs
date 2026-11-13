use std::fmt;
use std::io;

use herdr_core_tui::model::{Operation, Tab};
use herdr_core_tui::surfaces::PaneStream;
use herdr_core_tui::terminal::{Point, Selection};
use ratatui::layout::Rect;

use crate::client::terminal_text::{safe_web_url, url_at_byte, url_byte_range};
use crate::client::ui_action::FrontendCommand;
use crate::client::ui_domain as schema;
use crate::layout::{find_in_direction, NavDirection};
use crate::terminal_text::{
    paragraph_motion_target, word_motion_target, RetainedTextBuffer, TerminalSearchDirection,
    TerminalTextPoint, TerminalWordMotion,
};

const MAX_SEARCH_QUERY_BYTES: usize = 4096;
const SEARCH_MATCH_WINDOW: usize = 1024;

#[derive(Debug)]
pub(super) struct Error {
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

fn failure(code: &'static str, message: impl Into<String>) -> io::Error {
    io::Error::other(Error {
        code,
        message: message.into(),
    })
}

impl super::App {
    pub(super) fn runtime_command(
        &mut self,
        owner: Option<&str>,
        command: FrontendCommand,
    ) -> io::Result<Option<schema::ResponseResult>> {
        use FrontendCommand::*;
        let result = match command {
            PaneScroll(params) => {
                self.runtime_target(owner, &params.pane_id)?;
                self.streams
                    .pane_mut(&params.pane_id)
                    .expect("runtime target validated above")
                    .view
                    .scroll_to_offset(params.offset_from_bottom)?;
                self.surface_dirty = true;
                self.dirty = true;
                schema::ResponseResult::Ok {}
            }
            PaneClear(params) => {
                self.runtime_target(owner, &params.pane_id)?;
                let pane = self
                    .streams
                    .pane_mut(&params.pane_id)
                    .expect("runtime target validated above");
                pane.view.clear_screen()?;
                pane.selection = None;
                pane.revision = pane.revision.wrapping_add(2);
                self.surface_dirty = true;
                self.dirty = true;
                self.layout_dirty = true;
                schema::ResponseResult::Ok {}
            }
            PaneEditScrollback(_) => {
                return Err(failure(
                    "unsupported_action",
                    "Opening scrollback in an external editor is not supported.",
                ))
            }
            PaneSelectionRead(params) => {
                let pane = self.stable_runtime_target(
                    owner,
                    &params.pane_id,
                    params.content_revision,
                    None,
                )?;
                let text = pane.view.selected_text(Selection {
                    anchor: Point {
                        row: params.anchor.row as usize,
                        col: params.anchor.col,
                    },
                    head: Point {
                        row: params.cursor.row as usize,
                        col: params.cursor.col,
                    },
                })?;
                schema::ResponseResult::PaneSelection {
                    pane_id: params.pane_id,
                    text,
                }
            }
            PaneCopyMotion(params) => {
                let pane = self.stable_runtime_target(
                    owner,
                    &params.pane_id,
                    params.content_revision,
                    None,
                )?;
                let terminal = pane.view.terminal();
                let fallback = TerminalTextPoint {
                    row: params.cursor.row,
                    col: params.cursor.col,
                };
                let target = match params.motion {
                    schema::PaneCopyMotion::LineEnd | schema::PaneCopyMotion::FirstNonBlank => {
                        let width = terminal.cols().map_err(io::Error::other)?.max(1);
                        let text = terminal
                            .read_text_screen(
                                (0, params.cursor.row),
                                (width - 1, params.cursor.row),
                                false,
                            )
                            .map_err(io::Error::other)?;
                        let col = if params.motion == schema::PaneCopyMotion::LineEnd {
                            crate::copy_mode::last_character_col(&text)
                        } else {
                            crate::copy_mode::first_non_blank_col(&text)
                        }
                        .unwrap_or(0)
                        .min(width - 1);
                        TerminalTextPoint {
                            row: params.cursor.row,
                            col,
                        }
                    }
                    schema::PaneCopyMotion::PreviousParagraph
                    | schema::PaneCopyMotion::NextParagraph => paragraph_motion_target(
                        terminal,
                        params.cursor.row,
                        if params.motion == schema::PaneCopyMotion::PreviousParagraph {
                            -1
                        } else {
                            1
                        },
                    )
                    .map(|target| TerminalTextPoint {
                        col: params.cursor.col,
                        ..target
                    })
                    .unwrap_or(fallback),
                    motion => word_motion_target(
                        terminal,
                        params.cursor.row,
                        params.cursor.col,
                        word_motion(motion),
                    )
                    .unwrap_or(fallback),
                };
                schema::ResponseResult::PaneCopyMotion {
                    pane_id: params.pane_id,
                    cursor: schema::PaneTextPoint {
                        row: target.row,
                        col: target.col,
                    },
                    content_revision: pane.revision,
                }
            }
            PaneCopySearch(params) => {
                if params.query.len() > MAX_SEARCH_QUERY_BYTES {
                    return Err(failure(
                        "query_too_large",
                        "Copy search query is too large.",
                    ));
                }
                let pane = self.stable_runtime_target(
                    owner,
                    &params.pane_id,
                    Some(params.content_revision),
                    None,
                )?;
                let terminal = pane.view.terminal();
                let buffer = RetainedTextBuffer::new_search(
                    terminal.cols().map_err(io::Error::other)?,
                    terminal.screen_text_rows().map_err(io::Error::other)?,
                    0,
                );
                let result = buffer.search_window(
                    &params.query,
                    params.query.chars().any(char::is_uppercase),
                    terminal.active_screen().map_err(io::Error::other)?,
                    match params.direction {
                        schema::PaneCopySearchDirection::Forward => {
                            TerminalSearchDirection::Forward
                        }
                        schema::PaneCopySearchDirection::Backward => {
                            TerminalSearchDirection::Backward
                        }
                    },
                    TerminalTextPoint {
                        row: params.cursor.row,
                        col: params.cursor.col,
                    },
                    params.previous.map(|range| {
                        (
                            TerminalTextPoint {
                                row: range.start.row,
                                col: range.start.col,
                            },
                            TerminalTextPoint {
                                row: range.end.row,
                                col: range.end.col,
                            },
                        )
                    }),
                    SEARCH_MATCH_WINDOW,
                );
                schema::ResponseResult::PaneCopySearch {
                    pane_id: params.pane_id,
                    content_revision: pane.revision,
                    matches: result
                        .matches
                        .into_iter()
                        .map(|matched| schema::PaneTextRange {
                            start: schema::PaneTextPoint {
                                row: matched.start.row,
                                col: matched.start.col,
                            },
                            end: schema::PaneTextPoint {
                                row: matched.end.row,
                                col: matched.end.col,
                            },
                        })
                        .collect(),
                    total: result.total as u64,
                    current: result.current.map(|index| index as u32),
                    current_global: result.current_global.map(|index| index as u64),
                }
            }
            PaneLinkResolve(params) => {
                let pane = self.checked_runtime_link(owner, &params)?;
                let regions = pane
                    .view
                    .terminal()
                    .viewport_link_regions(
                        params.col,
                        u32::from(params.viewport_row),
                        url_byte_range,
                    )
                    .map_err(io::Error::other)?;
                schema::ResponseResult::PaneLinkResolved {
                    regions: regions
                        .into_iter()
                        .map(|region| schema::PaneLinkRegion {
                            row: region.row,
                            start_col: region.start_col,
                            end_col: region.end_col,
                        })
                        .collect(),
                }
            }
            PaneLinkActivate(params) => {
                let pane = self.checked_runtime_link(owner, &params)?;
                let url = pane
                    .view
                    .terminal()
                    .viewport_link_target(params.col, u32::from(params.viewport_row))
                    .map_err(io::Error::other)?
                    .and_then(|target| match target {
                        crate::ghostty::LinkTarget::Uri(uri) => {
                            safe_web_url(&uri).map(str::to_owned)
                        }
                        crate::ghostty::LinkTarget::Text { text, clicked_byte } => {
                            url_at_byte(&text, clicked_byte)
                                .and_then(safe_web_url)
                                .map(str::to_owned)
                        }
                    });
                schema::ResponseResult::PaneLinkActivated {
                    url,
                    handled: false,
                }
            }
            PaneFocusDirection(params) => {
                let (_, tab, source) =
                    self.backend.pane_target(owner, params.pane_id.as_deref())?;
                let target = self.runtime_neighbor(&tab, &source, params.direction)?;
                if let Some(target) = &target {
                    self.backend.focus_pane(owner, target)?;
                    self.runtime_layout_changed();
                }
                schema::ResponseResult::Ok {}
            }
            PaneResize(params) => {
                let (workspace, tab, source) =
                    self.backend.pane_target(owner, params.pane_id.as_deref())?;
                let focused = self.backend.view().panes.get(&tab.id).map(String::as_str);
                let mut tile = tab.tile_layout(focused).map_err(io::Error::other)?;
                let id = tile
                    .pane_id(&source)
                    .ok_or_else(|| failure("pane_not_found", "Pane is missing from its layout."))?;
                let area = self.runtime_layout_area();
                let before = tile.layout.splits(area);
                let changed = tile.layout.resize_pane(
                    id,
                    direction(params.direction),
                    params
                        .amount
                        .filter(|amount| amount.is_finite())
                        .unwrap_or(0.05)
                        .abs()
                        .min(0.5),
                    area,
                );
                if changed {
                    for split in tile.layout.splits(area) {
                        if before.iter().any(|previous| {
                            previous.path == split.path && previous.ratio != split.ratio
                        }) {
                            let id = tab.layout.split_at(&split.path).ok_or_else(|| {
                                failure("stale_target", "The split handle no longer exists.")
                            })?;
                            self.backend.apply_operation(
                                owner.map(str::to_owned),
                                Operation::SetRatio {
                                    workspace: workspace.id.clone(),
                                    tab: tab.id.clone(),
                                    split: id.into(),
                                    ratio: split.ratio,
                                },
                            )?;
                        }
                    }
                    self.runtime_layout_changed();
                }
                schema::ResponseResult::Ok {}
            }
            PaneSwap(params) => {
                let Some(direction) = params.direction else {
                    return Err(failure(
                        "invalid_pane_swap",
                        "Choose a direction or both source and target panes.",
                    ));
                };
                if params.source_pane_id.is_some() || params.target_pane_id.is_some() {
                    return Err(failure(
                        "invalid_pane_swap",
                        "Choose a direction or both source and target panes.",
                    ));
                }
                let (_, tab, source) =
                    self.backend.pane_target(owner, params.pane_id.as_deref())?;
                let target = self.runtime_neighbor(&tab, &source, direction)?;
                if let Some(target) = &target {
                    self.backend.swap_views(owner, &source, target)?;
                    self.runtime_layout_changed();
                }
                schema::ResponseResult::Ok {}
            }
            _ => return Ok(None),
        };
        Ok(Some(result))
    }

    fn runtime_target(&self, owner: Option<&str>, id: &str) -> io::Result<&PaneStream> {
        if owner != self.backend.view().active_owner.as_deref() {
            return Err(failure("stale_target", "The selected machine changed."));
        }
        let address = self
            .backend
            .saved_address(id)
            .ok_or_else(|| failure("pane_not_found", "The terminal view has closed."))?;
        if !self
            .geometry
            .specs
            .iter()
            .any(|spec| spec.id == id && spec.address == address)
        {
            return Err(failure("stale_target", "The pane is no longer visible."));
        }
        self.streams
            .pane(id)
            .filter(|pane| pane.spec.address == address)
            .ok_or_else(|| failure("pane_not_found", "The terminal view is not attached."))
    }

    fn stable_runtime_target(
        &self,
        owner: Option<&str>,
        id: &str,
        revision: Option<u64>,
        offset: Option<u64>,
    ) -> io::Result<&PaneStream> {
        let pane = self.runtime_target(owner, id)?;
        if pane.view.presentation_pending()
            || !pane.revision.is_multiple_of(2)
            || revision.is_some_and(|revision| revision != pane.revision)
            || offset.is_some_and(|offset| {
                offset != pane.view.scroll_metrics().offset_from_bottom as u64
            })
        {
            return Err(failure(
                "stale_content",
                "Pane content or viewport changed.",
            ));
        }
        Ok(pane)
    }

    fn checked_runtime_link(
        &self,
        owner: Option<&str>,
        params: &schema::PaneLinkActivateParams,
    ) -> io::Result<&PaneStream> {
        let pane = self.stable_runtime_target(
            owner,
            &params.pane_id,
            params.content_revision,
            params.offset_from_bottom,
        )?;
        let size = pane.view.size();
        if params.col >= size.0 || params.viewport_row >= size.1 {
            return Err(failure(
                "stale_target",
                "The link is outside the current viewport.",
            ));
        }
        Ok(pane)
    }

    fn runtime_layout_area(&self) -> Rect {
        let size = self.shell.surface_size(self.size.0, self.size.1);
        Rect::new(0, 0, size.cols, size.rows)
    }

    fn runtime_neighbor(
        &self,
        tab: &Tab,
        pane: &str,
        target: schema::PaneDirection,
    ) -> io::Result<Option<String>> {
        let tile = tab.tile_layout(Some(pane)).map_err(io::Error::other)?;
        let panes = tile.layout.panes(self.runtime_layout_area());
        let source = panes
            .iter()
            .find(|source| tile.stable_id(source.id) == Some(pane))
            .ok_or_else(|| failure("pane_not_found", "Pane is missing from its layout."))?;
        Ok(find_in_direction(source, direction(target), &panes)
            .and_then(|id| tile.stable_id(id).map(str::to_owned)))
    }

    fn runtime_layout_changed(&mut self) {
        self.layout_dirty = true;
        self.view_dirty = true;
        self.dirty = true;
        self.sync_shell();
    }
}

fn direction(value: schema::PaneDirection) -> NavDirection {
    match value {
        schema::PaneDirection::Left => NavDirection::Left,
        schema::PaneDirection::Right => NavDirection::Right,
        schema::PaneDirection::Up => NavDirection::Up,
        schema::PaneDirection::Down => NavDirection::Down,
    }
}

fn word_motion(value: schema::PaneCopyMotion) -> TerminalWordMotion {
    match value {
        schema::PaneCopyMotion::NextWordStart => TerminalWordMotion::NextStart,
        schema::PaneCopyMotion::PreviousWordStart => TerminalWordMotion::PreviousStart,
        schema::PaneCopyMotion::NextWordEnd => TerminalWordMotion::NextEnd,
        schema::PaneCopyMotion::NextBigWordStart => TerminalWordMotion::NextBigStart,
        schema::PaneCopyMotion::PreviousBigWordStart => TerminalWordMotion::PreviousBigStart,
        schema::PaneCopyMotion::NextBigWordEnd => TerminalWordMotion::NextBigEnd,
        _ => unreachable!("line and paragraph motions are handled separately"),
    }
}
