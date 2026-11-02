<!-- Made from the docstrings in crates/azad/src/frontend/starlark/; do not edit. To make it again: AZAD_UPDATE_REFERENCE=1 cargo test -p azad --test reference -->

# Starlark built-ins

A `tasks.star` file defines the actions and goals of its package: the directory that holds it.
Its label is `//<directory>`, such as `//services/orders`, and `//` for the root of the workspace.
`azad run //services/orders:check` runs the goal `check` of that package.
`azad run //services/...:check` runs the goal `check` of every package at or below `services` (decision 0023), and `azad run check` does the same for the current directory; with no package at or below it, it runs the goal of the package that holds it.
`--include` and `--exclude` choose among them by tags.

These functions are available in `tasks.star` files and in modules.
A module is a `.star` file that a `tasks.star` file loads, such as `load("//lib/go.star", "go_module")`, or `load("@std//test.star", "split")` for the standard library.
A module can define functions that declare actions, but it cannot declare actions when it is loaded.

`config` is a struct of the public names of the `config.star` files from the root of the workspace down to the package; a name in a deeper directory replaces the same name above it.
`tags` is the exception: the tags of every `config.star` from the root down to the package are added to each task and action of the package.

Paths:

- The patterns of `glob()` and `files()` are relative to the package directory.
- The keys of `inputs` and `caches` are absolute paths in the sandbox, such as `/src`.
- `outputs` are relative to `/work`, the directory in which the command starts.

Two namespaces give values that are not actions (decision 0021): `semver`, for semantic versions, and `git`, for the history of the workspace's repository.
`@std//version.star` builds versions on them: `commit_calver()` and `latest_release()`.

`@std//project.star` makes a project's goals from parts (decision 0025): `part()` puts tasks in roles, such as `fmt` or `test`, and `project()` makes a goal for each role, and the goal `ci`.

A package can also be declared as data in `tasks.yaml`, which calls modules that return parts (decision 0027); `docs/yaml.md` describes it.

## action

```python
def action(
    *,
    name: str,
    cmd: list[str],
    root,
    inputs: dict[str, typing.Any] = ...,
    outputs: list[str] = [],
    env: dict[str, str] = ...,
    caches: dict[str, str] = ...,
    tags: list[str] = [],
) -> action
```

A hermetic action: a command in a sandbox, with no network, that runs again only when its inputs change.

#### Parameters

* `name`: (required)

  The name of the action in its package, which gives the label `//<package>:<name>`.

  It can hold letters, digits, `_`, `-` and `.`.
* `cmd`: (required)

  The program and its arguments.

  No shell starts it; use `["sh", "-c", "..."]` for a shell command.
* `root`: (required)

  The root file system, usually an `image()`.

* `inputs`: (optional)

  The sources to mount, read-only, by absolute path in the sandbox, such as `{"/src": glob(["**"])}`.

  A source is a `glob()`, an `image()`, or an output of another action (`<action>.out("bin")`).
  An action that reads the output of another action runs after it.
* `outputs`: (defaults to: `[]`)

  The directories that the command writes and that are kept, relative to `/work`, such as `["bin"]`.

  Other actions read them with `<action>.out("bin")`.
* `env`: (optional)

  The environment variables.

  The environment of the image is the base; a variable here replaces the variable of the image of that name.
* `caches`: (optional)

  The cache volumes, by absolute path in the sandbox, such as `{"/cache/go": "go-build"}`.

  A cache volume is a writable directory that a tool keeps between runs, such as the build cache of a compiler.
  Its content is not part of the digest, so it must not change the result (decision 0011).
* `tags`: (defaults to: `[]`)

  Names that `azad run --include` and `--exclude` select by, such as `dotnet` (decision 0023). The `tags` of the `config.star` files above the package are added. Tags are not part of the digest.

#### Details

The sandbox holds the root file system, the inputs (read-only) and the cache volumes.
The command starts in `/work`, an empty directory that it can write.
The directories of `outputs` are kept; the other files that the command writes are discarded.
The result is cached by the digest of the action: its command, environment, root and inputs.

