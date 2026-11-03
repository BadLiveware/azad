"""Azad builds and tests Azad: if we cannot run our own CI, we cannot run anyone's.

These are tasks (decision 0015). `azad run check` runs Cargo on this host, with your toolchain and your `target/`;
`azad run --isolated check`, as in CI, runs the same commands in Azad's image.
"""

# Azad's image (decision 0037): CircleCI's Rust image, because it has rustfmt and clippy, which the official image does
# not, with bubblewrap, so that the sandbox tests start sandboxes inside Azad's sandbox. `dev/ci/Dockerfile` makes it,
# and the workflow `CI image` publishes it. In the image, rustup and Cargo have no home, the image's Cargo home is
# read-only, and rustup uses the image's toolchain, which it cannot change. Without incremental builds and with less
# debug information, the build that CI keeps (decision 0017) stays small. On the host, none of this applies.
CARGO_HOME = "/tmp/cargo-home"
RUST = image(
    "ghcr.io/badliveware/azad-ci:8b83ed025c80@sha256:27e9c37c03c3095198ab15f7e4a7794decb6082bf6813602033d1063d5de8ff5",
    env = {
        "HOME": "/tmp/home",
        "RUSTUP_HOME": "/home/circleci/.rustup",
        "RUSTUP_TOOLCHAIN": "1.99.1",
        "CARGO_HOME": CARGO_HOME,
        "CARGO_INCREMENTAL": "0",
        "CARGO_PROFILE_DEV_DEBUG": "line-tables-only",
        "CARGO_PROFILE_TEST_DEBUG": "line-tables-only",
        "AZAD_DROP_OWN_BUILD": "1",
    },
)

# In the image, and on a CI runner, each run gets new copies of the sources, so Cargo builds this workspace's own crates
# again anyway; dropping their build at the end keeps what CI stores small. CI sets AZAD_DROP_OWN_BUILD on the runner;
# your machine keeps the build.
DROP_OWN_BUILD = 'if [ -n "$AZAD_DROP_OWN_BUILD" ]; then cargo clean --quiet --offline -p azad -p azad-cli -p azad-gha-runner; fi'

def cargo_command(command):
    """Runs `command`, then drops the build of the workspace's own crates in the image, and exits as `command` did."""
    return command + "; status=$?; " + DROP_OWN_BUILD + "; exit $status"

def cargo_caches(task):
    """What a task that runs Cargo keeps between runs: `target/`, which is your own on this host, and in the image a
    volume at the same place; and in the image, Cargo's downloads. Each task has its own, so that tasks run at the
    same time, and CI starts each from the copy of its last run (decision 0017)."""
    return {"target": "azad-" + task + "-target", CARGO_HOME: "azad-" + task + "-cargo-home"}

# The renderer's tests read the problem matcher of the Azad action.
SOURCES = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rustfmt.toml",
    "clippy.toml",
    "crates/**",
    "action/cargo-matcher.json",
]

# The integration tests that need no sandbox, and those that do: the sandbox tests start bubblewrap, on the host or, in
# the image, inside Azad's own sandbox. One Cargo command builds each dependency once; two that select packages
# differently can build a dependency twice, with other features.
SANDBOX_FREE_TESTS = ["sandbox_free", "jobs", "parts"]
SANDBOX_TESTS = ["sandboxed", "cli"]

fmt = task(
    name = "fmt",
    cmd = "cargo fmt --all --check",
    sources = SOURCES,
    image = RUST,
    doc = "Check that the code is formatted",
)

clippy = task(
    name = "clippy",
    cmd = cargo_command("cargo clippy --workspace --all-targets --locked -- -D warnings"),
    sources = SOURCES,
    image = RUST,
    network = True,
    caches = cargo_caches("clippy"),
    doc = "Lint the code, with warnings as errors",
)

test = task(
    name = "test",
    cmd = cargo_command("cargo test --workspace --locked --no-fail-fast --lib --bins " + " ".join(["--test " + name for name in SANDBOX_FREE_TESTS])),
    # The reference test compares the built-ins with their page, and the YAML test the parser with its schema.
    sources = SOURCES + ["docs/starlark.md", "docs/tasks.schema.json"],
    image = RUST,
    network = True,
    caches = cargo_caches("test"),
    doc = "Run the unit tests, and the integration tests that need no sandbox",
)

sandbox_tests = task(
    name = "sandbox-tests",
    cmd = cargo_command("cargo test --workspace --locked " + " ".join(["--test " + name for name in SANDBOX_TESTS])),
    sources = SOURCES + ["dev/**"],
    image = RUST,
    network = True,
    caches = cargo_caches("sandbox-tests"),
    doc = "Run the tests that start sandboxes, which need bubblewrap",
)

examples = task(
    name = "examples",
    cmd = cargo_command("dev/check-examples.sh"),
    sources = SOURCES + ["dev/**", "examples/**", "std/**", ".github/workflows/*-example.yml"],
    exclude = ["examples/*/rootfs/**"],
    image = RUST,
    network = True,
    caches = cargo_caches("examples"),
    doc = "Run every example twice: the second run must run nothing",
)

goal("check", require = [fmt, clippy, test, sandbox_tests], doc = "Check a change")

# The orchestrator runs this for each push and pull request. In CI, this commit's own orchestrator runs it, as it would
# for a webhook, next to stage 0's `//:check` and `//:examples` (decisions 0036 and 0037).
goal(
    "ci",
    require = [fmt, clippy, test, sandbox_tests, examples],
    on = ["push", "pull_request"],
    doc = "What CI checks: the checks of a change, and the examples",
)
