use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use herdr_core_tui::connection::Client;
use herdr_core_tui::image_staging::{remove_many, StagedImage};

use super::Target;

pub(super) enum Command {
    Upload {
        id: String,
        target: Target,
        image: crate::platform::ClipboardImage,
        cancelled: Arc<AtomicBool>,
    },
    Remove {
        id: String,
        client: Client,
    },
    Finish {
        client: Client,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Stop,
}

pub(super) enum Event {
    Transport(Client),
    Prepared {
        id: String,
        path: String,
    },
    Completed {
        id: String,
        result: Result<String, String>,
    },
    Removed(String),
    Notice(String),
}

pub(super) fn run(
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
    wake: mpsc::SyncSender<()>,
) {
    let mut images = BTreeMap::<String, StagedImage>::new();
    let mut connection = None;
    let emit = |event| {
        let _ = events.send(event);
        let _ = wake.try_send(());
    };
    while let Ok(command) = commands.recv() {
        match command {
            Command::Upload {
                id,
                target,
                image,
                cancelled,
            } => {
                let result = (|| -> Result<String, String> {
                    let client = transport(&mut connection, &target.local_core, &emit)?;
                    validate_program(&client, &target)?;
                    if cancelled.load(Ordering::Acquire) {
                        return Err("The image paste was cancelled before upload".into());
                    }
                    let stage = StagedImage::prepare(&client, Some(&target.owner), image.extension)
                        .map_err(|error| format!("Image staging setup failed: {error}"))?;
                    let path = stage.path().to_owned();
                    images.insert(id.clone(), stage);
                    emit(Event::Prepared {
                        id: id.clone(),
                        path: path.clone(),
                    });
                    images
                        .get_mut(&id)
                        .ok_or("The image stage was lost")?
                        .upload(&client, &image.bytes, &cancelled)
                        .map_err(|error| error.to_string())?;
                    validate_program(&client, &target)?;
                    Ok(path)
                })();
                let result = match result {
                    Ok(path) => Ok(path),
                    Err(error) => {
                        if let Some(stage) = images.get(&id) {
                            let cleanup = connection
                                .as_ref()
                                .ok_or_else(|| {
                                    std::io::Error::other("the image connection is unavailable")
                                })
                                .and_then(|client| stage.remove(client));
                            match cleanup {
                                Ok(()) => {
                                    images.remove(&id);
                                    emit(Event::Removed(id.clone()));
                                    Err(error)
                                }
                                Err(cleanup) => Err(format!(
                                    "{error}. Cleanup failed for {}: {cleanup}",
                                    stage.path()
                                )),
                            }
                        } else {
                            Err(error)
                        }
                    }
                };
                emit(Event::Completed { id, result });
            }
            Command::Remove { id, client } => {
                let Some(stage) = images.get(&id) else {
                    continue;
                };
                let cleanup = local_core(&client)
                    .and_then(|id| transport(&mut connection, id, &emit))
                    .and_then(|client| stage.remove(&client).map_err(|error| error.to_string()));
                match cleanup {
                    Ok(()) => {
                        images.remove(&id);
                        emit(Event::Removed(id));
                    }
                    Err(error) => emit(Event::Notice(format!(
                        "Could not remove the staged image {}: {error}",
                        stage.path()
                    ))),
                }
            }
            Command::Finish { client, reply } => {
                let cleanup = if images.is_empty() {
                    Ok(())
                } else {
                    local_core(&client)
                        .and_then(|id| transport(&mut connection, id, &emit))
                        .and_then(|client| {
                            remove_many(&client, &images.values().collect::<Vec<_>>())
                                .map_err(|error| error.to_string())
                        })
                };
                let result = cleanup.map_err(|error| {
                    format!(
                        "{error}. Owned image paths: {}",
                        images
                            .values()
                            .map(StagedImage::path)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                });
                if result.is_ok() {
                    images.clear();
                }
                let _ = reply.send(result);
            }
            Command::Stop => break,
        }
    }
}

fn local_core(client: &Client) -> Result<&str, String> {
    client
        .hello()
        .pointer("/core/id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "the image connection has no core identity".into())
}

fn transport(
    connection: &mut Option<Client>,
    expected: &str,
    emit: &impl Fn(Event),
) -> Result<Client, String> {
    if let Some(client) = connection
        .as_ref()
        .filter(|client| client.error().is_none())
    {
        if local_core(client)? == expected {
            return Ok(client.clone());
        }
        return Err("the image connection belongs to another core".into());
    }
    let client = Client::connect().map_err(|error| error.to_string())?;
    if local_core(&client)? != expected {
        let _ = client.disconnect();
        return Err("the image connection reached another core; no image was uploaded".into());
    }
    emit(Event::Transport(client.clone()));
    *connection = Some(client.clone());
    Ok(client)
}

fn validate_program(client: &Client, target: &Target) -> Result<(), String> {
    let record = client
        .request(
            "terminal.get",
            serde_json::json!({"terminal": target.address}),
        )
        .map_err(|error| error.to_string())?;
    if record["state"] != "live" || record["pid"].as_u64() != Some(target.pid) {
        return Err(
            "The image's terminal program ended or was replaced; the paste was held".into(),
        );
    }
    if !target.foreground_matches(
        record
            .pointer("/foreground/pid")
            .and_then(serde_json::Value::as_u64),
    ) {
        return Err("The image's reported foreground program changed or became unavailable; the paste was held".into());
    }
    Ok(())
}
