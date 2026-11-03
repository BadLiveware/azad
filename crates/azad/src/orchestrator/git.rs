//! The commits that the orchestrator checks: fetched from the event's repository into a bare mirror that the
//! orchestrator keeps, and checked out into a worktree for the run.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ir::Digest;

const GIT: &str = "git";
const REPOSITORIES_DIR: &str = "repositories";
const FETCHED_REF: &str = "refs/azad/fetched";
const DEFAULT_BRANCH_REF: &str = "refs/azad/default";
const BRANCH_PREFIX: &str = "refs/heads/";
/// Tags too, so that evaluation can read them (decision 0021); a moved tag is updated, and a deleted one removed.
const TAGS_REFSPEC: &str = "+refs/tags/*:refs/tags/*";
const MIRROR_NAME_LENGTH: usize = 16;
const NO_CONFIG: &str = "/dev/null";

#[derive(Debug, thiserror::Error)]
#[error("git {command}: {reason}")]
pub(super) struct GitError {
    command: String,
    reason: String,
}

/// A run's checkout of a commit, removed when the value is dropped.
#[derive(Debug)]
pub(super) struct Checkout {
    mirror: PathBuf,
    dir: PathBuf,
}

impl Checkout {
    pub(super) fn path(&self) -> &Path {
        &self.dir
    }

    /// Whether the default branch, as the fetch found it, holds `sha`.
    pub(super) fn is_on_default_branch(&self, sha: &str) -> bool {
        git(&self.mirror, &["merge-base", "--is-ancestor", sha, DEFAULT_BRANCH_REF]).is_ok()
    }
}

/// What a fetch needs: the repository, the ref of the event and its commit, the default branch, and an HTTP header
/// that authorizes the fetch, such as a GitHub App's token for a private repository.
#[derive(Debug)]
pub(super) struct Fetch<'a> {
    pub(super) url: &'a str,
    pub(super) git_ref: &'a str,
    pub(super) sha: &'a str,
    pub(super) default_branch: &'a str,
    pub(super) header: Option<String>,
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let dir = self.dir.display().to_string();
        git(&self.mirror, &["worktree", "remove", "--force", &dir]).ok();
    }
}

/// Fetches the event's ref, the default branch and the tags of the repository, and checks out the event's commit,
/// which the ref must hold, at `dir`. The mirror keeps git's commit-graph file up to date, so that evaluation counts
/// the commits of a large history fast.
pub(super) fn check_out(state: &Path, fetch: &Fetch<'_>, dir: &Path) -> Result<Checkout, GitError> {
    let Fetch { url, git_ref, sha, .. } = *fetch;
    let name = Digest::of_bytes(url.as_bytes()).hex();
    let mirror = state.join(REPOSITORIES_DIR).join(format!("{}.git", &name[..MIRROR_NAME_LENGTH]));
    if !mirror.is_dir() {
        std::fs::create_dir_all(&mirror).map_err(|e| error("init", e.to_string()))?;
        git(&mirror, &["init", "--quiet", "--bare"])?;
    }
    let fetched = format!("+{git_ref}:{FETCHED_REF}");
    let default = format!("+{BRANCH_PREFIX}{}:{DEFAULT_BRANCH_REF}", fetch.default_branch);
    let args = ["fetch", "--quiet", "--no-tags", "--force", "--prune", url, &fetched, &default, TAGS_REFSPEC];
    authorized_git(&mirror, &args, fetch.header.as_deref())?;
    git(&mirror, &["commit-graph", "write", "--reachable", "--split"])?;
    git(&mirror, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .map_err(|_| error("fetch", format!("{url} {git_ref} does not hold the commit {sha}")))?;
    git(&mirror, &["worktree", "prune"])?;
    let target = dir.display().to_string();
    git(&mirror, &["worktree", "add", "--quiet", "--detach", &target, sha])?;
    Ok(Checkout { mirror, dir: dir.to_path_buf() })
}

fn git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    authorized_git(dir, args, None)
}

