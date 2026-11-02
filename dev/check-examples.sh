#!/usr/bin/env bash
# Runs the examples. M1, M2 and M6 run twice: the first run must make the goal hold, and the second must run nothing.
# The GHA workflows have jobs that are impure and run every time; their checks are below. Azad's own check runs this script as its task `examples`.
# Usage: dev/check-examples.sh [azad binary]   (without a binary, it builds target/debug/azad first)
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$repo"
if [ $# -eq 0 ]; then
    cargo build --quiet -p azad-cli
    cargo build --quiet -p azad-gha-runner --target x86_64-unknown-linux-musl
fi
azad=${1:-"$repo/target/debug/azad"}

twice() {
    echo "== $*"
    "$azad" run "$@"
    second=$("$azad" run "$@" 2>&1 | tail -1)
    echo "$second"
    case "$second" in
        *" 0 ran,"*) ;;
        *) echo "the second run of $* ran actions again" >&2; exit 1 ;;
    esac
}

rm -rf examples/m1/rootfs
dev/host-rootfs.sh examples/m1/rootfs sh cat mkdir sort uniq wc tee
twice examples/m1/pipeline.azad:check
twice //examples/m2-go:example
twice //examples/m6-modules:example

echo "== .github/workflows/m3-example.yml"
"$azad" check .github/workflows/m3-example.yml
"$azad" run .github/workflows/m3-example.yml

# GHA+: the second run skips the hinted steps, and the workflow without its hints gives the same output.
gha_plus=.github/workflows/gha-plus-example.yml
report=$(mktemp)
echo "== $gha_plus"
"$azad" check "$gha_plus"
"$azad" run "$gha_plus"
"$azad" run "$gha_plus" --report "$report" | tail -1
grep -q '"cached":2' "$report" || { echo "the second run of $gha_plus did not skip its hinted steps" >&2; cat "$report" >&2; exit 1; }
unhinted=target/gha-plus-unhinted.yml
grep -v 'AZAD_' "$gha_plus" > "$unhinted"
"$azad" run "$unhinted" --root "$repo" >/dev/null 2>&1
words() { "$azad" log "$1" build --root "$repo" 2>/dev/null | grep '^words: '; }
diff <(words "$gha_plus") <(words "$unhinted") || { echo "the workflow without its hints gives other output" >&2; exit 1; }
words "$gha_plus"

# Modules: the workflow calls std/test.split, and the second run gets every test's result from the cache.
modules=.github/workflows/modules-example.yml
echo "== $modules"
"$azad" check "$modules"
"$azad" run "$modules"
"$azad" run "$modules" --report "$report" 2>&1 | tail -1
grep -q '"cached":4' "$report" || { echo "the second run of $modules ran its tests again" >&2; cat "$report" >&2; exit 1; }

