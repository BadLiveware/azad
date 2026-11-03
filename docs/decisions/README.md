# Decisions

Each file records one decision that is expensive to change: what we chose, why, and what follows from it.
A decision is not edited after it is made; a later decision replaces it and says so.

| # | Decision | Status |
|---|---|---|
| [0001](0001-hash-function.md) | SHA-256 is the default hash function | Accepted, 2026-10-30 |
| [0002](0002-ir-encoding.md) | The Action IR uses deterministic CBOR | Accepted, 2026-10-30 |
| [0003](0003-root-file-system.md) | The root file system of an action is an OCI image | Accepted, 2026-10-30 |
| [0004](0004-sandbox.md) | The first sandbox is bubblewrap, behind a trait | Accepted, 2026-10-30 |
| [0005](0005-rule-engine.md) | Derivation rules use `datafrog` | Accepted, 2026-10-30 |
| [0006](0006-local-state.md) | Local state is SQLite and files | Replaced by 0009 |
| [0007](0007-fetch-kind.md) | A fetch is an action with network, cached by its action digest | Accepted, 2026-10-30 |
| [0008](0008-image-pulls.md) | Azad pulls images itself | Accepted, 2026-10-30 |
| [0009](0009-local-state-in-files.md) | Local state is files | Accepted, 2026-10-30 |
| [0010](0010-gha-jobs-run-through-a-runner.md) | A GHA job is one action that runs a runner in the sandbox | Accepted, 2026-10-30 |
| [0011](0011-cache-volumes.md) | Cache volumes | Accepted, 2026-10-30 |
| [0012](0012-remote-caches.md) | Remote caches: signed entries, content on demand | Accepted, 2026-10-31 |
| [0013](0013-gha-plus-hints.md) | GHA+ hints split a job at its hinted steps | Accepted, 2026-10-31 |
| [0014](0014-modules.md) | Modules: typed calls that every frontend can make | Accepted, 2026-10-31 |
| [0015](0015-local-runs-use-host-tools.md) | Local runs use the host's tools; CI runs the same tasks in images | Accepted, 2026-10-31 |
| [0016](0016-image-config.md) | The environment of an image is the base of an action's environment | Accepted, 2026-10-31 |
| [0017](0017-volumes-in-the-remote-cache.md) | Cache volumes travel through the remote cache | Accepted, 2026-10-31 |
| [0018](0018-microvm-sandbox.md) | A microVM sandbox | Accepted, 2026-11-01 |
| [0019](0019-executor-service-and-sandbox-groups.md) | The executor as a service, sandbox groups, volume disks, and the network of a VM | Accepted, 2026-11-01 |
| [0020](0020-local-orchestrator.md) | A local orchestrator, and an action cache with trust levels | Accepted, 2026-11-01 |
| [0021](0021-built-ins-for-versions-and-git-history.md) | Built-ins for versions and git history, and a standard library that users could write | Accepted, 2026-11-01 |
| [0022](0022-notes-and-verbosity.md) | Notes say what was decided and why, and the verbosity chooses which to show | Accepted, 2026-11-01 |
| [0023](0023-targets-across-packages-and-tags.md) | Targets across packages, tags, and filters | Accepted, 2026-11-01 |
| [0024](0024-tasks-in-images-on-the-working-tree.md) | Tasks in their images on the working tree, images from Dockerfiles, and secrets | Accepted, 2026-11-01 |
| [0025](0025-projects-from-parts.md) | A project's goals from parts | Accepted, 2026-11-02 |
| [0026](0026-sources-and-deps-across-packages.md) | Sources and dependencies across packages | Accepted, 2026-11-02 |
| [0027](0027-tasks-yaml.md) | A package declared in YAML | Accepted, 2026-11-02 |
| [0028](0028-dependency-graph.md) | A dependency graph that each package declares, as coarse or as fine as it needs | Accepted, 2026-11-02 |
| [0029](0029-one-binary-for-each-kind-of-user.md) | One binary for each kind of user | Accepted, 2026-11-03 |
| [0030](0030-azad-releases-itself.md) | Azad releases itself | Accepted, 2026-11-03 |
| [0031](0031-project-files-and-ecosystem-modules.md) | Built-ins that read project files, and ecosystem modules in the standard library | Accepted, 2026-11-03 |
