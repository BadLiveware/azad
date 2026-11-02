# 0027: A package declared in YAML

Status: accepted, 2026-11-02.

## Context

Most people know YAML from CI systems and Kubernetes, and many projects in a monorepo need only data: which commands run, what they read and write, and which shared template they use.
Starlark is the right tool to write templates, but it asks every project to write a small program.
YAML that grows expressions, conditions or templating (as `${{ }}` did in GitHub Actions) becomes a second, weaker language, so this frontend must not grow them.

## Decision

- A package can be declared in `tasks.yaml` in place of `tasks.star`; a directory with both is an error.
- The file has three keys, all optional:
  - `uses`: calls of modules that return parts, as `module: <file>:<name>` and `with:` the arguments by parameter;
  - `tasks`: tasks by name, with the fields of `task()`, and `role` and `ci`, which put a task in a role's goal and in the goal `ci` as `part()` does;
  - `goals`: goals by name, with `require`, `doc` and `on`.
- The only reference is `{config: NAME}`, a value of `config.star`, such as a pinned image.
  There are no expressions, no conditions and no templating.
- Azad checks the file completely, with its unknown keys and wrong types as errors that name the key, and turns it into the `tasks.star` program that says the same.
  So both kinds of file mean exactly the same thing, through one evaluator.
- `module()` takes `returns = "parts"`: such a module gives parts for `project()`, and its call has no name.
  A parameter type `image` takes an image, from `image()`, `config.star` or a reference as text.
- `goal(require = ...)` also takes the names or labels of tasks and actions of the package, such as those that a module made.
- `docs/tasks.schema.json` is the JSON Schema of the file, for editors; a test checks that it names the parser's keys.

## Consequences

- Templates stay in Starlark: a YAML file can only call a module that declares typed parameters.
- What YAML cannot say, such as a loop, a condition, a computed version or a low-level `action()`, stays in `tasks.star`.
- A strict file is less forgiving than most YAML: a number where text is needed, such as `PORT: 8080` in `env`, is an error that asks for quotes.
