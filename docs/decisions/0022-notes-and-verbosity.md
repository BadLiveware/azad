# 0022: Notes say what was decided and why, and the verbosity chooses which to show

Status: accepted, 2026-11-01.

## Context

Azad makes decisions that the user does not see in the result: for example, the count of a commit came from GitHub's API, because the clone is shallow (decision 0021, amendment).
A decision that changes what happens must be visible, or a user cannot understand why a run did what it did.
A good release versioner shows this: each decision carries the resolver that made it and its reason, such as `Version increment: minor (from ConventionalCommitResolver: feat: ...)`.
Its information level shows the decisions, and its debug level shows the details.

`azad why` explains one action after a run, from the run's records.
It does not show decisions that evaluation makes while it loads a goal.

## Decision

- A note is what was decided, and why, in one sentence: `git: the commit count of 3f9c2d1 (4812) came from GitHub's API (example/monorepo), because this clone is shallow`.
  It has a level:
  - normal: a decision that changes what happens, such as facts from another source;
  - diagnostic: a detail of how a decision was made, such as the commit that a ref named, or a count from the local repository.
- A frontend puts its notes in the package (`Package::notes`), once each; the engine gives them as events (`Event::Note`) when it loads a goal.
- `azad --verbosity minimal|normal|diagnostic` chooses what is shown beyond the results, warnings and errors.
  The names come from .NET's verbosity levels.
  - `minimal`: the results, the summary of the remote cache, warnings and errors.
  - `normal`, the default: also the normal notes, and downloads and slow steps.
  - `diagnostic`: also the diagnostic notes.
- The renderers show a note as a line (`note: ...`).
  In GitHub Actions, a normal note is a notice annotation, so the run's page shows it; a diagnostic note is a line of the log.
  The Azad action takes `verbosity` as an input.
- `azad why` stays the tool that explains one action after a run.

## Consequences

- The sources of git facts are the first notes; other decisions can become notes in the same way, such as a goal that runs no action because every result is known.
- Notes are not kept in run records, so `azad why` does not show them.
- The renderers' test runs each verbosity through the plain and the GitHub renderer.

## Amendment, 2026-11-03: `print()`

A `.star` file can call Starlark's `print()`, such as to see a value while it writes a module.
What it prints is a normal note that names the file that was evaluated: `//tools/show.star printed: ...`.
