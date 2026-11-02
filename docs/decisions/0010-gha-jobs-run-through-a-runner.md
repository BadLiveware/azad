# 0010: A GHA job is one action that runs a runner in the sandbox

Status: accepted, 2026-10-30.

## Context

A GHA (GitHub Actions) step can read what earlier steps did: `if:` conditions with `failure()`, `${{ steps.x.outputs.y }}`, and files such as `GITHUB_OUTPUT` and `GITHUB_ENV`.
Steps share a file system and the processes that they start.
So the steps of a job need an interpreter while they run, as GitHub's own runner is.

## Decision

The GHA frontend compiles each job, and each combination of a job's matrix, to one impure action:

- Its root is the image for the job's `runs-on` label; `ubuntu-latest` and the other Ubuntu labels map to `node:24-bookworm`, by digest.
- Its command is `azad-gha-runner`, a static binary that the engine gives as a tool, with a job plan (`job.json`, the `azad-gha-plan` schema) that the frontend writes.
- Its inputs are the repository's working tree when a step checks out, each remote action at its revision, and the results and artifacts of the jobs in `needs`.
- Its outputs are `meta` (the job's result and outputs) and `artifacts`.

The runner runs the steps in order and evaluates expressions with `azad-gha-expr`.
`actions/checkout`, `actions/upload-artifact` and `actions/download-artifact` are built into the runner; `actions/cache` does nothing, because Azad's own caching applies.
Other actions run as GitHub runs them: Node and composite actions.

The frontend evaluates only what must be known before a job starts: `runs-on` and the matrix.

## Consequences

- The IR, the core and the engine know nothing about GHA; a job is an ordinary action.
- A job is impure, so it runs every time; GHA+ (phase 9) lets steps declare their inputs and become cache hits.
- A job with `if: always()`, `failure()` or `cancelled()` does not run after a failed dependency, because the core skips what depends on a failure; `azad check` reports it as unsupported.
- Docker (container jobs, services, Docker actions), reusable workflows, matrices that expressions make, and PowerShell are not supported yet; `azad check` reports each one before anything runs.
- Secrets are empty, and `github.token` is empty, as on a pull request from a fork.
- The runner must be built for `x86_64-unknown-linux-musl`, so that it runs in any image.
- The combinations of a matrix come in GitHub's order: the first key in the file is the outer loop, then `exclude`, then `include`.
  The action of a combination is `<job>.<index>`, where the index is `strategy.job-index`, because a label must be a safe name.
  The output shows the action with the values of its combination, as GitHub names a matrix job `test (3.12, fast)`: `//ci.yml:test.1 (3.12, slow)`.
- A failed job exits with the exit code of the step that failed it, so that Azad reports that code.
- The renderers show the workflow commands of a job's output as GitHub's log viewer does: they hide `::debug::` unless `RUNNER_DEBUG` or `ACTIONS_STEP_DEBUG` is set, show the title of a group, and mark errors, warnings and notices.
  The stored log keeps the commands.
  In GitHub Actions, the commands of a job's output do not become annotations of the job that runs Azad: that output can come from code that the job does not trust.

## Amendment, 2026-10-31

Phase 17 changes the last consequence, so that errors show on the lines of a pull request, as they do when GitHub runs the tools itself:

- In GitHub Actions, the `::error::`, `::warning::` and `::notice::` commands in the output of an action become annotations of the job that runs Azad.
  This applies to every action, not only to GHA jobs.
- Azad never passes such a line on.
  It reads the level, the message, the title, the file and the positions (`line`, `endLine`, `col`, `endColumn`), and writes a new command from these values only, with GitHub's escapes.
  It writes the command after the action's group, where commands are not stopped.
- The file becomes its path in the repository.
  An absolute path is read below the place where the action sees the workspace: `/work` for an isolated task, the runner's `GITHUB_WORKSPACE` for a GHA job, and the workspace itself for a host task.
  A relative path is relative to the directory in which the command starts: the package directory of a task, and the workspace of a GHA job.
  A file outside the workspace, or in the output of an `action()`, whose paths are sandbox paths, is dropped with its positions; the annotation stays, without a line.
- All other commands in the output stay stopped, for example `set-env`, `add-path`, `set-output`, `save-state`, `add-mask`, `stop-commands`, `echo` and `add-matcher`.
  The token that stops them is random, so that the output cannot guess it.
- The Azad action adds a problem matcher for the errors and warnings of Cargo while Azad runs, and removes it after.
  GitHub's runner applies problem matchers to every line that it does not take as a command, also between `::stop-commands::` and its token.
  Cargo prints paths relative to its workspace, and GitHub reads them as relative to the repository; so the matcher finds the right file when the Cargo workspace is the root of the repository.
- The output of code that the job does not trust can still put an annotation on any file of the repository, as it can when GitHub runs the tools itself.
  It cannot change the state of the job.
