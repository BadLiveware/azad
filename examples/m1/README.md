# Example: milestone M1

A graph of four actions in the text form of the Action IR: `sort` reads the sources, `count` and `unique` read the output of `sort`, and `report` reads both.

```sh
dev/host-rootfs.sh examples/m1/rootfs sh cat mkdir sort uniq wc tee
cargo run -p azad-cli -- run examples/m1/pipeline.azad:check
cargo run -p azad-cli -- run examples/m1/pipeline.azad:check      # all cache hits
cargo run -p azad-cli -- why examples/m1/pipeline.azad:check count          # explains the last run
cargo run -p azad-cli -- why examples/m1/pipeline.azad:check count --next   # what the next run would do
```

Change `src/words.txt` and run again: only the actions whose inputs changed run.
