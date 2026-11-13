use std::collections::HashSet;
use std::fmt;
use std::io;

#[cfg(test)]
use serde_json::json;
use serde_json::Value;

use crate::connection::Client;
use crate::machine;

const CANONICAL_FOLDERS: &str = "for path do (if cd -P -- \"$path\" 2>/dev/null; then printf '%s\\000' \"$PWD\"; else printf '\\000'; fi); done";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
}

#[derive(Debug)]
pub enum Error {
    Machine(machine::Error),
    InvalidOutput(String),
    Dirty(String),
    InUse(String),
    Collision(String),
    Unsafe(String),
    Mutation {
        action: String,
        cause: Box<Error>,
        observed: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Machine(error) => error.fmt(f),
            Self::InvalidOutput(message) => write!(f, "invalid worktree result: {message}"),
            Self::Dirty(path) => write!(
                f,
                "worktree {path:?} has changes; commit or move them before removal"
            ),
            Self::InUse(message) => write!(f, "worktree is in use: {message}"),
            Self::Collision(path) => write!(f, "worktree destination {path:?} already exists"),
            Self::Unsafe(message) => message.fmt(f),
            Self::Mutation {
                action,
                cause,
                observed,
            } => write!(f, "could not confirm {action}: {cause}; {observed}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Machine(error) => Some(error),
            Self::Mutation { cause, .. } => Some(cause.as_ref()),
            _ => None,
        }
    }
}

impl From<machine::Error> for Error {
    fn from(error: machine::Error) -> Self {
        Self::Machine(error)
    }
}

impl From<Error> for io::Error {
    fn from(error: Error) -> Self {
        let kind = match &error {
            Error::Machine(error) => error.io_error_kind(),
            _ => io::ErrorKind::Other,
        };
        Self::new(kind, error)
    }
}

pub struct Worktrees<'a> {
    client: &'a Client,
    on: Option<&'a str>,
}

impl<'a> Worktrees<'a> {
    pub fn new(client: &'a Client, on: Option<&'a str>) -> Self {
        Self { client, on }
    }

    pub fn root(&self, cwd: &str) -> Result<String, Error> {
        let output = self.git(
            cwd,
            &["rev-parse", "--show-toplevel"],
            "finding repository root",
        )?;
        output_path(&output)
    }

    pub fn list(&self, repo: &str) -> Result<Vec<Worktree>, Error> {
        let output = self.git(
            repo,
            &["worktree", "list", "--porcelain", "-z"],
            "listing worktrees",
        )?;
        parse_worktrees(&output)
    }

