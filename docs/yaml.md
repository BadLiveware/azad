# tasks.yaml

A `tasks.yaml` file declares a package as data, in place of a `tasks.star` file (decision 0027).
It means exactly what the same `tasks.star` means: Azad checks the file and turns it into that program.
Editors can check the file with the JSON Schema `docs/tasks.schema.json`, for example with a first line `# yaml-language-server: $schema=<url of the schema>`.

## Example

```yaml
# Parts from modules: the standard tasks of a kind of project.
uses:
  - module: //tools/azad/dotnet.star:standard
    with:
      sdk: {config: DOTNET_SDK}
      solution: billing.slnx

# The package's own tasks.
tasks:
  e2e:
    cmd: pnpm playwright test
    image: {config: NODE}
    sources: ["**"]
    exclude: ["node_modules/**"]
    network: true
    role: test
    ci: true

# Goals beyond the roles' goals.
goals:
  release:
    require: [dotnet-build, e2e]
    doc: What a release needs
```

## Keys

| Key | What it holds |
|---|---|
| `package` | `deps`, `exports` and `provides`, as `package()` takes them (decision 0028). |
| `file_sets` | Named sets of the package's files by name, with `sources`, and optionally `exclude` and `deps`, as `file_set()` takes them. |
| `uses` | A list of calls. `module` is `<file>:<name>` of a `module()` with `returns = "parts"`; `with` gives its arguments by parameter. |
| `tasks` | Tasks by name. Each has the fields of `task()` (see `docs/starlark.md`), and `role` and `ci`. |
| `goals` | Goals by name, with `require`, and optionally `doc` and `on`. |

The fields of a task:

| Field | Value |
|---|---|
| `cmd` | Text, which the shell runs, or a list of words. |
| `sources`, `exclude`, `generates`, `tags`, `secrets` | Lists of text. A pattern after `//` is of the workspace (decision 0026). |
| `deps` | Names of tasks of this package, or labels of tasks of other packages, such as `//libs/logging:build`. |
| `image` | A reference with its digest, or `{config: NAME}`. |
| `env`, `caches` | Mappings of text to text. Put numbers in quotes. |
| `network`, `ci` | `true` or `false`. |
| `local` | `host` or `image`. |
| `doc` | Text. |
| `dependencies` | `true` for the dependencies of the package, or a list of them, such as `["//libs/core:contracts"]` (decision 0028). |
| `role` | The role whose goal holds the task, such as `test` or `fmt-check`, as `part()` gives it. `ci: true` puts the task in the goal `ci` too. |

`goals.<name>.require` names tasks of this package, also those that a module made, or their labels.

## What stays in Starlark

YAML has values and one reference, `{config: NAME}`.
A loop, a condition, a computed value such as a version from `git`, a low-level `action()`, and a module itself are written in `tasks.star` and in modules.
A wrong key or type is an error that names the key, before anything runs.
