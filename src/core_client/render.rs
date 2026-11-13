use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

use super::terminal::{Selection, TerminalView};

pub fn paint_terminal(
    view: &TerminalView,
    target: &mut Buffer,
    area: Rect,
    selection: Option<Selection>,
    show_cursor: bool,
) -> Option<(u16, u16)> {
    let visible = area.intersection(target.area);
    if visible.is_empty() {
        return None;
    }
    for y in visible.y..visible.bottom() {
        for x in visible.x..visible.right() {
            let local = (x - area.x, y - area.y);
            let output = &mut target[(x, y)];
            if local.0 < view.size().0 && local.1 < view.size().1 {
                *output = view.buffer()[local].clone();
                if output.symbol().width() > usize::from(visible.right() - x) {
                    output.set_symbol(" ");
                }
                if selection
                    .is_some_and(|selection| selection.contains(view.point_at(local.0, local.1)))
                {
                    output.set_style(output.style().add_modifier(Modifier::REVERSED));
                }
            } else {
                output.reset();
                output.set_style(view.blank_style());
            }
        }
    }
    let (x, y) = view.cursor().filter(|_| show_cursor)?;
    let position = (area.x.saturating_add(x), area.y.saturating_add(y));
    visible.contains(position.into()).then_some(position)
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::time::Instant;

    use super::*;

    fn view(cols: u16, rows: u16) -> TerminalView {
        TerminalView::new(cols, rows, 1_000_000).expect("terminal view")
    }

    #[test]
    fn clips_wide_cells_at_the_pane_edge_without_touching_neighbors() {
        let mut view = view(4, 2);
        view.feed("A界Z".as_bytes());
        view.prepare().expect("prepare");
        let mut target = Buffer::empty(Rect::new(0, 0, 8, 2));
        for cell in &mut target.content {
            cell.set_symbol(".");
        }
        paint_terminal(&view, &mut target, Rect::new(2, 0, 2, 2), None, false);
        assert_eq!(target[(2, 0)].symbol(), "A");
        assert_eq!(target[(3, 0)].symbol(), " ");
        assert_eq!(target[(4, 0)].symbol(), ".");
        assert_eq!(target[(1, 0)].symbol(), ".");
    }

    #[test]
    fn selection_and_cursor_use_pane_coordinates_without_mutating_the_view() {
        let mut view = view(8, 2);
        view.feed(b"hello");
        view.prepare().expect("prepare");
        let selection = Selection {
            anchor: view.point_at(1, 0),
            head: view.point_at(3, 0),
        };
        let original = view.buffer().clone();
        let mut target = Buffer::empty(Rect::new(0, 0, 20, 5));
        let cursor = paint_terminal(
            &view,
            &mut target,
            Rect::new(4, 2, 8, 2),
            Some(selection),
            true,
        );
        assert_eq!(cursor, Some((9, 2)));
        assert!(target[(5, 2)].modifier.contains(Modifier::REVERSED));
        assert!(!target[(4, 2)].modifier.contains(Modifier::REVERSED));
        assert_eq!(view.buffer(), &original);
        assert_eq!(view.selected_text(selection).expect("copy"), "ell");
    }

    #[test]
    #[ignore = "rendering profile; run with --ignored --nocapture"]
    fn render_scale_profile() {
        let screen = Rect::new(0, 0, 120, 45);
        let mut baseline = None;
        for panes in [1u16, 15] {
            let columns = if panes == 1 { 1 } else { 3 };
            let rows = panes / columns;
            let width = screen.width / columns;
            let height = screen.height / rows;
            let mut views = (0..panes)
                .map(|_| {
                    let mut view = view(width, height);
                    for _ in 0..height {
                        view.feed(b"populated terminal output\r\n");
                    }
                    view.prepare().expect("initial paint");
                    view
                })
                .collect::<Vec<_>>();
            let mut target = Buffer::empty(screen);
            let started = Instant::now();
            for tick in 0..100 {
                for (index, view) in views.iter_mut().enumerate() {
                    view.feed(format!("\x1b[H{tick:04}").as_bytes());
                    view.prepare().expect("updated paint");
                    let index = u16::try_from(index).expect("pane index");
                    let area = Rect::new(
                        (index % columns) * width,
                        (index / columns) * height,
                        width,
                        height,
                    );
                    black_box(paint_terminal(view, &mut target, area, None, false));
                }
            }
            let elapsed = started.elapsed();
            let idle_started = Instant::now();
            for _ in 0..100 {
                for view in &mut views {
                    assert!(!view.prepare().expect("idle view"));
                }
            }
            let idle = idle_started.elapsed();
            let baseline = *baseline.get_or_insert(elapsed.as_secs_f64());
            println!(
                "panes={panes} fixed_cells={} update_us={} idle_us={} ratio={:.3}",
                screen.width * screen.height,
                elapsed.as_micros(),
                idle.as_micros(),
                elapsed.as_secs_f64() / baseline
            );
        }
    }
}
