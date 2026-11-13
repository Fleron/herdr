use std::collections::HashSet;
use std::hint::black_box;
use std::sync::mpsc;
use std::time::Instant;

use herdr_core_tui::frame::render_ansi::BlitEncoder;
use herdr_core_tui::model::{new_id, Axis, LayoutNode, Operation, Pane, Tab, ViewState, Workspace};
use herdr_core_tui::persistence::LayoutStore;
use serde_json::json;

use super::{
    surface::SurfaceBuilder, Backend, ClientEndpointId, ClientShellConfig, ClientShellState,
};
use crate::config::Config;
use crate::kitty_graphics::HostCellSize;

use crate::core_test_support as core_support;
use core_support::{wait_until, CoreSession};

fn tree(panes: &[Pane], depth: usize) -> LayoutNode {
    if panes.len() == 1 {
        return LayoutNode::Pane {
            id: panes[0].id.clone(),
            extra: Default::default(),
        };
    }
    let middle = panes.len() / 2;
    LayoutNode::Split {
        id: new_id(),
        axis: if depth.is_multiple_of(2) {
            Axis::Horizontal
        } else {
            Axis::Vertical
        },
        ratio: middle as f32 / panes.len() as f32,
        first: Box::new(tree(&panes[..middle], depth + 1)),
        second: Box::new(tree(&panes[middle..], depth + 1)),
        extra: Default::default(),
    }
}

#[test]
#[ignore = "manual whole frontend rendering profile with real core terminals"]
fn render_scale_profile() {
    let mut baseline = None;
    for count in [1, 15] {
        let session = CoreSession::new();
        let client = session.client();
        let mut panes = Vec::new();
        for _ in 0..count {
            let record = client.request("terminal.run", json!({
                "argv":["/bin/sh","-c","printf '\\033[32mPROFILE_READY\\033[0m\\n'; exec cat"],
                "no_parent":true,
            })).unwrap();
            panes.push(Pane::new(record["address"].as_str().unwrap()));
        }
        let mut tab = Tab::new("profile-tab", &panes[0].terminal);
        tab.layout = tree(&panes, 0);
        tab.panes = panes;
        let mut workspace = Workspace::new("profile", session.root.to_str().unwrap(), None);
        workspace.tabs.push(tab);
        let mut store = LayoutStore::load(&client, None).unwrap();
        store.apply(Operation::AddWorkspace(workspace)).unwrap();
        store.save(&client).unwrap();
        let (wake, _) = mpsc::sync_channel(1);
        let mut backend =
            Backend::from_client(client.clone(), ViewState::default(), wake.clone()).unwrap();
        wait_until(|| {
            backend.poll().unwrap();
            backend.layouts_ready()
        });
        let config = Config::default();
        let mut shell = ClientShellState::new(
            ClientShellConfig::from_config(&config).with_startup_onboarding(false),
        );
        shell.set_core_catalog(&backend.machine_views());
        let endpoint = ClientEndpointId::Core {
            machine_id: backend.local_id().into(),
            local: true,
        };
        let snapshot = backend.snapshot(None);
        shell.set_endpoint_snapshot_for_generation(
            &endpoint,
            backend.generation(),
            Box::new(snapshot.clone()),
        );
        assert!(shell.activate_endpoint_projection(&endpoint));
        let mut streams = herdr_core_tui::surfaces::Streams::new(
            client,
            Default::default(),
            None,
            1_000_000,
            wake,
        );
        let size = shell.surface_size(120, 45);
        let geometry = SurfaceBuilder::geometry(
            backend.document(),
            backend.view(),
            backend.inventory(),
            &streams,
            (size.cols, size.rows),
            &config,
        )
        .unwrap();
        assert_eq!(geometry.specs.len(), count);
        let retained: HashSet<_> = geometry.specs.iter().map(|spec| spec.id.clone()).collect();
        streams.reconcile(&geometry.specs, &retained).unwrap();
        wait_until(|| {
            streams.poll().unwrap();
            geometry.specs.iter().all(|spec| {
                streams
                    .pane(&spec.id)
                    .is_some_and(|pane| pane.input_ready())
            })
        });
        let mut surface = SurfaceBuilder::default();
        let mut encoder = BlitEncoder::new();
        let frame = |iteration,
                     streams: &mut herdr_core_tui::surfaces::Streams,
                     surface: &mut SurfaceBuilder,
                     shell: &mut ClientShellState,
                     encoder: &mut BlitEncoder| {
            for spec in &geometry.specs {
                streams.pane_mut(&spec.id).unwrap().view.feed(
                    format!("\x1b[1;1H\x1b[3{}mPROFILE {iteration}", iteration % 7).as_bytes(),
                );
            }
            SurfaceBuilder::prepare_visible(&geometry, streams).unwrap();
            let composed = surface
                .build(
                    &snapshot,
                    &geometry,
                    streams,
                    &config,
                    shell.palette(),
                    HostCellSize::default(),
                )
                .unwrap();
            shell.set_pane_surface(composed);
            let frame = shell.compose(120, 45).unwrap().frame;
            let encoded = encoder.encode(&frame, false);
            black_box(encoded.bytes.len());
            encoder.commit(frame, encoded);
        };
        for iteration in 0..10 {
            frame(
                iteration,
                &mut streams,
                &mut surface,
                &mut shell,
                &mut encoder,
            );
        }
        let started = Instant::now();
        for iteration in 10..210 {
            frame(
                iteration,
                &mut streams,
                &mut surface,
                &mut shell,
                &mut encoder,
            );
        }
        let nanos = started.elapsed().as_nanos() / 200;
        let first = *baseline.get_or_insert(nanos);
        println!(
            "regular frontend panes={count} screen_cells=5400 ns_per_frame={nanos} scale={:.2}",
            nanos as f64 / first as f64
        );
        drop(streams);
        drop(backend);
        session.finish();
    }
}
