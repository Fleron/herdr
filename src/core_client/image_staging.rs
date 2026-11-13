use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::connection::Client;
use crate::frame::MAX_CLIPBOARD_IMAGE_PAYLOAD;
use crate::machine;

// Core accepts at most 8 MiB of decoded exec stdin per call.
const WRITE_CHUNK_BYTES: usize = 4 * 1024 * 1024;

pub struct StagedImage {
    owner: Option<String>,
    owner_id: String,
    directory: String,
    path: String,
    token: String,
    attempted: bool,
}

impl StagedImage {
    pub fn prepare(client: &Client, owner: Option<&str>, extension: &str) -> io::Result<Self> {
        let owner_id = owner
            .or_else(|| client.hello().pointer("/core/id").and_then(Value::as_str))
            .ok_or_else(|| io::Error::other("the owning core has no identity"))?;
        let temporary = owner_temporary_directory(client, owner, owner_id)?;
        let token = crate::model::new_id();
        let directory = format!(
            "{}/herdr-clipboard-image-{token}",
            temporary.trim_end_matches('/')
        );
        let extension = match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => "jpg",
            "gif" => "gif",
            "webp" => "webp",
            "bmp" => "bmp",
            _ => "png",
        };
        Ok(Self {
            owner: owner.map(str::to_owned),
            owner_id: owner_id.to_owned(),
            path: format!("{directory}/image.{extension}"),
            directory,
            token,
            attempted: false,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    pub fn upload(
        &mut self,
        client: &Client,
        bytes: &[u8],
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        if self.attempted {
            return Err(io::Error::other("the image upload was already attempted"));
        }
        if bytes.is_empty() || bytes.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
            return Err(io::Error::other(
                "clipboard images must contain 1 byte to 16 MiB",
            ));
        }
        self.attempted = true;
        check_cancelled(cancelled)?;
        self.verify_owner(client)?;
        self.execute(client, CREATE, "creating a private image staging directory")?;
        for chunk in bytes.chunks(WRITE_CHUNK_BYTES) {
            check_cancelled(cancelled)?;
            machine::exec_input(
                client,
                self.owner(),
                &[
                    "sh",
                    "-ec",
                    APPEND,
                    "herdr-image-upload",
                    &self.directory,
                    &self.path,
                    &self.token,
                ],
                chunk,
                "uploading a clipboard image",
            )?
            .ensure_success("uploading a clipboard image")?;
        }
        check_cancelled(cancelled)
    }

    pub fn remove(&self, client: &Client) -> io::Result<()> {
        remove_many(client, &[self])
    }

    fn verify_owner(&self, client: &Client) -> io::Result<()> {
        owner_temporary_directory(client, self.owner(), &self.owner_id).map(|_| ())
    }

    fn execute(&self, client: &Client, script: &str, action: &str) -> io::Result<()> {
        machine::exec(
            client,
            self.owner(),
            None,
            &[
                "sh",
                "-ec",
                script,
                "herdr-image-stage",
                &self.directory,
                &self.path,
                &self.token,
            ],
            &[],
            action,
        )?
        .ensure_success(action)?;
        Ok(())
    }
}

pub fn remove_many(client: &Client, images: &[&StagedImage]) -> io::Result<()> {
    let mut owners = std::collections::BTreeMap::<_, Vec<_>>::new();
    for image in images {
        owners.entry(&image.owner_id).or_default().push(*image);
    }
    let mut failures = Vec::new();
    for images in owners.values() {
        if let Err(error) = images[0].verify_owner(client) {
            failures.push(error.to_string());
            continue;
        }
        let mut offset = 0;
        while offset < images.len() {
            let mut argv = vec!["sh", "-ec", REMOVE, "herdr-image-cleanup"];
            let mut bytes = 0;
            while let Some(image) = images.get(offset) {
                let size = image.directory.len() + image.path.len() + image.token.len() + 3;
                if bytes > 0 && bytes + size > 32 * 1024 {
                    break;
                }
                argv.extend([
                    image.directory.as_str(),
                    image.path.as_str(),
                    image.token.as_str(),
                ]);
                bytes += size;
                offset += 1;
            }
            let result = machine::exec(
                client,
                images[0].owner(),
                None,
                &argv,
                &[],
                "removing owned clipboard images",
            )
            .and_then(|output| output.ensure_success("removing owned clipboard images"));
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(failures.join("; ")))
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "the image paste was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn owner_temporary_directory(
    client: &Client,
    owner: Option<&str>,
    expected: &str,
) -> io::Result<String> {
    let output = machine::exec(
        client,
        owner,
        None,
        &["sh", "-ec", OWNER, "herdr-image-owner"],
        &[],
        "checking the owning core before staging an image",
    )?
    .ensure_success("checking the owning core before staging an image")?;
    let separator = output
        .stdout
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| io::Error::other("image staging did not report its owning core"))?;
    let status = &output.stdout[..separator];
    let directory = &output.stdout[separator + 1..];
    let status: Value = serde_json::from_slice(status).map_err(io::Error::other)?;
    if status.pointer("/core/id").and_then(Value::as_str) != Some(expected) {
        return Err(io::Error::other(
            "the image staging command reached a different core",
        ));
    }
    let directory = std::str::from_utf8(directory).map_err(io::Error::other)?;
    if !directory.starts_with('/') || directory.contains(['\0', '\r', '\n']) {
        return Err(io::Error::other(
            "the owning core returned an invalid temporary directory",
        ));
    }
    Ok(directory.to_owned())
}

const OWNER: &str = r#"set -eu
if command -v hc >/dev/null 2>&1; then hc=hc
elif test -x "$HOME/.local/bin/hc"; then hc=$HOME/.local/bin/hc
else printf '%s\n' 'hc is not available on the owning machine' >&2; exit 1
fi
"$hc" server status --json
printf '\000%s' "${TMPDIR:-/tmp}"
"#;

const CREATE: &str = r#"set -eu
umask 077
mkdir -- "$1"
printf '%s' "$3" > "$1/.owner"
: > "$2"
"#;

const APPEND: &str = r#"set -eu
test "$(cat -- "$1/.owner")" = "$3"
test -f "$2" && test ! -L "$2"
cat >> "$2"
"#;

const REMOVE: &str = r#"set -eu
failed=0
while test "$#" -ge 3; do
    dir=$1; file=$2; token=$3; shift 3
    test -d "$dir" || continue
    if test "$(cat -- "$dir/.owner")" = "$token"; then
        rm -f -- "$file" "$dir/.owner" && rmdir -- "$dir" || failed=1
    else
        failed=1
    fi
done
exit "$failed"
"#;
