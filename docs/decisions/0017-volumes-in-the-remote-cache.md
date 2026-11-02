# 0017: Cache volumes travel through the remote cache

Status: accepted, 2026-10-31.
It replaces the consequence of decision 0011 that volumes are per machine.

## Context

A CI (continuous integration) runner starts empty.
So a tool's incremental state, which a cache volume keeps (decision 0011), is empty on every run, and a one-line change builds everything from cold.
At the end of M6, the self-build took 5 minutes 22 seconds after any change; `cargo` with `rust-cache` took under one minute, because `rust-cache` keeps Cargo's state in the GitHub Actions (GHA) cache.
The remote cache (decision 0012) already moves results between machines; it can move volumes too.

## Decision

- When an action that is about to run declares a volume that this machine does not have, Azad restores the volume from the newest copy in the remote cache, if there is one.
  Actions that need the same volume at the same time wait for one restore.
- At the end of a run, Azad stores a copy of each volume that an action used and that changed in the run, when the run may write to the remote cache.
- A copy is a gzipped tar of the volume's directory: its data, its input checkouts, and the record of whose checkouts its last run read (decision 0011).
  The tar keeps modification times, because tools such as Cargo compare them.
- The object is named `azad1-volume-<id>-<time>`, where `<id>` is a hash of the volume's name and platform and `<time>` is when it was stored.
  A provider gives the newest object whose name starts with `azad1-volume-<id>-`: the GHA cache with its restore keys, a directory by the times of its files.
- A copy is not signed.
  A result never depends on a volume (decision 0011), but a tool that trusts its state wrongly could be misled by a planted volume, so the trust comes from the provider:
  the GHA cache keeps the entries of a pull request's branch apart, and a run on `main` reads only entries of `main`.
  A run that has no signing key (decision 0012), such as one from a fork, writes no volumes, as it writes no results.

- A host task (decision 0015) can name directories of its package as its caches, such as Cargo's `target/`.
  They are kept state in the same way: a new machine whose workspace does not have the directory starts it from the newest copy, and a run that changed it stores a new copy.
  Their copies have other names than volumes, because a volume's copy holds more than its data.
- A copy is stored only when the files whose content changed since the run first used the state are at least a megabyte and a hundredth of the state.
  Tools keep records of their own that change on every new machine, such as Cargo's `.rustc_info.json` and `.global-cache`; while only those changed, the copy that the cache has is as good, and a new one would cost an upload of the whole state on every run.
  The summary of a run names, for each stored copy, how much changed and its largest file.

## Consequences

- In CI, a change rebuilds what the tool rebuilds after that change locally, plus the time to restore the volumes.
- Volumes can be large, so tasks should keep them small, for example with less debug information in an isolated build.
- A provider without a lookup by prefix restores no volumes; results still work.
- Other providers (an OCI registry, object stores) must keep volumes apart by trust level before they store them.
