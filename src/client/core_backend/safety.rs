use std::io;

use herdr_core_tui::connection::Client;
use serde_json::Value;

pub(super) fn owner_inventory(
    client: &Client,
    owner: Option<&str>,
    expected_id: &str,
) -> io::Result<Vec<Value>> {
    let action = "checking the owning core before removing a worktree";
    let output = herdr_core_tui::machine::exec(
        client,
        owner,
        None,
        &["sh", "-c", SCRIPT, "herdr-owner-inventory"],
        &[],
        action,
    )?
    .ensure_success(action)?;
    decode_owner_inventory(&output.stdout, owner, expected_id)
}

fn decode_owner_inventory(
    bytes: &[u8],
    owner: Option<&str>,
    expected_id: &str,
) -> io::Result<Vec<Value>> {
    let parts = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    if parts.len() != 4 || !parts[3].is_empty() {
        return Err(invalid(
            "owner inventory did not contain two status records and one snapshot",
        ));
    }
    let before: Value = serde_json::from_slice(parts[0]).map_err(invalid)?;
    let snapshot: Value = serde_json::from_slice(parts[1]).map_err(invalid)?;
    let after: Value = serde_json::from_slice(parts[2]).map_err(invalid)?;
    for status in [&before, &after] {
        if status.pointer("/core/id").and_then(Value::as_str) != Some(expected_id) {
            return Err(invalid(
                "the owner-side hc command connected to a different core; no worktree was removed",
            ));
        }
    }
    if before["pid"].as_u64().is_none_or(|pid| pid == 0) || before["pid"] != after["pid"] {
        return Err(invalid(
            "the owning core restarted during the inventory check; retry after refreshing",
        ));
    }
    if snapshot["seq"].as_u64().is_none() {
        return Err(invalid("owner inventory has no event sequence"));
    }
    let records = snapshot["terminals"]
        .as_array()
        .ok_or_else(|| invalid("owner inventory has no terminals array"))?;
    let mut terminals = Vec::new();
    let mut addresses = std::collections::BTreeSet::new();
    for record in records {
        let address = record["address"]
            .as_str()
            .filter(|address| !address.is_empty())
            .ok_or_else(|| invalid("owner inventory contains a terminal with no address"))?;
        if address.contains('/') {
            continue;
        }
        if !addresses.insert(address) {
            return Err(invalid(
                "owner inventory repeated a local terminal identity",
            ));
        }
        match record["state"].as_str() {
            Some("finished") => continue,
            Some("live") => {}
            _ => {
                return Err(invalid(
                    "owner inventory contains an unknown terminal lifecycle state",
                ));
            }
        }
        if record["cwd"].as_str().is_none_or(str::is_empty) {
            return Err(invalid(
                "owner inventory contains a live terminal without its working directory",
            ));
        }
        for path in ["/last_cwd", "/foreground/cwd"] {
            if record
                .pointer(path)
                .is_some_and(|value| !value.is_null() && value.as_str().is_none_or(str::is_empty))
            {
                return Err(invalid(
                    "owner inventory contains an invalid current working directory",
                ));
            }
        }
        if record
            .get("foreground")
            .is_some_and(|value| !value.is_null() && value.get("cwd").is_none())
        {
            return Err(invalid(
                "owner inventory contains foreground state without its current working directory",
            ));
        }
        let mut record = record.clone();
        record["address"] = Value::String(herdr_core_tui::model::terminal_address(owner, address));
        terminals.push(record);
    }
    Ok(terminals)
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

const SCRIPT: &str = r#"set -eu
if command -v hc >/dev/null 2>&1; then
    hc=hc
elif test -x "$HOME/.local/bin/hc"; then
    hc=$HOME/.local/bin/hc
else
    printf '%s\n' 'hc is not available on the owning machine' >&2
    exit 1
fi
"$hc" server status --json
printf '\000'
dir=$(mktemp -d "${TMPDIR:-/tmp}/herdr-inventory.XXXXXX")
pid=''
cleanup() {
    if test -n "$pid"; then
        kill "$pid" 2>/dev/null || :
        wait "$pid" 2>/dev/null || :
    fi
    rm -f -- "$dir/events"
    rmdir -- "$dir"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
mkfifo "$dir/events"
"$hc" events --json > "$dir/events" &
pid=$!
IFS= read -r snapshot < "$dir/events"
kill "$pid" 2>/dev/null || :
wait "$pid" 2>/dev/null || :
pid=''
printf '%s\000' "$snapshot"
"$hc" server status --json
printf '\000'
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(before: &Value, snapshot: &Value, after: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [before, snapshot, after] {
            bytes.extend(serde_json::to_vec(value).unwrap());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn destructive_inventory_rejects_restart_partial_schema_and_truncation() {
        let status = json!({"core": {"id": "owner"}, "pid": 42});
        let snapshot = json!({"seq": 8, "terminals": [{"address": "#1", "state": "live", "cwd": "/repo", "foreground": {"cwd": "/repo/work"}}]});
        let encoded = payload(&status, &snapshot, &status);
        let records = decode_owner_inventory(&encoded, Some("owner"), "owner").unwrap();
        assert_eq!(records[0]["address"], "owner/#1");
        assert_eq!(records[0]["foreground"]["cwd"], "/repo/work");
        assert!(decode_owner_inventory(&encoded[..encoded.len() - 1], None, "owner").is_err());
        assert!(decode_owner_inventory(&encoded, None, "different").is_err());
        let restarted = json!({"core": {"id": "owner"}, "pid": 43});
        assert!(
            decode_owner_inventory(&payload(&status, &snapshot, &restarted), None, "owner")
                .is_err()
        );
        for snapshot in [
            json!({"seq": 8}),
            json!({"seq": 8, "terminals": [{"address": "#1", "state": "live", "cwd": "/repo", "foreground": {}}]}),
            json!({"seq": 8, "terminals": [{"address": "#1", "state": "unknown", "cwd": "/repo"}]}),
        ] {
            assert!(
                decode_owner_inventory(&payload(&status, &snapshot, &status), None, "owner")
                    .is_err()
            );
        }
    }
}
