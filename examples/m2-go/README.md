# Example: milestone M2

A small Go module with one dependency, built and tested from `tasks.star`.
`//examples/modules/go.star` defines the actions: `words-deps` downloads the dependencies once per `go.sum`, `words-build` builds without the test files, and `words-test` vets and tests.

```sh
cargo run -p azad-cli -- run //examples/m2-go:example
cargo run -p azad-cli -- run //examples/m2-go:example     # nothing runs
cargo run -p azad-cli -- why //examples/m2-go:example words-build          # explains the last run
cargo run -p azad-cli -- why //examples/m2-go:example words-build --next   # what the next run would do
```

Change `words/words_test.go` and run again: only `words-test` runs.
