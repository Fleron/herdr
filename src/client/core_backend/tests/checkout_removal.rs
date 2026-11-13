use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

use herdr_core_tui::model::{Document, Operation, Tab, Workspace};
use serde_json::json;

use super::{backend, core_support::wait_until, core_support::CoreSession, start};

struct GitGate(PathBuf);

impl Drop for GitGate {
    fn drop(&mut self) {
        if let Ok(mut gate) = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.0)
        {
            let _ = gate.write_all(b"continue\n");
        }
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn checkout_removal_stays_pending_through_real_git_and_queues_its_layout_continuation() {
    for (disconnected, discard_layout) in [(false, false), (true, false), (false, true)] {
        let session = CoreSession::new();
        let repository = session.repository("remove-checkout");
        let checkout = session.root.join("linked");
        session.git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "linked",
                checkout.to_str().unwrap(),
            ],
        );
        let checkout = std::fs::canonicalize(checkout).unwrap();
        let client = session.client();
        let program = start(&client, repository.to_str().unwrap(), "exec sleep 60");
        let address = program["address"].as_str().unwrap();
        let mut workspace = Workspace::new("checkout view", checkout.to_str().unwrap(), None);
        workspace.tabs.push(Tab::new("outside checkout", address));
        let workspace_id = workspace.id.clone();
        let mut backend = backend(&session);
        backend
            .apply_operation(None, Operation::AddWorkspace(workspace))
            .unwrap();
        wait_until(|| {
            backend.poll().unwrap();
            !backend.pending()
                && backend
                    .repository(None, checkout.to_str().unwrap())
                    .is_some()
        });
        let target = crate::client::ui_model::CheckoutTarget {
            machine_id: backend.local_id().to_owned(),
            folder: checkout.to_str().unwrap().into(),
            label: "linked".into(),
            repo_key: Some(
                backend
                    .repository(None, checkout.to_str().unwrap())
                    .unwrap()
                    .key
                    .clone(),
            ),
        };

        let fifo = session.root.join("git-removal-gate");
        assert!(session
            .command("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let gate = GitGate(fifo.clone());
        let ready = session.root.join("git-removal-ready");
        let once = session.root.join("git-removal-once");
        let hook = session.root.join("fsmonitor-gate");
        let quote = |path: &std::path::Path| {
            format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
        };
        std::fs::write(&hook, format!(
            "#!/bin/sh\nif mkdir {} 2>/dev/null; then\n  exec 3<>{}\n  : >{}\n  IFS= read -r released <&3\nfi\nexit 1\n",
            quote(&once), quote(&fifo), quote(&ready),
        )).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        session.git(
            &repository,
            &["config", "core.fsmonitor", hook.to_str().unwrap()],
        );

        backend.remove_checkout(&target, None).unwrap();
        assert!(backend.pending_mutation());
        assert!(
            !backend.pending(),
            "Git work precedes its layout continuation"
        );
        wait_until(|| ready.exists());
        assert!(checkout.is_dir());
        assert!(backend.document().workspace(&workspace_id).is_some());
        if discard_layout {
            backend
                .apply_operation(
                    None,
                    Operation::RenameWorkspace {
                        workspace: workspace_id.clone(),
                        name: "discard during removal".into(),
                    },
                )
                .unwrap();
        }
        backend.discard_pending().unwrap();
        if discard_layout {
            assert!(!backend.pending());
            assert!(!backend.stores[&None].loaded);
            assert!(backend.document().workspace(&workspace_id).is_none());
        }
        assert!(
            backend.pending_mutation(),
            "discarding layouts cannot cancel approved Git work"
        );
        if disconnected {
            backend.disconnected();
            assert!(
                backend.pending_mutation(),
                "disconnect cannot forget running Git work"
            );
        }
        drop(gate);
        if discard_layout {
            let mut delayed = Vec::new();
            loop {
                let job = backend.jobs.recv_timeout(Duration::from_secs(10)).unwrap();
                if matches!(&job, super::super::jobs::JobResult::RemoveCheckout { .. }) {
                    let mut change = super::super::Change::default();
                    backend.apply_job(job, &mut change).unwrap();
                    assert!(!checkout.exists());
                    assert!(
                        backend.pending_mutation(),
                        "successful Git removal still needs an authoritative layout for cleanup"
                    );
                    assert!(!backend.pending());
                    assert!(change.completions.is_empty());
                    assert!(change.notices.is_empty());
                    break;
                }
                delayed.push(job);
            }
            for job in delayed {
                backend.job_sender.send(job).unwrap();
            }
        }

        let mut finished = false;
        let mut notices = Vec::new();
        wait_until(|| {
            let change = backend.poll().unwrap();
            notices.extend(change.notices);
            if change.mutation {
                assert!(!backend.pending_mutation());
                if !disconnected {
                    assert!(backend.document().workspace(&workspace_id).is_none());
                    assert!(
                        backend.pending(),
                        "layout persistence owns the remaining continuation"
                    );
                }
                finished = true;
            }
            finished
        });
        assert!(!checkout.exists());
        if disconnected {
            assert!(backend.document().workspace(&workspace_id).is_some());
            assert!(notices
                .iter()
                .any(|notice| notice.contains("saved views were left unchanged")));
        } else {
            wait_until(|| {
                backend.poll().unwrap();
                !backend.pending()
            });
            let saved = client
                .request("doc.get", json!({"name": "herdr-tui"}))
                .unwrap();
            let saved = Document::from_value(saved["value"].clone()).unwrap();
            assert!(saved.workspace(&workspace_id).is_none());
        }
        assert_eq!(
            client
                .request("terminal.get", json!({"terminal":address}))
                .unwrap()["state"],
            "live"
        );
        drop(backend);
        session.finish();
    }
}
