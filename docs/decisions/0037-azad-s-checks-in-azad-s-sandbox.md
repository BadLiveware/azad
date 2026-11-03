# 0037: Azad's checks run in Azad's sandbox

Status: accepted, 2026-11-03.

## Context

Azad's sandbox tests and examples were tasks without an image, so they ran on the host's tools, also in CI (decision 0015, amendment).
They start bubblewrap, and the Rust image of Azad's tasks did not have it.
The orchestrator runs only isolated tasks, because an event runs code that someone else wrote, so it ran only a part of Azad's checks: `//:isolated-check`, without the sandbox tests and the examples (decision 0036).

A bubblewrap sandbox can start another one: Linux lets a user namespace make a user namespace in it, and bubblewrap mounts its own `/proc` in its own PID (process ID) namespace.
In an isolated run in an image with bubblewrap, all of the sandbox tests and the examples pass.
What was missing was bubblewrap in the image.

## Decision

- `dev/ci/Dockerfile` is the image of Azad's own tasks: CircleCI's Rust image, with bubblewrap and the targets of `rust-toolchain.toml`.
  The workflow `CI image` publishes it for amd64 and arm64 as `ghcr.io/badliveware/azad-ci` when a push to main changes it.
- `tasks.star` names it by its digest, as it named CircleCI's image before, and every task of `tasks.star` runs in it in an isolated run: `fmt`, `clippy`, `test`, `sandbox-tests` and `examples`.
  `sandbox-tests` and `examples` get the network, for Cargo's downloads and for the images that the examples pull.
- It is an image by reference, not `image(dockerfile = ...)`, because a run of a pull request builds no image from a Dockerfile (decision 0024, amendment), and the orchestrator runs pull requests.
  So a change of the image takes two commits: the Dockerfile, and then its new digest in `tasks.star`.
- The goal `ci` holds all of the checks, `//:check` and the examples, and it names `push` and `pull_request`, so the orchestrator runs all of Azad's CI.
  `//:isolated-check` goes, because nothing is left out of it any more.
- In CI, no task runs on the runner any more, so the self-build jobs no longer prepare the runner's Rust toolchain for them.
- A local run is as before: `azad run check` runs the tasks on the host's tools, and the sandbox tests start the host's bubblewrap.

## Consequences

- The sandbox tests run in bubblewrap 0.6.1 of Ubuntu 22.04 in CI, and in the host's bubblewrap locally, so both versions are tested.
- The VM tests still say that they cannot run and pass, because the sandbox has no `/dev/kvm`.
- The examples script makes a git repository of the files, outside the work directory, when it runs in a sandbox, because the GHA examples copy the working tree as git sees it.
- The job `orchestrator` in Azad's CI runs `//:ci` forced (decision 0036, amendment): this commit's own orchestrator runs all of Azad's checks in its sandbox.
