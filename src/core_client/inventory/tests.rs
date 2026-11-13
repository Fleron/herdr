use super::*;
use serde_json::json;

fn record(address: &str) -> Value {
    json!({"address": address, "state": "live", "pid": 42, "title": "before", "keep": true,
        "foreground": {"pid": 42, "name": "sh", "argv": ["sh"], "cwd": "/work"}})
}

fn event(seq: u64, kind: &str, terminal: &str, data: Value) -> Value {
    json!({"method": "event", "params": {"seq": seq, "type": kind, "terminal": terminal, "data": data}})
}

fn box_profile(inventory: &mut Inventory) {
    inventory
        .set_machines(&[json!({"id": "core-box", "name": "box"})])
        .unwrap();
}

#[test]
fn loss_keeps_identity_live_while_status_and_turn_have_separate_meaning() {
    let mut inventory =
        Inventory::new(&json!({"seq": 10, "terminals": [record("box/t1")]})).unwrap();
    box_profile(&mut inventory);
    inventory
        .apply(&event(11, "lost", "box/t1", json!({"reason": "holder_lost", "exit_code": null, "signal": null, "finished_ms": null})))
        .unwrap();
    assert_eq!(inventory.get("box/t1").unwrap()["state"], "live");
    assert_eq!(inventory.get("box/t1").unwrap()["pid"], Value::Null);
    assert!(inventory.get("box/t1").unwrap().get("foreground").is_none());
    inventory
        .apply(&event(12, "status", "box/t1", json!({"status": {"state": "blocked", "detail": "permission"}, "records": {"child": {"state": "working"}}, "reported": true})))
        .unwrap();
    inventory
        .apply(&event(
            13,
            "turn",
            "box/t1",
            json!({"turn": 7, "state": "done"}),
        ))
        .unwrap();
    let terminal = inventory.get("box/t1").unwrap();
    assert_eq!(terminal["state"], "live");
    assert_eq!(terminal["turn"], 7);
    assert_eq!(terminal["status"]["detail"], "permission");
    assert_eq!(terminal["records"]["child"]["state"], "working");
    inventory
        .apply(&event(
            14,
            "finished",
            "box/t1",
            json!({"reason": "exited", "exit_code": 0, "signal": null, "finished_ms": 99}),
        ))
        .unwrap();
    assert_eq!(inventory.live().count(), 0);
    let mut replacement = record("box/t1");
    replacement["pid"] = json!(43);
    let change = inventory
        .apply(&event(15, "restarted", "box/t1", replacement))
        .unwrap();
    assert_eq!(change.restarted, ["core-box/t1"]);
    assert_eq!(inventory.live().count(), 1);
}

#[test]
fn gaps_require_resubscription_and_missed_replaces_the_complete_snapshot() {
    let mut inventory =
        Inventory::new(&json!({"seq": 10, "terminals": [record("t1"), record("box/t2")]})).unwrap();
    box_profile(&mut inventory);
    assert!(
        !inventory
            .apply(&event(9, "title", "t1", json!({"title": "old"})))
            .unwrap()
            .terminals
    );
    assert!(
        inventory
            .apply(&event(12, "title", "t1", json!({"title": "gap"})))
            .unwrap()
            .resubscribe
    );
    assert_eq!(inventory.seq, 10);
    assert_eq!(inventory.get("t1").unwrap()["title"], "before");
    let change = inventory
        .apply(&json!({"method": "missed", "params": {"seq": 12, "terminals": [record("box/t3")]}}))
        .unwrap();
    assert!(change.terminals && change.documents && change.refresh_machines);
    assert!(!inventory.identities_ready());
    box_profile(&mut inventory);
    assert_eq!(inventory.records().count(), 1);
    assert!(inventory.get("box/t3").is_some());
    assert!(inventory.get("t1").is_none());
    assert!(
        !inventory
            .apply(&event(11, "removed", "box/t3", json!({})))
            .unwrap()
            .terminals
    );
    let same_seq = inventory
        .apply(&json!({"method": "missed", "params": {"seq": 12, "terminals": [record("box/t3")]}}))
        .unwrap();
    assert!(same_seq.documents && same_seq.refresh_machines);
    box_profile(&mut inventory);
}