#### Examples

```python
go = image("golang:1.25@sha256:<hex>")
build = action(
    name = "build",
    cmd = ["sh", "-c", "cd /src && go build -o /work/bin/ ./..."],
    root = go,
    inputs = {"/src": glob(["**/*.go", "go.mod", "go.sum"])},
    outputs = ["bin"],
    caches = {"/root/.cache/go-build": "go-build"},
)
```

---

## fetch

```python
def fetch(
    *,
    name: str,
    cmd: list[str],
    root,
    inputs: dict[str, typing.Any],
    outputs: list[str] = [],
    env: dict[str, str] = ...,
    caches: dict[str, str] = ...,
    tags: list[str] = [],
) -> action
```

A fetch: an action that can use the network to download dependencies, cached by the digest of its inputs.

#### Parameters

* `name`: (required)

  As for `action()`.

* `cmd`: (required)

  As for `action()`.

* `root`: (required)

  As for `action()`.

* `inputs`: (required)

  As for `action()`, and it must hold the lock files, such as `go.sum`.

* `outputs`: (defaults to: `[]`)

  As for `action()`.

* `env`: (optional)

  As for `action()`.

* `caches`: (optional)

  As for `action()`.

* `tags`: (defaults to: `[]`)

  As for `action()`.

#### Details

The inputs must hold the lock files that pin what it downloads (decision 0007).
So the same inputs give the same files.

---

## file\_set

```python
def file_set(
    *,
    name: str,
    sources: list[str],
    exclude: list[str] = [],
    deps: list[str] = [],
) -> None
```

A named set of files of the package (decision 0028), which other packages can depend on in place of all of the package's files, such as `"//libs/core:contracts"`.

#### Parameters

* `name`: (required)

  The name of the set in its package.

* `sources`: (required)

  Patterns of its files, relative to the package, of the files that git tracks or would track.

* `exclude`: (defaults to: `[]`)

  Patterns of files to leave out.

* `deps`: (defaults to: `[]`)

  The dependencies that come with the set, as `package(deps = ...)` takes them.

#### Examples

```python
file_set(name = "contracts", sources = ["Contracts/**"], deps = [":analyzers"])
```

---

## files

```python
def files(include: list[str], *, exclude: list[str] = []) -> list[str]
```

The paths of the files that `glob()` with the same patterns selects, relative to the package, sorted.

#### Parameters

* `include`: (required)

  As for `glob()`.

* `exclude`: (defaults to: `[]`)

  As for `glob()`.

#### Details

A module can make one action per file with it.

---

## git.commit

```python
def git.commit(rev: str = ...) -> commit
```

A commit of the workspace's repository, with the facts that its ID fixes: its date, its parents and its count.

#### Parameters

* `rev`: (optional)

  The revision; `HEAD` when it is not given.

#### Details

`rev` can be anything that `git rev-parse` takes, such as `HEAD`, a branch, a tag or a commit ID.
A ref can move, so the package records the commit that each ref named when it was evaluated; a commit ID is
fixed and needs no record.
`HEAD` is the commit that the working tree is on, not the files of the working tree; in a run of the
orchestrator, it is the commit of the event.

#### Examples

```python
head = git.commit()
version = "%s.%d" % (head.date[:7].replace("-", "."), head.count())
```

---

## git.tags

```python
def git.tags(pattern: str = "*") -> dict[str, str]
```

The tags of the workspace's repository whose names match `pattern`, with the commit each one names, by name.

#### Parameters

* `pattern`: (defaults to: `"*"`)

  A pattern of tag names, where `*` matches any text, such as `v*`; all tags when it is not given.

#### Details

A tag can move, so the package records the commit that each tag named when it was evaluated.

#### Examples

```python
releases = [semver.parse(name[1:]) for name in git.tags("v*")]
```

---

## glob

```python
def glob(include: list[str], *, exclude: list[str] = []) -> source
```

The files of the package that match a pattern of `include` and no pattern of `exclude`, as a source.

#### Parameters

