//! `git`: facts about the history of the workspace's repository, during evaluation (decision 0021).
//!
//! The facts of a commit are fixed by its ID. A ref can move, so each ref that evaluation resolves is recorded in the
//! package (`Package::git_refs`). Where a fact comes from is a note (`Package::notes`): another source than the local
//! repository is a decision that the user sees at the normal verbosity.
//!
//! The derive macros of the `starlark` crate (`ProvidesStaticType`) generate `unsafe impl` blocks, so this module allows
//! `unsafe_code`; it has no `unsafe` code of its own.
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::environment::{GlobalsBuilder, Methods, MethodsBuilder};
use starlark::eval::Evaluator;
use starlark::values::dict::AllocDict;
use starlark::values::{NoSerialize, StarlarkValue, starlark_value};
use starlark::{StarlarkPagablePanic, starlark_module, starlark_simple_value};

use crate::frontend::starlark::builtins::collector;
use crate::frontend::starlark::values::Collector;
use crate::frontend::{Note, NoteLevel};
use crate::history::{CommitFacts, LocalRepository, Lookup, Source};

const HEAD: &str = "HEAD";
const TAGS_PREFIX: &str = "refs/tags/";
const DATE_FORMAT: &str = "%Y-%m-%d";
const COMMIT_HEX_LEN: usize = 40;
const SHORT_ID_LEN: usize = 7;

/// A commit of the workspace's repository, with the facts that its ID fixes.
#[derive(Debug, Clone, PartialEq, Eq, ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
pub(crate) struct CommitValue {
    id: String,
    time: i64,
    parents: Vec<String>,
    message: String,
}

starlark_simple_value!(CommitValue);

impl fmt::Display for CommitValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "commit({})", self.id)
    }
}

#[starlark_value(type = "commit")]
impl<'v> StarlarkValue<'v> for CommitValue {
    fn get_methods() -> Option<&'static Methods> {
        Some(COMMIT_METHODS.methods())
    }
}

impl From<CommitFacts> for CommitValue {
    fn from(facts: CommitFacts) -> Self {
        CommitValue { id: facts.id, time: facts.time, parents: facts.parents, message: facts.message }
    }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(SHORT_ID_LEN)]
}

/// The history of the repository of the package that is evaluated.
fn lookup(collector: &Collector) -> anyhow::Result<Lookup> {
    let local = LocalRepository::discover(&collector.root)?;
    Ok(Lookup { local, fallback: collector.history.clone() })
}

/// Adds a note to the package, once.
fn note(collector: &Collector, level: NoteLevel, text: String) {
    let mut defined = collector.defined.borrow_mut();
    let note = Note { level, text };
    if !defined.notes.contains(&note) {
        defined.notes.push(note);
    }
}

fn count(this: &CommitValue, eval: &Evaluator<'_, '_, '_>) -> anyhow::Result<u64> {
    let collector = collector(eval, "commit.count")?;
    let (count, source) = lookup(collector)?.count(&this.id)?;
    let id = short(&this.id);
    match source {
        Source::Local => note(
            collector,
            NoteLevel::Diagnostic,
            format!("git: the commit count of {id} is {count}, from the local repository"),
        ),
        Source::Fallback(name) => note(
            collector,
            NoteLevel::Normal,
            format!("git: the commit count of {id} ({count}) came from {name}, because this clone is shallow"),
        ),
    }
    Ok(count)
}

fn first_parents(this: &CommitValue, limit: usize, eval: &Evaluator<'_, '_, '_>) -> anyhow::Result<Vec<CommitValue>> {
    let collector = collector(eval, "commit.first_parents")?;
    let chain = lookup(collector)?.first_parents(&this.id, limit)?;
    if let Some((added, name)) = &chain.borrowed {
        let local = chain.commits.len() - added;
        note(
            collector,
            NoteLevel::Normal,
            format!(
                "git: {added} of the first parents of {} came from {name}, because this clone ends after {local}",
                short(&this.id)
            ),
        );
    }
    Ok(chain.commits.into_iter().map(CommitValue::from).collect())
}

fn utc_date(time: i64) -> anyhow::Result<String> {
    let timestamp = jiff::Timestamp::from_second(time).map_err(|e| anyhow::anyhow!("git: the date {time}: {e}"))?;
    Ok(timestamp.strftime(DATE_FORMAT).to_string())
}

starlark::methods_static!(COMMIT_METHODS = commit_methods);

