# 0028: A dependency graph that each package declares, as coarse or as fine as it needs

Status: accepted, 2026-11-02.

## Context

Decision 0026 lets a task name other directories (`//libs/core/**`) and tasks of other packages.
Every project then had to list its whole dependency graph: a .NET project that references `libs/core` also listed what `libs/core` references, and every change to that graph changed every project below it.
Azad knows no build system, so it cannot read references from `.csproj`, `package.json` or `pyproject.toml`.

A team also wants to choose how much to declare: a coarse graph where builds are cheap, and fine edges where a build is slow or expensive.

## Decision

- `package(deps, exports, provides)` says, once in a `tasks.star` or under `package:` in a `tasks.yaml`, what the package gives the packages that depend on it:
  - `deps`: its immediate dependencies only: packages, such as `//libs/core`, or sets of files and tasks of packages, such as `//libs/core:contracts`;
  - `exports`: patterns of the files that it gives dependents; by default all the files that git tracks or would track, so `.gitignore` keeps build output out without any knowledge of the build;
  - `provides`: its tasks whose outputs dependents get, such as the build of a library.
- `file_set(name, sources, exclude, deps)` names a set of a package's files, with the dependencies that come with it, as a finer target than the whole package.
- `task(dependencies = True)` reads the dependencies of the task's package; `dependencies = [...]` reads only those named.
  The engine walks the graph: each package's or set's files, at their workspace paths, the outputs of provided and named tasks, which run first, and the same for their dependencies, transitively.
  All of them are part of the task's digest.
  A cycle is an error that names it.
- Granularity is the declaring package's choice, and a dependent does not change when its dependency gets finer:
  1. a package edge, with all the package's files;
  2. `exports`, so that files that dependents do not need do not run them again;
  3. `file_set()`, so that a dependent takes a part of a package;
  4. a dependency on a task's outputs, such as a packed library: results are keyed by content, so a change that does not change the output keeps dependents cached (early cutoff);
  5. a task that names only the dependencies that it reads.
- Modules decide which tasks take `dependencies`: a build or a test does, a formatter does not.

## Consequences

- A package names only its own dependencies, and its dependents get them through it.
- A missing edge makes an isolated run fail where the files are missing, not a stale cache hit.
- Azad could later check the declarations by tracing what a task opens in its sandbox, without knowing any language; the declarations stay the source of the cache key, because it must be known before the task runs.
- The dependency files of a package are listed with `git ls-files`, so a workspace must be a git repository, as it already must for listing packages.