* `include`: (required)

  The patterns of the files to select, such as `["**/*.go", "go.mod"]`.

* `exclude`: (defaults to: `[]`)

  The patterns of the files to leave out, such as `["**/*_test.go"]`.

#### Details

The patterns are relative to the package directory, the directory of the `tasks.star`.
`*` does not match `/`; `**` does.
The files keep their paths relative to the package: at `/src`, `glob(["cmd/*.go"])` gives `/src/cmd/main.go`.
Only files and links are selected; a directory is there when it holds a selected file.
A glob that matches no file gives a warning when the goal loads.

#### Examples

```python
src = glob(["**/*.go", "go.mod", "go.sum"], exclude = ["**/*_test.go"])
```

---

## goal

```python
def goal(
    name: str,
    *,
    require: list,
    doc: str = "",
    on: list[str] = [],
) -> None
```

A goal of the package: it holds when each action in `require` succeeds.

#### Parameters

* `name`: (required)

  The name of the goal, such as `check`.

* `require`: (required)

  The actions and tasks that must succeed: their values, or the names or labels of tasks and actions of this package that are defined before the goal, such as those that a module made.

  An action runs after the actions whose outputs it reads.
* `doc`: (defaults to: `""`)

  What the goal is for, for `azad list`.

* `on`: (defaults to: `[]`)

  The events on which the orchestrator (`azad serve`) runs the goal: `push`, `pull_request`, or both.

  A push to the default branch runs at the trust level `main`; a pull request, and a push to another branch,
  at `pull-request`.

#### Details

`azad run //<package>:<name>` runs what the goal needs.
In the package directory, `azad run <name>` does the same.
A task or action can also run by its own name, as if a goal required only it.

#### Examples

```python
goal("check", require = [build, test], on = ["push", "pull_request"])
```

---

## image

```python
def image(
    reference: None | str = None,
    *,
    env: dict[str, str] = ...,
    dockerfile: None | str = None,
    context: None | str = None,
    target: str = "",
    args: dict[str, str] = ...,
    contexts: dict[str, str] = ...,
) -> source
```

An OCI (Open Container Initiative) image, as a source: by a reference with its digest, or built from a Dockerfile of the workspace.

#### Parameters

* `reference`: (defaults to: `None`)

  The reference, such as `python:3.13-slim@sha256:<hex>` or `ghcr.io/owner/tool@sha256:<hex>`.

* `env`: (optional)

  Variables that using the image needs, such as a writable `CARGO_HOME`. A task gets them only when it runs in the image, not on the host.

* `dockerfile`: (defaults to: `None`)

  Instead of `reference`, a Dockerfile of the workspace, by its path from the root, such as `//tools/dotnet.Dockerfile`.

* `context`: (defaults to: `None`)

  The build context of the Dockerfile, from the root; its directory when it is not given.

* `target`: (defaults to: `""`)

  The stage of the Dockerfile to build; the last one when it is not given.

* `args`: (optional)

  The build arguments.

* `contexts`: (optional)

  BuildKit's named build contexts, which `COPY --from=<name>` reads, by name, as paths from the root, such as `{"src-logging": "//libs/logging"}`; what `contexts` of `docker buildx bake` gives.

#### Details

A tag can move to another image, so a reference must hold the digest; a tag in it is only for people.
`azad pull <name>:<tag>` prints the reference of what the tag points to now.
Azad pulls the image for the platform of the host.
The `Env` of the image is the base environment of an action that has the image as its root (decision 0016);
`env` goes on top of it, and the action's `env` on top of both.

An image from a Dockerfile is built by the host's container runtime, Docker or Podman, with every feature that
it has (decision 0024). A task that runs in its image locally runs in the runtime; an isolated run stores the
image's file system, once for each content, and runs the task in Azad's sandbox. A run of a pull request does not
build one, because a build runs the Dockerfile's steps with the host's runtime.

Choose a `target` that holds the toolchain alone, such as a `base` stage with the language and its package
manager. A stage that also installs the project's dependencies or builds it does that work in every new build,
stores its result in the image, and gives tasks that do the same work again in their own sandbox.

