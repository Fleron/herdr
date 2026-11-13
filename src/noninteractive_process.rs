pub(crate) use crate::process_command::command;
use std::process::Command;

pub(crate) fn curl_command() -> Command {
    command("curl")
}
