# 0018: A microVM sandbox

Status: accepted, 2026-11-01.

## Context

Azad replaces GitHub Actions (GHA) completely (see "What Azad is" in `AGENTS.md`), so it needs an executor of its own.
Its sandbox until now is bubblewrap: namespaces on the host's kernel.
That is enough for a laptop, but not for CI that runs code from pull requests next to other work: the kernel is shared, and a job cannot have its own `/dev/kvm`.

Cloud Hypervisor microVMs are a proven way to run CI jobs: each job gets its own kernel, and a VM boots in a fraction of a second.
The hard parts are known: the guest kernel, read-only images as EROFS (Enhanced Read-Only File System) disks on persistent memory, the VMM (virtual machine monitor) without privileges, and what to do when a guest reboots or panics.
A runner that runs GHA jobs in VMs never sends work across the VM boundary, because the GHA runner in the guest pulls its job from GitHub; Azad must send the work in and get the results out.

## Decision

- The sandbox contract gets a second implementation, `sandbox::vm`, next to bubblewrap: one Cloud Hypervisor VM for each run of an action.
  `--sandbox vm` chooses it; bubblewrap stays the default.
- The VM gives the command the same view as bubblewrap, so an action gives the same outputs in both: the root's entries read-only, `/proc`, a minimal `/dev`, `/tmp` and `/work` writable, the read-only mounts, and the uid and gid of the host user.
- The data plane is disk images:
  - the root of an action becomes a read-only EROFS image, made once for each root tree and given to the VM as persistent memory, so the guest maps it with DAX (direct access) and does not copy it;
  - the work directory, `/tmp`, the read-only mounts and the run's spec go on an ext4 run disk for each run, made with `mkfs.ext4 -d` from hard links, without root.
- The guest init is a static Rust binary, `azad-guest`, in an initramfs that Azad makes; there is no systemd and no other process in the guest.
  It runs the command and sends each line of output, the exit code and a tar of the work directory to the host over vsock (a socket between the host and one VM), in frames of the crate's protocol.
  The host puts the work directory in place, so the local backend does not know which sandbox ran the action.
- The guest kernel is Azad's own build: `guest/kernel` builds a kernel.org release from Cloud Hypervisor's guest configuration (`ch_defconfig`), with `azad.config` merged over it, which holds only the options that the guest init and the VM's devices need.
  The build fails when an option of `azad.config` does not survive the merge.
- Cloud Hypervisor runs as the user, with Landlock and its default seccomp filter.
  The host kills the VM at the action's time limit, when the guest has not booted in 30 seconds, when the event monitor reports a reboot or a panic (Cloud Hypervisor would boot the guest again), and when the guest does not power off soon after it sent everything.
- The first version gives no network and no cache volumes: an action that needs either fails with a clear error. Phase 19 adds both.

## Consequences

- A command's first line arrives about a third of a second after the VM starts, on a laptop; the image of a root is made once, and a run disk costs a `mkfs.ext4`.
- The VM sandbox needs `/dev/kvm`, Cloud Hypervisor, `mkfs.erofs` and `mkfs.ext4` on the host, and Linux on x86_64.
- The bubblewrap sandbox now makes its root read-only, as the contract always said: before, a command could create files at the top of `/`, which went nowhere.
  The VM sandbox's tests found this, because they compare the two.
- The run disk is a copy of the work directory and the mounts, made from hard links where it can be; large inputs could become read-only images of their own later.
- Events that need root in the guest, such as a panic or a reboot, cannot happen from an action, which runs as the host user; the host still handles them.

## Amendment, 2026-11-01

Decision 0019 replaces one VM for each run with one VM for each sandbox group, whose runs share it, and adds network and cache volumes.

## Amendment, 2026-11-03

Decision 0029 moves the microVM sandbox out of `azad`: `azad-executor --sandbox vm` runs it, and `azad --executor <socket>` sends a goal there.
