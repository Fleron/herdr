use std::collections::{BTreeMap, BTreeSet};

use herdr_core_tui::connection::Client;
use herdr_core_tui::{machine, worktrees::Worktrees};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repository {
    pub key: String,
    pub label: String,
    pub root: String,
    pub primary: String,
    pub branch: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    pub linked: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Metadata {
    pub repositories: BTreeMap<String, Repository>,
    pub errors: BTreeMap<String, String>,
    pub directories: BTreeMap<String, String>,
    pub home: Option<String>,
    pub current: Option<String>,
    pub hostname: Option<String>,
    pub clock: Option<OwnerClock>,
    pub identities: BTreeMap<String, RepositoryIdentity>,
    pub worktrees: BTreeMap<String, Vec<herdr_core_tui::worktrees::Worktree>>,
    pub unavailable: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OwnerClock {
    pub timestamp: i64,
    pub offset: i32,
    pub sampled: std::time::Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RepositoryIdentity {
    pub primary: String,
    pub label: String,
}

impl super::Backend {
    pub(super) fn request_metadata(&mut self, state_changed: bool) {
        let periodic = std::time::Instant::now() >= self.metadata_due;
        if !periodic && !state_changed {
            return;
        }
        self.metadata_due = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut owners =
            BTreeMap::<Option<String>, BTreeSet<String>>::from([(None, BTreeSet::new())]);
        for profile in &self.profiles {
            if let Some(id) = profile["id"].as_str() {
                owners.entry(Some(id.into())).or_default();
            }
        }
        for (owner, roots) in &self.metadata_watch {
            owners
                .entry(owner.clone())
                .or_default()
                .extend(roots.iter().cloned());
        }
        for workspace in &self.document.workspaces {
            owners
                .entry(workspace.machine.clone())
                .or_default()
                .insert(workspace.folder.clone());
        }
        for record in self.inventory.records() {
            let Some(address) = record["address"].as_str() else {
                continue;
            };
            let Some(cwd) = record
                .pointer("/foreground/cwd")
                .and_then(serde_json::Value::as_str)
                .or_else(|| record["cwd"].as_str())
            else {
                continue;
            };
            let owner = address.split_once('/').map(|(owner, _)| owner.to_owned());
            owners.entry(owner).or_default().insert(cwd.to_owned());
        }
        for (owner, mut folders) in owners {
            folders.insert(self.default_folder(owner.as_deref(), None));
            if self.metadata_in_flight.contains(&owner)
                || (!periodic && self.metadata_folders.get(&owner) == Some(&folders))
            {
                continue;
            }
            if owner.as_deref().is_some_and(|id| {
                !self
                    .profiles
                    .iter()
                    .any(|profile| profile["id"] == id && profile["state"] == "connected")
            }) {
                continue;
            }
            self.metadata_folders.insert(owner.clone(), folders.clone());
            self.metadata_in_flight.insert(owner.clone());
            let client = self.client.clone();
            let sender = self.job_sender.clone();
            let wake = self.wake.clone();
            let generation = self.generation;
            std::thread::spawn(move || {
                let result = discover(&client, owner.as_deref(), folders);
                let _ = sender.send(super::jobs::JobResult::Metadata {
                    generation,
                    owner,
                    result,
                });
                let _ = wake.try_send(());
            });
        }
    }
}

#[derive(Clone)]
struct Checkout {
    root: String,
    common: String,
    git_dir: String,
    branch: Option<String>,
    ahead_behind: Option<(usize, usize)>,
    bare: bool,
}

pub(super) fn discover(
    client: &Client,
    owner: Option<&str>,
    folders: BTreeSet<String>,
) -> Metadata {
    let mut result = Metadata::default();
    let inspected = match inspect_many(client, owner, &folders) {
        Ok(facts) => facts,
        Err(error) => {
            result
                .errors
                .extend(folders.into_iter().map(|folder| (folder, error.clone())));
            return result;
        }
    };
    result.home = Some(inspected.home);
    result.current = Some(inspected.current);
    result.hostname = inspected.hostname;
    result.clock = inspected.clock;
    result.directories = inspected.directories;
    let mut facts = inspected.entries;
    let mut repositories = BTreeMap::<String, Result<String, String>>::new();
    let mut pending_primary = BTreeMap::<String, (String, bool)>::new();
    for checkout in facts.values().filter_map(|fact| fact.as_ref().ok()) {
        if repositories.contains_key(&checkout.common)
            || pending_primary.contains_key(&checkout.common)
        {
            continue;
        }
        let primary = (|| {
            let trees = Worktrees::new(client, owner)
                .list(&checkout.root)
                .map_err(|error| error.to_string())?;
            let candidate = trees
                .first()
                .ok_or_else(|| "Git returned no worktrees".to_owned())?;
            result
                .worktrees
                .insert(checkout.common.clone(), trees.clone());
            if let Some(known) = facts
                .values()
                .filter_map(|fact| fact.as_ref().ok())
                .find(|fact| fact.root == candidate.path)
            {
                if known.git_dir == checkout.common
                    && known.common == checkout.common
                    && known.bare == candidate.bare
                {
                    return Ok(Some(candidate.path.clone()));
                }
            }
            pending_primary.insert(
                checkout.common.clone(),
                (candidate.path.clone(), candidate.bare),
            );
            Ok(None)
        })();
        match primary {
            Ok(Some(path)) => {
                repositories.insert(checkout.common.clone(), Ok(path));
            }
            Ok(None) => {}
            Err(error) => {
                repositories.insert(checkout.common.clone(), Err(error));
            }
        }
    }
    let checkout_folders = result
        .worktrees
        .values()
        .flatten()
        .map(|tree| tree.path.clone())
        .collect::<BTreeSet<_>>();
    let verified =
        (!checkout_folders.is_empty()).then(|| inspect_many(client, owner, &checkout_folders));
    match &verified {
        Some(Ok(inspected)) => {
            result.directories.extend(
                inspected
                    .directories
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            facts.extend(
                inspected
                    .entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        Some(Err(error)) => {
            result.unavailable.extend(
                result
                    .worktrees
                    .keys()
                    .map(|key| (key.clone(), error.clone())),
            );
        }
        None => {}
    }
    if !pending_primary.is_empty() {
        for (common, (path, bare)) in pending_primary {
            let primary = match &verified {
                Some(Ok(facts)) => facts.entries.get(&path).ok_or_else(|| "Git did not return the primary checkout's identity".to_owned())
                    .and_then(|fact| fact.clone()).and_then(|fact| {
                        if fact.git_dir != common || fact.common != common || fact.bare != bare {
                            Err("Git's primary worktree did not match the repository's common directory".into())
                        } else { Ok(fact.root) }
                    }),
                Some(Err(error)) => Err(error.clone()),
                None => Err("the repository has no worktree identity to verify".into()),
            };
            repositories.insert(common, primary);
        }
    }
    for (folder, fact) in facts {
        let repository = fact.and_then(|fact| {
            let primary = repositories
                .get(&fact.common)
                .ok_or_else(|| "repository identity was not returned".to_owned())?
                .clone()?;
            let label = primary
                .trim_end_matches('/')
                .rsplit('/')
                .find(|part| !part.is_empty())
                .unwrap_or(&primary)
                .to_owned();
            Ok(Repository {
                key: fact.common.clone(),
                label,
                root: fact.root,
                primary,
                branch: fact.branch,
                ahead_behind: fact.ahead_behind,
                linked: fact.git_dir != fact.common,
            })
        });
        match repository {
            Ok(repository) => {
                result.repositories.insert(folder, repository);
            }
            Err(error) => {
                result.errors.insert(folder, error);
            }
        }
    }
    for (key, primary) in repositories {
        match primary {
            Ok(primary) => {
                let label = primary
                    .trim_end_matches('/')
                    .rsplit('/')
                    .find(|part| !part.is_empty())
                    .unwrap_or(&primary)
                    .to_owned();
                result
                    .identities
                    .insert(key, RepositoryIdentity { primary, label });
            }
            Err(error) => {
                result.unavailable.insert(key, error);
            }
        }
    }
    result
}

impl Metadata {
    pub(super) fn retain_unavailable(&mut self, previous: &Self) {
        for (folder, error) in self.errors.clone() {
            if let Some(repository) = previous.repositories.get(&folder) {
                self.repositories
                    .entry(folder.clone())
                    .or_insert_with(|| repository.clone());
                if !self.identities.contains_key(&repository.key) {
                    self.unavailable
                        .entry(repository.key.clone())
                        .or_insert(error);
                }
            }
        }
        for (key, identity) in &previous.identities {
            if !self.identities.contains_key(key) && self.errors.contains_key(&identity.primary) {
                self.unavailable
                    .entry(key.clone())
                    .or_insert_with(|| self.errors[&identity.primary].clone());
            }
        }
        for key in self.unavailable.keys() {
            if let Some(identity) = previous.identities.get(key) {
                self.identities
                    .entry(key.clone())
                    .or_insert_with(|| identity.clone());
            }
            if let Some(worktrees) = previous.worktrees.get(key) {
                self.worktrees
                    .entry(key.clone())
                    .or_insert_with(|| worktrees.clone());
            }
            for (folder, repository) in previous
                .repositories
                .iter()
                .filter(|(_, repository)| &repository.key == key)
            {
                self.repositories
                    .entry(folder.clone())
                    .or_insert_with(|| repository.clone());
            }
        }
    }
}

struct Inspected {
    entries: BTreeMap<String, Result<Checkout, String>>,
    directories: BTreeMap<String, String>,
    home: String,
    current: String,
    hostname: Option<String>,
    clock: Option<OwnerClock>,
}

fn inspect_many(
    client: &Client,
    owner: Option<&str>,
    folders: &BTreeSet<String>,
) -> Result<Inspected, String> {
    let action = "reading repository metadata";
    let mut argv = vec!["sh", "-c", SCRIPT, "herdr-git-metadata"];
    argv.extend(folders.iter().map(String::as_str));
    let output = machine::exec(
        client,
        owner,
        None,
        &argv,
        &[("LC_ALL", "C"), ("GIT_OPTIONAL_LOCKS", "0")],
        action,
    )
    .map_err(|error| error.to_string())?
    .ensure_success(action)
    .map_err(|error| error.to_string())?;
    let mut fields = output.stdout.split(|byte| *byte == 0);
    let text = |value: Option<&[u8]>| {
        value
            .ok_or_else(|| "Git metadata ended inside a checkout record".to_owned())
            .and_then(|value| String::from_utf8(value.to_vec()).map_err(|error| error.to_string()))
    };
    let mut home = text(fields.next())?;
    let current = text(fields.next())?;
    let hostname = text(fields.next())?;
    let datetime = text(fields.next())?;
    let clock = (|| {
        let (seconds, offset) = datetime.split_once(' ')?;
        let sign = match offset.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        if offset.len() != 5 || !offset[1..].bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let hours = offset[1..3].parse::<i32>().ok()?;
        let minutes = offset[3..].parse::<i32>().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        Some(OwnerClock {
            timestamp: seconds.parse().ok()?,
            offset: sign * (hours * 3600 + minutes * 60),
            sampled: std::time::Instant::now(),
        })
    })();
    if !current.starts_with('/') {
        return Err("the owning machine returned a non-absolute working directory".into());
    }
    if home.is_empty() {
        home = current.clone();
    } else if !home.starts_with('/') {
        home = format!("{current}/{home}");
    }
    let mut facts = BTreeMap::new();
    let mut directories = BTreeMap::new();
    for expected in folders {
        let folder = text(fields.next())?;
        if &folder != expected {
            return Err("Git metadata did not preserve the requested checkout identities".into());
        }
        let location = text(fields.next())?;
        match location.as_str() {
            "unavailable" => {
                facts.insert(
                    folder.clone(),
                    Err(format!(
                        "could not inspect {folder:?}: {}",
                        output.stderr.trim()
                    )),
                );
                continue;
            }
            "directory" => {
                let directory = text(fields.next())?;
                if !directory.starts_with('/') {
                    return Err(
                        "the owning machine returned a non-absolute checkout directory".into(),
                    );
                }
                directories.insert(folder.clone(), directory);
            }
            _ => return Err("Git metadata contains an unknown folder result".into()),
        }
        let status = text(fields.next())?;
        let result = match status.as_str() {
            "error" => Err(format!(
                "could not inspect {folder:?}: {}",
                output.stderr.trim()
            )),
            "ok" => {
                let root = text(fields.next())?;
                let common = text(fields.next())?;
                let git_dir = text(fields.next())?;
                let branch = text(fields.next())?;
                let counts = text(fields.next())?;
                let bare = match text(fields.next())?.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("Git returned an invalid bare-repository flag".into()),
                };
                if root.is_empty() || common.is_empty() || git_dir.is_empty() {
                    return Err("Git returned an empty checkout identity".into());
                }
                let ahead_behind = if counts.is_empty() {
                    None
                } else {
                    let values = counts.split_whitespace().collect::<Vec<_>>();
                    if values.len() != 2 {
                        return Err("Git upstream counts did not contain two values".into());
                    }
                    Some((
                        values[0].parse().map_err(|_| "invalid Git ahead count")?,
                        values[1].parse().map_err(|_| "invalid Git behind count")?,
                    ))
                };
                Ok(Checkout {
                    root,
                    common,
                    git_dir,
                    branch: (!branch.is_empty()).then_some(branch),
                    ahead_behind,
                    bare,
                })
            }
            _ => return Err("Git metadata contains an unknown checkout result".into()),
        };
        facts.insert(folder, result);
    }
    if fields.next() != Some(&[][..]) || fields.next().is_some() {
        return Err("Git metadata contains incomplete or unexpected records".into());
    }
    Ok(Inspected {
        entries: facts,
        directories,
        home,
        current,
        hostname: (!hostname.is_empty()).then_some(hostname),
        clock,
    })
}

const SCRIPT: &str = r#"newline='
'
current=$(pwd -P && printf .) || exit
current=${current%.}
current=${current%"$newline"}
home=${HOME-}
if test -n "$home"; then
    if physical_home=$(cd -- "$home" && pwd -P && printf .); then
        physical_home=${physical_home%.}
        home=${physical_home%"$newline"}
    fi
fi
hostname=$(hostname 2>/dev/null) || hostname=
datetime=$(date '+%s %z' 2>/dev/null) || datetime=
printf '%s\000%s\000%s\000%s\000' "$home" "$current" "$hostname" "$datetime"
for input do
    printf '%s\000' "$input"
    case "$input" in '~') folder=${HOME-};; '~/'*) folder=${HOME-}/${input#??};; *) folder=$input;; esac
    if directory=$(cd -- "$folder" && pwd -P && printf .); then
        directory=${directory%.}
        directory=${directory%"$newline"}
        printf 'directory\000%s\000' "$directory"
    else
        printf 'unavailable\000'
        continue
    fi
    if (
        cd -- "$directory" || exit
        bare=$(git rev-parse --is-bare-repository) || exit
        common=$(git rev-parse --path-format=absolute --git-common-dir && printf .) || exit
        common=${common%.}
        common=${common%"$newline"}
        common=$(cd -- "$common" && pwd -P && printf .) || exit
        common=${common%.}
        common=${common%"$newline"}
        gitdir=$(git rev-parse --path-format=absolute --git-dir && printf .) || exit
        gitdir=${gitdir%.}
        gitdir=${gitdir%"$newline"}
        gitdir=$(cd -- "$gitdir" && pwd -P && printf .) || exit
        gitdir=${gitdir%.}
        gitdir=${gitdir%"$newline"}
        if test "$bare" = true; then
            root=$common
            branch=''
            counts=''
        else
            root=$(git rev-parse --show-toplevel && printf .) || exit
            root=${root%.}
            root=${root%"$newline"}
            root=$(cd -- "$root" && pwd -P && printf .) || exit
            root=${root%.}
            root=${root%"$newline"}
            if branch=$(git symbolic-ref --quiet --short HEAD); then :; else
                code=$?
                test "$code" = 1 || exit "$code"
            fi
            counts=''
            if test -n "$branch"; then
                upstream=$(git for-each-ref --format='%(upstream)' "refs/heads/$branch") || exit
                if test -n "$upstream"; then counts=$(git rev-list --left-right --count "HEAD...$upstream") || exit; fi
            fi
        fi
        printf 'ok\000%s\000%s\000%s\000%s\000%s\000%s\000' "$root" "$common" "$gitdir" "$branch" "$counts" "$bare"
    ); then :; else
        printf 'error\000'
    fi
done
"#;