    pub fn open(&self, repo: &str, path: &str) -> Result<Worktree, Error> {
        let worktree = self
            .list(repo)?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| {
                Error::Unsafe(format!("{path:?} is not a worktree of this repository"))
            })?;
        if worktree.bare {
            return Err(Error::Unsafe(
                "a bare repository has no working directory to open".to_string(),
            ));
        }
        if worktree.prunable {
            return Err(Error::Unsafe(format!(
                "worktree {path:?} is missing or prunable"
            )));
        }
        let observed = self.root(path)?;
        if observed != path {
            return Err(Error::Unsafe(format!(
                "worktree path changed to {observed:?}; refresh the worktree list"
            )));
        }
        Ok(worktree)
    }

    pub fn create(&self, repo: &str, branch: &str) -> Result<Worktree, Error> {
        self.create_with_options(repo, branch, None, None)
    }

    pub fn create_with_options(
        &self,
        repo: &str,
        branch: &str,
        base: Option<&str>,
        destination: Option<&str>,
    ) -> Result<Worktree, Error> {
        let repository = self.root(repo)?;
        let branch = output_path(&self.git(
            &repository,
            &["check-ref-format", "--branch", branch],
            "validating branch name",
        )?)?;
        let existing = self.list(&repository)?;
        let checked_out: Vec<&str> = existing
            .iter()
            .filter(|entry| entry.branch.as_deref() == Some(&branch))
            .map(|entry| entry.path.as_str())
            .collect();
        if !checked_out.is_empty() {
            return Err(Error::InUse(format!(
                "branch {branch:?} is checked out at {}",
                checked_out.join(", ")
            )));
        }
        let home = self.checked_exec(
            None,
            &["sh", "-c", "printf '%s' \"$HOME\""],
            "finding the machine home directory",
        )?;
        let home = String::from_utf8(home)
            .map_err(|error| Error::InvalidOutput(format!("machine home is not UTF-8: {error}")))?;
        if !home.starts_with('/') {
            return Err(Error::InvalidOutput(
                "machine home is not an absolute path".to_string(),
            ));
        }
        let home = output_path(&self.checked_exec(
            Some(&home),
            &["pwd", "-P"],
            "resolving the machine home directory",
        )?)?;
        let requested = match destination {
            Some("~") => home.clone(),
            Some(path) if path.starts_with("~/") => {
                format!("{}/{}", home.trim_end_matches('/'), &path[2..])
            }
            Some(path) if path.starts_with('/') => path.to_owned(),
            Some(path) => format!("{repository}/{path}"),
            None => {
                let repo_name = repository.rsplit('/').find(|part| !part.is_empty()).ok_or_else(|| Error::Unsafe(
                    "a repository at the filesystem root needs an explicit checkout destination".into()))?;
                format!(
                    "{}/.herdr/worktrees/{repo_name}/{}",
                    home.trim_end_matches('/'),
                    branch_slug(&branch)
                )
            }
        };
        let (parent, name) = requested
            .rsplit_once('/')
            .ok_or_else(|| Error::Unsafe("worktree destination has no parent".into()))?;
        if name.is_empty() || matches!(name, "." | "..") {
            return Err(Error::Unsafe(
                "worktree destination must name a new checkout directory".into(),
            ));
        }
        self.checked_exec(
            None,
            &["mkdir", "-p", "--", parent],
            "creating the worktree parent directory",
        )?;
        let parent = machine::resolve_folder(self.client, self.on, None, parent)
            .map_err(|error| Error::Unsafe(format!("resolving worktree destination: {error}")))?;
        let path = format!("{}/{name}", parent.trim_end_matches('/'));
        if existing.iter().any(|entry| entry.path == path) || self.path_exists(&path)? {
            return Err(Error::Collision(path));
        }
        let reference = format!("refs/heads/{branch}");
        let exists = self.exec(
            Some(&repository),
            &["git", "show-ref", "--verify", "--quiet", &reference],
            "checking branch existence",
        )?;
        let existing_branch = match (exists.code, exists.signal) {
            (Some(0), None) => true,
            (Some(1), None) => false,
            _ => return Err(exists.command_error("checking branch existence").into()),
        };
        self.checked_exec(
            None,
            &["mkdir", "-p", &parent],
            "creating the worktree parent directory",
        )?;
        let args = if existing_branch {
            vec!["worktree", "add", "--", path.as_str(), branch.as_str()]
        } else {
            vec![
                "worktree",
                "add",
                "-b",
                branch.as_str(),
                "--",
                path.as_str(),
                base.unwrap_or("HEAD"),
            ]
        };
        if let Err(error) = self.git(&repository, &args, "creating worktree") {
            return Err(self.mutation_error(&repository, &path, "creating worktree", error));
        }
        let worktree = self
            .list(&repository)
            .map_err(|error| {
                self.mutation_error(&repository, &path, "verifying created worktree", error)
            })?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| {
                Error::Unsafe(format!(
                    "Git reported success but did not list worktree {path:?}"
                ))
            })?;
        if worktree.branch.as_deref() != Some(&branch) {
            return Err(Error::Unsafe(format!(
                "created worktree {path:?} has an unexpected branch; refresh before opening"
            )));
        }
        Ok(worktree)
    }

    pub fn remove(
        &self,
        repo: &str,
        path: &str,
        live_terminals: &[Value],
        referenced_folders: &[String],
    ) -> Result<(), Error> {
        let worktree = self.open(repo, path)?;
        if worktree.locked {
            return Err(Error::Unsafe(format!(
                "worktree {path:?} is locked; unlock it in Git before removal"
            )));
        }
        let git_dir = output_path(&self.git(
            path,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
            "identifying worktree",
        )?)?;
        let common_dir = output_path(&self.git(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            "identifying repository",
        )?)?;
        if git_dir == common_dir {
            return Err(Error::Unsafe(
                "the main worktree cannot be removed here".to_string(),
            ));
        }
        self.require_unused(path, live_terminals, referenced_folders)?;
        let status = self.git(
            path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            "checking worktree changes",
        )?;
        if !status.is_empty() {
            return Err(Error::Dirty(path.to_string()));
        }
        if let Err(error) = self.git(
            repo,
            &["worktree", "remove", "--", path],
            "removing worktree",
        ) {
            return Err(self.mutation_error(repo, path, "removing worktree", error));
        }
        let remaining = self.list(repo).map_err(|error| {
            self.mutation_error(repo, path, "verifying removed worktree", error)
        })?;
        if remaining.iter().any(|entry| entry.path == path) || self.path_exists(path)? {
            return Err(Error::Unsafe(format!(
                "Git reported success but worktree {path:?} is still present"
            )));
        }
        Ok(())
    }

    fn mutation_error(&self, repo: &str, path: &str, action: &str, cause: Error) -> Error {
        let observed = match self.list(repo) {
            Ok(worktrees) if worktrees.iter().any(|entry| entry.path == path) => {
                format!("Git currently lists {path:?}; refresh before continuing")
            }
            Ok(_) => format!("Git does not currently list {path:?}; no branch was deleted"),
            Err(error) => format!("the current Git state could not be read: {error}"),
        };
        Error::Mutation {
            action: action.to_string(),
            cause: Box::new(cause),
            observed,
        }
    }

    fn require_unused(
        &self,
        path: &str,
        terminals: &[Value],
        folders: &[String],
    ) -> Result<(), Error> {
        let terminal_paths = terminal_paths(terminals, self.on);
        let mut usages: Vec<(Option<&str>, &str)> = terminal_paths
            .into_iter()
            .map(|(address, cwd)| (Some(address), cwd))
            .collect();
        usages.extend(folders.iter().map(|folder| (None, folder.as_str())));
        for (address, cwd) in &usages {
            if path_contains(path, cwd) {
                return Err(in_use_error(path, *address));
            }
        }
        if usages.is_empty() {
            return Ok(());
        }
        let mut argv = vec!["sh", "-c", CANONICAL_FOLDERS, "sh"];
        argv.extend(usages.iter().map(|(_, cwd)| *cwd));
        let output = self.checked_exec(None, &argv, "resolving active worktree paths")?;
        let canonical = decode_folders(&output, usages.len())?;
        for ((address, _), cwd) in usages.into_iter().zip(canonical) {
            if cwd.as_deref().is_some_and(|cwd| path_contains(path, cwd)) {
                return Err(in_use_error(path, address));
            }
        }
        Ok(())
    }

    fn path_exists(&self, path: &str) -> Result<bool, Error> {
        for option in ["-e", "-L"] {
            let output = self.exec(
                None,
                &["test", option, path],
                "checking worktree destination",
            )?;
            match (output.code, output.signal) {
                (Some(0), None) => return Ok(true),
                (Some(1), None) => {}
                _ => return Err(output.command_error("checking worktree destination").into()),
            }
        }
        Ok(false)
    }

    fn git(&self, cwd: &str, args: &[&str], action: &str) -> Result<Vec<u8>, Error> {
        let mut argv = vec!["git", "-c", "diff.autoRefreshIndex=false", "-C", cwd];
        argv.extend_from_slice(args);
        self.checked_exec(None, &argv, action)
    }

    fn checked_exec(
        &self,
        cwd: Option<&str>,
        argv: &[&str],
        action: &str,
    ) -> Result<Vec<u8>, Error> {
        let output = self.exec(cwd, argv, action)?.ensure_success(action)?;
        Ok(output.stdout)
    }

    fn exec(
        &self,
        cwd: Option<&str>,
        argv: &[&str],
        action: &str,
    ) -> Result<machine::Output, Error> {
        machine::exec(
            self.client,
            self.on,
            cwd,
            argv,
            &[("GIT_TERMINAL_PROMPT", "0"), ("GIT_OPTIONAL_LOCKS", "0")],
            action,
        )
        .map_err(Into::into)
    }
}

