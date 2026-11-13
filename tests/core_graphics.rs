#![cfg(unix)]

mod core_support;

use std::time::{Duration, Instant};

use base64::Engine as _;
use core_support::CoreSession;
use herdr_core_tui::connection::{AttachOptions, Attachment, Client, StreamEvent};
use herdr_core_tui::kitty_graphics;
use herdr_core_tui::terminal::TerminalView;
use herdr_core_tui::{ghostty, layout};
use kitty_graphics::model::{SurfaceGraphicsFormat, SurfaceGraphicsTarget};
use kitty_graphics::surface::{
    collect_terminal_scene, ClientState, DeliveryCache, Occlusion, TerminalPane, Visibility,
};
use kitty_graphics::HostCellSize;
use ratatui::layout::Rect;
use serde_json::json;

fn receive_until(
    attachment: &Attachment,
    view: &mut TerminalView,
    mut ready: impl FnMut(&TerminalView) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready(view) {
        assert!(
            Instant::now() < deadline,
            "terminal graphics did not arrive"
        );
        match attachment.recv_timeout(Duration::from_millis(100)) {
            Ok(StreamEvent::Data(bytes)) => {
                view.feed(&bytes);
                for reply in view.take_replies() {
                    attachment.automatic_input(&reply).unwrap();
                }
                attachment.credit(bytes.len()).unwrap();
                view.prepare().unwrap();
            }
            Ok(StreamEvent::Resize { cols, rows }) => {
                view.resize(cols, rows).unwrap();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            event => panic!("terminal stream ended before graphics arrived: {event:?}"),
        }
    }
}

fn screen(view: &TerminalView) -> String {
    view.buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn program(client: &Client, png: &[u8]) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(png);
    let script = format!(
        "stty -echo; printf 'IMAGE_WAITING'; IFS= read -r line; \
         printf '\\033_Ga=T,f=100,t=d,i=7,p=3,c=4,r=2,q=2;{payload}\\033\\\\'; \
         printf 'IMAGE_DRAWN'; \
         while IFS= read -r line; do \
           if [ \"$line\" = delete ]; then \
             printf '\\033_Ga=d,d=I,i=7,q=2\\033\\\\IMAGE_DELETED'; \
           fi; \
         done"
    );
    client
        .request(
            "terminal.run",
            json!({
                "argv": ["/bin/sh", "-c", script],
                "no_parent": true,
                "size": {"cols": 40, "rows": 12}
            }),
        )
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn live_core_png_uses_the_regular_graphics_compositor_and_deletes_cleanly() {
    let session = CoreSession::new();
    let client = session.client();
    let pixels = [255, 0, 0, 255, 0, 255, 0, 255];
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
    }
    let address = program(&client, &png);
    let attachment = client
        .attach(&AttachOptions::new(&address, 40, 12))
        .unwrap();
    let mut view = TerminalView::new(40, 12, 1024 * 1024).unwrap();
    view.set_cell_size(8, 16).unwrap();
    receive_until(&attachment, &mut view, |view| {
        screen(view).contains("IMAGE_WAITING")
    });
    attachment.send_data(b"draw\n").unwrap();
    receive_until(&attachment, &mut view, |view| {
        !view.terminal().kitty_image_placements().unwrap().is_empty()
            && screen(view).contains("IMAGE_DRAWN")
    });
    let cell = HostCellSize {
        width_px: 8,
        height_px: 16,
    };
    let collect = |view: &TerminalView, delivered: &DeliveryCache| {
        collect_terminal_scene(
            &[TerminalPane {
                pane_id: layout::PaneId::from_raw(1),
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "core-pane".into(),
                },
                area: Rect::new(0, 0, 40, 12),
                scrollback_offset: 0,
                terminal: view.terminal(),
            }],
            cell,
            delivered,
        )
        .unwrap()
    };
    let (scene, delivered, _) = collect(&view, &DeliveryCache::default());
    assert_eq!(scene.placements.len(), 1);
    assert_eq!(scene.assets.len(), 1);
    match scene.assets[0].key.format {
        SurfaceGraphicsFormat::Png => assert_eq!(scene.assets[0].data, png),
        SurfaceGraphicsFormat::Rgba => assert_eq!(scene.assets[0].data, pixels),
        SurfaceGraphicsFormat::Rgb => panic!("RGBA upload lost its alpha channel"),
    }
    let mut compositor = ClientState::new();
    compositor.set_scope("actual-core-png");
    compositor.set_scene(scene);
    let output = String::from_utf8(
        compositor
            .encode_output(Visibility::Main, (0, 0), None, cell, &Occlusion::default())
            .into_inline_bytes(),
    )
    .unwrap();
    assert!(output.contains("a=t,"));
    assert!(output.contains("a=p,"));

    attachment.send_data(b"delete\n").unwrap();
    receive_until(&attachment, &mut view, |view| {
        view.terminal().kitty_image_placements().unwrap().is_empty()
            && screen(view).contains("IMAGE_DELETED")
    });
    let (scene, _, _) = collect(&view, &delivered);
    assert!(scene.placements.is_empty());
    compositor.set_scene(scene);
    let cleanup = String::from_utf8(
        compositor
            .encode_output(Visibility::Main, (0, 0), None, cell, &Occlusion::default())
            .into_inline_bytes(),
    )
    .unwrap();
    assert!(cleanup.contains("a=d,"));
    assert!(client.error().is_none());
    drop(attachment);
    assert_eq!(
        client
            .request("terminal.get", json!({"terminal": address}))
            .unwrap()["state"],
        "live"
    );
    session.finish();
}

