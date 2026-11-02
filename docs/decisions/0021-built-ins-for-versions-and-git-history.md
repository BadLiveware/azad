# 0021: Built-ins for versions and git history, and a standard library that users could write

Status: accepted, 2026-11-01.

## Context

Release versions come from the history of a repository.
A large monorepo that Azad is tried on stamps each release `<year>.<month>.<commit count>`: the count is how many commits the commit contains, and the year and month come from the latest committer date among the commit and its last 25 first parents.
In GHA, a shallow checkout has no history, so a script reads both facts from GitHub's GraphQL API, with retries and paging.
Other repositories use semantic versions (semver) from tags, and their pipelines parse and compare versions in shell.

Evaluation could not read any of this: a `tasks.star` file sees only the files of the workspace.
Parsing semver is easy to get almost right, and the `semver` crate, which Cargo uses, gets it right.
Reading git history fast needs git's commit-graph file and a real git library.

Azad must not hold the knowledge of any ecosystem: how .NET builds, what a version scheme is, how a deploy tool works.
That belongs in modules, which the user owns or the standard library gives.
A standard library module must be one that a user could write, so that it proves that the public built-ins are enough.

## Decision

### The rule for built-ins and the standard library

- A built-in is a generic primitive over data: files (`glob`), images, actions, and now versions and git history.
  It never names a tool or a scheme of one ecosystem.
- The standard library uses only the built-ins that every `.star` file gets.
  When a standard module needs something that is missing, the missing thing becomes a public built-in.
- Built-ins are written in Rust; a module is written in Starlark.
  Azad has no plugins in other languages; a tool in another language runs in an action.

### `semver`

- `semver.parse(text)` gives a version value, with Cargo's rules (the `semver` crate).
  Versions compare by precedence, so `sorted()` and `max()` work, and they can be keys of a dict.
- `semver.valid(text)`, and `semver.matches(requirement, version)` with Cargo's requirements (`^1.4`, `~1.4.2`, `>=1.2, <1.5`).
- A version has `major`, `minor`, `patch`, `pre`, `build`, and `bump("major" | "minor" | "patch")`.
  A bump gives the next release; a pre-release of that release is released as it is, so `1.3.0-rc.1` bumped at `minor` is `1.3.0`.

### `git`

- `git.commit(rev = "HEAD")` gives a commit of the workspace's repository: `id`, `time` and `date` (the committer date, in UTC), `parents`, `count()` (`git rev-list --count`), and `first_parents(limit)`.
- `git.tags(pattern = "*")` gives the tags whose names match, with the commit that each names.
- Azad reads the repository with gix (gitoxide), a git library in Rust.
  libgit2 would also avoid running `git`, but it is a C library, which makes static and cross-platform builds harder.
  gix reads git's commit-graph file: with it, counting about 340,000 commits takes about 0.1 seconds; without it, 1.1 seconds.
- The facts of a commit are fixed by its ID, because the ID covers all of its ancestors.
  So they are pure inputs, and the count is kept in memory by repository and ID.
- A ref can move.
  Each ref that evaluation resolves (`HEAD`, a branch, a tag) is recorded in the package (`Package::git_refs`) with the commit it named; a commit ID is not recorded.
- `HEAD` is the commit that the working tree is on, not the files of the working tree.
  In a run of the orchestrator it is the event's commit, because the run's checkout is that commit.
- A shallow clone does not have the history.
  `count()`, and `first_parents()` past the clone's boundary, are errors that say so; a count or a date that is silently wrong would give a wrong version.
- `git` works while a `tasks.star` file is evaluated, as `glob()` does, and so in the functions of modules that it calls; a `config.star` file cannot use it.

### The orchestrator's mirror

- The mirror fetches the tags with the event's ref, and removes tags that the origin removed.
- It writes git's commit-graph file after each fetch (`--split`, so each write adds only what is new).

### `@std//version.star`

- `commit_calver(commit = None, window = 25)`: `<year>.<month>.<count>`, the scheme above, with `HEAD` when no commit is given.
- `latest_release(prefix = "v")`: the highest release among the tags `<prefix><semver>`, without pre-releases.
- Both use only `git` and `semver`.

## Consequences

- On that monorepo's history, `commit_calver()` gives the version that its CI gave the chart of the same commit, with no call to GitHub.
- A version that depends only on a commit is known when the plan is made, so it can name images, charts and releases without values from actions that ran.
  A version that only a running action can compute still needs `.value()`, which phase 6 left for later.
- The values that evaluation reads from git reach actions only through their commands, environment and inputs, so `azad why` already names a changed value.
  The recorded refs are the inputs of an evaluation, for a cache of evaluations, which Azad does not have yet; `azad why` does not show them yet.
- The engine's other git operations (the files of the working tree, `ls-remote`, and the fetch of a source at a revision) still run `git`; moving them to gix needs gix's network transport, and comes when they need a change.
- A GHA checkout is shallow unless it sets `fetch-depth: 0`, so a goal that counts commits fails there, with a message that says how to fix it.

## Amendment, 2026-11-01: another source for the history that a shallow clone lacks

A GitHub Actions checkout is shallow, because a full clone of a large repository is slow, so `count()` failed there.
GitHub has no variable for the commit count: `GITHUB_RUN_NUMBER` counts runs, not commits.
GitHub's GraphQL API gives both facts, as a release script can read them: `Commit.history.totalCount`, and chains of first parents.

- The history has two sources behind one interface (`azad::history`): the local repository first, and another source (`History`) for what a shallow clone lacks.
  A chain of first parents is read locally to the clone's boundary, and the rest comes from the other source.
- The other source is GitHub's API for one repository: `azad --github-history <owner/name>`, with the token in `$AZAD_GITHUB_TOKEN`, and `--github-graphql-url` for GitHub Enterprise Server.
  It reads 10 first parents in one query, because GitHub refuses a query deeper than 25 levels, and it tries a failed request three times.
- The core does not know GitHub Actions: the Azad action (an adapter) turns the source on with the job's token (input `github-token`, the default `github.token`).
- Without another source, a shallow clone is still an error, with the two ways to fix it.
  Locally, a developer who made a shallow clone chose it, and Azad does not call an API with their credentials unasked.
- Facts from either source are fixed by the commit ID, so they are the same; a count is kept in memory by repository and ID.
- Evaluation now can use the network.
  The facts that it reads never change, so the result does not depend on when it runs, but a failed request fails the load.
- Where each fact came from is a note (decision 0022): another source is a decision that changes what happens, so the user sees it by default.
- Azad keeps its own secrets from host actions: `$AZAD_GITHUB_TOKEN`, `$AZAD_SIGNING_KEY`, and the address and token of the GitHub Actions cache (`withheld` in the engine's config).
  Before this, host actions got the whole environment of `azad`, and so the signing key and the cache token too.

Consequences:

- In a clone of that monorepo with one commit, `commit_calver()` gives the same version through GitHub's API as the full clone, and two notes say that the count and 24 of the 25 first parents came from GitHub's API.
- A script that reads versions from GitHub's API can go while Azad still runs inside GitHub Actions jobs; it does not have to wait for Azad's own orchestrator.
- `crates/azad/tests/versions.rs` checks the source against a server that answers as GitHub's API does, from the full repository: GitHub cannot be reached from a test.
  It checks the version, the notes, and that a wrong token fails.
- Facts from the other source are not in the remote cache yet; each machine asks again.
