# 0012: Remote caches

Status: accepted, 2026-10-31.

## Context

A CI runner starts with an empty store, so the local cache does not help it.
Results must outlive a machine: in the GitHub Actions (GHA) cache, in a directory on a shared disk, and later in an OCI (Open Container Initiative) registry or an object store.
Some of this storage is not under the control of the people who trust the results.

## Decision

- A provider stores whole objects by name, as files: `contains`, `get` and `put`.
  Objects do not change; a `put` of a name that exists keeps the object.
  The first providers are a directory (`--cache dir:<path>`) and the GHA cache (`--cache gha`).
- The remote cache keeps four kinds of objects:
  - `result-<action>`: a signed entry, "this action has this result";
  - `tree-<digest>`: a pack of an output tree, a gzipped tar of all its blobs, named by their digests;
  - `blob-<digest>`: a log;
  - `alias-<hash of the name>`: a signed entry, "this image for this platform has this tree".
  A provider such as the GHA cache has a high cost per object, so a tree is one object, not one per file.
- Entries are signed with Ed25519, and keys are written `name:base64`, as Nix writes them.
  A reader believes an entry only when a trusted key signed it: a key in `.azad/trusted-keys` of the workspace, or in `$AZAD_TRUSTED_KEYS`.
  A writer needs a signing key in `$AZAD_SIGNING_KEY`; without it, the cache is read-only, such as in a pull request from a fork.
  Content needs no signature: a pack is refused when a blob does not have the digest that names it, or when the pack does not hold the whole tree.
- A hit fetches only the entry.
  Content is fetched when an action that runs needs it, and an image is pulled only when an action that runs uses it.
  So a run in which every result is known downloads almost nothing, as Bazel's "build without the bytes".
- An entry is a hit only when the cache still has its output trees.
  Providers drop objects one at a time, and an entry is read more often than its content, so the content can go first.
  Such an entry is a miss: the action runs again and uploads its content again.
- A result or an alias that is known on this machine is uploaded when the cache does not have it, so a machine with a warm store fills an empty cache.
- The remote cache wraps any backend (`RemoteBackend`); the run loop and the local backend do not know about it.
- A cache failure does not fail a run: a failed read is a miss, and a failed write is skipped.
  Each run reports what the cache did, with its errors.

## Consequences

- A second CI run of the same inputs, on a new runner, runs nothing and pulls nothing.
- Who may write is decided by who has the signing key; who is believed is decided by the workspace's trusted keys.
  A pull request can trust its own key, but that changes only its own run.
- In the GHA cache, entries are also scoped by branch: a run reads its own branch's entries and the default branch's.
- When content is dropped between the check of an entry and the run of an action that needs it, that action fails with a clear error.
  Running the producer again needs the rules to take a result back; that is a later step.
- The GHA cache is available only to JavaScript actions, so Azad runs in GitHub Actions through its own action (`action.yml`).

## Amendment, 2026-10-31

- The tree of an image that the cache knows by its signed entry (a deferred pull) comes from the cache's pack of the tree when the cache has it, and from the registry only when it does not.
  The first machine that pulls the image stores the pack, as it stores the trees of results; the pack is checked against the tree digest, as every pack is.
  In CI, the cache gives the self-build's 700 MB Rust image faster than the registry does.
