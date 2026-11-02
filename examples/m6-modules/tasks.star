"""Milestone M6: the module std/test.split makes one action per test file (decision 0014).
.github/workflows/modules-example.yml calls the same module from a GHA workflow."""

load("@std//test.star", "split")

unit = split(
    name = "unit",
    files = "tests/*.sh",
    command = "sh {file}",
    inputs = ["lib/**"],
    image = "node:24-bookworm@sha256:e7add2a490bb17d97b485b8641bd88455fff7839ade0f5eba493c3508124d413",
)

goal("example", require = [unit])
