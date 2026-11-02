# 0013: GHA+ hints split a job at its hinted steps

Status: accepted, 2026-10-31.

## Context

A GHA (GitHub Actions) job is one impure action (decision 0010), so it runs every time.
GHA+ lets a step say what it reads and writes, in `env:` variables that GitHub ignores, so that Azad can skip the step when its inputs did not change.
A real step depends on what the steps before it did: `actions/setup-go` puts Go in the tool cache and on `PATH`, and `actions/checkout` puts the sources in the workspace.
So a hinted step cannot be taken out of its job alone; it needs the job's state at that point.

## Decision

- A `run:` step is hinted when its `env:` has `AZAD_KIND`:
  - `AZAD_KIND`: `hermetic` (no network) or `fetch` (network, as the `fetch` kind of decision 0007); both are cached by their inputs.
  - `AZAD_INPUTS`: patterns of the files below the workspace that the step reads, one per line or separated by spaces.
  - `AZAD_OUTPUTS`: patterns of the files below the workspace that the step writes, for the steps after it.
  The values are fixed text, not expressions, and they are not in the step's environment.
- The frontend splits a job with hinted steps into a chain of actions: a part for the steps before the first hinted step, the hinted step, a part for the steps up to the next hinted step, and so on.
  - A part is impure, as a job is.
    It saves the job's state as an output: the runner's state (env, path, step results, post actions, status) and its directories (workspace, home, temp, tool cache).
    The next part starts from that state.
  - A part also prepares the next hinted step: it evaluates the step's condition and expressions, and writes the resolved step, the input files that match `AZAD_INPUTS`, and the tool cache.
  - The hinted step is its own action, with only these inputs; its digest does not change when other files or earlier steps change.
    It runs the step, and gives its outputs, `GITHUB_ENV` and `GITHUB_PATH` changes, and the files that match `AZAD_OUTPUTS` to the next part.
  - The last part has the job's name and outputs, so jobs that need it do not change.
- A hinted step sees only stable variables: `GITHUB_WORKSPACE`, `RUNNER_TEMP`, `RUNNER_TOOL_CACHE`, `HOME`, `CI`, and the step's own files.
  It does not see `GITHUB_SHA`, `GITHUB_REF` or the run's ids, which change on each run; an expression in the script that reads them is resolved before, and is part of the digest.
  A script that names one of these variables (`$GITHUB_SHA`, `${GITHUB_REF}`) would quietly get an empty value, so its step runs without its hint, in its job, and `azad check` says why.
  A program that the script starts, such as `make`, can still read one without the script naming it; that is a wrong hint, and it is not detected.
- A hint never changes what a workflow means: without the hints, the job gives the same result, more slowly.
  A wrong hint fails loudly: a file that `AZAD_INPUTS` does not name is not in the step's workspace.

## Consequences

- A workflow with hints is still a valid workflow that GitHub runs.
- The parts still run every time, and each one stores the job's directories; a hint pays when its step costs more than that.
- When a hinted step fails, the parts after it do not run, so steps with `if: always()` or `failure()` after it do not run; `azad check` reports this.
- `continue-on-error` on a hinted step, and hints on `uses:` steps, are not supported yet.
  A hint that Azad cannot use does not stop the workflow: the step runs without it, and `azad check` reports why.