#[test]
#[ignore = "manual fixed-geometry graphics collection profile"]
fn fixed_geometry_graphics_collection_profile() {
    let cell = HostCellSize {
        width_px: 8,
        height_px: 16,
    };
    for count in [1, 15] {
        let (cols, rows) = if count == 1 { (120, 45) } else { (24, 15) };
        let mut terminals = Vec::new();
        for _ in 0..count {
            let mut terminal = ghostty::Terminal::new(cols, rows, 1024 * 1024).unwrap();
            terminal.enable_inline_kitty_graphics().unwrap();
            terminal.resize(cols, rows, 8, 16).unwrap();
            terminal.write(b"populated terminal\r\n");
            terminals.push(terminal);
        }
        for images in [false, true] {
            if images {
                for terminal in &mut terminals {
                    terminal
                        .write(b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=2,r=1,q=2;/wAA/w==\x1b\\");
                }
            }
            let panes: Vec<_> = terminals
                .iter()
                .enumerate()
                .map(|(index, terminal)| TerminalPane {
                    pane_id: layout::PaneId::from_raw(index as u32 + 1),
                    target: SurfaceGraphicsTarget::Pane {
                        pane_id: format!("pane-{index}"),
                    },
                    area: Rect::new(
                        (index % 5) as u16 * cols,
                        (index / 5) as u16 * rows,
                        cols,
                        rows,
                    ),
                    scrollback_offset: 0,
                    terminal,
                })
                .collect();
            let (_, delivered, _) =
                collect_terminal_scene(&panes, cell, &DeliveryCache::default()).unwrap();
            let (cached, _, _) = collect_terminal_scene(&panes, cell, &delivered).unwrap();
            assert!(cached.assets.is_empty());
            assert_eq!(cached.placements.len(), if images { count } else { 0 });
            let mut samples = Vec::new();
            for _ in 0..101 {
                let started = Instant::now();
                for _ in 0..100 {
                    std::hint::black_box(collect_terminal_scene(&panes, cell, &delivered).unwrap());
                }
                samples.push(started.elapsed().as_nanos() / 100);
            }
            samples.sort_unstable();
            eprintln!(
                "graphics collect cells=5400 panes={count} images={images} median_ns={} p95_ns={}",
                samples[50], samples[95]
            );
        }
    }
}
