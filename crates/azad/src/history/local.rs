//! The local repository, read with gix (gitoxide).

use std::path::{Path, PathBuf};

use gix::ObjectId;

use crate::history::{CommitFacts, HistoryError};

/// The repository of a working tree.
#[derive(Clone, Debug)]
pub struct LocalRepository {
    /// A directory of the working tree, from which gix finds the repository.
    workspace: PathBuf,
    /// The repository's common directory, which its worktrees share; it names the repository in caches.
    common_dir: PathBuf,
}

fn read_error(what: &str, error: impl std::fmt::Display) -> HistoryError {
    HistoryError::Read(format!("git: {what}: {error}"))
}

fn object_id(id: &str) -> Result<ObjectId, HistoryError> {
    ObjectId::from_hex(id.as_bytes()).map_err(|e| read_error(&format!("{id:?} is not a commit ID"), e))
}

impl LocalRepository {
    /// The repository that holds `workspace`.
    pub fn discover(workspace: &Path) -> Result<Self, HistoryError> {
        let repo = gix::discover(workspace)
            .map_err(|e| read_error(&format!("{} is not in a git repository", workspace.display()), e))?;
        Ok(LocalRepository { workspace: workspace.to_path_buf(), common_dir: repo.common_dir().to_path_buf() })
    }

    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    fn open(&self) -> Result<gix::Repository, HistoryError> {
        gix::discover(&self.workspace)
            .map_err(|e| read_error(&format!("the repository of {}", self.workspace.display()), e))
    }

    /// The commit that `rev` names: anything that `git rev-parse` takes.
    pub fn resolve(&self, rev: &str) -> Result<String, HistoryError> {
        let repo = self.open()?;
        let what = format!("{rev:?}");
        let commit = repo
            .rev_parse_single(rev)
            .map_err(|e| read_error(&what, e))?
            .object()
            .map_err(|e| read_error(&what, e))?
            .peel_to_commit()
            .map_err(|e| read_error(&format!("{what} does not name a commit"), e))?;
        Ok(commit.id.to_string())
    }

    pub fn facts(&self, id: &str) -> Result<CommitFacts, HistoryError> {
        facts(&self.open()?, object_id(id)?)
    }

    /// How many commits `id` contains, through every parent of a merge; an error in a shallow clone.
    pub fn count(&self, id: &str) -> Result<u64, HistoryError> {
        let repo = self.open()?;
        if repo.is_shallow().map_err(|e| read_error("whether the clone is shallow", e))? {
            return Err(HistoryError::Shallow { what: "the commit count".to_owned(), id: id.to_owned() });
        }
        let object = object_id(id)?;
        let walk = repo.rev_walk([object]).all().map_err(|e| read_error(&format!("the history of {id}"), e))?;
        let mut count = 0;
        for commit in walk {
            commit.map_err(|e| read_error(&format!("the history of {id}"), e))?;
            count += 1;
        }
        Ok(count)
    }

    /// The commit `id` and its first parents, at most `limit`, and whether the list is complete: it is not when a
    /// shallow clone ends before `limit` and before the first commit of the history.
    pub fn first_parents(&self, id: &str, limit: usize) -> Result<(Vec<CommitFacts>, bool), HistoryError> {
        let repo = self.open()?;
        let boundary = repo.shallow_commits().map_err(|e| read_error("the shallow boundary", e))?;
        let at_boundary = |id: &ObjectId| boundary.as_ref().is_some_and(|commits| commits.contains(id));
        let mut chain = vec![facts(&repo, object_id(id)?)?];
        while chain.len() < limit {
            let last = chain.last().expect("the chain starts with the commit");
            let Some(parent) = last.parents.first() else { return Ok((chain, true)) };
            if at_boundary(&object_id(&last.id)?) {
                return Ok((chain, false));
            }
            let parent = facts(&repo, object_id(parent)?)?;
            chain.push(parent);
        }
        Ok((chain, true))
    }
}

impl LocalRepository {
    /// The tags, by their full names (`refs/tags/<name>`), with the commit that each names.
    pub fn tags(&self) -> Result<Vec<(String, String)>, HistoryError> {
        let repo = self.open()?;
        let references = repo.references().map_err(|e| read_error("the refs", e))?;
        let mut tags = Vec::new();
        for reference in references.tags().map_err(|e| read_error("the tags", e))? {
            let mut reference = reference.map_err(|e| read_error("a tag", e))?;
            let name = reference.name().as_bstr().to_string();
            let commit = reference
                .peel_to_commit()
                .map_err(|e| read_error(&format!("the tag {name} does not name a commit"), e))?;
            tags.push((name, commit.id.to_string()));
        }
        Ok(tags)
    }
}

fn facts(repo: &gix::Repository, id: ObjectId) -> Result<CommitFacts, HistoryError> {
    let commit = repo.find_commit(id).map_err(|e| read_error(&format!("the commit {id}"), e))?;
    let time = commit.time().map_err(|e| read_error(&format!("the committer date of {id}"), e))?;
    Ok(CommitFacts {
        id: id.to_string(),
        time: time.seconds,
        parents: commit.parent_ids().map(|parent| parent.to_string()).collect(),
    })
}
