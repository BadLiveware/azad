# Azad: vision

Azad is a CI (continuous integration) system in which work is a function of its inputs.
It replaces GitHub Actions completely, and people can take one of its parts or all of them (see "What Azad is" in `AGENTS.md`).
This page says what Azad is for, the ideas it is built on, and what it is not.
`docs/plan.md` says where Azad is and what comes next.

## The problem

Most CI systems run a list of shell commands in a fresh machine.
They do not know what a command reads or writes, so they cannot know what is already done.
So they do the same work again on every run, and the author must tell them how to split the work into jobs and steps, and where to run each one.

The only general way to do less is to not do again what is already done.
That needs a system that knows the inputs and outputs of each piece of work.
Build systems such as Nix and Bazel know this, but they ask for a rewrite of everything before they give anything back.

## Goals

1. **Do not do again what is already done.**
   An action whose inputs did not change does not run; its result is known.
2. **No job or step.**
   People declare actions and their dependencies.
   Azad decides how to group and place them, for each run.
3. **The same definition in CI and locally.**
   Locally, Azad is a task runner: a task uses the host's tools, which are already fast there.
   In CI, the same task runs hermetically in its image, and is not done again when its inputs did not change.
   `--isolated` runs locally exactly what CI runs (decision 0015).
4. **Reusable definitions that compose.**
   Modules are typed functions that return tasks, and a project is the sum of the parts that it chooses, not a subclass of a template (decision 0025).
   Config is selected by the nearest `config.star`, and passed to modules as explicit arguments.
5. **Ingest simple GitHub Actions workflows.**
   An existing workflow runs without a rewrite, and gets better as its steps declare more about their inputs.
6. **Explain every decision.**
   "Why did this run?" and "why was this refused?" have an answer that can be shown.

## Overview: a compiler

Azad is a compiler.
Frontends compile what people write to an intermediate representation (IR), a core decides what must run and how, and executors run the result.

| Compiler | Azad |
|---|---|
| Frontends | Starlark, and GitHub Actions (GHA) workflows |
| IR | The Action IR: resolved actions and their inputs |
| Middle end | Rules and the planner |
| Target code | The plan |
| Target machines | Executors: a laptop, a CI host |

There are two levels:

| Level | Says | Made by | Differs per run? | In the digest? |
|---|---|---|---|---|
| **Action IR** | What must happen | Frontends | No | Yes |
| **Plan** | How and where | Planner | Yes | No |

The Action IR is the same on a laptop and on a CI host; only the plan differs.
This is what makes local and CI runs equal.

The comparison has one limit: outputs arrive during the run, so the core derives again after each event, like the adaptive execution of a query engine.

## The shape: a multi-stage Dockerfile, generalized

A pipeline is a set of stages, as in a multi-stage Dockerfile, but not tied to images:

| Dockerfile | Azad |
|---|---|
| `FROM image AS build` | An action's root image |
| `RUN ...` | The action's command: the project's own tools (`cargo`, `go`, `npm`) do the work |
| `COPY --from=build /out` | An input that is another action's output |
| The layer cache | Skipping an action whose digest has a known result |
| `RUN --mount=type=cache` | A cache volume: a directory that a tool keeps between runs, outside the digest |

Azad does not replace a project's build tool, and nothing has to be described in Azad to be built.
A stage can be as coarse as "build the workspace"; the tool inside keeps its own incremental cache in a cache volume.
Finer stages are a choice where they pay, not a requirement.

## Foundations

The base is **pure functions over content-addressed values**, the model of Nix.
Each question has its own foundation:

| Question | Foundation |
|---|---|
| What is an action, and what is "the same"? | Content addressing: a Merkle DAG (directed acyclic graph) in which the name of each node is the hash of its content |
| What is a module? | A function from config to a set of actions |
| How does config combine? | A merge that does not depend on order: the nearest value wins, and a conflict at one level is an error |
| What must run? | Facts and rules (Datalog), with the graph as a derived relation |
| What can an action reach? | Capabilities, which the sandbox enforces |
| How much is known before running? | Applicative, selective and monadic builds ("Build Systems à la Carte"; "Selective Applicative Functors") |

## Actions

An action is one command with declared inputs, declared outputs, an environment and a kind.
Its identity, the action digest, is the hash of all of these.

| Kind | Network | Cached by | Example |
|---|---|---|---|
| **Hermetic** | No | The digest of its inputs | Compiling, unit tests |
| **Fixed-output** | Yes | Its declared output digest, or the digest of a lock file | Fetching dependencies |
| **Impure** | Yes | Never | Deploys, tests against live services, steps with undeclared inputs |

The sandbox enforces the kind; the definition only claims it.
The outputs of an impure action are still stored by digest, so the actions after it can still be cache hits.

