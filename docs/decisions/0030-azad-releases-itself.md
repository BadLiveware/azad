# 0030: Azad releases itself

Status: accepted, 2026-11-03.

## Context

Azad's CI built `azad` from source in each run, and ran the checks with that build.
Releases came from a tag that someone pushed, and the version was `0.0.0`.
So Azad did not live with its own releases, and it did not version itself with its own tools, although `@std//version.star` exists for that.

GitHub Actions stays the machine: its runners are free, and the Azad action runs goals in them.

## Decision

- **Stage 0.** CI runs the checks with Azad's latest release, as a compiler builds itself with the release before it.
  The repository variable `AZAD_STAGE0` names another release, when the latest one cannot run the checks.
  Until the first release exists, stage 0 is built from the commit.
- **Versions.** A version is `<major>.<yyyymmdd>.<n>`, which is valid semantic versioning:
  - `major` counts breaking changes;
  - `yyyymmdd` is the UTC date of the commit, in place of minor and patch, which a small project does not tell apart well;
  - `n` counts the releases of that major on that day, from 0.
- **Conventional Commits decide.** A commit since the latest release makes a release when its type is `feat`, `fix` or `perf`, or when it is breaking (`!` in its header, or a `BREAKING CHANGE:` line); a breaking one raises `major`.
  `@std//version.star` has the rule, as `next_release()`, built on a new built-in, `commit.message`.
  Azad's commits use Conventional Commits from now on.
- **`//release:version`** writes the version that the commit makes, or nothing, for the workflow.
  It is a package of its own, so the goals that CI runs by label never walk the history.
- **Every green push to main that makes a release is released.**
  The binaries say the version (`$AZAD_VERSION` when they are built); a push that makes no release says `0.0.0-dev+<commit>`.
  The GitHub release has one archive for each binary (decision 0029); the images get the version, the major and `latest`.
- **A release runs Azad's check first.** The new `azad` runs `//:check` with `--force` before anything is published, because stage 0 has just checked the same commit and cached its results.

## Consequences

- A change to `tasks.star` that needs a feature of Azad waits for a release with that feature.
- A change to the release rule in `@std//version.star` takes effect one release later, because stage 0 computes the version.
- A release that cannot run the checks breaks CI until `AZAD_STAGE0` names an older release, or a fix is released.
- The release job needs the whole history and the tags; a shallow clone has no tags, and would compute the first release again.
