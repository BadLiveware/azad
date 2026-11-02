# 0009: Local state is files

Status: accepted, 2026-10-30. It replaces decision 0006.

## Context

Decision 0006 put the action cache, and later the image aliases and the records of finished runs, in SQLite.
Few people expect a database behind their build scripts, and the bundled SQLite needs a C compiler to build Azad.
The data is small and simple: lookups by digest, and an append-only list of runs per goal.
Blobs and trees are already files.

## Decision

All local state is files in the store directory:

| Directory | Holds | Named by |
|---|---|---|
| `blobs/` | Content | Its digest |
| `trees/` | Materialized trees | The tree digest |
| `ac/` | The digest of the result of each action | The action digest |
| `aliases/` | A digest under a name, such as the tree of a pulled image | The digest of the name |
| `runs/` | The record of each finished run, numbered per goal, and a `goal` file with its label | The digest of the goal label |
| `tmp/` | Files while they are written | |

A file is written to `tmp/` and renamed into place, so a reader never sees a partial file, and several processes can share a store.
A new run takes the next free number; two runs that finish at the same time get different numbers.

## Consequences

- Azad has no database, and a store can be read with `ls` and `cat`.
- There are no queries over run history; a need for them is a later decision.
- Executions in progress are not stored, as before; a run that must survive a restart needs a later decision, and a server has its own storage.
