# AGENTS.md

Rules for agents (and people) that change this repository.
Read `docs/vision.md` for the ideas Azad is built on, `docs/plan.md` for where Azad is and what comes next, and `docs/decisions/` before a change to a decided topic.

## What Azad is

Azad does two jobs, and each is useful alone:

- **Locally, a task runner**, as Make, Just and Task are, and the local part of Dagger: the same tasks run on the host's tools, or isolated in their images, exactly as CI runs them.
- **Optionally, a CI (continuous integration) system that replaces GitHub Actions (GHA) completely.**
  It does not replace GitHub: code, review, check runs and merging stay there.

Doing both at once makes the whole larger than the sum of its parts, but no part may need the others to be useful.

Its value as CI comes from owning both the definition of the work and the machines that run it.
GHA owns only the machines, and its steps are shell with no declared inputs or outputs, so it cannot know what to skip, where data is, or what a job needs.
When one system knows both, it can skip work whose result is known, place work where its inputs are, and size work from what it used before.

Azad has four parts:

| Part | What it owns | Where it is |
|---|---|---|
| **Pipeline language** | What people write: Starlark, modules, and GHA workflows that Azad ingests. Each frontend gives the Action IR and runs nothing | `frontend`, `azad-gha-runner` |
| **Core** | The Action IR, digests, the store, the rules that decide what must run, the action cache, and the explanations | `ir`, `store`, `scheduler`, `engine` |
| **Orchestrator** | Intake of events (pushes and pull requests, through webhooks), the planner, the durable state of runs, check runs on GitHub, targets, the merge queue, and policy | `orchestrator` (local: webhooks, durable runs, check runs to a fake GitHub, decision 0020) |
| **Executor** | Where actions run: a sandbox on a laptop (bubblewrap), and microVMs on hosts. It owns the hosts, the VMs, cache volumes and host identity | `sandbox` (bubblewrap, and `sandbox::vm` with `azad-guest`, decisions 0018 and 0019), `local`, `executor` (the service behind the plan) |

The parts meet only through three contracts:

1. **Pipeline language → core: the Action IR.**
2. **Orchestrator → executor: the plan.** Open a sandbox group with a shape and a trust level, then run an action in it; results come back as events (`executor`, decision 0019).
3. **Executor → action: the sandbox.** What an action can see: its inputs, the network or not, cache volumes.

People can take one part, several, or all of them:

| Use | Parts of Azad | What does the rest |
|---|---|---|
| A task runner on a laptop (`azad run`) | Pipeline language, core, local executor | Nothing else |
| Goals inside a GHA job (the Azad action) | Pipeline language, core, local executor | GHA starts the job; the GHA cache keeps results |
| Existing GHA workflows, run by Azad | GHA frontend, core, an executor | GHA or Azad's orchestrator |
| All of CI | All four parts | GitHub keeps code, review and merging |

Rules that follow from this:

- Each use in this table must work without the parts that it does not list: the task runner must never need the orchestrator or a VM.
- A part talks to another part only through its contract, never through its internals.
- Only adapters know GHA: the GHA frontend, the Azad action, and the GHA cache provider.
  The core, the orchestrator and the executor do not depend on GHA's job model.
- Until Azad has its own hosts, the orchestrator runs locally: it takes simulated push and pull request events, and gives check runs.
  It can also run nested in a GHA job.
- Only the task runner runs actions with the host's tools; runs that an event starts never do, because they run code that someone else wrote.
- The current priority is the executor and the orchestrator; the language and the GHA step runner change when real use shows a gap (see `docs/plan.md`).

## Architecture

| Crate | Holds |
|---|---|
| `azad` | The library, as modules: `ir`, `store`, `scheduler` (rules, `decide`, the run loop, the `Backend` trait), `frontend` (the contract, and the Starlark, YAML, GHA and text frontends), `sandbox`, `local` (the local backend), `executor` (the executor as a service behind the plan, and its client), `orchestrator` (webhooks, durable runs and check runs), `remote` (remote caches and their providers), `trust` (trust levels), `oci`, `history`, `log`, and `engine`, which composes them |
| `azad-cli` | The `azad` command: arguments and output only |
| `azad-gha-runner` | The GHA runner, a static binary that runs inside any image, with the GHA expression language (`expr`) and the job plan (`plan`), which the library uses too. It is a crate of its own because a musl build of the library would pull in everything else |
| `azad-guest` | The init of the microVM sandbox, a static binary, and the protocol between it and the host (`protocol`), which the library uses too (decision 0018) |

Keep the direction of the dependencies:

- `ir` depends on nothing else in Azad; `store` and `scheduler` depend only on `ir` and on each other.
- `engine` composes the parts; `azad-cli` depends only on the library.

- Put logic that another program could use in the `azad` library, never in `azad-cli`.
- Add a module to the library, not a crate; a new crate needs a reason, such as a different build target.
- A frontend returns a `Package`; it does not store, pull or run anything.
- A built-in is a generic primitive over data, such as files, actions, versions or git history; it never holds the knowledge of one ecosystem, such as how .NET builds or how a deploy tool works.
  That knowledge goes in modules (decision 0021).
- The standard library (`crates/azad/std/`) uses only the built-ins that every `.star` file gets; when it needs more, the missing primitive becomes a public built-in.
- The run loop talks to backends only through `scheduler::Backend`.

## Writing

- Use ASD-STE100 Simplified Technical English: short sentences, one meaning per word, active voice.
- Put each full sentence on its own line in Markdown.
- Define an acronym the first time a page uses it: `IR (intermediate representation)`.
- `docs/starlark.md` is made from the docstrings of the Starlark built-ins; change the docstrings, then make it again with `AZAD_UPDATE_REFERENCE=1 cargo test -p azad --test reference`.

## Code

- Write code with clear intent instead of comments.
  Comment only a workaround for a constraint that the code cannot show, such as a bug outside Azad.
- Prefer early returns, small functions, and `match` over chains of `if`; do not use nested conditional expressions.
- Use named constants, not magic strings and numbers.
- Handle only the errors that a function can act on; let the rest go up with context.
- `unsafe` is denied in the workspace; a crate that needs it allows it in one module and says why.

## Digests and encodings

- Anything that changes a digest or a stored encoding needs a decision record in `docs/decisions/`.
- Golden tests fix known digests; a change to a golden value is a decision, not a test fix.

## Tests

- Each item in `docs/plan.md` ends with tests that prove its "done when".
- Property tests cover the encodings, trees, and the merge of config; golden files fix digests and IR.
- Pure logic (`decide`, the rules, the encodings) is tested without a sandbox, a clock or a file system.
- Sandbox tests prove each claim of an action kind by trying to break it; the VM tests compare the microVM sandbox with bubblewrap.
- The examples under `examples/` run end to end in `dev/check-examples.sh`, and a second run must be all cache hits.
- Do not add tests that only repeat a constant or a configuration value.

## Before a commit

`cargo fmt --all`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace` pass.
