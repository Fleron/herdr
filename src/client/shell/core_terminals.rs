fn core_terminal_matches(overlay: &ClientCoreTerminalOverlay) -> Vec<usize> {
    let query = overlay.query.as_str().trim().to_lowercase();
    overlay
        .terminals
        .iter()
        .enumerate()
        .filter_map(|(index, terminal)| {
            let matches = query.is_empty()
                || terminal.label.to_lowercase().contains(&query)
                || terminal.machine_label.to_lowercase().contains(&query)
                || terminal.target.terminal.to_lowercase().contains(&query)
                || terminal
                    .cwd
                    .as_deref()
                    .is_some_and(|cwd| cwd.to_lowercase().contains(&query))
                || terminal
                    .status
                    .as_deref()
                    .is_some_and(|status| status.to_lowercase().contains(&query));
            matches.then_some(index)
        })
        .collect()
}

impl ClientShellState {
    pub(in crate::client::shell) fn open_core_terminal_picker(
        &mut self,
        terminals: Vec<crate::client::ui_model::CoreTerminalView>,
        title: impl Into<String>,
    ) {
        self.overlay = Some(ClientShellOverlay::CoreTerminals(
            ClientCoreTerminalOverlay {
                title: title.into(),
                query: TextEditor::default(),
                highlighted: 0,
                terminals,
            },
        ));
        self.mode = ClientShellMode::Terminal;
        self.chrome_drag = None;
    }

    pub(in crate::client::shell) fn core_terminal_picker_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::CoreTerminals(_))) {
            return false;
        }
        if key.kind == crossterm::event::KeyEventKind::Release {
            return true;
        }
        match key.code {
            crossterm::event::KeyCode::Esc => {
                self.overlay = None;
            }
            crossterm::event::KeyCode::Enter => self.submit_core_terminal(outcome),
            code => {
                if let Some(ClientShellOverlay::CoreTerminals(overlay)) = self.overlay.as_mut() {
                    let count = core_terminal_matches(overlay).len();
                    match code {
                        crossterm::event::KeyCode::Up => {
                            overlay.highlighted = overlay.highlighted.saturating_sub(1);
                        }
                        crossterm::event::KeyCode::Down => {
                            overlay.highlighted = overlay
                                .highlighted
                                .saturating_add(1)
                                .min(count.saturating_sub(1));
                        }
                        crossterm::event::KeyCode::PageUp => {
                            overlay.highlighted = overlay.highlighted.saturating_sub(10);
                        }
                        crossterm::event::KeyCode::PageDown => {
                            overlay.highlighted = overlay
                                .highlighted
                                .saturating_add(10)
                                .min(count.saturating_sub(1));
                        }
                        _ => {
                            if overlay.query.handle_key(key) == Some(true) {
                                overlay.highlighted = 0;
                            }
                        }
                    }
                }
            }
        }
        outcome.repaint = true;
        true
    }

    pub(in crate::client::shell) fn core_terminal_picker_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::CoreTerminals(_))) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                if super::super::contains(self.hits.overlay_cancel, point) {
                    self.overlay = None;
                } else if super::super::contains(self.hits.overlay_primary, point) {
                    self.submit_core_terminal(outcome);
                } else if let Some((_, index)) = self
                    .hits
                    .core_terminal_rows
                    .iter()
                    .find(|(area, _)| super::super::contains(*area, point))
                    .copied()
                {
                    if let Some(ClientShellOverlay::CoreTerminals(overlay)) = self.overlay.as_mut()
                    {
                        if let Some(position) = core_terminal_matches(overlay)
                            .iter()
                            .position(|item| *item == index)
                        {
                            overlay.highlighted = position;
                        }
                    }
                    self.submit_core_terminal(outcome);
                }
            }
            crossterm::event::MouseEventKind::ScrollUp
            | crossterm::event::MouseEventKind::ScrollDown => {
                if let Some(ClientShellOverlay::CoreTerminals(overlay)) = self.overlay.as_mut() {
                    let count = core_terminal_matches(overlay).len();
                    if mouse.kind == crossterm::event::MouseEventKind::ScrollUp {
                        overlay.highlighted = overlay
                            .highlighted
                            .saturating_sub(self.config.mouse_scroll_lines);
                    } else {
                        overlay.highlighted = overlay
                            .highlighted
                            .saturating_add(self.config.mouse_scroll_lines)
                            .min(count.saturating_sub(1));
                    }
                }
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }

    fn submit_core_terminal(&mut self, outcome: &mut ClientShellInput) {
        let target = match self.overlay.as_ref() {
            Some(ClientShellOverlay::CoreTerminals(overlay)) => core_terminal_matches(overlay)
                .get(overlay.highlighted)
                .and_then(|index| overlay.terminals.get(*index))
                .map(|terminal| terminal.target.clone()),
            _ => None,
        };
        if let Some(target) = target {
            self.overlay = None;
            self.chrome_drag = None;
            outcome
                .actions
                .push(ClientShellAction::AttachTerminal { target });
            outcome.repaint = true;
        }
    }
}

