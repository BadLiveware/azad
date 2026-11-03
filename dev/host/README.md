# A development host

`dev/host/host.sh` runs a Debian VM in QEMU as a host for Azad's services: the orchestrator, and the executor in it.
Use it to work on the orchestrator and the executor with real events, so that code from a pull request never runs on your own machine.

## What it isolates

- The VM runs without root.
  QEMU runs in new user, network and mount namespaces, and slirp4netns connects the namespace to the internet, as rootless containers do.
- The VM reaches only the public internet.
  nftables rules in the namespace refuse the host, private networks, a tailnet or VPN (`100.64.0.0/10`), link-local metadata addresses, and IPv6.
  Names resolve through slirp4netns, so the names of a VPN can resolve, but their addresses stay closed.
- Only two ports of the VM reach the host, both on its loopback address: SSH (2222) and the site (8485).
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

## Services

`deploy` installs the units in `dev/host/units/` and restarts the services that run.
Each reads its settings from a file in `/etc/azad/` in the VM, which you write there yourself, so that secrets never pass through the repository or the share:

| Service | File | Settings |
|---|---|---|
| `azad-orchestrator` | `/etc/azad/orchestrator.env` (root, 600) | `AZAD_WEBHOOK_SECRET`, the GitHub App's webhook secret; `AZAD_GITHUB_APP_ID`, its ID |
| | `/etc/azad/app.pem` (root:azad, 640) | The GitHub App's private key |
| `azad-relay` | `/etc/azad/relay.env` | `AZAD_RELAY_URL`, the relay channel that is the App's webhook URL, such as one of smee.io |
| `azad-site` | none | The site of the runs (decision 0034), which the host forwards to http://127.0.0.1:8485 |

The orchestrator keeps its store in `/var/lib/azad`.
Start the services with `dev/host/host.sh ssh sudo systemctl enable --now azad-orchestrator azad-relay azad-site`, and read their logs with `journalctl -u`.
A relay's channel can be read by anyone who has its URL, so use one only for a test repository (decision 0032).
