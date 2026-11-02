# 0029: One binary for each kind of user

Status: accepted, 2026-11-03.

## Context

Until now, one `azad` binary held all of Azad: the task runner, the executor service with the microVM sandbox (`azad executor`, `--sandbox vm`), and the orchestrator (`azad serve`, `azad simulate`, `azad checks`).
Azad's parts are useful alone ("What Azad is" in `AGENTS.md`), and each kind of user needs only its own part:

| User | Needs |
|---|---|
| A developer, and a CI job that uses the Azad action | The task runner |
| Anyone who runs GHA workflow files through Azad | The task runner, and the GHA runner that runs the steps inside the sandbox |
| A host that runs CI | The executor, its sandboxes, and the guest of its VMs |
| The control plane of CI | The orchestrator |

A developer who installs a task runner should not get a webhook server and a VM monitor, and a CI host should not get the developer's commands.

## Decision

- **`azad`** is the task runner: `run`, `why`, `log`, `list`, `check`, `ir`, `exec`, `cache`, `key`, `pull` and `ingest`.
  - `--executor <socket>` stays: it is a small client of the plan (contract 2), so a developer can run a goal through a local executor and its VMs.
  - `--sandbox` goes: an isolated run in this process uses bubblewrap, and the microVM sandbox lives in the executor.
- **`azad-executor`** is the executor service on a Unix socket, with `--sandbox bubblewrap` or `--sandbox vm`.
  Its VMs need `azad-guest` next to it.
- **`azad-orchestrator`** has `serve`, `simulate` and `checks`, and sends actions to an executor with `--executor`.
  It does not read a developer's personal defaults: before, `include` and `exclude` in `~/.config/azad/config.toml` changed which tasks an event ran.
- **`azad-gha-runner`** stays its own static binary; `azad` finds it next to itself, or in `$AZAD_GHA_RUNNER`.
- The three commands are binaries of the `azad-cli` crate; their logic stays in the library.
- A release has one archive for each binary, and the executor's archive holds `azad-guest` too.
  The images `azad` and `azad-bin` hold only `azad`.

## Consequences

- No digest and no stored encoding changes.
- `azad --sandbox vm run <goal>` becomes `azad-executor --socket <path> --sandbox vm`, and `azad --executor <path> run <goal>`.
- On macOS, where isolated runs and VMs cannot run, a release needs only `azad`.