fn render_core_terminal_picker_overlay(
    buffer: &mut Buffer,
    overlay: &ClientCoreTerminalOverlay,
    palette: &Palette,
) -> Option<OverlayRender> {
    let height = overlay
        .terminals
        .len()
        .saturating_mul(2)
        .saturating_add(7)
        .clamp(12, 26) as u16;
    let popup = popup(buffer.area, 96, height)?;
    let inner = panel(buffer, popup, palette.accent, palette.panel_bg)?;
    if inner.height < 6 {
        return None;
    }
    let normal = Style::default().fg(palette.text).bg(palette.panel_bg);
    put_text(
        buffer,
        inner.x,
        inner.y,
        inner.width,
        &overlay.title,
        normal.add_modifier(Modifier::BOLD),
    );
    let filtered = core_terminal_matches(overlay);
    let search = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    let count = format!("{}/{} terminals", filtered.len(), overlay.terminals.len());
    put_text(buffer, search.x, search.y, search.width, " / ", normal);
    let cursor = text_editor::render(
        buffer,
        Rect::new(
            search.x + 3,
            search.y,
            search.width.saturating_sub(4 + display_width(&count)),
            1,
        ),
        &overlay.query,
        normal,
    );
    put_right_text(
        buffer,
        search,
        search.y,
        &count,
        Style::default().fg(palette.overlay0).bg(palette.panel_bg),
    );
    let body = Rect::new(
        inner.x,
        inner.y + 3,
        inner.width,
        inner.height.saturating_sub(5),
    );
    let visible = usize::from(body.height / 2);
    let selected = overlay.highlighted.min(filtered.len().saturating_sub(1));
    let start = selected
        .saturating_sub(visible.saturating_sub(1))
        .min(filtered.len().saturating_sub(visible));
    let mut hits = Vec::new();
    for (row, index) in filtered
        .iter()
        .copied()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let terminal = &overlay.terminals[index];
        let area = Rect::new(body.x, body.y + row as u16 * 2, body.width, 2);
        let highlighted = row + start == selected;
        let style = if highlighted {
            Style::default().fg(contrast(palette)).bg(palette.accent)
        } else {
            normal
        };
        buffer.set_style(area, style);
        put_text(
            buffer,
            area.x,
            area.y,
            area.width,
            &format!(" {}", terminal.label),
            style.add_modifier(Modifier::BOLD),
        );
        let status = terminal
            .status
            .as_deref()
            .map(|status| format!("{} · {status}", terminal.machine_label))
            .unwrap_or_else(|| terminal.machine_label.clone());
        put_right_text(buffer, area, area.y, &status, style);
        let cwd = terminal.cwd.as_deref().unwrap_or(&terminal.target.terminal);
        put_text(
            buffer,
            area.x,
            area.y + 1,
            area.width,
            &format!(" {cwd}"),
            if highlighted {
                style
            } else {
                Style::default().fg(palette.overlay0).bg(palette.panel_bg)
            },
        );
        hits.push((area, index));
    }
    if filtered.is_empty() {
        let text = if overlay.query.as_str().is_empty() {
            " no running terminals reported"
        } else {
            " no matching terminals"
        };
        put_text(
            buffer,
            body.x,
            body.y,
            body.width,
            text,
            Style::default().fg(palette.overlay0).bg(palette.panel_bg),
        );
    }
    let buttons = row(inner, &[12, 12], 2, inner.height.saturating_sub(1));
    let [primary, cancel] = buttons.as_slice() else {
        return None;
    };
    button(
        buffer,
        *primary,
        " ↵ attach ",
        Style::default()
            .fg(contrast(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    button(
        buffer,
        *cancel,
        " esc cancel ",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    Some(OverlayRender {
        area: popup,
        primary: *primary,
        cancel: *cancel,
        core_terminal_search: search,
        core_terminal_rows: hits,
        cursor,
        ..Default::default()
    })
}