#### Examples

```python
python = image("python:3.13-slim@sha256:2d44738ebf4ce5f63b9d9ea871e9c7cfdc0426f7b0c97cac915fd88ded1d86bb")
tools = image(dockerfile = "//tools/azad/dotnet.Dockerfile", target = "format")
```

---

## module

```python
def module(
    *,
    name: str,
    params: dict[str, str],
    outputs: list[str] = [],
    returns: str = "action",
    implementation,
) -> module
```

A module: a function with typed parameters, which frontends other than Starlark can call too (decision 0014).

#### Parameters

* `name`: (required)

  The name of the module, such as `std/test.split`.

* `params`: (required)

  The type of each parameter, by name.

  A type is `string`, `strings`, `int`, `bool`, `patterns` (patterns of files of the package) or `image` (from
  `image()`, or a reference as text).
  A type with `?` at the end, such as `strings?`, is optional.
* `outputs`: (defaults to: `[]`)

  The outputs that the action `name` of each call declares.

* `returns`: (defaults to: `"action"`)

  What a call gives back: `"action"`, the action `name`, or `"parts"`.

* `implementation`: (required)

  The function that defines the actions of a call.

#### Details

A call of the module calls `implementation` with the arguments of the call.
A module that returns an action, the default, also gets the call's `name`, and the call must define an action
with that name, which declares the outputs of the module.
A module that returns parts gives `part()`s of `@std//project.star` for `project()`, and its call has no name;
a `tasks.yaml` file calls it with `uses:` (decision 0027).
A GHA workflow calls a module with `uses:` and `with:`, so each parameter type can also be given as text.

#### Examples

```python
def _lint(name, files, ruff):
    src = {"/src": glob(files)}
    return action(name = name, cmd = ["ruff", "check", "/src"], root = image(ruff), inputs = src)

lint = module(name = "lint", params = {"files": "patterns", "ruff": "string"}, implementation = _lint)
```

---

## package

```python
def package(
    *,
    deps: list[str] = [],
    exports: list[str] = [],
    provides: list[str] = [],
) -> None
```

What the package says of itself for the packages that depend on it (decision 0028); a file calls it at most once.

#### Parameters

* `deps`: (defaults to: `[]`)

  The package's immediate dependencies: packages, such as `"//libs/core"`, or the sets of files and tasks of packages, such as `"//libs/core:contracts"`. A task with `dependencies = True` reads them, and what they depend on, so a package names only its own dependencies.

* `exports`: (defaults to: `[]`)

  Patterns of the files that the package gives the tasks that depend on it, relative to it, such as `["**/*.cs", "**/*.csproj"]`; by default, all that git tracks or would track. Fewer files run fewer dependents again.

* `provides`: (defaults to: `[]`)

  Tasks of the package, by name, whose outputs the tasks that depend on it get, such as the build of a library; they run first.

#### Examples

```python
package(deps = ["//libs/logging"], provides = ["build"])
```

---

## semver.matches

```python
def semver.matches(requirement: str, version) -> bool
```

Whether `version` meets `requirement`, with Cargo's rules: `^1.4` holds `1.4.0` up to, and not with, `2.0.0`.

#### Parameters

* `requirement`: (required)

  Such as `^1.4`, `~1.4.2`, `>=1.2, <1.5` or `=1.4.2`.

* `version`: (required)

  A version from `semver.parse()`, or its text.

#### Details

A pre-release meets a requirement only when the requirement names a pre-release of the same version.

---

## semver.parse

```python
def semver.parse(text: str) -> semver
```

A semantic version (semver.org), parsed with Cargo's rules.

#### Parameters

* `text`: (required)

  The version, such as `1.4.2`, `1.4.2-rc.1` or `1.4.2+build.7`; no `v` before it.

#### Details

Versions compare by precedence, so `max()` and `sorted()` work on them: `1.4.0-rc.1 < 1.4.0 < 1.10.0`.
Two versions that differ only in build metadata are not equal, and compare by that metadata.
`str()` gives the version as text.

