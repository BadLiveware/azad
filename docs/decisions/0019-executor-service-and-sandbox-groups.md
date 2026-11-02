# 0019: The executor as a service, sandbox groups, volume disks, and the network of a VM

Status: accepted, 2026-11-01.

## Context

Decision 0018 gave Azad a microVM sandbox with one VM for each run of an action, no network and no cache volumes.
A VM per run pays a boot for each action, and many actions need network (fetches, dependency downloads) or a cache volume (a coarse build stage).

The executor is one of Azad's four parts ("What Azad is" in `AGENTS.md`), and the orchestrator must reach it through the plan (contract 2): open a sandbox group with a shape and a trust level, run actions in it, and get the results as events.
Until now, the executor lived only inside the `azad` process.

A CI guest with network must reach the internet only: never the host, other VMs, a metadata server or a private network.
The usual way is nftables rules on the host, made by an agent that runs as root, with tap devices that it makes.
Azad's executor must also run on a developer's machine, without root.

## Decision

### Sandbox groups

- One VM serves a sandbox group, and the runs of the group share it, also at the same time.
  The VM boots once, with no disks; the disks of each run are attached through Cloud Hypervisor's API while the run lasts, and detached after it.
- The guest init (`azad-guest`) is PID 1, and starts a worker process for each run.
  A worker has its own mount namespace, in which it mounts the run's disks and builds the command's root as before, and its own cgroup.
  When the command ends, the worker ends every process that the command left (through `cgroup.kill`), so a run cannot leave work behind in a shared VM.
  The cgroup's memory limit is the VM's memory without a reserve for the guest, so a run that uses too much memory is ended alone.
- The host finds a run's disks in the guest by the PCI address that the API gave for each, and talks to each run over a vsock connection of its own.
  The guest closes that connection when it no longer uses the disks; at a time limit, the host closes its side, and the guest ends the run.
- `MicroVm`, the sandbox, keeps two groups: one for runs without network and one for runs with network.
  A group whose VM ended (a panic, a reboot) is replaced at the next run.
- The guest brings up its loopback interface, as bubblewrap's new network namespace has it.
- A VM gets every CPU of the host, and 8 GiB of memory or half of the host's when that is less.
  The guest kernel sets up each page of memory at boot: on a laptop, a goal of three actions took 6.3 s with 8 GiB, 7.3 s with 16 GiB and 12.4 s with 60 GiB.

### Cache volumes as disks, and trust levels

- The sandbox contract names a volume with two forms: a directory, which bubblewrap mounts, and an ext4 disk, which a VM attaches.
  The VM sandbox makes the disk at its first use, with a journal, because the disk lives longer than a run.
  The volume's files are in the directory `data` of the disk, so commands do not see `lost+found`.
- A run has a trust level: `main`, `pull-request` or `none`.
  The store keeps the volumes of `main` where they always were (`caches/`), those of `pull-request` apart (`caches-pull-request/`), and gives a run of `none` a new, empty volume each time, which is removed after it.
  So a pull request never writes a volume that `main` reads.
- A run on a developer's machine is `main`; the orchestrator (phase 20) derives the level from the event, never from the code that runs.
- The two forms of a volume do not share their content, and remote caches (decision 0017) share only the directory form of `main`.

### The network of a VM

- A VM with network runs in a user namespace and a network namespace of its own, which an unprivileged user can make.
  In them, Cloud Hypervisor makes the VM's tap device, and nftables rules let the guest out to the internet only.
  slirp4netns connects the namespace to the host's network, as rootless containers do.
- The rules: the guest reaches no private network, no shared address space (which holds a tailnet), no loopback, no link-local address (which holds metadata servers), no multicast or reserved address, no address of the host's interfaces, no address of the namespace, and nothing over IPv6.
  The one exception is DNS to slirp4netns's resolver, which asks the host's resolver.
- Each VM has its own namespaces, so no VM can name another; the actions of one group share the VM's network, as the steps of a job do.
- The plan said that a helper with root would make tap devices and rules; it is not needed, so there is none.
  Hosts that run many VMs may still want the host's own rules and no user-space network stack; that comes with the hosts.

### The executor as a service

- `azad executor --socket <path>` serves the plan on a Unix socket, and `azad --executor <path>` runs a goal's actions through it.
  The task runner never needs the service: without `--executor`, actions run in the `azad` process, as before.
- Each connection is a sandbox group: the client opens it with a shape (whether its actions have network) and a trust level, and the group's VM stops when the connection closes.
  Messages are JSON, one per line, so that the protocol is easy to read and to test by hand.
- The client and the service share a store.
  An action goes to the service as the digest of its stored encoding, and the result comes back as the digest of the stored result.
- The service runs actions and keeps cache volumes; the client decides what runs and keeps the action cache.
  The service never writes the action cache, because a result that a pull request made must not become a result that `main` reads; trust levels for results come with the shared action cache.

## Consequences

- In a VM that runs already, the first line of a command arrives about 60 ms after the run starts, on a laptop; with a boot, it was 0.33 s.
- `crates/azad/tests/vm.rs` tests runs that share a VM at the same time, processes that a command leaves, a volume disk between runs (also below `/work`), runs through the executor service, and each network rule: a probe from the guest reaches none of the host's addresses, its loopback through slirp4netns, the namespace or a metadata server, while the same probe in bubblewrap reaches the host; a guest cannot reach a listener in another VM; and the internet is reachable.
  The probe was checked against rules that accept everything: it then reaches the host's addresses, and the test fails.
- Actions with network in a VM need slirp4netns, nft and sysctl on the host, and unprivileged user namespaces.
- The examples `m2-go` (with network) and `m6-modules` pass in VMs.
- The executor service has no authentication: the socket's file permissions decide who may use it.
- Colored output, which the local backend asks tools for in a terminal, is not asked for through the service yet.

## Amendment, 2026-11-01

Decision 0020 gives the action cache trust levels; the client of the executor service writes results at its own level.

## Amendment, 2026-11-03

Decision 0029 makes the service its own binary: `azad executor --socket <path>` is now `azad-executor --socket <path>`.