#[starlark_module]
fn commit_methods(builder: &mut MethodsBuilder) {
    /// The ID of the commit, as 40 hexadecimal digits.
    #[starlark(attribute)]
    fn id(this: &CommitValue) -> anyhow::Result<String> {
        Ok(this.id.clone())
    }

    /// The committer date, in seconds since 1970-01-01 UTC.
    #[starlark(attribute)]
    fn time(this: &CommitValue) -> anyhow::Result<i64> {
        Ok(this.time)
    }

    /// The committer date, in UTC, as `YYYY-MM-DD`.
    #[starlark(attribute)]
    fn date(this: &CommitValue) -> anyhow::Result<String> {
        utc_date(this.time)
    }

    /// The IDs of the parents, the first parent first; empty for a root commit.
    #[starlark(attribute)]
    fn parents(this: &CommitValue) -> anyhow::Result<Vec<String>> {
        Ok(this.parents.clone())
    }

    /// The commit message: the subject line, then the body after a blank line, without the line ends at the end.
    #[starlark(attribute)]
    fn message(this: &CommitValue) -> anyhow::Result<String> {
        Ok(this.message.clone())
    }

    /// How many commits this commit contains: itself and all of its ancestors, through every parent of a merge.
    ///
    /// The count of a commit is larger than the count of each of its parents, so on a branch that only moves forward,
    /// such as `main`, it follows the order of the commits.
    /// It is `git rev-list --count <id>`. A shallow clone does not have the history: the count comes from another
    /// source of history when one is given (`azad --github-history`), and is an error when none is.
    fn count<'v>(this: &CommitValue, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<u64> {
        count(this, eval)
    }

    /// The commit and its first parents, the commit first, at most `limit` commits.
    ///
    /// The list is shorter when the history has fewer. When a shallow clone ends before `limit`, the rest comes from
    /// another source of history when one is given, and it is an error when none is.
    ///
    /// # Arguments
    /// * `limit`: The most commits to give, from 1.
    fn first_parents<'v>(
        this: &CommitValue,
        limit: i32,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Vec<CommitValue>> {
        let limit = usize::try_from(limit).ok().filter(|limit| *limit > 0);
        let limit = limit.ok_or_else(|| anyhow::anyhow!("first_parents(): limit must be 1 or more"))?;
        first_parents(this, limit, eval)
    }
}

/// Records that evaluation resolved `name` to `id`.
fn record(collector: &Collector, name: String, id: &str) {
    note(collector, NoteLevel::Diagnostic, format!("git: {name} named {id}"));
    collector.defined.borrow_mut().git_refs.insert(name, id.to_owned());
}

fn is_commit_id(text: &str) -> bool {
    text.len() == COMMIT_HEX_LEN && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The functions of the namespace `git`.
#[starlark_module]
pub(crate) fn git_functions(builder: &mut GlobalsBuilder) {
    /// A commit of the workspace's repository, with the facts that its ID fixes: its date, its parents and its count.
    ///
    /// `rev` can be anything that `git rev-parse` takes, such as `HEAD`, a branch, a tag or a commit ID.
    /// A ref can move, so the package records the commit that each ref named when it was evaluated; a commit ID is
    /// fixed and needs no record.
    /// `HEAD` is the commit that the working tree is on, not the files of the working tree; in a run of the
    /// orchestrator, it is the commit of the event.
    ///
    /// # Arguments
    /// * `rev`: The revision; `HEAD` when it is not given.
    ///
    /// Examples:
    /// ```python
    /// head = git.commit()
    /// version = "%s.%d" % (head.date[:7].replace("-", "."), head.count())
    /// ```
    fn commit<'v>(
        #[starlark(default = HEAD)] rev: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<CommitValue> {
        let collector = collector(eval, "git.commit")?;
        let local = LocalRepository::discover(&collector.root)?;
        let id = local.resolve(rev).map_err(|e| anyhow::anyhow!("git.commit({rev:?}): {e}"))?;
        let commit = CommitValue::from(local.facts(&id)?);
        if !is_commit_id(rev) {
            record(collector, rev.to_owned(), &commit.id);
        }
        Ok(commit)
    }

    /// The tags of the workspace's repository whose names match `pattern`, with the commit each one names, by name.
    ///
    /// A tag can move, so the package records the commit that each tag named when it was evaluated.
    ///
    /// # Arguments
    /// * `pattern`: A pattern of tag names, where `*` matches any text, such as `v*`; all tags when it is not given.
    ///
    /// Examples:
    /// ```python
    /// releases = [semver.parse(name[1:]) for name in git.tags("v*")]
    /// ```
    fn tags<'v>(
        #[starlark(default = "*")] pattern: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<AllocDict<BTreeMap<String, String>>> {
        let collector = collector(eval, "git.tags")?;
        let matcher = globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .map_err(|e| anyhow::anyhow!("git.tags({pattern:?}): {e}"))?
            .compile_matcher();
        let mut tags = BTreeMap::new();
        for (full_name, id) in LocalRepository::discover(&collector.root)?.tags()? {
            let name = full_name.strip_prefix(TAGS_PREFIX).unwrap_or(&full_name).to_owned();
            if matcher.is_match(&name) {
                record(collector, full_name, &id);
                tags.insert(name, id);
            }
        }
        Ok(AllocDict(tags))
    }
}
