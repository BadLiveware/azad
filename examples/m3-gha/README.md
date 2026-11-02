# Example: milestone M3

The workflow is `.github/workflows/m3-example.yml`, so GitHub runs it natively on every push, and Azad runs the same file without a change: `build` checks out, sets up Go with `actions/setup-go`, tests and builds the M2 example, and uploads the program; `use` runs it for each matrix input.

```sh
cargo build --release -p azad-gha-runner --target x86_64-unknown-linux-musl
cargo run -p azad-cli -- check .github/workflows/m3-example.yml
cargo run -p azad-cli -- run .github/workflows/m3-example.yml
cargo run -p azad-cli -- run .github/workflows/m3-example.yml:use
cargo run -p azad-cli -- log .github/workflows/m3-example.yml use.0
```

`azad check` lists what Azad runs differently: `actions/checkout` copies the working tree, and `actions/cache` does nothing.
