#!/usr/bin/env bash
# A development host for Azad's services: a Debian VM in QEMU that runs the orchestrator and the executor, so that
# code from events never runs on the developer's machine. See dev/host/README.md.
#
# Usage: dev/host/host.sh up | down | reset | ssh [command...] | deploy | status
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
state=${AZAD_HOST_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/azad-host}
cpus=${AZAD_HOST_CPUS:-8}
memory=${AZAD_HOST_MEMORY:-16G}
disk_size=${AZAD_HOST_DISK:-40G}

readonly IMAGE_NAME=debian-13-generic-amd64-20261026-2643.qcow2
readonly IMAGE_URL=https://cloud.debian.org/images/cloud/trixie/20261026-2643/$IMAGE_NAME
readonly IMAGE_SHA512=5bd891c6e62d01639f52a5587b72a4607421e445732214be787dba6043cf9797a6ff90bed982aada6a6c7e619f1f956d961c4fad76757171abd607920aac098a
readonly SSH_PORT=2222
readonly SEED_PORT=8642
readonly USER_NAME=azad
readonly SHARE_TAG=azad
# slirp4netns's address for the namespace, and its DNS resolver.
readonly NAMESPACE_ADDRESS=10.0.2.100
readonly RESOLVER=10.0.2.3
readonly BOOT_TIMEOUT=600
readonly STOP_TIMEOUT=60

# The VM reaches the public internet only: never the host, its tailnet or VPN, private networks, or metadata servers.
readonly RULESET="
table inet host {
    chain output {
        type filter hook output priority 0; policy accept;
        ct state established,related accept
        oifname \"lo\" accept
        ip daddr $RESOLVER meta l4proto { tcp, udp } th dport 53 accept
        ip daddr { 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/3 } reject
        meta nfproto ipv6 reject
    }
}"

fail() {
    echo "host.sh: $*" >&2
    exit 1
}

running() {
    [ -f "$state/vm.pid" ] && kill -0 "$(cat "$state/vm.pid")" 2>/dev/null
}

# SSH without the user's own config, so that no agent and no other key reaches the VM.
host_ssh() {
    ssh -F /dev/null -p "$SSH_PORT" -i "$state/id_ed25519" \
        -o IdentitiesOnly=yes -o ForwardAgent=no -o StrictHostKeyChecking=accept-new \
        -o UserKnownHostsFile="$state/known_hosts" -o ConnectTimeout=5 -o LogLevel=ERROR \
        "$USER_NAME@127.0.0.1" "$@"
}

image() {
    [ -f "$state/$IMAGE_NAME" ] && return
    echo "host.sh: downloading $IMAGE_URL"
    local progress=--silent
    [ -t 2 ] && progress=--progress-bar
    curl -fL --show-error "$progress" -o "$state/$IMAGE_NAME.part" "$IMAGE_URL"
    echo "$IMAGE_SHA512  $state/$IMAGE_NAME.part" | sha512sum --check --quiet
    mv "$state/$IMAGE_NAME.part" "$state/$IMAGE_NAME"
}

# The VM's disk: an overlay on the image, so that `reset` starts again from the image.
disk() {
    [ -f "$state/disk.qcow2" ] && return
    qemu-img create -q -f qcow2 -b "$state/$IMAGE_NAME" -F qcow2 "$state/disk.qcow2" "$disk_size"
    rm -f "$state/known_hosts"
}

seed() {
    [ -f "$state/id_ed25519" ] || ssh-keygen -q -t ed25519 -N "" -C azad-host -f "$state/id_ed25519"
    mkdir -p "$state/seed" "$state/share/bin"
    sed "s|@SSH_KEY@|$(cat "$state/id_ed25519.pub")|" "$here/user-data" > "$state/seed/user-data"
    printf 'instance-id: azad-host\nlocal-hostname: azad-host\n' > "$state/seed/meta-data"
    : > "$state/seed/vendor-data"
}

# Runs in new user, network and mount namespaces: waits for slirp4netns, closes the network to all but the internet,
# serves the seed to the VM, and becomes the VM. The host's resolver may be on its loopback, which the namespace does
# not reach, so names resolve through slirp4netns's resolver.
inside() {
    echo "nameserver $RESOLVER" > "$state/resolv.conf"
    mount --bind "$state/resolv.conf" /etc/resolv.conf
    ip link set lo up
    for _ in $(seq 100); do
        ip route | grep -q '^default' && break
        sleep 0.1
    done
    ip route | grep -q '^default' || fail "slirp4netns did not connect the namespace"
    echo "$RULESET" | nft -f -
    python3 -m http.server "$SEED_PORT" --bind 127.0.0.1 --directory "$state/seed" > "$state/seed.log" 2>&1 &
    exec qemu-system-x86_64 \
        -name azad-host -machine q35,accel=kvm -cpu host -smp "$cpus" -m "$memory" \
        -display none -serial "file:$state/console.log" \
        -drive "file=$state/disk.qcow2,if=virtio,format=qcow2" \
        -nic "user,model=virtio-net-pci,hostfwd=tcp::$SSH_PORT-:22" \
        -smbios "type=1,serial=ds=nocloud;s=http://10.0.2.2:$SEED_PORT/" \
        -virtfs "local,path=$state/share,mount_tag=$SHARE_TAG,security_model=none,readonly=on"
}

