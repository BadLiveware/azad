# A development host

`dev/host/host.sh` runs a Debian VM in QEMU as a host for Azad's services: the orchestrator, and the executor in it.
Use it to work on the orchestrator and the executor with real events, so that code from a pull request never runs on your own machine.

## What it isolates

- The VM runs without root.
  QEMU runs in new user, network and mount namespaces, and slirp4netns connects the namespace to the internet, as rootless containers do.
- The VM reaches only the public internet.
  nftables rules in the namespace refuse the host, private networks, a tailnet or VPN (`100.64.0.0/10`), link-local metadata addresses, and IPv6.
  Names resolve through slirp4netns, so the names of a VPN can resolve, but their addresses stay closed.
- SSH to the VM uses a key of its own and no SSH config, agent or other key of yours.
- The VM sees one directory of the host, read-only: the binaries that `deploy` builds, at `/opt/azad/bin`.
- Everything is in the state directory, `~/.local/state/azad-host` (or `$AZAD_HOST_STATE`): the image, the VM's disk, its key, and its logs.
  Delete the directory to remove all of it.

## Use

| Command | What it does |
|---|---|
| `dev/host/host.sh up` | Starts the VM; the first time, it downloads the Debian image, checks its SHA-512, and installs bubblewrap, git, nftables and slirp4netns |
| `dev/host/host.sh deploy` | Builds the static binaries of the working tree, as a release does, into `/opt/azad/bin`, and restarts the orchestrator when it runs |
| `dev/host/host.sh ssh [command...]` | Opens a shell in the VM, or runs a command in it |
| `dev/host/host.sh status` | Says whether the VM runs, and what is deployed |
| `dev/host/host.sh down` | Shuts the VM down |
| `dev/host/host.sh reset` | Shuts the VM down and removes its disk, so that the next `up` starts from the image |

It needs `qemu-system-x86_64` and `qemu-img`, `slirp4netns`, `nft`, `socat`, `python3` and Docker, and `/dev/kvm` with nested virtualization for the microVM sandbox in the VM.
`AZAD_HOST_CPUS` (8), `AZAD_HOST_MEMORY` (16G) and `AZAD_HOST_DISK` (40G) size the VM.

The orchestrator's unit, `azad-orchestrator.service`, reads its secrets from `/etc/azad/orchestrator.env` in the VM, and keeps its store in `/var/lib/azad`.
It is not enabled until it has its secrets.
