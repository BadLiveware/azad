# 0023: Targets across packages, tags, and filters

Status: accepted, 2026-11-01.

## Context

A large monorepo can have over a hundred projects, each a package with its own `tasks.star`.
A developer wants `azad run fmt` at the root to format all of them, each with its own `fmt`; a target named one goal of one package.
A developer also wants to leave out what they never work on, such as the mobile apps, and CI wants to send each kind of work to the machine that can do it, such as macOS for the iOS app.

## Decision

### Patterns

- `//dir/...:name` is the goal or task `name` of every package at or below `dir`, as one goal; `//...:name` is every package of the workspace.
- The packages are the directories with a `tasks.star` that git tracks or would track.
- A package that does not define `name` is left out.
  A package whose `tasks.star` cannot be evaluated fails the run, because it could define `name`, and leaving it out would hide that its work did not run.
  When no package at or below `dir` defines `name`, it is an error.
- A short name on the command line, `azad run fmt` or `azad run <dir>:fmt`, is the pattern of its directory when packages are at or below it, and the label of the one package when that package is the only one.
  When no package is at or below the directory, it is the goal of the package that holds it, as before.
  So in a package without packages below it, nothing changes; in a directory of many packages, the short name runs all of them.
- Azad's own examples had goals named `check`, which `azad run check` at Azad's root would now run too; they are now named `example`.

### Tags

- A task or an action has tags: `task(tags = [...])`, `action(tags = [...])`.
  A tag holds letters, digits, `-`, `_` and `.`.
- `tags` in a `config.star` gives tags to every task and action below it.
  Unlike other names of `config.star`, which a deeper directory replaces, the tags of all the files from the root down to the package are added together.
  So templates add the ecosystem (`dotnet`, `pnpm`), a subtree adds its kind (`app`), and a task adds its own (`emulator`).
- Tags are not part of the digest: they choose what runs, not what a result is.
- `azad list` shows the tags of each task, and of each goal the tags of the tasks that it requires.

### Filters

- `--include a,b` runs only what has one of the tags; `--exclude c` does not run what has one of them, and wins over `--include`.
- A filter chooses among the tasks and actions that the target selects: what a goal requires, or what a pattern collects.
  What they need runs whatever its tags, so a filter never breaks a build.
- When the filter excludes everything that a target selects, it is an error that names what was excluded and why.
- What a filter leaves out is a note (decision 0022): at the normal level, how many, and by which tags, such as `//...:fmt: skipped 2 tasks tagged app (the personal defaults in ~/.config/azad/config.toml)`; each one at the diagnostic level.

### Personal defaults

- `include` and `exclude` in `azad/config.toml` in `$XDG_CONFIG_HOME` or `~/.config`, or in the file that `$AZAD_CONFIG` names, apply when the command line gives no `--include` and no `--exclude`.
- `$AZAD_CONFIG` set to nothing turns them off; the Azad action sets it so, so that CI never reads them.
  The orchestrator does not read them either; they are part of the command, not of the engine.

## Consequences

- `azad run fmt` at such a monorepo's root formats every project that defines `fmt`, and `exclude = ["app"]` in a developer's personal defaults leaves the apps out, with a note that says so.
- CI can send work by tags: `--exclude macos` on Linux, `--include macos` on a Mac, and `--include emulator` where KVM is.
- A broken `tasks.star` anywhere below a directory fails a short name there; `azad run //pkg:goal` still runs one package.
- `crates/azad/tests/patterns.rs` checks patterns, tags from `config.star`, the filter and what a selected task needs; `crates/azad-cli/tests/cli.rs` checks short names, personal defaults and the listing.
