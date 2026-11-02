# 0026: Sources and dependencies across packages

Status: accepted, 2026-11-02.

## Context

A task read only its own package: `sources` were relative to the package directory, and `deps` named tasks of the same file.
Projects in a monorepo often read other directories:

- a .NET project builds the projects that its `ProjectReference`s name, such as `apps/api` with `libs/core`;
- a uv project installs a path dependency from source, such as `../../infrastructure/monitoring-python`;
- a pnpm project installs a `file:` dependency, which can need that package's build first, such as a library's `dist/`.

Such a task failed in an isolated run, which has only its declared sources, and a local run reused its result when the other directory changed.
Azad does not trigger work: a task runs again when its declared inputs change.
So the fix is to declare those inputs, not to add rules that start one target after another.

## Decision

- A pattern in `sources` or `exclude` that starts with `//` is relative to the workspace's root, such as `//libs/core/**`.
  The directory before its first wildcard is a set of sources of its own; a directory inside another joins the outer set, so that no set holds another.
  An isolated run has each set at its workspace path, so relative paths such as `../../libs/core` work, and each set is part of the digest.
- A task whose sources are all in other directories has an empty directory at its package, where its command starts.
- `deps` takes the label of a task of another package, such as `"//libs/logging:build"`, as well as a task of the same file.
  The engine reads that package, adds the task and its own dependencies to the graph, and runs them first; each runs in its own package directory, and its outputs are at their workspace paths.
- Tags and `--include` or `--exclude` do not apply to dependencies, as before (decision 0023).

## Consequences

- A task without `//` patterns has the same sources, and so the same digest, as before.
- Patterns of other directories are explicit: Azad knows no build system, so it does not read `.csproj`, `package.json` or `pyproject.toml` to find them.
  A module may find them later, if Azad gets a generic built-in to read a file.
- A dependency's package is read once for each package that depends on it; a cache of read packages can come later.
