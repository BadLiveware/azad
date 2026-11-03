# Azad: plan

`docs/vision.md` says what Azad is for and the ideas it is built on.
This page says where Azad is now and what comes next.
`docs/decisions/` records each choice that is expensive to change.

## Where Azad is

Azad works as a task runner and as CI inside GitHub Actions (GHA), and its own orchestrator and executor work on one machine.

| Part | What works |
|---|---|
| **Pipeline language** | Tasks and actions in Starlark (`tasks.star`), or as data in `tasks.yaml`; typed modules; a project's goals from parts (`@std//project.star`); versions from git history (`@std//version.star`); targets across packages, tags and filters; a dependency graph that each package declares, as coarse or as fine as it needs; simple GHA workflows, and GHA+ hints that make their steps cacheable |
| **Core** | The Action IR (intermediate representation) in deterministic CBOR with SHA-256 digests; the store and the action cache as files, with trust levels; the rules that decide what must run; signed remote caches (a directory, the GHA cache); cache volumes that travel through the remote cache; `azad why`, `azad log`, and notes that say what Azad decided and why |
| **Orchestrator** | Local only: webhooks in GitHub's format, durable runs, trust levels from the event, and check runs to a fake GitHub; `azad-orchestrator simulate` sends events from a local repository |
| **Executor** | Tasks on the host's tools, or in their images through Docker or Podman, on the working tree; isolated actions in bubblewrap, or in Cloud Hypervisor microVMs with sandbox groups, volume disks and a network without root; the executor as a service behind the plan |

Azad is tried on two repositories:

- **Azad itself.** `//:check` (format, lint, tests, sandbox tests) is Azad's only CI check; on a runner that keeps its store, a one-line change takes about a minute.
- **A large monorepo** with about 125 projects in .NET, Node and Python, and mobile apps.
  `azad run fmt` at its root formats each project with its own tools or in its image, and each kind of project's CI goal holds in an isolated run, with all cache hits on the second run.
  This is the test of whether Azad is usable by a real repository: what it found became decisions 0023 to 0028.

## How we plan

- Each item says what is done when it is done, and a test or a measurement proves it.
- An item that changes a digest or a stored encoding needs a decision first.
- When an item is done, it leaves this page; its decision, its tests and the README keep what it made.
- The order follows the priority in `AGENTS.md`: the executor and the orchestrator first; the language changes when real use shows a gap.

## Next

### 1. Gaps that the monorepo found

Smaller items; each one is done when the monorepo no longer needs its workaround.

- No Dockerfile build check when the Dockerfile and its contexts did not change: today each isolated run asks the runtime, which costs 2 to 3 seconds for each image.
- A `config.star` that extends a value of its parent, not only replaces it.
- Modules that give a task its own list of dependencies, not only the package's.
- A check that a task's declared sources hold what it reads, from a trace in the sandbox, so that declarations cannot drift from what the tools read.
  Dependencies can now come from project files (decision 0031): `.csproj`, uv `pyproject.toml` and `package.json`, so `package(deps = ...)` does not drift from them; the sources of a task can still drift.

### 2. Hosts

The executor on machines that Azad owns.
Azad's hosts are VMs on a developer's machine, as the development host (`dev/host/`) is: there are no remote hosts, and no host has a public address, so what people see goes through GitHub (decision 0033).

- Host identity, signed releases of Azad and of the guest kernel, capacity, metrics and logs.
- The guest kernel becomes an Azad task, so CI runs the VM tests with a kernel from the remote cache.

Done when: a goal runs on a host through the plan, and CI runs the VM tests.

### 3. The planner

Grouping, placement where the inputs are, and sizing from history.

### 4. Remote execution

Several hosts, and a shared action cache with trust levels for each result.

### 5. Targets

Ordered access, memory of what was applied, promotion, and plans bound to their review (see "Later: state outside the build" in `docs/vision.md`).

