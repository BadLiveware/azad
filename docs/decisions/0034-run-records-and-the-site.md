# 0034: Run records, and a site of the runs

Status: accepted, 2026-11-03.

## Context

A check run on GitHub shows a goal's steps and the end of their output (decision 0033), within GitHub's limits.
People also want what CI services such as Buildkite and Depot give: a site with the runs, a timeline of their steps, the whole output of each step, followed live while it runs.
Azad's hosts are VMs on a developer's machine (`docs/plan.md`), so the site runs there too, and people reach it on that machine; a host with a public address can serve it to others later.

## Decision

### A record of each run

- The orchestrator appends each event of a run to `record.jsonl` in the run's directory, as it happens: one JSON object on each line, with the time in milliseconds since the Unix epoch.
  The events are: the run started, at a trust level; a goal started, with its steps; a step started; a line of a step's output, with its terminal escapes; a step ended, with its state and the reason of a failure; a goal ended, with its conclusion and title; and the run failed, with why.
- Each event is one write of one line, so a reader sees whole lines; a reader leaves out a last line that is not whole yet.
- The record is the one source of what a run did: the check runs are made from it (decision 0033), and the site reads it.
  A step's time is the time between its events, so a cached step has none.
- A run that starts again after a stop appends to the same record; its goals start again, and readers take the latest start.

### The site

- `azad-site` is a binary of its own, in the crate `azad-site`, for hosts that run the orchestrator.
  It reads the orchestrator's state (`AZAD_ORCHESTRATOR_STATE`) and never writes it, so it cannot start or change a run, and the orchestrator does not know it.
- It is written in Leptos: each page is rendered on the server, and the browser gets the same pages as WebAssembly, which hydrates them.
  `cargo leptos build` builds both halves; the browser's half must not depend on Azad's library, so the pages show types of the site's own (`model`), which the server makes from the records.
- Its pages are the runs, newest first; a run, with its goals, a table and a timeline of their steps, and metrics; and a step, with its whole output, with its colors.
  A page of a run that is not done fetches it again every 3 seconds, and a page of a running step follows its new lines through server-sent events, from the byte of the record after the lines that the page has.
- Text from a repository or a run, such as names and output, is always text on a page, never HTML: Leptos escapes it, and terminal escapes become classes of the stylesheet.
- `azad-orchestrator serve --site-url <url>` gives each check run a link to its run's page (GitHub's `details_url`).
- The site has no sign-in; it listens on the host's loopback address by default, and whoever reaches it reads the runs' output.

## Consequences

- `crates/azad/tests/orchestrator.rs` proves that a run's record holds its trust level, its goal, its steps in order, and every line of a step's output, and that a check run links to its run's page.
- The workspace builds the site's server half with the other crates; its browser half needs the `wasm32-unknown-unknown` target and `cargo-leptos`.
- Decision 0029 gets a fifth binary, `azad-site`, for the same hosts as `azad-orchestrator`.

## Amendment, 2026-11-03: a link in the summary

GitHub's page of a check run does not use a `details_url` of plain HTTP on a loopback address, such as the development host's site: its link goes back to the check run.
So the summary of each check run also starts with a link to its run's page, which GitHub shows; the `details_url` stays, for a site with an HTTPS address.
