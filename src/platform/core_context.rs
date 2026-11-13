use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use herdr_core_tui::connection::Client;
use herdr_core_tui::protocol::MAX_FRAME;
use serde_json::{json, Value};

pub(crate) fn caller(client: &Client) -> Option<String> {
    let terminal = std::env::var("HC_TERMINAL_ID").ok()?;
    let socket = std::env::var_os("HC_TERMINAL_SOCKET")?;
    let mut connection = UnixStream::connect(socket).ok()?;
    connection
        .set_read_timeout(Some(Duration::from_secs(1)))
        .ok()?;
    connection
        .set_write_timeout(Some(Duration::from_secs(1)))
        .ok()?;
    serde_json::to_writer(
        &mut connection,
        &json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{}}),
    )
    .ok()?;
    connection.write_all(b"\n").ok()?;
    let mut line = Vec::new();
    BufReader::new(connection.take((MAX_FRAME + 1) as u64))
        .read_until(b'\n', &mut line)
        .ok()?;
    if line.len() > MAX_FRAME {
        return None;
    }
    let reply: Value = serde_json::from_slice(&line).ok()?;
    let inherited = reply.get("result")?.get("core")?.get("id")?.as_str()?;
    let current = client.hello().get("core")?.get("id")?.as_str()?;
    (inherited == current).then_some(terminal)
}
