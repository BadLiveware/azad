# 0007: A fetch is an action with network, cached by its action digest

Status: accepted, 2026-10-30.

## Context

A build needs its dependencies, and a package manager downloads them over the network.
A fixed-output action (decision 0002, the `fixed-output` kind) must declare the digest of its output, but a package manager does not give one digest for the whole result.
It gives a lock file, which pins each package, and it checks each package against the lock file.

## Decision

A new action kind, `fetch`:

- it has network, like an impure action;
- it is cached by its action digest, like a hermetic action;
- its inputs must hold the lock files that pin what it downloads.

The Starlark function `fetch()` makes this kind, and `lock=` names the lock files.

## Consequences

- A change of a lock file changes the action digest, so the fetch runs again; nothing else makes it run again.
- The result is verified less strongly than a fixed output: it is only as good as the package manager's check against the lock file.
  The trust rule (later) treats a fetch result like a hermetic result.
- Adding the kind adds one encoding value (`"fetch"`); no existing digest changes.
