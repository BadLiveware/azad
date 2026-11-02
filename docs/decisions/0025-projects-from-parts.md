# 0025: A project's goals from parts

Status: accepted, 2026-11-02.

## Context

The first setup of a large monorepo gave each project its tasks through one template for each ecosystem, such as `dotnet_project(solution, image, fmt = None, build = None, test = None, test_filter = ..., check_format = False, prefix = "")`.
A template owns all of a project's tasks, and a project changes them through the parameters that the template has, as a subclass changes a base class through its hooks:

- Each new variation needs a new parameter, so the templates grew `fmt_check`, `check_format`, `test_filter`, `ci_tasks` and `overrides`.
- Tasks inside a template depend on each other where a project cannot see it: `fmt-check` came from `fmt`, so a project that replaced `fmt` lost its formatting check, and `check_format = True` then did nothing.
- Two templates do not combine: a directory with a .NET solution and a `package.json` needed `join()`, and a `prefix` for each template so that their task names did not collide.

`config.star` is different: it gives values, such as images, tags and `local`, to the directories below it.
That is scope for data, as `.editorconfig` is, and it does not have these problems.

## Decision

- `@std//project.star` gives `part()` and `project()`.
  It knows no ecosystem, and uses only the built-ins that every `.star` file gets (decision 0021).
- `part(ci = [...], **roles)` puts tasks in roles, such as `fmt`, `fmt-check`, `lint`, `typecheck`, `test` and `build`, and names the roles that CI checks.
  A role takes a task, a list of tasks, or None, which leaves the role out.
- `project(*parts, on = ["push", "pull_request"])` makes a goal for each role, which requires the tasks of every part in that role, and the goal `ci`, which requires the tasks of the roles that each part names, on the events `on`.
  An argument is a part or a list of parts, so a module can give a preset.
- A project chooses its parts.
  To change one task, it takes another part, or writes its own; to have two kinds of project in one directory, it takes the parts of both.
  Which roles CI checks is the choice of each part, so in one directory .NET's formatting can stay out of CI while Node's is in.
- The ecosystems' parts stay in the repository's modules, such as `dotnet(sdk)`, which gives the parts of a .NET project with that SDK.
  They take what they use as arguments, as decision 0021 and the vision say.
- `config.star` keeps giving data to the directories below it.

## Consequences

- A goal and a task of a package cannot have the same name, so the tasks of parts are named after their tool, such as `dotnet-fmt`, and `fmt` is a goal.
  `azad run fmt` runs the goal in every package, as it ran the task.
- A project that lists its parts does not get a part that a preset adds later; a project that takes the preset does.
  That is the cost of an explicit choice.
- The tasks of a setup that used the templates get new names, so each runs once more after the change.

## Amendment, 2026-11-02: what the review found

- The rule that a goal and a task of a package cannot have the same name was only written down: a goal hid a task or an action of its name without an error.
  `goal()`, `task()` and `action()` now fail when a name is both, in either order.
- An empty list in a role leaves the role out, as None does, so that a module can compute a role's tasks.
  Before, it made `project()` fail with an error about `goal()`.
