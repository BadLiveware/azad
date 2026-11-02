//! The commits that the orchestrator checks: fetched from the event's repository into a bare mirror that the
//! orchestrator keeps, and checked out into a worktree for the run.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ir::Digest;

const GIT: &str = "git";
const REPOSITORIES_DIR: &str = "repositories";
const FETCHED_REF: &str = "refs/azad/fetched";
/// Tags too, so that evaluation can read them (decision 0021); a moved tag is updated, and a deleted one removed.
const TAGS_REFSPEC: &str = "+refs/tags/*:refs/tags/*";
const MIRROR_NAME_LENGTH: usize = 16;

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
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let dir = self.dir.display().to_string();
        git(&self.mirror, &["worktree", "remove", "--force", &dir]).ok();
    }
}

/// Fetches `git_ref` and the tags of the repository at `url`, and checks out `sha`, which the ref must hold, at `dir`.
/// The mirror keeps git's commit-graph file up to date, so that evaluation counts the commits of a large history fast.
pub(super) fn check_out(state: &Path, url: &str, git_ref: &str, sha: &str, dir: &Path) -> Result<Checkout, GitError> {
    let name = Digest::of_bytes(url.as_bytes()).hex();
    let mirror = state.join(REPOSITORIES_DIR).join(format!("{}.git", &name[..MIRROR_NAME_LENGTH]));
    if !mirror.is_dir() {
        std::fs::create_dir_all(&mirror).map_err(|e| error("init", e.to_string()))?;
        git(&mirror, &["init", "--quiet", "--bare"])?;
    }
    let fetched = format!("+{git_ref}:{FETCHED_REF}");
    git(&mirror, &["fetch", "--quiet", "--no-tags", "--force", "--prune", url, &fetched, TAGS_REFSPEC])?;
    git(&mirror, &["commit-graph", "write", "--reachable", "--split"])?;
    git(&mirror, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .map_err(|_| error("fetch", format!("{url} {git_ref} does not hold the commit {sha}")))?;
    git(&mirror, &["worktree", "prune"])?;
    let target = dir.display().to_string();
    git(&mirror, &["worktree", "add", "--quiet", "--detach", &target, sha])?;
    Ok(Checkout { mirror, dir: dir.to_path_buf() })
}

fn git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new(GIT).arg("-C").arg(dir).args(args).output().map_err(|e| error(args[0], e.to_string()))?;
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
        let checkout = check_out(state.path(), &url, "refs/heads/main", &event, &dir).expect("a checkout");
        std::fs::write(checkout.path().join("tasks.star"), tasks).expect("a tasks.star");
        let package = evaluate(checkout.path(), "", "linux/amd64").expect("the package");
        let env = &package.actions["//:v"].env;
        assert_eq!(env["head"], event);
        assert_eq!(env["tags"], format!("{{\"v1.0.0\": {event:?}}}"));
        drop(checkout);

        run(repo, &["tag", "--delete", "v1.0.0"]);
        let checkout = check_out(state.path(), &url, "refs/heads/main", &event, &dir).expect("a checkout");
        std::fs::write(checkout.path().join("tasks.star"), tasks).expect("a tasks.star");
        let package = evaluate(checkout.path(), "", "linux/amd64").expect("the package");
        assert_eq!(package.actions["//:v"].env["tags"], "{}", "a tag deleted at the origin is gone");
    }
}
