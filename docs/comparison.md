# Azad and other tools

This page places Azad among tools that people know: Make, Just and Task (Taskfile), which run commands, and Bazel, which owns the whole build.
Azad sits between them.
`docs/vision.md` says why.

## In short

- **Make, Just and Task** run your commands on your machine.
  They are simple, fast to adopt, and know little about what a command reads, so CI must run everything again on each new runner.
- **Bazel** knows every input of every action, runs them hermetically, and shares results through a remote cache.
  In exchange, you describe the whole build in its rules, down to each library.
- **Azad** runs your own commands, as the simple tools do, and gives them what Bazel gives actions: declared inputs, a sandbox, content addresses and a shared cache.
  Locally, a task runs with your tools in your working tree.
  In CI, the same task runs isolated in its image, and is skipped when another machine already ran it with the same inputs.
  Azad is also a CI system, which none of the others are.

## Side by side

| | Make | Just | Task | Bazel | Azad |
|---|---|---|---|---|---|
| **What it is** | Builds files from rules | Runs commands | Runs commands, and skips some | A hermetic build system | A task runner, and a CI system |
| **Language** | Makefile | justfile | YAML | Starlark (BUILD files) | Starlark, or YAML (`tasks.yaml`) |
| **Skips work when** | A target file is newer than its prerequisites | Never | The checksums or times of `sources` match what this machine saw last | The action's digest has a known result | The action's digest has a known result |
| **What it knows about inputs** | The prerequisites, which it trusts | Nothing | The `sources` globs, which it trusts | Every input, which the sandbox enforces | The declared sources, which the sandbox enforces in an isolated run |
| **Sandbox** | No | No | No | Yes | In an isolated run: bubblewrap, or a microVM |
| **Shared cache** | No | No | No | Yes, through the Remote Execution API | Yes: signed entries in a directory or the GitHub Actions cache |
| **Remote execution** | No | No | No | Yes | Not yet (see `docs/plan.md`) |
| **Who does the build** | Your recipes | Your recipes | Your recipes | Bazel's rules for each language | Your own tools, such as `cargo`, `dotnet` or `pnpm` |
| **Reuse** | `include`, pattern rules | Imports | Includes | Macros and rules | Typed modules that return parts, and a `config.star` for each directory |
| **CI** | Another system runs it | Another system runs it | Another system runs it | Another system runs it | Built in: inside GitHub Actions, or with its own orchestrator and executor |

## Against Make, Just and Task

Locally, Azad is the same kind of tool: a task runs your command, on your tools, in your working tree, and does not run again when its sources did not change.

The difference is what the same definition does in CI:

- `--isolated` runs each task in its image, in a sandbox that holds only the files that the task declares.
  A task that reads an undeclared file fails, so a wrong declaration shows itself instead of giving an old result.
- Results go to a remote cache, signed, so a new CI runner does not do again what another machine did.
  Make, Just and Task keep their state on one machine, so on an empty runner they run everything.
- Azad holds many packages in one repository: `azad run fmt` at the root runs each package's own `fmt`, and each package declares only its immediate dependencies.
  Task has includes, but no shared template that a project can take and change, and no settings that apply to everything below a directory.
- `azad why` says why a task ran or was skipped, and what changed since its last run.

## Against Bazel

Bazel and Azad share their core: content addresses, hermetic actions, a remote cache, and Starlark.

The difference is how much you must describe:

- Bazel wants the whole build in its own rules.
  In return, it builds again exactly what changed, down to one library, and can run actions on many machines.
  The price is a rewrite of the build before it gives anything back, which is the first reason why teams do not adopt it.
- Azad treats a stage as a multi-stage Dockerfile does: "build the solution with `dotnet build`" is one action, and the tool keeps its own incremental state in a cache volume.
  You make the graph finer only where it pays, such as a slow library that many projects use (decision 0028).

The price of Azad's way is granularity: when any input of a stage changes, the whole stage runs again, and the tool inside must be fast.

Bazel is ahead on remote execution, a query language (`bazel query`), years of use in production, and Windows and macOS.

## Where Azad is weaker

- Azad is experimental: its formats and its language can change, and stored caches can stop working after an update.
- Azad runs only on Linux, and isolated runs need bubblewrap with user namespaces.
- Make and Just are one small program that most machines have, or get in seconds.
- For a small project with a few commands, Just or Task is simpler, and enough.

## Which one to choose

| You want | Choose |
|---|---|
| Names for a few commands | Just, or Task |
| Files built from other files, where times are enough | Make |
| Exact incremental builds and remote execution, and you can describe the whole build | Bazel |
| The commands that you already run to be your CI, skipped when nothing changed, without a new build | Azad |
