//! The git operations of the engine: the files of a working tree, and a remote repository at one commit.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::engine::EngineError;

const GIT: &str = "git";
const COMMIT_HEX_LEN: usize = 40;
const PEELED_SUFFIX: &str = "^{}";

fn git(args: &[&str], dir: Option<&Path>) -> Result<Vec<u8>, EngineError> {
    let mut command = Command::new(GIT);
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let output = command.args(args).output().map_err(|e| EngineError::Git(format!("cannot run git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EngineError::Git(format!("git {}: {}", args.join(" "), stderr.trim())));
    }
    Ok(output.stdout)
}

/// The files of `dir` that git tracks or would track, relative to `dir`, as they are in the working tree.
pub(crate) fn working_tree_files(dir: &Path) -> Result<BTreeSet<PathBuf>, EngineError> {
    let listing = git(&["ls-files", "-z", "--cached", "--others", "--exclude-standard"], Some(dir))?;
    Ok(listing
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(String::from_utf8_lossy(p).as_ref()))
        .collect())
}

/// git's exit code when a path is not ignored, and when `dir` is not in a repository.
const NOT_IGNORED: i32 = 1;
const NOT_A_REPOSITORY: i32 = 128;

/// The paths of `paths`, relative to `dir`, that git's ignore rules of the repository of `dir` ignore; a directory ends
/// with `/`. The paths do not have to exist, so this works for what an isolated action wrote in its sandbox.
pub(crate) fn ignored(dir: &Path, paths: &[String]) -> Result<BTreeSet<String>, EngineError> {
    let mut child = Command::new(GIT)
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "--no-index", "--stdin", "-z"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| EngineError::Git(format!("cannot run git: {e}")))?;
    let input: Vec<u8> = paths.iter().flat_map(|path| path.bytes().chain([0])).collect();
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(&input)
        .map_err(|e| EngineError::Git(format!("git check-ignore: {e}")))?;
    let output = child.wait_with_output().map_err(|e| EngineError::Git(format!("git check-ignore: {e}")))?;
    match output.status.code() {
        Some(0) => Ok(output
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()),
        Some(NOT_IGNORED | NOT_A_REPOSITORY) => Ok(BTreeSet::new()),
        _ => Err(EngineError::Git(format!("git check-ignore: {}", String::from_utf8_lossy(&output.stderr).trim()))),
    }
}

/// The commit that `rev` names in the repository at `url`: a commit as given, or the commit of a tag or branch.
pub(crate) fn resolve_rev(url: &str, rev: &str) -> Result<String, EngineError> {
    if rev.len() == COMMIT_HEX_LEN && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(rev.to_ascii_lowercase());
    }
    let listing = String::from_utf8_lossy(&git(&["ls-remote", url], None)?).into_owned();
    let refs: Vec<(&str, &str)> = listing.lines().filter_map(|line| line.split_once('\t')).collect();
    let find = |name: &str| refs.iter().find(|(_, r)| *r == name).map(|(commit, _)| (*commit).to_owned());
    let tag = format!("refs/tags/{rev}");
    find(&format!("{tag}{PEELED_SUFFIX}"))
        .or_else(|| find(&tag))
        .or_else(|| find(&format!("refs/heads/{rev}")))
        .ok_or_else(|| EngineError::Git(format!("{url} has no tag or branch {rev}")))
}

/// Writes the files of `commit` of the repository at `url` into `dest`, which must be an empty directory.
pub(crate) fn fetch(url: &str, commit: &str, dest: &Path) -> Result<(), EngineError> {
    git(&["init", "--quiet"], Some(dest))?;
    git(&["fetch", "--quiet", "--depth", "1", url, commit], Some(dest))?;
    git(&["-c", "advice.detachedHead=false", "checkout", "--quiet", "FETCH_HEAD"], Some(dest))?;
    Ok(())
}
