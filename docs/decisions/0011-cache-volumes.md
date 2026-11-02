# 0011: Cache volumes

Status: accepted, 2026-10-30.

## Context

A stage can be coarse, such as "build the workspace with Cargo" (see "The shape" in `docs/vision.md`).
The tool inside keeps its own incremental state, such as Cargo's target directory.
When that state starts empty on each run, every change rebuilds everything.
A multi-stage Dockerfile solves this with `RUN --mount=type=cache`.

## Decision

- An action declares `caches`: for each absolute sandbox path, the name of a cache volume.
  The declaration is part of the action and of its digest, as a cache mount is part of a Dockerfile instruction; the content of the volume is not.
- The encoding of an action holds the field `m` (path to name) only when the action declares a cache, so no existing digest changes.
- The store keeps one directory per cache name and platform, below `caches/`.
  The sandbox mounts it writable at the declared path.
- One run at a time writes a volume: a run takes a lock on the volume, and an action whose volume is locked gets a fresh, empty private volume instead.
  It is slower, but never shares writes.
- A result must not depend on the content of a volume; the tools are trusted to use their caches correctly, as BuildKit trusts them.
- The tools can only be correct when their inputs tell the truth.
  Many tools, such as Cargo and Make, take a file as unchanged when its modification time is older than their last build.
  The files of the store's shared trees are hard links to blobs, so a file has the time when its content was first stored: after a revert, a file has new content for the build but an old time.
  A test showed it: Cargo kept the code of the old content, and Azad cached the wrong result.
  So an action with a cache volume does not mount the shared trees.
  Its inputs come from checkouts in its first volume, which are updated as a version control checkout is: a file is written again, with the time of the write, exactly when its content at that path changes.
  Each volume records whose checkouts its last run read; when that differs, every file is written again, so that no file is older than a state built from other content.

## Consequences

- A coarse stage stays fast after a change, with no change to the build tool.
- The first run with a volume copies the action's inputs into it, and later runs write only what changed.
- The root image is still mounted from the shared trees; tools identify their toolchain by its content or version, not by the times of its files.
- Volumes grow until `azad cache clear` removes them; a size limit is a later step.
- Volumes are per machine; remote caches (phase 11) share results, not volumes.
- When runs get trust levels, the volumes of each level are kept apart, so that a pull request cannot write into the volume that `main` reads.

## Amendment, 2026-11-01

Decision 0019 keeps the volumes of each trust level apart, as the last consequence above says, and gives a volume a disk form for the VM sandbox.