up() {
    running && { echo "host.sh: the host runs; dev/host/host.sh ssh"; return; }
    for tool in qemu-system-x86_64 qemu-img slirp4netns nft unshare socat python3 ssh; do
        command -v "$tool" > /dev/null || fail "$tool is not installed"
    done
    mkdir -p "$state"
    image
    disk
    seed
    setsid unshare --user --map-root-user --net --mount -- "$0" inside > "$state/inside.log" 2>&1 < /dev/null &
    local vm=$!
    echo "$vm" > "$state/vm.pid"
    for _ in $(seq 100); do
        [ "$(readlink "/proc/$vm/ns/net")" != "$(readlink /proc/self/ns/net)" ] && break
        sleep 0.05
    done
    rm -f "$state/slirp.sock"
    slirp4netns --configure --mtu=65520 --disable-host-loopback --api-socket "$state/slirp.sock" "$vm" tap0 \
        > "$state/slirp.log" 2>&1 &
    echo $! > "$state/slirp.pid"
    for _ in $(seq 100); do
        [ -S "$state/slirp.sock" ] && break
        sleep 0.1
    done
    local forward="{\"execute\": \"add_hostfwd\", \"arguments\": {\"proto\": \"tcp\", \"host_addr\": \"127.0.0.1\", \"host_port\": $SSH_PORT, \"guest_addr\": \"$NAMESPACE_ADDRESS\", \"guest_port\": $SSH_PORT}}"
    echo "$forward" | socat - "UNIX-CONNECT:$state/slirp.sock" | grep -q '"return"' || fail "slirp4netns did not forward SSH"
    echo "host.sh: booting; the first boot installs packages (console: $state/console.log)"
    local start=$SECONDS
    until host_ssh cloud-init status --wait > /dev/null 2>&1; do
        running || fail "the VM stopped; see $state/inside.log and $state/console.log"
        [ $((SECONDS - start)) -lt "$BOOT_TIMEOUT" ] || fail "no SSH after ${BOOT_TIMEOUT}s; see $state/console.log"
        sleep 2
    done
    echo "host.sh: the host is up after $((SECONDS - start))s; dev/host/host.sh ssh"
}

down() {
    running || { echo "host.sh: the host does not run"; return; }
    local vm
    vm=$(cat "$state/vm.pid")
    host_ssh sudo systemctl poweroff > /dev/null 2>&1 || true
    for _ in $(seq "$STOP_TIMEOUT"); do
        kill -0 "$vm" 2>/dev/null || break
        sleep 1
    done
    kill -- "-$vm" 2>/dev/null || true
    [ -f "$state/slirp.pid" ] && kill "$(cat "$state/slirp.pid")" 2>/dev/null || true
    rm -f "$state/vm.pid" "$state/slirp.pid" "$state/slirp.sock"
    echo "host.sh: the host is down"
}

reset() {
    down
    rm -f "$state/disk.qcow2" "$state/known_hosts"
    echo "host.sh: the next up starts from the image"
}

# The binaries of the working tree, built static as a release builds them, in the VM's /opt/azad/bin.
deploy() {
    local version
    version="dev-$(git -C "$repo" rev-parse --short HEAD)"
    docker build --target dist --build-arg "AZAD_VERSION=$version" --output "type=local,dest=$state/share/bin" "$repo"
    if running; then
        host_ssh sudo systemctl try-restart azad-orchestrator
    fi
    echo "host.sh: deployed $version"
}

status() {
    if ! running; then
        echo "host.sh: the host does not run"
        return
    fi
    echo "host.sh: the host runs (pid $(cat "$state/vm.pid")), state in $state"
    host_ssh 'uptime; ls /opt/azad/bin 2>/dev/null; systemctl is-active azad-orchestrator || true'
}

case "${1:-}" in
    up) up ;;
    down) down ;;
    reset) reset ;;
    ssh) shift; host_ssh "$@" ;;
    deploy) deploy ;;
    status) status ;;
    inside) inside ;;
    *) fail "usage: dev/host/host.sh up | down | reset | ssh [command...] | deploy | status" ;;
esac
