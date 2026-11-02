//! The `github` context of a local run, from the git repository of the workspace.

use std::path::Path;
use std::process::Command;

use serde_json::{Map, Value, json};

const EVENT_NAME: &str = "push";
const ACTOR: &str = "azad";
const SERVER_URL: &str = "https://github.com";
const API_URL: &str = "https://api.github.com";
const HEADS_PREFIX: &str = "refs/heads/";

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// `owner/repo` from a GitHub remote URL such as `https://github.com/o/r.git` or `git@github.com:o/r.git`.
fn repository_of(url: &str) -> Option<String> {
    let path = url.split_once("github.com").map(|(_, rest)| rest.trim_start_matches([':', '/']))?;
    let repository = path.trim_end_matches('/').trim_end_matches(".git");
    (repository.matches('/').count() == 1).then(|| repository.to_owned())
}

/// The context for every job of a workflow in `root`. The job name is added per job.
pub(crate) fn context(root: &Path, workflow: &str, workspace: &str) -> Map<String, Value> {
    let sha = git(root, &["rev-parse", "HEAD"]).unwrap_or_default();
    let reference = git(root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    let ref_name = reference.strip_prefix(HEADS_PREFIX).unwrap_or(&reference).to_owned();
    let fallback = root.file_name().map(|n| format!("local/{}", n.to_string_lossy())).unwrap_or_default();
    let repository =
        git(root, &["remote", "get-url", "origin"]).and_then(|url| repository_of(&url)).unwrap_or(fallback);
    let owner = repository.split('/').next().unwrap_or_default().to_owned();
    let value = json!({
        "event_name": EVENT_NAME,
        "event": {},
        "sha": sha,
        "ref": reference,
        "ref_name": ref_name,
        "ref_type": "branch",
        "repository": repository,
        "repository_owner": owner,
        "actor": ACTOR,
        "triggering_actor": ACTOR,
        "workflow": workflow,
        "workspace": workspace,
        "server_url": SERVER_URL,
        "api_url": API_URL,
        "run_id": "0",
        "run_number": "1",
        "run_attempt": "1",
        "token": "",
    });
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repositories_are_read_from_remote_urls() {
        assert_eq!(repository_of("https://github.com/o/r.git"), Some("o/r".to_owned()));
        assert_eq!(repository_of("git@github.com:o/r.git"), Some("o/r".to_owned()));
        assert_eq!(repository_of("https://gitlab.com/o/r"), None);
    }
}
