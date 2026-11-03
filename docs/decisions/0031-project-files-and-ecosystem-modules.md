# 0031: Built-ins that read project files, and ecosystem modules in the standard library

Status: accepted, 2026-11-03.

## Context

Decision 0028 lets each package declare its immediate dependencies with `package(deps = ...)`.
The project files already say most of them: a `.csproj` has its `ProjectReference` items, a `pyproject.toml` has its `[tool.uv.sources]`, and a `package.json` has its `file:` and `workspace:` versions.
Each repository wrote the same list again by hand, and a list by hand drifts from the files that the tools read.

Decision 0026 said that a module may read these files later, if Azad gets a generic built-in to read a file.
Decision 0021 says that a built-in holds no knowledge of an ecosystem, and that the standard library uses only the built-ins that every `.star` file gets.

Reading a project file correctly is cumbersome: MSBuild imports `Directory.Build.props` files and sets properties under conditions, uv has workspaces whose members are globs, and pnpm, npm and Yarn each declare a workspace in their own way.
Each repository should not write this again.

## Decision

### Generic built-ins

- `read(path)` gives the text of a file of the workspace.
  The path is relative to the package, or to the root after `//`, as `sources` patterns are (decision 0026).
  An absolute path, a path above the root, and a link to a file outside the workspace are errors; a missing file is an error that names it; the content must be UTF-8, and a byte order mark at its start is dropped.
- The package records each file that evaluation read, by its `//` path, with the digest of its content (`Package::read_files`), and a diagnostic note says it, as decision 0021 records git refs.
  A file can change, so it is an input of the evaluation, for a cache of evaluations that Azad does not have yet.
