# 0036: The orchestrator in a GHA job

Status: accepted, 2026-11-03.

## Context

The orchestrator runs as a service with a GitHub App (decision 0032), on a host that takes webhooks.
Azad has no host that is always on: the development host is a VM on a developer's machine.
A repository that cannot install the App, or that has no host, still wants what the orchestrator gives: goals that name their events, runs at the event's trust level, nothing on the host's tools, and a check run for each goal with its steps and output.
Azad's own CI is the first such repository, and running its CI through the orchestrator is the test of whether the orchestrator can run real CI.

## Decision

- `azad-orchestrator gha` runs in a GitHub Actions (GHA) job, once, for the job's event.
  It reads the event from `$GITHUB_EVENT_NAME` and the file at `$GITHUB_EVENT_PATH`, which has the format of a webhook's payload, so the same code reads it as a delivery.
- It runs the event as `serve` runs a delivery (`Orchestrator::run_once`): it fetches the commit, runs each goal whose `on` names the event at the event's trust level, isolated, and writes a check run for each goal.
  It exits with a failure when a goal fails, so the job fails too.
- It writes the check runs and fetches the commit with the job's token (`GitHub::with_token`), which needs the permission `checks: write`.
  GitHub shows these check runs as GitHub Actions' own.
- Each run reads and writes the remote cache of `--cache`, as `azad --cache` names it: `gha` by default.
  GHA keeps the cache of a pull request apart from the cache of the default branch, as for the Azad action, so a pull request's results do not reach `main`.
- The Azad action runs it with `orchestrate: true`, because GitHub gives the cache service's address and token only to JavaScript actions.
- There is no durable state between jobs, so the parts of decision 0035 that need it come from GHA:
  - GHA's `concurrency` cancels the runs of outdated commits.
  - GHA's "Re-run jobs" runs the event again.
  - A workflow that runs on `push` and `pull_request` gets one run for each event.
- Remote caches are now named in the library (`RemoteConfig::from_spec`), so that `azad` and `azad-orchestrator` read them the same way.

## Consequences

- Azad's CI has a job `orchestrator`, in which this commit's own `azad-orchestrator` runs `//:isolated-check` (fmt, clippy and the tests that need no sandbox) for each push and pull request.
  It does not gate a release yet: it runs next to `//:check`, which keeps the sandbox tests and the examples.
- The CLI test runs `azad-orchestrator gha` with an event file and a fake of GitHub's API: it writes a check run with the job's token, and a task without an image fails it and never runs on the runner.
- The sandbox tests still run on the host, because they start bubblewrap; a sandbox in a sandbox needs a decision of its own.

## Amendment, 2026-11-03: forced runs, and one lock for each mirror

- In Azad's CI, the job `orchestrator` starts after the build of its binary, and by then `//:check` has stored the results of the same steps at the same trust level.
  So each of its steps was a hit, and the job proved only that the orchestrator reads the event, the cache and the check runs.
- `azad-orchestrator gha --force` runs every step again, as `azad run --force` does, and the action gives it the input `force`.
  The orchestrator keeps the `force` of its engine's configuration, which `serve` never sets.
- The job `orchestrator` runs forced while it runs next to `//:check`, so that it proves that its steps run in the orchestrator's sandbox in GHA.
  When it replaces `//:check`, it stops forcing, and hits from earlier commits are what it is for.
- Runs at the same time fetched into the same refs of a repository's mirror, and git failed one of them.
  A file lock beside the mirror now holds it for one fetch, checkout or removal at a time.
