# 0006: Local state is SQLite and files

Status: accepted, 2026-10-30; replaced by decision 0009 the same day.

## Context

A local run must survive a crash or a reboot: executions in progress, events, and the action cache.
Blobs can be large, and a database is a poor place for them.

## Decision

- Blobs are files in the store directory, named by their digest, in subdirectories by the first bytes of the hash.
  A blob is written to a temporary file and renamed into place, so a reader never sees a partial blob.
- Executions, events and the action cache index are in SQLite, in write-ahead log (WAL) mode.
- Storage is behind traits, so a server can use another database later.

## Consequences

- The store can be checked by hashing its files again.
- One machine's state is one directory, which is easy to delete, copy and inspect.
- Garbage collection of blobs that nothing references is a later step.
