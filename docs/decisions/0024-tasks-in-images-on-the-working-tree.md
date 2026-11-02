# 0024: Tasks in their images on the working tree, images from Dockerfiles, and secrets

Status: accepted, 2026-11-01.

## Context

A local run runs tasks on the host's tools (decision 0015).
In a monorepo with many toolchains, such as one with .NET, Node with three package managers, Python, Swift and Gradle, a developer does not want every toolchain on their machine.
`--isolated` runs tasks in their images, but on a copy of their sources, so a task that changes the developer's files, such as a formatter, cannot use it.

Some tools need an image that no registry has, so a task's image must also come from a Dockerfile.
A Dockerfile can use anything that BuildKit can do, such as `RUN --mount=type=secret`; Azad does not try to build it itself.
Docker or Podman is on most developers' machines, and on macOS it brings the Linux VM that images need.

Restores from private registries need tokens, which must not be part of an action's digest.

## Decision

### Where a local run runs a task

- `task(local = "host" | "image")`: on this host's tools, or in the task's image, through this host's container runtime, on the files of the workspace.
  `local` in a `config.star` is the default for the tasks below it (the deepest file wins); without one, it is `host`.
- `azad run --local host|image` and `local` in the personal defaults (decision 0023) choose for every task with an image.
- An isolated run is unchanged: a task with an image runs in it, in Azad's sandbox, on a copy of its sources.

### A host action in a container

- A task that runs in its image locally is a host action with a container: `Kind::Host { workspace, container }`.
  The container names the image, by reference or by Dockerfile, and the cache volumes at absolute paths.
  It is encoded only when it is there, so the digests of other host actions do not change.
- It is cached on this machine only, as every host action is (decision 0015): the workspace is not hermetic.
- The runtime is `$AZAD_CONTAINER_RUNTIME`, or `docker` or `podman` on `PATH`.
  Azad needs one only when a task runs in its image; without one, the error says how to go on, and `azad check` reports it before anything runs.
- The workspace is mounted at its own path, so paths in the output of a tool are the paths of the files.
- The command runs as the owner of the workspace, so the files that it writes are theirs: rootful Docker runs it with `--user`, rootless Podman with `--userns=keep-id`, and rootless Docker as root in the container, which is the owner on the host.
- A cache at an absolute path is a cache volume of Azad's store, mounted into the container; a cache at a relative path is that directory of the workspace, as for the host.
- `HOME` is `/tmp` when the task does not set it, because the user has no home in the image.
- When the task takes too long, the container is removed: stopping the runtime's client does not stop it.

### Images from Dockerfiles

- `image(dockerfile = "//tools/dotnet.Dockerfile", context = ..., target = ..., args = ...)`: paths from the workspace root, so that a `config.star` can define the image.
- The container runtime builds it before the task runs (`build --quiet`), with every feature that it has; its own cache makes a build of an unchanged Dockerfile fast.
- The Dockerfile is part of the action's digest, so a change to it runs the task again.
  The rest of the context is not: a change there changes the image at the next run of the task, but does not cause one.
- A sandbox cannot build an image from a Dockerfile yet, so an isolated run of such a task is an error that says so.

### Secrets

- `task(secrets = ["NUGET_TOKEN"])`: variables of `azad`'s environment that the command gets.
  Their names are part of the digest (`Spec::secrets`), and their values are not: a secret authorizes, it does not change a result.
- A container gets them through the runtime's environment (`--env NAME`), never as arguments, which other users of the host can read; a Dockerfile build gets them as `--secret id=NAME,env=NAME`.
  A sandboxed action gets them in its environment.
- A secret that is not set is an error that names it; Azad's own secrets (decision 0021, amendment) are never given.
- On the host, a command has the whole environment anyway, apart from Azad's own secrets.

## Consequences

- A developer with only Docker can run every task that has an image, with the same image that CI uses.
- `crates/azad-cli/tests/cli.rs` runs tasks in real containers when Docker or Podman is there: the files belong to their owner, `--local host` runs the same task on the host, an image from a Dockerfile, a cache volume, a secret, and the errors without a runtime and for an isolated run; `$AZAD_CONTAINER_TESTS` makes a missing runtime a failure.
- `crates/azad/tests/ir.rs` checks that the encoding and the text form keep containers and secrets.
- Not done yet: Dockerfile images in sandboxes and CI, and macOS.
  A build for macOS could not be checked on Linux, because two C dependencies (`ring` and `blake3`) need a macOS toolchain; it needs a job on a macOS runner.

## Amendment, 2026-11-02: Dockerfile images in isolated runs

A repository often keeps a project's toolchain in the project's Dockerfile, and its CI runs isolated.
With this decision as it was, such a task needed the same image pinned a second time, by reference, which drifts from the Dockerfile.

- An isolated run builds the image of a Dockerfile with the host's container runtime, as a local run does, and stores the image's file system and `Env` as it stores a pulled image.
  The task then runs in Azad's own sandbox: the runtime builds the image, but runs nothing of the task.
- A stored image is found again by its content, a digest of its layers' digests and its `Env`, not by its image ID: BuildKit gives a rebuild of the same steps a new ID, because it records when it built.
  So the runtime's export runs only when the image's files or environment change.
- The image's tree is the action's root, so a change to the image runs the task again, and an unchanged rebuild keeps its result cached.
- A run at the trust level `pull-request` does not build images from Dockerfiles, because a build runs the Dockerfile's steps with the host's runtime, on code that is not merged; a run on `main`, and a local run, do.
- An isolated build gets no secrets yet: a Dockerfile that needs `RUN --mount=type=secret` builds only in a local run.


## Amendment, 2026-11-02: named build contexts

Dockerfiles that `docker buildx bake` builds often read other directories as BuildKit's named build contexts, with `COPY --from=<name>`, so that the main context stays the project's own directory.

- `image(dockerfile = ..., contexts = {"<name>": "//<path>"})` gives them, by paths from the workspace root; the runtime gets each as `--build-context <name>=<path>`, in a local run and in an isolated run.
- A container image from a Dockerfile encodes its contexts only when it has some, so the digests of other actions do not change.
