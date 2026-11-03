# 0032: The orchestrator on GitHub, through a GitHub App

Status: accepted, 2026-11-03.

## Context

Decision 0020 made the orchestrator work locally: simulated events, and check runs to a fake GitHub.
It said that a client of GitHub's API would take the same calls, and that a tag runs at the trust level `none` until a proof shows that its commit is on the default branch.
Item 1 of `docs/plan.md` runs the orchestrator against GitHub itself, on a private test repository.

## Decision

### A GitHub App

- `azad-orchestrator serve --github-app-id <id> --github-app-key <file>` writes check runs on GitHub through a GitHub App, through GitHub's REST API (`--github-api`, `https://api.github.com` by default).
  Without them, check runs go to the fake GitHub, as before.
- The App signs in with a JSON Web Token (JWT): RS256, signed with the App's private key, which lives nine minutes and starts one minute in the past, for clocks that differ.
- For each repository, the orchestrator finds the App's installation (`GET /repos/{repository}/installation`), and gets the installation's token (`POST /app/installations/{id}/access_tokens`).
  It keeps the token in memory until five minutes before it ends.
- Check runs are created queued, then updated to in progress, then completed with a conclusion and a summary (`POST` and `PATCH /repos/{repository}/check-runs`).
  The `CheckRuns` trait now takes the repository with each update, because GitHub's API needs it.
- The App needs these permissions: checks (read and write), contents (read), metadata (read) and pull requests (read); and the events push and pull request.

### Fetches

- The orchestrator fetches a repository of an installation with its token, as git's HTTP header `Authorization: Basic` with the user `x-access-token`.
  git gets the header in its environment (`GIT_CONFIG_COUNT`), not in its arguments, so other processes cannot see it, and git never asks for a password.
- Each fetch also fetches the default branch, to `refs/azad/default` in the mirror.

### Tags

- A push of a tag runs at the trust level `main` when the default branch holds the tag's commit, because `main` already ran that code; else it runs at `none`, as before.
  The proof is `git merge-base --is-ancestor` against the default branch that the same fetch got.
- `azad-orchestrator simulate tag <tag>` sends a push of a tag.

### Deliveries

GitHub must reach `/webhook`.
The development host (`dev/host/`) has no public address, so a relay, such as smee.io, takes GitHub's deliveries and a client in the host sends them on.
A relay's channel can be read by anyone with its URL, so it is only for a test repository; the webhook secret's signature is what makes a delivery trusted.

## Consequences

- `crates/azad/tests/orchestrator.rs` proves the calls against a fake of GitHub's API: the JWT is signed with the App's key and valid now, the installation's token is got once, and the check run is created and completed with it.
  It also proves that a tag of a commit outside the default branch writes nothing that `main` reads, and that a tag of the default branch's commit fills `main`'s cache.
  The test key in `crates/azad/tests/data/github-app-test-key.pem` was made for the test and is not a secret.
- A token lives in the orchestrator's memory only; a restart gets a new one.
- The orchestrator does not read GitHub's history API for evaluation, because its mirror has the whole history.
