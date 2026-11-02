#!/bin/sh
# Downloads every crates.io package in a Cargo.lock, checks it against the lock's checksum, and unpacks it as a
# directory source that `cargo --offline` can build from. Azad runs this as a fetch keyed by Cargo.lock.
# Usage: vendor-crates.sh <Cargo.lock> <dest>
#        vendor-crates.sh --one <name> <version> <checksum> <dest>   (used by the script itself, for each crate)
set -eu

download="https://static.crates.io/crates"
registry="registry+https://github.com/rust-lang/crates.io-index"
parallel=8

if [ "${1:-}" = --one ]; then
    name=$2 version=$3 checksum=$4 dest=$5
    archive="$dest/.$name-$version.crate"
    curl --fail --silent --show-error --location --retry 3 -o "$archive" "$download/$name/$name-$version.crate"
    echo "$checksum  $archive" | sha256sum --check --quiet
    mkdir -p "$dest/$name-$version"
    tar -xzf "$archive" -C "$dest/$name-$version" --strip-components=1
    rm -f "$archive"
    printf '{"files":{},"package":"%s"}\n' "$checksum" > "$dest/$name-$version/.cargo-checksum.json"
    exit 0
fi

lock=${1:?usage: vendor-crates.sh <Cargo.lock> <dest>}
dest=${2:?usage: vendor-crates.sh <Cargo.lock> <dest>}
mkdir -p "$dest"
awk -v registry="$registry" '
    /^\[\[package\]\]/ { name = ""; version = ""; source = "" }
    /^name = /     { gsub(/"/, "", $3); name = $3 }
    /^version = /  { gsub(/"/, "", $3); version = $3 }
    /^source = /   { gsub(/"/, "", $3); source = $3 }
    /^checksum = / { gsub(/"/, "", $3); if (source == registry) print name, version, $3 }
' "$lock" > "$dest/.crates"
export SCRIPT="$0" DEST="$dest"
xargs -P "$parallel" -L 1 sh -c 'exec "$SCRIPT" --one "$1" "$2" "$3" "$DEST"' sh < "$dest/.crates"
echo "vendored $(wc -l < "$dest/.crates") crates"
rm -f "$dest/.crates"