#[test]
fn document_events_keep_owner_name_and_version_without_inventing_terminal_data() {
    let mut inventory = Inventory::new(&json!({"seq": 0, "terminals": []})).unwrap();
    box_profile(&mut inventory);
    let change = inventory
        .apply(&json!({"method": "event", "params": {"seq": 1, "type": "doc", "machine": "box", "data": {"name": "herdr-tui", "version": 4}}}))
        .unwrap();
    assert!(change.documents && !change.terminals);
    assert_eq!(
        change.document,
        Some(DocumentChange {
            name: "herdr-tui".into(),
            machine: Some("core-box".into()),
            version: 4
        })
    );
    assert_eq!(inventory.records().count(), 0);
    assert!(
        inventory
            .apply(&json!({"method": "heartbeat", "params": {"seq": 2}}))
            .unwrap()
            .resubscribe
    );
    inventory.apply(&json!({"method": "event", "params": {"seq": 2, "type": "future-extension", "data": null}})).unwrap();
    assert_eq!(inventory.seq, 2);
    assert!(
        !inventory
            .apply(&json!({"method": "heartbeat", "params": {"seq": 2}}))
            .unwrap()
            .resubscribe
    );
}

#[test]
fn malformed_snapshots_and_events_do_not_silently_create_empty_state() {
    assert!(Inventory::new(&json!({"terminals": []})).is_err());
    assert!(Inventory::new(&json!({"seq": 0})).is_err());
    assert!(Inventory::new(&json!({"seq": 0, "terminals": [record("t1"), record("t1")]})).is_err());
    let mut inventory = Inventory::new(&json!({"seq": 0, "terminals": [record("t1")]})).unwrap();
    assert!(inventory
        .apply(&event(1, "created", "t1", record("t2")))
        .is_err());
    assert_eq!(inventory.seq, 0);
    assert!(inventory
        .apply(&event(1, "size", "t1", json!({"cols": 100})))
        .is_err());
    assert!(inventory.get("t1").unwrap().get("cols").is_none());
}

#[test]
fn observed_run_reply_fills_absent_identity_without_overwriting_newer_events() {
    let mut inventory = Inventory::new(&json!({"seq": 0, "terminals": []})).unwrap();
    assert!(inventory.insert_observed(record("t1")).unwrap());
    inventory
        .apply(&event(1, "title", "t1", json!({"title": "new"})))
        .unwrap();
    assert!(!inventory.insert_observed(record("t1")).unwrap());
    assert_eq!(inventory.get("t1").unwrap()["title"], "new");
    assert!(
        inventory
            .apply(&event(2, "labels", "t9", json!({"labels": {}})))
            .unwrap()
            .resubscribe
    );
    assert_eq!(inventory.seq, 1);
}

#[test]
fn complete_profiles_resolve_distinct_owners_without_rewriting_opaque_labels() {
    let mut remote = record("box/t1");
    remote["parent"] = json!("box/t3");
    remote["labels"] = json!({"parent": "box/opaque"});
    let mut inventory =
        Inventory::new(&json!({"seq": 4, "terminals": [record("t1"), remote, record("other/t1")]}))
            .unwrap();
    assert_eq!(inventory.records().count(), 1);
    assert!(inventory.terminals.contains_key("box/t1"));
    assert!(!inventory.identities_ready());
    let change = inventory
        .set_machines(&[
            json!({"id": "core-box", "name": "box"}),
            json!({"id": "core-other", "name": "other"}),
        ])
        .unwrap();
    assert!(change.terminals && !change.resubscribe);
    assert!(inventory.identities_ready());
    assert_eq!(inventory.records().count(), 3);
    assert_eq!(inventory.canonical_address("box/t1"), "core-box/t1");
    let remote = inventory.get("core-box/t1").unwrap();
    assert_eq!(remote["address"], "core-box/t1");
    assert_eq!(remote["parent"], "core-box/t3");
    assert_eq!(remote["labels"]["parent"], "box/opaque");
    assert!(inventory.get("core-other/t1").is_some());
    assert!(inventory.get("t1").is_some());
}

