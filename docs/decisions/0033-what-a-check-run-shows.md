# 0033: What a check run shows

Status: accepted, 2026-11-03.

## Context

A developer sees Azad's work on a pull request through its check runs (decision 0032).
GitHub Actions shows a job's steps, their times and their logs; a check run of another App shows only its title, a summary and a text in Markdown, of at most 65535 characters each, and a link to details elsewhere.
Azad's hosts are VMs on a developer's machine with no public address, so there is no elsewhere: the check run itself must show what happened.

## Decision

- One check run for each goal, as GitHub Actions has one for each job; the goal's actions are its steps.
- The title says how the goal went: `1 ran, 2 cached, 1 failed`; while it runs, `Running: 3 of 6 steps done`; when it cannot run, why, such as `//:lint would run with the host's tools`.
- The summary is a table of the steps, one row for each action, with its state (queued, running, ran, cached, failed or skipped, each with an icon), the reason of a failure or a skip, and its time, then one line of metrics: how many steps, how many came from the cache, how long the goal took, and how long its steps ran together.
- The text holds the end of the steps' output, without terminal escapes: failed and running steps first, open, with their last 100 lines; then the steps that ran, folded, with their last 20 lines.
  A cached step shows no output, because it did not run now.
  A code fence is longer than any run of backticks in the output, so that no line ends it early.
  Steps that do not fit in GitHub's limit are counted, not shown.
- While a goal runs, its check run shows its progress at most every 5 seconds, which keeps a run far below GitHub's limit of 5000 calls an hour for an installation.
- The fake GitHub keeps the same title, summary and text, and `azad-orchestrator checks` shows each check run's title.

## Consequences

- `crates/azad/tests/orchestrator.rs` proves that a check run of a goal with a failed step shows both steps in the table, the metrics, and the failed step's output, open and before the others.
- A step's time is measured by the orchestrator, from the moment it starts to the moment it ends; a result does not store how long its action took, so a cached step has no time.
- The whole log of a step is in the host's store; the check run shows its end only.
