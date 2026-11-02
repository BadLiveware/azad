#!/usr/bin/env bash
# Builds a root file system from the host's own programs and the libraries that they load, for examples and manual
# tests that must not need network access. The result is the same on one host every time, but differs between hosts.
# Usage: dev/host-rootfs.sh <dest> [program...]
set -euo pipefail

dest=${1:?usage: dev/host-rootfs.sh <dest> [program...]}
shift
programs=("$@")
if [ ${#programs[@]} -eq 0 ]; then
    programs=(sh bash cat cp mkdir ls sleep echo wc rm touch sort head tr)
fi

mkdir -p "$dest/usr/bin"
ln -sfn usr/bin "$dest/bin"

copy() {
    mkdir -p "$(dirname "$2")"
    cp -L "$1" "$2"
    chmod 0755 "$2"
}

for program in "${programs[@]}"; do
    source=$(type -P "$program")
    copy "$source" "$dest/usr/bin/$program"
    ldd "$source" | tr -s ' \t' '\n' | grep '^/' | while read -r library; do
        copy "$library" "$dest$library"
    done
done
