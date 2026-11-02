//! The git operations of the engine: the files of a working tree, and a remote repository at one commit.

use std::collections::BTreeSet;
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