/// git with an HTTP header for its requests, given in its environment, so that no other process sees it in the
/// arguments. git reads no config of the host's user or system, so no credential helper, askpass program or URL
/// rewrite of the person who runs the orchestrator takes part, and it never asks for a password.
fn authorized_git(dir: &Path, args: &[&str], header: Option<&str>) -> Result<String, GitError> {
    let mut command = Command::new(GIT);
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", NO_CONFIG)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS");
    if let Some(header) = header {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env("GIT_CONFIG_VALUE_0", header);
    }
    let output = command.output().map_err(|e| error(args[0], e.to_string()))?;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned()),
        false => Err(error(args[0], String::from_utf8_lossy(&output.stderr).trim().to_owned())),
    }
}

fn error(command: &str, reason: String) -> GitError {
    GitError { command: command.to_owned(), reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::frontend::starlark::evaluate;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new(GIT)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Azad", "-c", "user.email=azad@example.com", "-c", "commit.gpgsign=false"])
            .args(["-c", "tag.gpgsign=false"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn evaluation_in_a_checkout_sees_the_event_commit_as_head_and_the_tags_of_the_repository() {
        let origin = tempfile::TempDir::new().expect("a directory");
        let state = tempfile::TempDir::new().expect("a directory");
        let repo = origin.path();
        run(repo, &["init", "--quiet", "--initial-branch", "main"]);
        run(repo, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
        run(repo, &["tag", "v1.0.0"]);
        let event = run(repo, &["rev-parse", "HEAD"]);
        run(repo, &["commit", "--quiet", "--allow-empty", "-m", "after the event"]);
        let url = repo.display().to_string();
        let tasks = "action(name = \"v\", cmd = [\"true\"], root = image(\"base@sha256:\" + \"1\" * 64), \
                     env = {\"head\": git.commit().id, \"tags\": str(git.tags())})\n";

        let dir = state.path().join("run");
        let fetch = Fetch { url: &url, git_ref: "refs/heads/main", sha: &event, default_branch: "main", header: None };
        let checkout = check_out(state.path(), &fetch, &dir).expect("a checkout");
        std::fs::write(checkout.path().join("tasks.star"), tasks).expect("a tasks.star");
        let package = evaluate(checkout.path(), "", "linux/amd64").expect("the package");
        let env = &package.actions["//:v"].env;
        assert_eq!(env["head"], event);
        assert_eq!(env["tags"], format!("{{\"v1.0.0\": {event:?}}}"));
        drop(checkout);

        run(repo, &["tag", "--delete", "v1.0.0"]);
        let checkout = check_out(state.path(), &fetch, &dir).expect("a checkout");
        std::fs::write(checkout.path().join("tasks.star"), tasks).expect("a tasks.star");
        let package = evaluate(checkout.path(), "", "linux/amd64").expect("the package");
        assert_eq!(package.actions["//:v"].env["tags"], "{}", "a tag deleted at the origin is gone");
    }

    #[test]
    fn a_fetch_sends_the_header_that_authorizes_it_and_never_asks_for_a_password() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}/owner/repo.git", listener.local_addr().expect("an address"));
        let seen = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("git connects");
            let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
            let mut lines = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("a line");
                if line.trim().is_empty() {
                    break;
                }
                lines.push(line.trim().to_owned());
            }
            stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").ok();
            lines
        });
        let state = tempfile::TempDir::new().expect("a directory");
        let header = "Authorization: Basic eC1hY2Nlc3MtdG9rZW46Z2hzX3Rlc3Q=".to_owned();
        let sha = "1".repeat(40);
        let fetch = Fetch {
            url: &url,
            git_ref: "refs/heads/main",
            sha: &sha,
            default_branch: "main",
            header: Some(header.clone()),
        };
        let error = check_out(state.path(), &fetch, &state.path().join("run")).expect_err("the server refuses");
        let lines = seen.join().expect("the server saw a request");
        assert!(lines.iter().any(|line| line.eq_ignore_ascii_case(&header)), "{lines:?}");
        assert!(error.to_string().contains("terminal prompts disabled"), "git asked someone for a password: {error}");
    }
}