#### Examples

```python
version = semver.parse("1.4.2")
next = version.bump("minor")    # 1.5.0
```

---

## semver.valid

```python
def semver.valid(text: str) -> bool
```

Whether `text` is a semantic version that `semver.parse()` takes.

#### Parameters

* `text`: (required)

  The text to check, such as the part of a tag after its `v`.

---

## shell

```python
def shell(
    *,
    name: str,
    cmd: list[str],
    root,
    inputs: dict[str, typing.Any] = ...,
    outputs: list[str] = [],
    env: dict[str, str] = ...,
    caches: dict[str, str] = ...,
    tags: list[str] = [],
) -> action
```

An impure action: an action that can use the network, and that runs each time a goal needs it.

#### Parameters

* `name`: (required)

  As for `action()`.

* `cmd`: (required)

  As for `action()`.

* `root`: (required)

  As for `action()`.

* `inputs`: (optional)

  As for `action()`.

* `outputs`: (defaults to: `[]`)

  As for `action()`.

* `env`: (optional)

  As for `action()`.

* `caches`: (optional)

  As for `action()`.

* `tags`: (defaults to: `[]`)

  As for `action()`.

#### Details

Use it for work whose result does not depend only on its inputs, such as a deploy.
Its outputs are stored by digest, so an action that reads them is a cache hit when they did not change.

---

## task

```python
def task(
    *,
    name: str,
    cmd,
    sources: list[str] = [],
    exclude: list[str] = [],
    generates: list[str] = [],
    deps: list = [],
    image = ...,
    env: dict[str, str] = ...,
    caches: dict[str, str] = ...,
    network: bool = False,
    tags: list[str] = [],
    local: None | str = None,
    secrets: list[str] = [],
    doc: str = "",
    dependencies = ...,
) -> action
```

A task (decision 0015): work described by the files of the workspace, which runs on the host locally and in its image in CI.

#### Parameters

* `name`: (required)

  The name of the task in its package.

* `cmd`: (required)

  One line that the shell runs, or a list of words: the program and its arguments.

* `sources`: (defaults to: `[]`)

  Patterns of the files that the task reads, relative to the package, as for `glob()`; or, after `//`, relative to the workspace's root, such as `//libs/core/**` for a project that this one builds from source (decision 0026). An isolated run has them at the same paths, and they are part of the digest.

* `exclude`: (defaults to: `[]`)

  Patterns of files to leave out of `sources`, in the same two forms.

* `generates`: (defaults to: `[]`)

  Directories that the task writes, relative to the package, for the tasks after it.

* `deps`: (defaults to: `[]`)

  Tasks that must succeed first; the task reads what they generate, at their paths. A task of this file is the value of its `task()`; a task of another package is its label, such as `"//libs/logging:build"`, whose package Azad reads too (decision 0026).

* `image`: (optional)

  The image of an isolated run, from `image()`.

* `env`: (optional)

  Variables of the command; on the host, they replace the host's variables of the same name.

* `caches`: (optional)

  State that the task's tools keep between runs, by path, and the name of each. A path relative to the package, such as `target`, is that directory in both forms: on the host, the real one, and in the image, a cache volume at the same place. An absolute path is a cache volume at that path of the sandbox, such as a tool's download directory; on the host, the tool uses its own. A new machine, such as a CI runner, starts each from its newest copy in the remote cache (decision 0017).

* `network`: (defaults to: `False`)

  Whether an isolated run has network access, as `fetch()` has.

* `tags`: (defaults to: `[]`)

  Names that `azad run --include` and `--exclude` select by, such as `dotnet` or `emulator` (decision 0023). The `tags` of the `config.star` files above the package are added. Tags are not part of the digest.

* `local`: (defaults to: `None`)

  Where a local run runs the task: `"host"`, on this host's tools, or `"image"`, in its image through Docker or Podman, on the files of the workspace (decision 0024). The default is `local` of the nearest `config.star`, or `"host"`. `azad run --local` and the personal defaults can choose for every task.