fn output_path(output: &[u8]) -> Result<String, Error> {
    let text = std::str::from_utf8(output)
        .map_err(|error| Error::InvalidOutput(format!("path is not UTF-8: {error}")))?;
    let path = text.strip_suffix('\n').unwrap_or(text);
    if path.is_empty() || path.contains('\0') {
        return Err(Error::InvalidOutput("empty or invalid path".to_string()));
    }
    Ok(path.to_string())
}

pub fn parse_worktrees(output: &[u8]) -> Result<Vec<Worktree>, Error> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    let mut paths = HashSet::new();
    for field in output.split(|byte| *byte == 0) {
        let field = std::str::from_utf8(field).map_err(|error| {
            Error::InvalidOutput(format!("worktree path or branch is not UTF-8: {error}"))
        })?;
        if field.is_empty() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
        } else if let Some(path) = field.strip_prefix("worktree ") {
            if current.is_some() || path.is_empty() || !paths.insert(path.to_string()) {
                return Err(Error::InvalidOutput(
                    "malformed or duplicate worktree record".to_string(),
                ));
            }
            current = Some(Worktree {
                path: path.to_string(),
                branch: None,
                bare: false,
                locked: false,
                prunable: false,
            });
        } else {
            let worktree = current.as_mut().ok_or_else(|| {
                Error::InvalidOutput("worktree attribute without a path".to_string())
            })?;
            if let Some(branch) = field.strip_prefix("branch ") {
                worktree.branch = Some(
                    branch
                        .strip_prefix("refs/heads/")
                        .unwrap_or(branch)
                        .to_string(),
                );
            } else if field == "bare" {
                worktree.bare = true;
            } else if field == "locked" || field.starts_with("locked ") {
                worktree.locked = true;
            } else if field == "prunable" || field.starts_with("prunable ") {
                worktree.prunable = true;
            }
        }
    }
    if let Some(worktree) = current {
        worktrees.push(worktree);
    }
    Ok(worktrees)
}

