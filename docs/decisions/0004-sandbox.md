# 0004: The first sandbox is bubblewrap, behind a trait

Status: accepted, 2026-10-30.

## Context

The sandbox enforces what an action claims: which files it can read, whether it has network, and how much it can use.
A sandbox of our own, with Linux namespaces, Landlock and seccomp, is a large piece of security-critical code.
bubblewrap is small, well reviewed, and used by Flatpak for the same purpose.

## Decision

A `Sandbox` trait defines what Azad needs: run a command with a root tree, input trees mounted read-only, a writable work directory, network or not, and limits.
The first implementation runs `bwrap` as a process.
A hermetic action gets `--unshare-net`, and it sees only its root image, its inputs and its work directory.
Limits on time, memory and processes come from rlimits first, and from cgroups later.

## Consequences

- Azad runs on Linux only, and needs `bwrap` and unprivileged user namespaces.
- A read of an undeclared file fails because the file does not exist in the sandbox; this is what makes a wrong hint fail loudly.
- A group of actions that share one sandbox (phase 7) needs a long-lived sandbox, which the trait must allow.
- A later implementation, in microVMs or in our own code, replaces this one without a change to the rest of Azad.
