use std::collections::BTreeMap;
use std::fmt;
use std::io;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::connection::{self, Client};

pub struct Output {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Output {
    pub fn ensure_success(self, action: &str) -> Result<Self, Error> {
        if self.code == Some(0) && self.signal.is_none() {
            Ok(self)
        } else {
            Err(self.command_error(action))
        }
    }

    pub fn command_error(self, action: &str) -> Error {
        Error::Command {
            action: action.to_string(),
            code: self.code,
            signal: self.signal,
            stderr: self.stderr,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Core {
        action: String,
        error: connection::Error,
    },
    InvalidReply {
        action: String,
        reason: String,
    },
    TimedOut {
        action: String,
        truncated: bool,
        code: Option<i32>,
        signal: Option<i32>,
        stderr: String,
    },
    Truncated {
        action: String,
        code: Option<i32>,
        signal: Option<i32>,
        stderr: String,
    },
    Command {
        action: String,
        code: Option<i32>,
        signal: Option<i32>,
        stderr: String,
    },
}

impl Error {
    pub(crate) fn io_error_kind(&self) -> io::ErrorKind {
        match self {
            Self::Core {
                error: connection::Error::Transport { kind, .. },
                ..
            } => *kind,
            Self::Core {
                error: connection::Error::Protocol(_),
                ..
            }
            | Self::InvalidReply { .. } => io::ErrorKind::InvalidData,
            Self::TimedOut { .. } => io::ErrorKind::TimedOut,
            _ => io::ErrorKind::Other,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core { action, error } => write!(f, "{action}: {error}"),
            Self::InvalidReply { action, reason } => {
                write!(f, "{action}: invalid machine.exec reply: {reason}")
            }
            Self::TimedOut {
                action,
                truncated,
                code,
                signal,
                stderr,
            } => {
                write!(f, "{action} timed out")?;
                if *truncated {
                    f.write_str(" and exceeded core's output limit")?;
                }
                write!(
                    f,
                    " (exit {code:?}, signal {signal:?}); output is incomplete"
                )?;
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
            Self::Truncated {
                action,
                code,
                signal,
                stderr,
            } => {
                write!(
                    f,
                    "{action} exceeded core's output limit (exit {code:?}, signal {signal:?}); output is incomplete"
                )?;
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
            Self::Command {
                action,
                code,
                signal,
                stderr,
            } => {
                write!(f, "{action} failed (exit {code:?}, signal {signal:?})")?;
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<Error> for io::Error {
    fn from(error: Error) -> Self {
        Self::new(error.io_error_kind(), error)
    }
}

pub fn exec(
    client: &Client,
    on: Option<&str>,
    cwd: Option<&str>,
    argv: &[&str],
    env: &[(&str, &str)],
    action: &str,
) -> Result<Output, Error> {
    let env: BTreeMap<_, _> = env.iter().copied().collect();
    let reply = client
        .request(
            "machine.exec",
            json!({"argv":argv,"on":on,"cwd":cwd,"env":env}),
        )
        .map_err(|error| Error::Core {
            action: action.to_string(),
            error,
        })?;
    decode(reply, action)
}

pub fn exec_input(
    client: &Client,
    on: Option<&str>,
    argv: &[&str],
    input: &[u8],
    action: &str,
) -> Result<Output, Error> {
    let reply = client
        .request(
            "machine.exec",
            json!({"argv": argv, "on": on, "input": STANDARD.encode(input)}),
        )
        .map_err(|error| Error::Core {
            action: action.to_string(),
            error,
        })?;
    decode(reply, action)
}

pub fn resolve_folder(
    client: &Client,
    on: Option<&str>,
    base: Option<&str>,
    folder: &str,
) -> io::Result<String> {
    let script = "case \"$1\" in '~') p=$HOME;; '~/'*) p=$HOME/${1#??};; *) p=$1;; esac; cd -- \"$p\" && pwd -P";
    let output = exec(
        client,
        on,
        base,
        &["sh", "-c", script, "herdr-folder", folder],
        &[],
        "resolving folder",
    )?
    .ensure_success("resolving folder")?;
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(io::Error::other("folder lookup returned an empty path"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

fn decode(reply: Value, action: &str) -> Result<Output, Error> {
    let invalid = |reason| Error::InvalidReply {
        action: action.to_string(),
        reason,
    };
    let boolean = |name| {
        reply
            .get(name)
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid(format!("missing or invalid {name}")))
    };
    let integer = |name| match reply.get(name) {
        Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| invalid(format!("invalid {name}"))),
        None => Err(invalid(format!("missing {name}"))),
    };
    let bytes = |name| {
        let encoded = reply
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("missing or invalid {name}")))?;
        STANDARD
            .decode(encoded)
            .map_err(|error| invalid(format!("invalid {name} encoding: {error}")))
    };
    let timed_out = boolean("timed_out")?;
    let truncated = boolean("truncated")?;
    let output = Output {
        code: integer("code")?,
        signal: integer("signal")?,
        stdout: bytes("stdout")?,
        stderr: String::from_utf8_lossy(&bytes("stderr")?)
            .trim()
            .to_string(),
    };
    if timed_out {
        return Err(Error::TimedOut {
            action: action.to_string(),
            truncated,
            code: output.code,
            signal: output.signal,
            stderr: output.stderr,
        });
    }
    if truncated {
        return Err(Error::Truncated {
            action: action.to_string(),
            code: output.code,
            signal: output.signal,
            stderr: output.stderr,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Value {
        json!({"code":0,"signal":null,"stdout":"","stderr":"","truncated":false,"timed_out":false})
    }

    #[test]
    fn incomplete_exec_results_cannot_become_a_complete_list_or_clean_status() {
        let mut truncated = reply();
        truncated["truncated"] = json!(true);
        assert!(matches!(
            decode(truncated, "status"),
            Err(Error::Truncated {
                code: Some(0),
                signal: None,
                ..
            })
        ));
        let mut timed_out = reply();
        timed_out["timed_out"] = json!(true);
        assert!(matches!(
            decode(timed_out, "status"),
            Err(Error::TimedOut {
                truncated: false,
                ..
            })
        ));
        let mut malformed = reply();
        malformed["stdout"] = json!("***");
        assert!(matches!(
            decode(malformed, "status"),
            Err(Error::InvalidReply { .. })
        ));
    }

    #[test]
    fn missing_and_malformed_required_fields_are_protocol_errors() {
        for field in [
            "code",
            "signal",
            "stdout",
            "stderr",
            "truncated",
            "timed_out",
        ] {
            let mut missing = reply();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                matches!(
                    decode(missing, "folder lookup"),
                    Err(Error::InvalidReply { .. })
                ),
                "missing {field}"
            );
        }
        for (field, value) in [
            ("code", json!("0")),
            ("signal", json!(i64::MAX)),
            ("truncated", Value::Null),
            ("timed_out", json!(0)),
            ("stderr", json!("***")),
        ] {
            let mut invalid = reply();
            invalid[field] = value;
            assert!(
                matches!(
                    decode(invalid, "folder lookup"),
                    Err(Error::InvalidReply { .. })
                ),
                "invalid {field}"
            );
        }
    }

    #[test]
    fn exit_status_and_signal_failures_preserve_observed_details() {
        let mut failed = reply();
        failed["code"] = json!(7);
        failed["stderr"] = json!(STANDARD.encode("refused"));
        let error = decode(failed, "Git")
            .unwrap()
            .ensure_success("Git")
            .err()
            .unwrap();
        assert!(
            matches!(error, Error::Command { code: Some(7), signal: None, ref stderr, .. } if stderr == "refused")
        );
        let mut signaled = reply();
        signaled["signal"] = json!(9);
        assert!(matches!(
            decode(signaled, "Git").unwrap().ensure_success("Git"),
            Err(Error::Command {
                signal: Some(9),
                ..
            })
        ));
    }

    #[test]
    fn simultaneous_timeout_and_truncation_remain_distinguishable() {
        let mut incomplete = reply();
        incomplete["timed_out"] = json!(true);
        incomplete["truncated"] = json!(true);
        incomplete["code"] = Value::Null;
        incomplete["signal"] = json!(9);
        let error = decode(incomplete, "folder lookup").err().unwrap();
        assert!(matches!(
            error,
            Error::TimedOut {
                truncated: true,
                code: None,
                signal: Some(9),
                ..
            }
        ));
        assert!(error.to_string().contains("timed out and exceeded"));
    }

    #[test]
    fn successful_binary_output_is_preserved() {
        let expected = b"worktree /repo/a\nline\0\0";
        let mut complete = reply();
        complete["stdout"] = json!(STANDARD.encode(expected));
        let output = decode(complete, "listing worktrees")
            .unwrap()
            .ensure_success("listing worktrees")
            .unwrap();
        assert_eq!(output.stdout, expected);
    }
}