## The Action IR

The IR is data, not a language.
Frontends resolve modules, config and loops, so the IR has none of them.
It has a text form for reading and debugging, but nobody writes it by hand.

- **Canonical encoding.** Digests come from the IR, so the encoding is fixed and specified.
- **Output references.** An input can be the output of another action; it becomes a digest when that action finishes.
- **Guards.** A condition on an action that is in the graph, decided by an output at run time.
- **Actions that output IR.** Work that is only known at run time, such as a dynamic matrix, is an action that produces more IR.
- **Calls.** A call names a module, a version and typed arguments, and the core expands it.
- **Untrusted.** The receiver computes every digest again, and policy, not the IR, adds trust.

## Facts and rules

The core is a set of facts and rules.
Frontends give facts (actions and dependencies), executions give facts (outputs and failures), and rules derive what must run:

```prolog
needed(A)   :- goal(A).
needed(D)   :- needed(A), depends_on(A, D).
blocked(A)  :- depends_on(A, D), not has_output(D, _).
must_run(A) :- needed(A), not blocked(A), not has_result(A).
```

Rules are data, so each decision can be shown as a derivation.

Rules decide what must exist.
A small state machine, `decide(state, event) → (new state, effects)`, makes one execution happen and cleans up after it.
A run has no state of its own: it is a goal and derived facts.
The unit is the action, not the run, so two runs that need the same digest share one execution.

## Definitions

People write Starlark, a deterministic, sandboxed dialect of Python, or declare a package as data in `tasks.yaml`, which Azad turns into the same Starlark (decision 0027).
A module is a function that returns parts, and a project's goals come from its parts:

```python
load("@std//project.star", "part", "project")

def _dotnet(solution, sdk):
    build = task(name = "dotnet-build", cmd = ["dotnet", "build", solution], image = sdk, sources = ["**"], generates = ["bin"])
    test = task(name = "dotnet-test", cmd = ["dotnet", "test", solution], image = sdk, deps = [build], sources = ["**"])
    return [part(build = build, test = test, ci = ["test"])]

dotnet = module(name = "dotnet", params = {"solution": "string", "sdk": "image"}, returns = "parts", implementation = _dotnet)

project(dotnet(solution = "App.sln", sdk = config.DOTNET_SDK))
```

Three rules for the language:

1. **Evaluation time and run time are separate.**
   A run-time value is an opaque reference, so a Starlark `if` on it is an error that points to a guard.
2. **Definitions cannot see the trigger.**
   A goal means the same locally and in CI.
3. **The safe way is the short way.**
   An isolated task and `action()` are hermetic; network access must be asked for.

Config is an explicit input of a module, not inherited silently, so a change of unrelated config does not invalidate the cache.

## Ingesting GitHub Actions

The GHA frontend is a second compiler: a workflow gives the same IR as Starlark.
A step becomes an impure action, and a job becomes a group of actions that share one sandbox.
Steps with known meaning give facts without any change: `actions/checkout` is a source input, and the artifact actions are edges between groups.

**GHA+** is an additive form of GHA: one valid workflow file, with hints that Azad reads.

- Recognized actions: a module is a real GHA action, and Azad turns its name into a module call.
- Reserved variables: hints such as `AZAD_INPUTS` and `AZAD_KIND` go in `env:`, which GHA ignores.

A hint never changes what a workflow means.
If all hints are deleted, the result is the same, only slower.
A wrong hint fails loudly, because the sandbox denies the undeclared read.

## The standard library

Starlark is how the standard library is written, not how it is called.
Rules and options are data, so every frontend has them.
Code that generates actions is called through a typed module interface, so a GHA workflow can call it too.

## Later: state outside the build

Builds are pure functions; deployed environments are not.
A **target** is a named external thing with memory: ordered access, a record of what it last applied, promotion between stages, and waiting without holding a machine.
Targets come after the fundamentals work.

## Non-goals

- Describing everything in Azad, as Bazel asks.
  Azad orchestrates stages; the project's build tools stay in charge inside them.
- Replacing GitHub as the place for code review, check runs and merging.
- Full compatibility with every GHA feature.
  A workflow that Azad cannot run gets a clear reason before it starts.
- A plugin system for arbitrary scripts that run around actions.
- Windows hosts, and macOS hosts at first.

## Prior art

Nix (derivations and action kinds), Bazel and Buck2 (Starlark, remote execution, the Remote Execution API), BuildKit (an IR for builds), Dagger (the same run locally and in CI), Zuul (speculative merge queues and trusted config), Concourse and Kargo (resources and promotion), GitLab (resource groups, merge trains, central policy), and Nx (affected tasks and flaky detection).