fn branch_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in branch.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator {
            slug.push('-');
            separator = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "worktree".to_string()
    } else {
        slug.to_string()
    }
}

fn path_contains(root: &str, cwd: &str) -> bool {
    let root = root.trim_end_matches('/');
    cwd == root
        || cwd
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn in_use_error(path: &str, address: Option<&str>) -> Error {
    match address {
        Some(address) => Error::InUse(format!(
            "terminal {address} uses {path:?}; stop it before removal"
        )),
        None => Error::InUse(format!(
            "a saved workspace uses {path:?}; close that workspace before removal"
        )),
    }
}

fn decode_folders(output: &[u8], expected: usize) -> Result<Vec<Option<String>>, Error> {
    let output = output
        .strip_suffix(&[0])
        .ok_or_else(|| Error::InvalidOutput("path resolution reply is incomplete".to_string()))?;
    let paths: Result<Vec<_>, Error> = output
        .split(|byte| *byte == 0)
        .map(|path| {
            if path.is_empty() {
                return Ok(None);
            }
            let path = std::str::from_utf8(path).map_err(|error| {
                Error::InvalidOutput(format!("resolved path is not UTF-8: {error}"))
            })?;
            Ok(Some(path.to_string()))
        })
        .collect();
    let paths = paths?;
    if paths.len() != expected {
        return Err(Error::InvalidOutput(
            "path resolution reply has the wrong number of paths".to_string(),
        ));
    }
    Ok(paths)
}

fn terminal_paths<'a>(terminals: &'a [Value], machine: Option<&str>) -> Vec<(&'a str, &'a str)> {
    let mut paths = Vec::new();
    for terminal in terminals {
        if terminal.get("state").and_then(Value::as_str) != Some("live") {
            continue;
        }
        let Some(address) = terminal.get("address").and_then(Value::as_str) else {
            continue;
        };
        if address.split_once('/').map(|(owner, _)| owner) != machine {
            continue;
        }
        for path in [
            terminal.get("cwd"),
            terminal.get("last_cwd"),
            terminal.pointer("/foreground/cwd"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            paths.push((address, path));
        }
    }
    paths
}

pub fn terminal_using_path<'a>(
    terminals: &'a [Value],
    machine: Option<&str>,
    path: &str,
) -> Option<&'a str> {
    terminal_paths(terminals, machine)
        .into_iter()
        .find_map(|(address, cwd)| path_contains(path, cwd).then_some(address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_porcelain_preserves_spaces_newlines_unicode_and_flags() {
        let parsed = parse_worktrees(b"worktree /repo/main\0HEAD abc\0branch refs/heads/main\0\0worktree /repo/a b\n\xc3\xa9\0HEAD def\0detached\0locked maintenance\0prunable gone\0\0").unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert_eq!(parsed[1].path, "/repo/a b\n\u{e9}");
        assert!(parsed[1].locked && parsed[1].prunable);
        assert!(parse_worktrees(b"branch refs/heads/main\0").is_err());
        assert!(parse_worktrees(b"worktree /same\0\0worktree /same\0\0").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn actual_git_porcelain_round_trips_a_linked_path_with_newlines() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = Scratch(
            std::path::PathBuf::from("/tmp").join(format!("hctui-git-{}", crate::model::new_id())),
        );
        std::fs::create_dir(&root.0).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", &root.0)
                .args(["-C", root.0.to_str().unwrap()])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        git(&["init", "-b", "main"]);
        std::fs::write(root.0.join("seed"), "seed\n").unwrap();
        git(&["add", "seed"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "seed",
        ]);
        let path = root.0.join("linked space\nline");
        git(&[
            "worktree",
            "add",
            "-b",
            "topic/names",
            "--",
            path.to_str().unwrap(),
            "HEAD",
        ]);
        let output = git(&["worktree", "list", "--porcelain", "-z"]);
        let worktrees = parse_worktrees(&output).unwrap();
        assert_eq!(worktrees.len(), 2);
        let linked = worktrees
            .iter()
            .find(|worktree| worktree.branch.as_deref() == Some("topic/names"))
            .unwrap();
        assert_eq!(
            linked.path,
            std::fs::canonicalize(&path).unwrap().to_str().unwrap()
        );
        assert!(!linked.locked && !linked.bare && !linked.prunable);
        let output = std::process::Command::new("sh")
            .args(["-c", CANONICAL_FOLDERS, "sh", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            decode_folders(&output.stdout, 1).unwrap(),
            vec![Some(linked.path.clone())]
        );
    }

    #[test]
    fn in_use_checks_cover_unplaced_remote_terminals_and_observed_child_directories() {
        let terminals = vec![
            json!({"address":"t1","state":"live","cwd":"/repo/topic"}),
            json!({"address":"box/t2","state":"live","cwd":"/other","last_cwd":"/repo/topic/src"}),
            json!({"address":"other/t3","state":"live","cwd":"/repo/topic"}),
            json!({"address":"box/t4","state":"finished","cwd":"/repo/topic"}),
        ];
        assert_eq!(
            terminal_using_path(&terminals, Some("box"), "/repo/topic"),
            Some("box/t2")
        );
        assert_eq!(
            terminal_using_path(&terminals, None, "/repo/topic"),
            Some("t1")
        );
        assert_eq!(
            terminal_using_path(&terminals, Some("box"), "/repo/top"),
            None
        );
    }

    #[test]
    #[cfg(unix)]
    fn real_core_git_worktree_lifecycle_refuses_dirty_and_live_usage_and_keeps_branch() {
        let session = crate::test_support::CoreSession::new();
        let repo = session.root.join("repository");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = session
                .command("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(&["init", "-b", "main"]);
        std::fs::write(repo.join("seed"), "seed\n").unwrap();
        git(&["add", "seed"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "seed",
        ]);
        let client = session.client();
        let worktrees = Worktrees::new(&client, None);
        let canonical_repo = worktrees.root(repo.to_str().unwrap()).unwrap();
        let repository = canonical_repo.as_str();
        let created = worktrees.create(repository, "topic/names").unwrap();
        assert_eq!(created.branch.as_deref(), Some("topic/names"));
        assert_eq!(worktrees.open(repository, &created.path).unwrap(), created);
        assert!(matches!(
            worktrees.create(repository, "topic-names"),
            Err(Error::Collision(_))
        ));
        std::fs::write(
            std::path::Path::new(&created.path).join("untracked"),
            "keep me",
        )
        .unwrap();
        assert!(matches!(
            worktrees.remove(repository, &created.path, &[], &[]),
            Err(Error::Dirty(_))
        ));
        std::fs::remove_file(std::path::Path::new(&created.path).join("untracked")).unwrap();
        let run = client
            .request(
                "terminal.run",
                json!({"argv":["/bin/sh"],"cwd":created.path,"no_parent":true}),
            )
            .unwrap();
        let terminal = run;
        let address = terminal["address"].as_str().unwrap();
        assert!(matches!(
            worktrees.remove(
                repository,
                &created.path,
                std::slice::from_ref(&terminal),
                &[]
            ),
            Err(Error::InUse(_))
        ));
        client
            .request("terminal.kill", json!({"terminal":address}))
            .unwrap();
        assert!(matches!(
            worktrees.remove(
                repository,
                &created.path,
                &[],
                std::slice::from_ref(&created.path)
            ),
            Err(Error::InUse(_))
        ));
        worktrees
            .remove(repository, &created.path, &[], &[])
            .unwrap();
        assert!(!std::path::Path::new(&created.path).exists());
        git(&["show-ref", "--verify", "refs/heads/topic/names"]);
        assert!(matches!(
            worktrees.remove(repository, repository, &[], &[]),
            Err(Error::Unsafe(_))
        ));
        drop(client);
        session.finish();
    }
}