#[test]
fn renamed_profile_merges_staged_facts_by_event_revision_without_duplicate_identity() {
    let mut inventory =
        Inventory::new(&json!({"seq": 1, "terminals": [record("box/t1")]})).unwrap();
    box_profile(&mut inventory);
    let mut newer = record("renamed/t1");
    newer["title"] = json!("newer");
    let changed = inventory
        .apply(&event(2, "created", "renamed/t1", newer))
        .unwrap();
    assert!(changed.refresh_machines);
    assert_eq!(inventory.get("core-box/t1").unwrap()["title"], "before");
    assert!(inventory.get("renamed/t1").is_none());
    let resolved = inventory
        .set_machines(&[json!({"id": "core-box", "name": "renamed"})])
        .unwrap();
    assert!(!resolved.resubscribe);
    assert_eq!(inventory.terminals.len(), 1);
    assert_eq!(inventory.get("core-box/t1").unwrap()["title"], "newer");
    assert_eq!(inventory.machine_name("core-box"), Some("renamed"));
    assert!(inventory.get("box/t1").is_none());
    assert!(
        inventory
            .apply(&event(
                3,
                "title",
                "box/t1",
                json!({"title": "stale-alias"})
            ))
            .unwrap()
            .resubscribe
    );
    assert_eq!(inventory.get("core-box/t1").unwrap()["title"], "newer");
}

#[test]
fn reused_alias_never_retags_known_owner_or_staged_ambiguous_facts() {
    let mut inventory =
        Inventory::new(&json!({"seq": 1, "terminals": [record("box/t1")]})).unwrap();
    box_profile(&mut inventory);
    inventory.set_machines(&[]).unwrap();
    assert!(inventory.get("core-box/t1").is_none());
    let mut replacement = record("box/t1");
    replacement["pid"] = json!(900);
    inventory
        .apply(&event(2, "created", "box/t1", replacement.clone()))
        .unwrap();
    let change = inventory
        .set_machines(&[json!({"id": "new-core", "name": "box"})])
        .unwrap();
    assert!(change.resubscribe);
    assert_eq!(inventory.records().count(), 0);
    assert!(inventory.terminals.contains_key("box/t1"));
    assert!(inventory.get("new-core/t1").is_none());
    assert!(
        inventory
            .set_machines(&[json!({"id": "new-core", "name": "box"})])
            .unwrap()
            .resubscribe
    );
    assert!(inventory.get("new-core/t1").is_none());
    inventory
        .apply(&json!({"method": "missed", "params": {"seq": 2, "terminals": [replacement]}}))
        .unwrap();
    let change = inventory
        .set_machines(&[json!({"id": "new-core", "name": "box"})])
        .unwrap();
    assert!(!change.resubscribe);
    assert!(inventory.get("core-box/t1").is_none());
    assert_eq!(inventory.get("new-core/t1").unwrap()["pid"], 900);
}

#[test]
fn equally_recent_conflicting_records_request_a_snapshot_instead_of_picking_a_row() {
    let mut first = record("core-box/t1");
    first["title"] = json!("first");
    let mut second = record("box/t1");
    second["title"] = json!("second");
    let mut inventory = Inventory::new(&json!({"seq": 5, "terminals": [first, second]})).unwrap();
    let change = inventory
        .set_machines(&[json!({"id": "core-box", "name": "box"})])
        .unwrap();
    assert!(change.resubscribe);
    assert_eq!(inventory.terminals.len(), 2);
    assert_eq!(inventory.records().count(), 0);
    assert!(inventory.get("core-box/t1").is_none());
}