* `secrets`: (defaults to: `[]`)

  Variables of `azad`'s environment that the command gets in its image, by name, such as a registry's token. Their values are not part of the digest. On the host, the command has the whole environment.

* `doc`: (defaults to: `""`)

  What the task is for.

* `dependencies`: (optional)

  The dependencies that the task reads (decision 0028): `True` for those of its `package()`, a list of them, such as `["//libs/core:contracts"]`, or `False`, the default. It gets each one's files and provided outputs at their workspace paths, and those of their dependencies, which run first; all of them are part of its digest.

#### Details

A local run runs the command on this host, with its tools and environment, in the package directory, and the
task writes its files there.
`--isolated`, which CI uses, runs it in `image`, in a sandbox whose work directory holds only the task's
`sources` and what its `deps` generate, at the same paths as in the workspace.
Both forms skip the task when its command, `env`, sources and the output of its deps did not change since it
last succeeded; a local run also runs it again when a directory that it generates was changed or removed.
A task without an image runs on the host in both, as CI runs it on its runner; use it for work that needs the
host, such as tests that start sandboxes.

#### Examples

```python
build = task(name = "build", cmd = "go build -o out/app .", sources = ["**/*.go", "go.*"], generates = ["out"], image = go)
test = task(name = "test", cmd = "out/app --self-test", deps = [build], image = go)
goal("check", require = [test])
```

---

## action.label

```python
action.label: str
```

The label of the action, such as `//services/orders:build`.

---

## action.out

```python
def action.out(path: str) -> source
```

The output directory `path` of this action, as a source for another action.

#### Parameters

* `path`: (required)

  One of the outputs of the action, such as `bin`.

#### Details

An action that has it as an input runs after this action.

---

## semver.build

```python
semver.build: str
```

The build metadata, such as `abc123` of `1.4.2+abc123`; empty when there is none.

---

## semver.bump

```python
def semver.bump(part: str) -> semver
```

The next release that raises `part`, without pre-release or build metadata.

#### Parameters

* `part`: (required)

  `"major"`, `"minor"` or `"patch"`.

#### Details

A pre-release of the release that the bump gives is released as it is, because it comes before it:
`1.3.0-rc.1` bumped at `minor` is `1.3.0`, and at `patch` too; at `major` it is `2.0.0`.

---

## semver.major

```python
semver.major: int
```

The major version: `1` of `1.4.2`.

---

## semver.minor

```python
semver.minor: int
```

The minor version: `4` of `1.4.2`.

---

## semver.patch

```python
semver.patch: int
```

The patch version: `2` of `1.4.2`.

---

## semver.pre

```python
semver.pre: str
```

The pre-release, such as `rc.1` of `1.4.2-rc.1`; empty for a release.

---

## commit.count

```python
def commit.count() -> int
```

How many commits this commit contains: itself and all of its ancestors, through every parent of a merge.

The count of a commit is larger than the count of each of its parents, so on a branch that only moves forward,
such as `main`, it follows the order of the commits.
It is `git rev-list --count <id>`. A shallow clone does not have the history: the count comes from another
source of history when one is given (`azad --github-history`), and is an error when none is.

---

## commit.date

```python
commit.date: str
```

The committer date, in UTC, as `YYYY-MM-DD`.

---

## commit.first\_parents

```python
def commit.first_parents(limit: int) -> list[commit]
```

The commit and its first parents, the commit first, at most `limit` commits.

#### Parameters

* `limit`: (required)

  The most commits to give, from 1.

#### Details

The list is shorter when the history has fewer. When a shallow clone ends before `limit`, the rest comes from
another source of history when one is given, and it is an error when none is.

---

## commit.id

```python
commit.id: str
```

The ID of the commit, as 40 hexadecimal digits.

---

## commit.parents

```python
commit.parents: list[str]
```

The IDs of the parents, the first parent first; empty for a root commit.

---

## commit.time

```python
commit.time: int
```

The committer date, in seconds since 1970-01-01 UTC.