### 6. The merge queue

Entries, a speculative base, batches, and bisection that reuses results.

### 7. Policy and trust

Who may run what, central gates that a repository cannot remove, and secrets for impure actions.

### 8. Logs and history

- Done: each run's record holds its goals, its steps and their whole output (decision 0034); check runs show the steps, metrics and the end of the output (decision 0033); and `azad-site` shows the runs, a timeline of their steps, and each step's whole output, live while it runs.
- Runs as a dataset: the history of a goal's time, its cache hits and its failures, across runs, on the site.
- How long records and stores are kept, and what removes them.

## Known debts

Things that are not done, from earlier work; each one is taken when a use needs it.

- **Modules and evaluation.**
  No lock file for modules outside the workspace, module versions, or the refs of remote GHA actions.
  Evaluation results and module expansions are not cached, so `azad why` cannot show the git refs that evaluation read.
- **The language.**
  No `.value()` (small typed results between actions), no `when=` guards, and no `tool()`; an image covers the toolchain.
- **GHA.**
  No reusable workflows, Docker in jobs (`services:`, `container:`, `docker://` actions), matrices from expressions, PowerShell steps, or `if:` on a module step.
  A GHA+ part stores the tool cache again instead of reusing the one of the part before it.
- **Caches.**
  A producer does not run again when the cache dropped its content after its entry was checked (decision 0012).
  No limits on what a cache holds, and no OCI registry or object store providers.
- **GitHub.**
  The orchestrator works as a GitHub App only; it does not run nested in a GHA job yet, for a repository that cannot install the App.
  It runs a goal once for each event, so a goal with `on = ["push", "pull_request"]` runs twice for the commit of a pull request from a branch of the same repository.
- **The site.**
  `azad-site` is not in the releases yet: only the development host's `deploy` builds it.
  CI checks its server half with the workspace, but not its browser half, which needs `cargo-leptos`.
  A cached step has no time, because a result does not store how long its action took.
- **Runs.**
  The executions of a local run live in memory, so a run does not survive a restart of `azad`.
  Azad's own goals do not name events, because its sandbox tests run on the host.
- **Platforms.**
  The task runner runs on Linux and macOS; isolated runs, the executor and the orchestrator need Linux, and the microVM sandbox needs x86_64.
  Windows is not supported.

## History

| Milestone | Done | What it made |
|---|---|---|
| M1: The kernel | 2026-10-30 | Content addressing, the Action IR, bubblewrap, the rules and the run loop |
| M2: Starlark pipelines | 2026-10-30 | `tasks.star`, images that Azad pulls itself, `fetch` |
| M3: Simple GHA | 2026-10-30 | The GHA frontend and `azad-gha-runner` |
| M4: Fast stages, clear output | 2026-10-30 | Cache volumes, and the terminal, plain and GitHub renderers |
| M5: Shared caches, Azad on GitHub | 2026-10-31 | Signed remote caches, and the Azad action |
| M6: GHA+ and modules | 2026-10-31 | GHA+ hints, and typed modules that Starlark and GHA can call |
| M7: A tool people use | 2026-11-01 | The task runner on host tools, and Azad as its own CI gate |
| M8: CI of our own, locally | 2026-11-01 | The microVM sandbox, the executor service, and the local orchestrator |
| A monorepo | 2026-11-02 | Versions from git history, targets across packages, tasks in images on the working tree, parts, `tasks.yaml`, and the dependency graph |
| Releases | 2026-11-03 | Azad releases and versions itself, its CI runs on its latest release, and the Azad action installs a release |
| Real GitHub | 2026-11-03 | The orchestrator as a GitHub App on a private repository, through the development host: check runs, fetches with the App's token, and tags on the default branch as `main` |
| Check runs and a site | 2026-11-03 | A check run shows a goal's steps, metrics and output; a record of each run; `azad-site` in Leptos, with live logs, on the development host |
