# 0020: A local orchestrator, and an action cache with trust levels

Status: accepted, 2026-11-01.

## Context

The orchestrator is the part of Azad that turns events (pushes and pull requests) into runs and check runs ("What Azad is" in `AGENTS.md`).
Until Azad has its own hosts, it runs locally: it takes simulated events from a local git repository, and its check runs go to a fake GitHub.
The same code must later take GitHub's real webhooks and call GitHub's real API.

Decision 0019 gave cache volumes trust levels, but the action cache had one level: any run wrote results that every later run read.
A pull request runs code that its author wrote; if its results went into the cache that `main` reads, a pull request could put a wrong result behind the digest of an action of `main`.

## Decision

### Events and trust levels

- `azad serve` takes GitHub's webhook deliveries on `/webhook`: the events `push` and `pull_request` (opened, synchronize, reopened), in GitHub's format.
  A delivery must have an HMAC-SHA256 signature of its payload, in `X-Hub-Signature-256`, under the secret in `$AZAD_WEBHOOK_SECRET`; other deliveries get 401 and run nothing.
  A delivery ID that came before runs nothing again, because GitHub delivers again when it is not sure.
- The trust level comes from the event, never from the code that runs: a push to the default branch is `main`; a pull request, and a push to another branch, is `pull-request`; anything else, such as a tag, is `none`.
  Tags are `none` because a tag can name a commit that is not on the default branch; a proof that it is comes with the real GitHub.
- A goal names its events: `goal("check", require = [...], on = ["push", "pull_request"])`.
  An event runs every goal of the commit's workspace that names it.

### Runs

- Each accepted event becomes a run in durable state, in the store's `orchestrator/runs/`: the event, the IDs of its check runs, and a mark when it is done.
  A worker runs the runs that are not done, oldest first, also after a restart; a run that fails is reported on its check runs and is done, so it does not run again and again.
  One orchestrator at a time serves a store, through a lock, so no run runs twice.
- The orchestrator fetches the event's ref (`refs/heads/<branch>`, or `refs/pull/<n>/head`, where GitHub keeps the head of a pull request) into a bare mirror of the repository, and checks the commit out into a worktree for the run.
- Goals run isolated, in their images, at the event's trust level, through the executor service when `--executor` names one.
  The orchestrator never runs an action with the host's tools: a goal that would (a task without an image, decision 0015) fails its check run before anything runs, because an event runs code that anyone who can push or open a pull request wrote.
- Each goal gets a check run, as GitHub has them: queued, in progress, then completed with `success` or `failure` and a summary of what ran, came from the cache, and failed.
  The `CheckRuns` trait takes these calls; the fake GitHub keeps them as files, which `azad checks` shows, and a client of GitHub's API implements the same trait later.
- `azad simulate push [<branch>]` and `azad simulate pull-request <n> [<branch>]` send signed events from the workspace's git repository, with its path as the clone URL; for a pull request, they set `refs/pull/<n>/head`, as GitHub does.

### The action cache

- A run reads the results of `main` and of its own level, and writes only to its own level: `main` writes `ac/` (where results always were), `pull-request` writes `ac-pull-request/`, and `none` writes nothing.
  A run on a developer's machine is `main`, so nothing changes for the task runner.
- The executor service still never writes the action cache; its client does, at the client's trust level.

## Consequences

- `crates/azad/tests/orchestrator.rs` proves the "done when" of phase 20, through an executor service: a push to `main` fills the cache; a later pull request gets a check run whose summary shows a hit from `main`'s run; a plan at the level `main` still has to run the pull request's changed action, and a plan at `pull-request` has a hit.
  The test fails when every run is `main`.
- It also tests a wrong signature, and a run that was accepted before a restart; the CLI test sends a simulated pull request through `azad serve`, and a task without an image fails its check run without running.
- The webhook intake is a small HTTP/1.1 reader in the library (one request per connection, a body of at most 25 MiB), so that the orchestrator needs no web framework; HTTPS comes from a proxy in front of it, as GitHub requires it.
- Azad's own goals do not name events yet: its sandbox tests run on the host, which needs bubblewrap, so the orchestrator would refuse them.
- The GHA frontend does not give workflows events from their `on:` yet; that would let Azad run a repository's existing workflows on simulated events.

## Amendment, 2026-11-03

Decision 0029 makes the orchestrator its own binary: `azad serve`, `azad simulate` and `azad checks` are now `azad-orchestrator serve`, `simulate` and `checks`, and it no longer reads personal defaults.

## Amendment, 2026-11-03: GitHub

Decision 0032 adds the client of GitHub's API, as a GitHub App, and gives a tag the trust level `main` when the default branch holds its commit.