- `toml.decode(text)`, `yaml.decode(text)` and `xml.decode(text)` give values, as Starlark's `json.decode` does: dicts, lists, strings, ints, floats, bools and None.
  - TOML (Tom's Obvious Minimal Language): a date or a time is its text.
  - YAML: one document, or None for none; anchors and aliases are resolved; a key that is a number or a bool is its text, as in JSON.
  - XML: an element is `{"tag", "attrs", "children", "text"}`: the local names of the element and of its attributes, the child elements in their order, and the text directly in the element, with the white space at its ends removed.
    Namespaces are dropped, because MSBuild files of the old style and SDK-style files differ only by the default namespace.
    Two attributes with one local name are an error, and so is a document type declaration.
  - roxmltree reads XML: it is small, it has no dependencies, and its license is MIT or Apache-2.0.
- `files()` takes patterns that start with `//`, and gives `//` paths of the workspace; one call takes patterns of one kind.
- `workspace_path(path)` gives the `//` path of a path of the package, and `package_of(path)` the label of the package that holds a path (the nearest directory at or above it with a `tasks.star` or a `tasks.yaml`), or None.
- A selection by patterns now reads only the directories that can hold a match: the directories on the way to the part of the pattern before its first wildcard, and below it only as deep as the pattern reaches, at any depth after `**`.
  So `//Directory.Build.props` reads the root and not the workspace below it, in `files()` and in the sources of tasks.
  The files that a selection keeps do not change, so no digest changes.

### Ecosystem modules

The standard library has modules that a package uses only when it loads them; nothing is automatic, and nothing is like Bazel's rules for each language.
They are written in Starlark, only with public built-ins, so that a user could write them.

- `@std//dotnet.star`: `project_info(path)`, `dependencies(path)`, the parts `build_part()`, `test_part()`, `format_part()` and `pack_part()`, and the module `standard`, which chooses the parts from what the project says.
- `@std//python.star`: `dependencies(pyproject)`, from `[tool.uv.sources]` entries with `path` or `workspace = true`.
- `@std//node.star`: `dependencies(manifest)`, from `file:`, `link:` and `workspace:` versions, and from names of packages of the workspace.
- `@std//paths.star`: `//` paths: `join()`, `dirname()`, `ancestors()` and others.

A package takes its dependencies from a project file with `package(deps = dependencies("src/App/App.csproj"))`; Azad finds the rest of the graph through the packages themselves (decision 0028).

### Standard layouts only

The modules know the standard layout of each ecosystem, as its tools document it.
Anything else is the user's to declare by hand, with `package(deps = ...)` and their own parts.

For .NET, the module reads the nearest `Directory.Build.props`, the project file and the nearest `Directory.Build.targets`, in the order that MSBuild evaluates them.
It follows an import of the next file above with `$([MSBuild]::GetPathOfFileAbove('<file>', '$(MSBuildThisFileDirectory)../'))`, and skips imports of the SDK and of the MSBuild toolset, which add no project of the workspace.
It also follows an import by a plain path, relative to the importing file, optionally after `$(MSBuildThisFileDirectory)`; an import of a file that does not exist adds nothing.
An item below NuGet's folder of packages (`$(NuGetPackageRoot)`, `$(NuGetPackageFolders)` or `$(Pkg<Name>)`) names no file of the workspace, and is left out.
It resolves `$(Name)` of a property that these files set without a Condition, and `$(MSBuildThisFileDirectory)`, `$(MSBuildProjectDirectory)` and `$(MSBuildProjectName)`.
It counts `ProjectReference` items, and the files that other items name outside the project's package, such as `<Compile Include="../../libs/orders/src/*.cs" />`; for a wildcard, the package of the directory before it and every package below that directory.
Items whose Include is a name, such as `PackageReference`, name no file.

### Over-approximation

An extra dependency only costs a rebuild.
A missing one breaks an isolated build, and makes a host run skip work that it must do.
So the modules take the union: `ProjectReference` items and other items under every Condition, such as one for each target framework, the target frameworks under every Condition, and, for Node, every dependency whose name is a package of the workspace, with any version.

### Fail, do not guess

When a module needs a value and cannot know it, it fails, and the message names the file and the element or entry, and says to declare the dependency by hand.
Examples: a property that the SDK, the command line or the environment sets, a property function, a property that a Condition sets, an `<Import>` by a property or a wildcard, a referenced project that no package holds, and a workspace member that does not exist.

## Consequences

- A change to a project's references changes the package's dependencies at the next evaluation, with no list to keep.
- Each package that loads a module reads its project files, and the files of its workspace, at each evaluation; a cache of evaluations can use `read_files` later.
- The standard parts of .NET name their tasks after the tool (`dotnet-build`), so a package has one .NET project in them; a package with more chooses its own parts.
- A project that uses more of MSBuild than the module reads gets an error, not a wrong graph.
- Whether a task's declared sources hold what it reads is still not checked; a trace in the sandbox can do that later (`docs/plan.md`).

## Amendment, 2026-11-03: solutions, `$(ProjectDir)` and the build's folders

A trial on a large repository found three gaps in the .NET module.

- `dependencies()` takes a solution, a `.slnx` or a `.sln` file, as well as a project.
  A package builds a solution, and the solution, not the directory, says which projects are in the build.
  Taking every project file below a directory can add a project that only another package's solution builds, and so make a cycle.
  The dependencies of a solution are the packages that hold its projects, and the dependencies of each of its `.csproj`, `.fsproj` and `.vbproj` projects, without the solution's own package.
  A `.slnx` lists its projects as `<Project Path="...">` elements at any depth below `<Folder>` elements; a `.sln` lists them as `Project(...)` lines, and its solution folders are left out.
- `$(ProjectDir)` resolves to the project's directory with a `/` at its end, as the SDK sets it.
- An item below a folder that the build writes (`$(MSBuildProjectExtensionsPath)`, `$(BaseIntermediateOutputPath)`, `$(IntermediateOutputPath)`, `$(BaseOutputPath)`, `$(OutputPath)` or `$(OutDir)`) names no file of the workspace, and is left out, as an item below NuGet's folder of packages is.
  This also holds when another property holds the folder, such as `<None Include="$(_SdkPropsForPack)" />` with `$(MSBuildProjectExtensionsPath)` in that property.
