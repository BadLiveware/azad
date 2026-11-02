# Azad

> [!WARNING]
> Azad is experimental.
> Its formats, its commands and the language of `tasks.star` can change without notice, and stored caches can stop working after an update.
> Do not use it for work that you depend on.

A CI (continuous integration) system where work is a function of its inputs, and a task runner for your machine.
Pipelines are written in Starlark, or read from simple GitHub Actions (GHA) workflows, and compile to a graph of content-addressed actions.
An action whose inputs did not change does not run again.

The same `tasks.star` runs in two places (decision 0015).
On your machine, a task runs with your tools, in your working tree, as with Make or Taskfile; the tools stay fast in the way they always are.
In CI (`--isolated`), the same task runs in its image, in a sandbox that holds only the files it names, and its result is shared through a remote cache.

## The name

Azad (pronounced *ah-ZAHD*) comes from two places.
In Iain M. Banks' novel *The Player of Games*, Azad is a game so complex that an empire uses it to decide every rank and every power, and in the novel the word means "machine", or any system that holds itself together.
The word itself is *āzād* (آزاد), "free" in Persian, Kurdish, Urdu and Hindi: free of GitHub Actions, and of work that was already done.

## Documentation

`docs/vision.md` says what Azad is for, and `docs/plan.md` says what works now and what comes next.
[`docs/starlark.md`](docs/starlark.md) is the reference of the functions that a `tasks.star` file can call, such as `task()`, `image()`, `goal()` and `action()`.

## What works

- Tasks in Starlark (`tasks.star`) or YAML (`tasks.yaml`): on the host locally, in their image through Docker or Podman with `--local image`, or in a sandbox with `--isolated`, and skipped when their files did not change.
- Images from a registry, pinned by digest, or built from a Dockerfile of the repository.
- Many packages in one repository: `azad run fmt` at the root runs every package's `fmt`, tags and personal defaults choose what runs, and each package declares its immediate dependencies.
- Modules that return parts, so a project's goals come from the parts that it chooses (`@std//project.star`).
- Lower-level actions, which always run in their image, for steps that must be hermetic everywhere.
- Cache volumes, so a tool such as Cargo keeps its incremental state between runs.
- Simple GHA workflows, run without a change: `run:` steps, `actions/checkout`, `setup-*` actions, matrices, job outputs and artifacts.
- GHA+ hints: a step that says what it reads and writes is skipped when those files did not change.
- Remote caches (a directory, or the GitHub Actions cache), with signed entries, and Azad as a GitHub Action (`action.yml`).
- Its own CI, on one machine: a microVM sandbox (Cloud Hypervisor), the executor as a service, and an orchestrator that takes GitHub's webhooks and gives check runs.

## Install

Azad runs on Linux, on x86_64 and aarch64, and needs `bwrap` (bubblewrap) with unprivileged user namespaces for isolated runs.
On Ubuntu 24.04 and later, AppArmor blocks them; allow them with `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0`.

A release holds three static binaries, which must stay in one directory: `azad`, `azad-gha-runner`, which runs the steps of GHA workflows inside any image, and `azad-guest`, the init of the microVM sandbox.

| From | How |
|---|---|
| A release | Download `azad-<arch>-unknown-linux-musl.tar.gz` from [Releases](https://github.com/BadLiveware/azad/releases), and unpack it into a directory on your `PATH` |
| An image | `COPY --from=ghcr.io/badliveware/azad-bin:main / /usr/local/bin/` in a Dockerfile; the image holds only the binaries |
| A container | `docker run --rm --security-opt seccomp=unconfined --security-opt apparmor=unconfined --security-opt systempaths=unconfined -v "$PWD:/work" -v azad:/root/.cache ghcr.io/badliveware/azad:main run --isolated check`; the image has bubblewrap and git, the options let bubblewrap make its namespaces and mount `/proc`, and the volume keeps Azad's store |
| The source | `cargo install --path crates/azad-cli`, and `cargo build --release -p azad-gha-runner -p azad-guest --target x86_64-unknown-linux-musl`; a build of `azad` in this repository finds the other two in `target/` |

The images have the tag `main` for the latest commit, `sha-<commit>` for each commit, and a version for each release.

## First run

```sh
azad list                      # the goals and tasks of the workspace, and what they are for
azad run check                 # Azad checks itself: cargo fmt, clippy and test, on this host
azad run check                 # nothing changed, so nothing runs
azad run clippy                # one task, by its name
azad run --isolated check      # the same tasks in their image, as CI runs them
azad why check test            # why the task ran, or was a cache hit
azad log check test            # the output of the task
```

`azad run --force` runs a goal's tasks again even when nothing changed.

A goal has a short name in the package of the current directory, which is the nearest directory at or above it that holds a `tasks.star`.
`azad run check` and `azad run :check` run that package's goal `check`, and `azad run app:check` runs the goal `check` of the package of the directory `app`.
`why`, `log`, `ir` and `check` take the same names.
Without an action, `why` and `log` take the one that failed, or the goal's only one.

## Examples

| Example | Shows |
|---|---|
| [`examples/m1`](examples/m1) | A graph written in the text form of the IR (intermediate representation) |
| [`examples/m2-go`](examples/m2-go) | A Go module built and tested from `tasks.star` |
| [`examples/m3-gha`](examples/m3-gha) | A GHA workflow, `.github/workflows/m3-example.yml`, that GitHub and Azad both run |
| [`.github/workflows/gha-plus-example.yml`](.github/workflows/gha-plus-example.yml) | GHA+ hints (decision 0013): the second run skips the hinted steps |
| [`examples/m6-modules`](examples/m6-modules) | `std/test.split`, called from Starlark and from a workflow |

`dev/check-examples.sh` runs all of them twice, and checks that the second run runs nothing.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