#[test]
fn incomplete_or_ambiguous_profiles_are_not_accepted_as_authoritative_identity() {
    let mut inventory =
        Inventory::new(&json!({"seq": 0, "terminals": [record("box/t1")]})).unwrap();
    assert!(inventory.set_machines(&[json!({"name": "box"})]).is_err());
    assert!(inventory
        .set_machines(&[
            json!({"id": "a", "name": "box"}),
            json!({"id": "a", "name": "other"})
        ])
        .is_err());
    assert!(inventory
        .set_machines(&[
            json!({"id": "a", "name": "box"}),
            json!({"id": "b", "name": "box"})
        ])
        .is_err());
    assert!(inventory
        .set_machines(&[
            json!({"id": "a", "name": "box"}),
            json!({"id": "b", "name": "a"})
        ])
        .is_err());
    assert!(inventory.set_machines(&[]).unwrap().resubscribe);
    assert!(inventory.terminals.contains_key("box/t1"));
    assert_eq!(inventory.records().count(), 0);
}

#[test]
fn listing_uses_captured_owner_even_when_current_inventory_reuses_its_alias() {
    let mut inventory =
        Inventory::new(&json!({"seq": 1, "terminals": [record("box/t1")]})).unwrap();
    inventory
        .set_machines(&[json!({"id": "new-owner", "name": "box"})])
        .unwrap();
    let listing = TerminalListing::parse(&json!({
        "terminals": [record("box/t1"), record("t1")],
        "machines": [{"id": "old-owner", "name": "box", "state": "connected"}]
    }))
    .unwrap();
    assert_eq!(listing.terminals[0].record["address"], "old-owner/t1");
    assert_eq!(listing.terminals[0].machine_name.as_deref(), Some("box"));
    assert_eq!(listing.terminals[1].record["address"], "t1");
    assert_eq!(listing.terminals[1].machine_name, None);
    assert_eq!(inventory.canonical_address("box/t1"), "new-owner/t1");
    assert!(listing.warnings.is_empty());
}

#[test]
fn listing_accepts_local_only_replies_with_omitted_empty_machines() {
    for records in [vec![], vec![record("t1")]] {
        let response = json!({"terminals": records, "hidden_finished": 0});
        let listing = TerminalListing::parse(&response).unwrap();
        assert_eq!(listing.terminals.len(), records.len());
        assert!(listing.warnings.is_empty());
        for (listed, expected) in listing.terminals.iter().zip(records) {
            assert_eq!(listed.record, expected);
            assert_eq!(listed.machine_name, None);
        }
    }
}

#[test]
fn listing_rejects_missing_owners_and_malformed_or_duplicate_records() {
    for value in [
        json!({"terminals": [record("box/t1")]}),
        json!({"terminals": [record("box/t1")], "machines": []}),
        json!({"terminals": [], "machines": null}),
        json!({"terminals": [], "machines": {}}),
        json!({"terminals": [], "machines": ""}),
        json!({"terminals": [record("t1"), record("t1")], "machines": []}),
        json!({"terminals": [{"address": "t1"}], "machines": []}),
        json!({"terminals": [], "machines": [{"id": "a", "name": "box"}]}),
    ] {
        assert!(TerminalListing::parse(&value).is_err(), "accepted {value}");
    }
    let missing_owner = TerminalListing::parse(&json!({
        "terminals": [record("box/t1")], "hidden_finished": 0
    }))
    .unwrap_err();
    assert!(missing_owner
        .to_string()
        .contains("no captured owner for box/t1"));
    let listing = TerminalListing::parse(&json!({"terminals": [], "machines": [
        {"id": "a", "name": "box", "state": "unreachable"}
    ]}))
    .unwrap();
    assert!(listing
        .warnings
        .iter()
        .any(|warning| warning == "box: unreachable"));
}
