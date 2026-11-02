# 0014: Modules: typed calls that every frontend can make

Status: accepted, 2026-10-31.

## Context

The standard library is written in Starlark, but a GHA workflow cannot run Starlark.
"Starlark is how the standard library is written, not how it is called" (`docs/vision.md`): code that makes actions must be callable from every frontend, with the same result.

## Decision

- A module is a Starlark value that `module(name, params, outputs, implementation)` makes.
  `params` gives each parameter a type: `string`, `strings`, `int`, `bool` or `patterns`, with `?` for an optional one.
  Every call also has a `name`; the module names its actions with it, and must define an action with exactly that name that declares `outputs`.
  Callers that have only text, such as GHA `with:` inputs, can give text for every type.
- Starlark calls a module as a function; the call checks the arguments, runs the implementation, and checks the call's action.
- A frontend that cannot run Starlark returns calls in its package: the module's file and name, the call's name, its package (for labels) and its directory (for file patterns), and the arguments as JSON values.
  The engine expands each call with the same evaluator, through a program that loads the module and calls it, so a call gives the same actions from every frontend.
- A module can list the files that match patterns when it is evaluated, with `files()`, which agrees with `glob()`; so it can make one action per file.
- The standard library is part of Azad: `load("@std//test.star", "split")`. Its version is Azad's version.
- The GHA frontend recognizes actions that are modules: `uses: BadLiveware/azad/std/test-split@<ref>` becomes a call of `@std//test.star`'s `split`.
  The same directory holds a composite action, so GitHub runs the step natively.
  The job's image is the module's `image` when `with:` does not give one.
  A module's actions read only what they declare, not the job's state, so they run outside the job; the job waits for the call's output, and the step does nothing in the job.
- The first module is `std/test.split`: one action per test file, which sees only that file and the shared `inputs`, and an action with the call's name that collects their reports.

## Consequences

- A change to one test runs that test only, from Starlark and from a workflow.
- A module step cannot have `if:` or expressions in `with:` yet, because calls are expanded before anything runs; `azad check` reports both.
- A module's actions run before the job's steps, not between them; a module that needs files that earlier steps make is not possible yet.
- Not done yet: a lock file for `load()` of modules outside the workspace and for module versions, and caching of expansions.
