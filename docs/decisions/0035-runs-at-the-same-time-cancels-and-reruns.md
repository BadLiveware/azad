# 0035: Runs at the same time, cancels of outdated runs, re-runs, and one check run for each commit

Status: accepted, 2026-11-03.

## Context

The orchestrator ran one run at a time, and the goals of a run one after the other (decision 0020).
A pull request waited for every run that came before it, also for runs whose commits nobody needs any more.
A goal with `on = ["push", "pull_request"]` got two check runs for the commit of a pull request from a branch of the same repository: one for the push, and one for the pull request.
GitHub's "Re-run" buttons did nothing, so the only way to run a flaky check again was a new commit.
These are what people use each day in a CI service, so the orchestrator needs them before Azad's own CI can run on it.

## Decision

### Runs and goals at the same time

- At most 8 runs run at the same time, oldest first, and the goals of a run run at the same time.
- Their actions go through one pool (`engine::Pool`), which `Engine::with_pool` gives to the engine of each run:
  - At most `azad-orchestrator serve --parallelism` actions (4 by default) run at the same time, for all runs together.
  - An action that runs already, with the same digest at the same trust level, does not run again.
    The action that comes second waits for the result of the first, and its output says so.
    The trust level is part of the key, so that a run of `main` never gets the result of a pull request's action.
  - An impure action always runs, because its effect is the reason to run it.
- A step that waits for room in the pool already shows as running.

### Cancels

- A newer commit of a branch other than the default branch, or of a pull request, cancels the runs of its older commits that are not done.
  Each commit of the default branch runs, because its results fill the cache that every run reads, and a tag always runs.
  "Newer" is the order in which the orchestrator accepted the events.
- A cancel is durable: a mark in the run's directory, and an event `run_cancelled` in its record, with why.
  The run's check runs complete at once with the conclusion `cancelled`, and they change no more.
- A cancelled run starts no more actions: `Runner::with_stop` (and `Engine::run_until_stopped`) stops the dispatch of new actions.
  Actions that run already run to their end, and their results go to the cache, where the newer commit can find them.
  The goals of a cancelled run end with the conclusion `cancelled`, and their steps that did not start show as skipped.
- A run that was cancelled before it started, or before a restart, completes its check runs as cancelled when the worker takes it.

### One check run for each goal and commit

- A run claims the check of each of its goals on its commit at its trust level, with a file in `orchestrator/claims/`, before it makes the check run.
  A goal that another run claimed gets no check run in this run.
- The same commit at the same trust level gives the same actions, so a second check run could only repeat the first.
  A push to the default branch and a pull request of the same commit have different trust levels, so each gets its check runs.

### Re-runs

- GitHub sends `check_run` with the action `rerequested` when someone presses "Re-run" on a check run, and `check_suite` with `rerequested` for "Re-run all checks".
- The orchestrator finds the newest run that has a check run that the request names, and adds a run with that run's event and the goals to run again.
  The new run makes new check runs, without a claim, and runs at the trust level of the event, as the first run did.
- The cache stays as it is: what succeeded is a hit, and what failed runs again.
- `azad-orchestrator simulate rerun <check-id>` and `simulate rerun-all [<commit>]` send the same requests to a local orchestrator.

## Consequences

- `crates/azad/tests/sandboxed/orchestrator.rs` proves these:
  - A newer commit of a branch cancels the older run while its step runs, and two commits of `main` both run.
  - A push and a pull request of one commit give one check run.
  - "Re-run" and "Re-run all checks" give new check runs, with hits from the cache.
  - Two goals of a run overlap in time, and an action that both need runs once.
- A cancel does not stop an action that runs already, so a long step still takes its place in the pool until it ends.
  To stop it, the executor contract needs a way to stop an action; that comes when a use needs it.
- The GitHub App needs the events `check_run` and `check_suite`, so that re-runs reach the orchestrator.
- `Conclusion` has `cancelled`, as GitHub has it, and the site shows cancelled runs and goals.
