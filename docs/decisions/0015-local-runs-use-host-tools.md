# 0015: Local runs use the host's tools; CI runs the same tasks in images

Status: accepted, 2026-10-31.

## Context

Locally, Azad is used as a task runner, in the place of Taskfile, `just` or Make.
The project's own tools (`cargo`, `go`, `dotnet`) are already fast on a developer's machine: they keep incremental state in the working tree, and Azad cannot do better.
Today every action runs in its image, in a sandbox with absolute paths such as `/src`.
So `//:check` of Azad pulls a 1.5 GB Rust image and keeps a second Cargo target directory, next to the one that the developer already has.

In CI the situation is the opposite.
Runners start empty, so the tools have no state, and Azad knows from content which work is already done.
There, hermetic actions in images and the remote cache are what make a run fast.

The current actions cannot run on the host: their commands use sandbox paths (`cd /src`, `/crates`), which do not exist there.

## Decision

- Local runs and CI runs have the same targets: one definition, one name, one command (`azad run check`).
  Nothing is defined only for local use or only for CI; the mode decides where a task runs, not which tasks exist.
- A new built-in, `task()`, describes work in terms of the workspace, not of a sandbox:
  - `cmd` runs in the task's package directory.
  - `sources`: patterns of the files that the task reads, relative to the package.
  - `generates`: patterns of the files that the task writes, relative to the package.
  - `deps`: tasks that must succeed first.
  - `image`: the image that CI uses; `env` and `caches` as for `action()`.
  The files between tasks are files in the workspace, as in Taskfile: a task that reads what another generates names it in `sources` and the other task in `deps`.
- **Locally**, a task runs on the host, in the real package directory, with the host's tools and the developer's environment, and the task's `env` on top.
  It writes in place, so tools keep their state where they always do (for example `target/`).
  There is no sandbox and no image pull.
  A task is skipped when its command, `env`, `sources` and the outputs of its `deps` are the same as at its last success on this machine; this record is local and is never shared, because it cannot describe the host.
  `--force` runs it anyway.
- **In CI** (`--isolated`, which the Azad action sets), a task compiles to a hermetic `action()`:
  the root is `image`, the files that `sources` selects are at their workspace paths below `/workspace`, the command runs in the package directory there, and the files that `generates` selects are its outputs.
  The `generates` of each `dep` are placed at their workspace paths, so the command sees the same tree as on the host.
  These actions are cached and shared, as now.
- `--isolated` also works locally, to run exactly what CI runs.
- `action()` stays: it is the hermetic form that tasks compile to, and modules and frontends still use it.
  An `action()` uses sandbox paths, so it runs in its image locally too; pipelines written with `action()` keep working unchanged, and a pipeline moves to `task()` to get host runs.

## Consequences

- Vision goal 3 changes from "the same run" to "the same definition": one task runs on the host locally and hermetically in CI, and `--isolated` reproduces CI locally.
- A local run can pass where CI fails, for example when a file that `sources` does not name exists on the host, or the host has another version of a tool.
  `--isolated` finds such a difference, and CI always runs isolated.
- The local skip record can be stale after a change of a host tool, as Taskfile's checksums can; `--force` is the way out.
- GHA workflows still run in images locally: their steps assume a runner, not the developer's machine.
- Interactive tasks (a development server, a REPL) become possible locally, because the task has the terminal; they are impure and never skipped.

## Amendment, 2026-10-31

Phase 16 built this decision, and changed some details:

- An isolated task does not see its files below `/workspace`.
  Its work directory (`/work`) starts with them, at their paths in the workspace, as files that the command may change; the command starts in the package directory there, and its outputs are directories below the work directory.
  So the action has two new parts: files that the work directory starts with, and the directory in which the command starts.
  A host action has the kind `host`, which names the workspace; it is cached on this machine only, and its result counts only while the directories that it generated are still the same in the workspace.
- What an image needs to be used, such as a writable `CARGO_HOME`, goes in `image(..., env = {...})`: a task gets it only when it runs in the image.
- A task without an image runs on the host in both modes; in CI, that is the runner.
  It is for work that needs the host, such as tests that start sandboxes, and it is never shared through a remote cache.
  (Phase 16 first made such a task an error in an isolated run; phase 17 needed Azad's own sandbox tests in CI.)
- A task's `caches` with a path relative to its package, such as `target`, name that directory in both modes: on the host the real one, and in the image a volume at the same place below the work directory.
  An absolute path is a volume at that path of the sandbox, and has no meaning on the host.
  CI restores both from the remote cache (decision 0017).
- A task can run by its own name, as if a goal required only it.
- When the terminal view shows colors, each command gets variables that ask for colored output; they are not part of the action, so its digest does not change.
