""".NET projects of a standard layout (decision 0031): what a project file says, the packages of the projects that it
references, and the standard parts of a project for `@std//project.star`.

The module reads MSBuild files as MSBuild evaluates them for an SDK-style project: the nearest `Directory.Build.props`
at or above the project, the project file, and the nearest `Directory.Build.targets`, in that order, so that a later
value replaces an earlier one. A `Directory.Build.props` or `Directory.Build.targets` can import the next one above it
with `$([MSBuild]::GetPathOfFileAbove('<file>', '$(MSBuildThisFileDirectory)../'))`. Any file can import another by
its path, relative to the importing file as MSBuild resolves it, optionally after `$(MSBuildThisFileDirectory)`; an
import of a file that does not exist adds nothing. An import of the SDK, or of a file of the MSBuild toolset
(`$(MSBuildToolsPath)`, `$(MSBuildExtensionsPath)`, `$(MSBuildExtensionsPath32)`, `$(MSBuildBinPath)` or
`$(VSToolsPath)`), adds no project of the workspace and is not read. Any other import, such as one by a property or a
wildcard, is an error, because the module cannot know what it adds.

An item below NuGet's folder of packages (`$(NuGetPackageRoot)`, `$(NuGetPackageFolders)`, or `$(Pkg<Name>)` when no
file sets it), or below a folder that the build writes (`$(MSBuildProjectExtensionsPath)`,
`$(BaseIntermediateOutputPath)`, `$(IntermediateOutputPath)`, `$(BaseOutputPath)`, `$(OutputPath)` or `$(OutDir)`),
names no file of the workspace, and is left out, also through another property.

A value can use `$(Name)` of a property that these files set without a Condition, and `$(MSBuildThisFileDirectory)`,
`$(MSBuildProjectDirectory)`, `$(ProjectDir)` and `$(MSBuildProjectName)`. A value that the module needs and cannot resolve, such as a
property that the SDK, the command line or the environment sets, a property function, or a property that a Condition
sets, is an error that names the file and the element. The module never guesses.

The safety rule is over-approximation: an extra dependency only costs a rebuild, but a missing one breaks isolated
builds and makes host runs skip work that they must do. So `ProjectReference` items count under every Condition, such
as one for each target framework, and so do the files that other items name outside the project's package, such as
`<Compile Include="../../libs/orders/src/*.cs" />`: in an isolated run, a wildcard that matches nothing is no error
for MSBuild, and only a missing type would show the gap."""

load("@std//paths.star", "ancestors", "basename", "dirname", "join", "relative_to")
load("@std//project.star", "part")

_PROPS = "Directory.Build.props"
_TARGETS = "Directory.Build.targets"
_PROJECT = "Project"
_CONDITION = "Condition"
_WEB_SDK = "microsoft.net.sdk.web"
_TEST_SDK_PACKAGE = "microsoft.net.test.sdk"
_EXECUTABLE_OUTPUTS = ["exe", "winexe"]
_PATH_OF_FILE_ABOVE = "$([MSBuild]::GetPathOfFileAbove("
_THIS_FILE_DIRECTORY = "$(MSBuildThisFileDirectory)"
_TOOLSET_PROPERTIES = [
    "$(MSBuildToolsPath)",
    "$(MSBuildExtensionsPath)",
    "$(MSBuildExtensionsPath32)",
    "$(MSBuildBinPath)",
    "$(VSToolsPath)",
]
# Properties that name NuGet's folder of packages, which is never in the workspace: an item below one names no file of
# a package of the workspace. `$(Pkg<Name>)` is the folder of one package, from `GeneratePathProperty`.
_OUTSIDE_PROPERTIES = [
    "nugetpackageroot",
    "nugetpackagefolders",
    "msbuildprojectextensionspath",
    "baseintermediateoutputpath",
    "intermediateoutputpath",
    "baseoutputpath",
    "outputpath",
    "outdir",
]
_PACKAGE_PATH_PREFIX = "pkg"
_PROJECT_EXTENSIONS = [".csproj", ".fsproj", ".vbproj"]
_SOLUTION = ".sln"
_XML_SOLUTION = ".slnx"
_SOLUTION_FOLDER = "{2150E333-8FDC-42A3-9474-1A3956D46DE8}"
_GROUPS = {"PropertyGroup": "property", "ItemGroup": "item"}
_SCOPES = ["Choose", "When", "Otherwise", "ImportGroup"]

# Items whose Include is a name, not a path.
_NAME_ITEMS = [
    "PackageReference",
    "PackageVersion",
    "GlobalPackageReference",
    "PackageDownload",
    "FrameworkReference",
    "Using",
    "InternalsVisibleTo",
    "AssemblyAttribute",
    "AssemblyMetadata",
    "ProjectCapability",
    "SupportedPlatform",
    "CompilerVisibleProperty",
    "CompilerVisibleItemMetadata",
    "TrimmerRootAssembly",
    "COMReference",
]

# The files above a project that the SDK, NuGet and MSBuild read for it: a change to one of them can change the build.
_SETTINGS_FILES = [_PROPS, _TARGETS, "Directory.Packages.props", "global.json", "NuGet.config", "nuget.config", "NuGet.Config"]

# What the tasks write in the projects' directories, which is not a source.
_BUILD_OUTPUT = ["**/bin/**", "**/obj/**"]
_PACK_OUTPUT = "nupkg"
_NUGET_CACHE = {"/root/.nuget/packages": "dotnet-nuget-packages"}

# Starlark has no `while`, so each walk has a bound far above any real project.
_STEPS = 100000

_BY_HAND = "declare the dependency in package(deps = ...) by hand, and leave dotnet.dependencies() out"

def _where(file, element):
    """A file and an element of it, as messages name them: `//App/App.csproj: <ProjectReference Include="...">`."""
    attrs = " ".join(['%s="%s"' % (name, value) for name, value in element["attrs"].items()])
    if attrs:
        return "%s: <%s %s>" % (file, element["tag"], attrs)
    return "%s: <%s>" % (file, element["tag"])

def _stem(name):
    """A file's name without its extension: `App` of `App.csproj`."""
    return name.rsplit(".", 1)[0]

def _with_slash(directory):
    if directory.endswith("/"):
        return directory
    return directory + "/"

def _nearest(name, directory):
    """The `//` path of the nearest file `name` in `directory` or above it; None when there is none."""
    for candidate in ancestors(directory):
        found = files([join(candidate, name)])
        if found:
            return found[0]
    return None

def _known(text):
    return struct(text = text, problem = None, outside = False)

def _unknown(problem):
    return struct(text = None, problem = problem, outside = False)

def _outside_workspace():
    """The value of a property that names a folder outside the workspace, such as NuGet's or the build's output."""
    return struct(text = None, problem = None, outside = True)

def _file_frame(file, conditional, chain, state):
    """The elements of an MSBuild file to evaluate, each with whether a Condition holds it."""
    root = xml.decode(read(file))
    if root["tag"] != _PROJECT:
        fail("%s: the root element is <%s>, not <Project>" % (file, root["tag"]))
    if file not in state["files"]:
        state["files"].append(file)
    state["sdks"].extend(root["attrs"].get("Sdk", "").split(";"))
    return {"file": file, "queue": [(child, conditional) for child in root["children"]], "chain": chain + [file]}

def _path_of_file_above(file, value):
    """The import `$([MSBuild]::GetPathOfFileAbove('<name>', '$(MSBuildThisFileDirectory)<path>'))` of `file`, as
    struct(found), where found is the file that it names or None; None when `value` is not that import."""
    text = value.replace(" ", "")
    if not text.startswith(_PATH_OF_FILE_ABOVE) or not text.endswith("))"):
        return None
    arguments = [argument.strip("'\"") for argument in text[len(_PATH_OF_FILE_ABOVE):-2].split(",")]
    if len(arguments) != 2 or not arguments[1].startswith(_THIS_FILE_DIRECTORY):
        return None
    name, start = arguments[0], arguments[1].removeprefix(_THIS_FILE_DIRECTORY).replace("\\", "/")
    if "$" in name or "$" in start:
        return None
    directory = join(dirname(file), start)
    if directory == None:
        return struct(found = None)
    return struct(found = _nearest(name, directory))

def _imported(file, element):
    """The file that an `<Import>` of `file` reads, or None for an import that adds no project of the workspace."""
    value = element["attrs"].get("Project", "").strip()
    for toolset in _TOOLSET_PROPERTIES:
        if value.startswith(toolset):
            return None
    above = _path_of_file_above(file, value)
    if above != None:
        return above.found
    by_path = _import_by_path(file, value)
    if by_path == None:
        fail("%s: the module follows only an import by a path, or of the next %s or %s above with " % (
            _where(file, element),
            _PROPS,
            _TARGETS,
        ) + "GetPathOfFileAbove, so it cannot know what this import adds; %s" % _BY_HAND)
    return by_path.found

def _import_by_path(file, value):
    """The file that an import by a path reads: relative to `file`, optionally after `$(MSBuildThisFileDirectory)`, with
    `found` None when it is not a file of the workspace. None when `value` is not a plain path."""
    path = value.removeprefix(_THIS_FILE_DIRECTORY).replace("\\", "/")
    if not path or "$" in path or "@" in path or "%" in path or "*" in path or "?" in path or path.startswith("/"):
        return None
    resolved = join(dirname(file), path)
    if resolved == None or not files([resolved]):
        return struct(found = None)
    return struct(found = resolved)

def _follow(start, state):
    """Adds the properties and items of `start`, and of the files that it imports, to `state`, in MSBuild's order."""
    stack = [_file_frame(start, False, [], state)]
    for _ in range(_STEPS):
        if not stack:
            return
        frame = stack[-1]
        if not frame["queue"]:
            stack.pop()
            continue
        element, conditional = frame["queue"].pop(0)
        file, tag = frame["file"], element["tag"]
        conditional = conditional or _CONDITION in element["attrs"]
        if tag in _GROUPS:
            for child in element["children"]:
                held = conditional or _CONDITION in child["attrs"]
                state["entries"].append(struct(kind = _GROUPS[tag], file = file, element = child, conditional = held))
        elif tag in _SCOPES:
            scoped = [(child, conditional or tag == "Choose") for child in element["children"]]
            stack.append({"file": file, "queue": scoped, "chain": frame["chain"]})
        elif tag == "Sdk":
            state["sdks"].append(element["attrs"].get("Name", ""))
        elif tag == "Import" and "Sdk" in element["attrs"]:
            state["sdks"].append(element["attrs"]["Sdk"])
        elif tag == "Import":
            imported = _imported(file, element)
            if imported in frame["chain"]:
                fail("%s: the imports form a cycle: %s" % (_where(file, element), " -> ".join(frame["chain"] + [imported])))
            if imported != None:
                stack.append(_file_frame(imported, conditional, frame["chain"], state))
    fail("dotnet: the imports of %s are deeper than %d steps" % (start, _STEPS))

def _evaluation(project):
    """What MSBuild's files say for `project`, a `//` path: the properties and items in order, the SDKs, and the
    files that were read."""
    state = {"entries": [], "sdks": [], "files": []}
    directory = dirname(project)
    for file in [_nearest(_PROPS, directory), project, _nearest(_TARGETS, directory)]:
        if file != None:
            _follow(file, state)
    state["properties"] = _properties(state["entries"], project)
    return state

def _closing(text, opening):
    """The index of the `)` that closes the `(` at `opening`; -1 when none does."""
    depth = 0
    for index in range(opening, len(text)):
        if text[index] == "(":
            depth += 1
        elif text[index] == ")":
            depth -= 1
            if depth == 0:
                return index
    return -1

def _value(definitions, name):
    """The value of a property from its definitions since the last one without a Condition."""
    held = [definition for definition in definitions if definition.conditional]
    if held:
        return _unknown("%s sets %s under a Condition, which the module does not evaluate" % (held[0].where, name))
    return struct(text = definitions[0].text, problem = definitions[0].problem, outside = definitions[0].outside)

def _property(name, properties, file, project, where):
    """The value of `$(name)` in `file`."""
    key = name.strip().lower()
    if not key.replace("_", "").replace("-", "").isalnum():
        return _unknown("%s: $(%s) is a property function, which the module does not run" % (where, name))
    if key == "msbuildthisfiledirectory":
        return _known(_with_slash(dirname(file)))
    if key == "msbuildprojectdirectory":
        return _known(dirname(project))
    if key == "projectdir":
        return _known(_with_slash(dirname(project)))
    if key == "msbuildprojectname":
        return _known(_stem(basename(project)))
    if key in _OUTSIDE_PROPERTIES:
        return _outside_workspace()
    definitions = properties.get(key)
    if not definitions and key.startswith(_PACKAGE_PATH_PREFIX):
        return _outside_workspace()
    if not definitions:
        return _unknown(
            "%s: $(%s) is not set in the files that the module reads; the SDK, the command line or the environment " % (where, name) +
            "can set it",
        )
    return _value(definitions, name)

def _expand(text, properties, file, project, where):
    """`text` with each `$(Name)` in it replaced by the property's value, as struct(text, problem)."""
    for marker in ["@(", "%("]:
        if marker in text:
            return _unknown("%s: %r names items or metadata (%s...), which the module does not evaluate" % (where, text, marker))
    done, rest = "", text
    for _ in range(_STEPS):
        start = rest.find("$(")
        if start < 0:
            return _known(done + rest)
        end = _closing(rest, start + 1)
        if end < 0:
            return _unknown("%s: %r has a $( that is not closed" % (where, text))
        value = _property(rest[start + 2:end], properties, file, project, where)
        if value.problem != None or value.outside:
            return value
        done, rest = done + rest[:start] + value.text, rest[end + 1:]
    fail("%s: %r has more than %d properties" % (where, text, _STEPS))

def _properties(entries, project):
    """The definitions of each property, by its name in lower case, since its last definition without a Condition."""
    properties = {}
    for entry in entries:
        if entry.kind != "property":
            continue
        name = entry.element["tag"].lower()
        where = _where(entry.file, entry.element)
        value = _expand(entry.element["text"], properties, entry.file, project, where)
        definition = struct(
            text = value.text,
            problem = value.problem,
            outside = value.outside,
            conditional = entry.conditional,
            where = where,
        )
        if entry.conditional:
            properties[name] = properties.get(name, []) + [definition]
        else:
            properties[name] = [definition]
    return properties

def _needed(state, name, project):
    """The value of a property that `project_info()` needs; None when no file sets it."""
    definitions = state["properties"].get(name.lower())
    if not definitions:
        return None
    value = _value(definitions, name)
    if value.problem != None:
        fail("%s.\nproject_info(%s) needs %s: set it without a Condition, or choose this project's parts yourself" % (
            value.problem,
            project,
            name,
        ))
    return value.text

def _flag(state, name, project):
    """A property that is true or false; None when no file sets it."""
    text = _needed(state, name, project)
    if text == None:
        return None
    flag = text.strip().lower()
    if flag == "true":
        return True
    if flag == "false":
        return False
    fail("%s: %s is %r, not true or false" % (project, name, text))

def _is_test(state, project):
    """Whether the project is a test project: IsTestProject, or a reference to Microsoft.NET.Test.Sdk."""
    flag = _flag(state, "IsTestProject", project)
    if flag != None:
        return flag
    references = [
        entry
        for entry in state["entries"]
        if entry.kind == "item" and entry.element["tag"] == "PackageReference" and
           entry.element["attrs"].get("Include", "").strip().lower() == _TEST_SDK_PACKAGE
    ]
    if [entry for entry in references if not entry.conditional]:
        return True
    if references:
        fail("%s references Microsoft.NET.Test.Sdk under a Condition, which the module does not evaluate; set " % (
            _where(references[0].file, references[0].element)
        ) + "IsTestProject to true or false for %s" % project)
    return False

def _kind(state, project):
    if _is_test(state, project):
        return "test"
    sdks = [sdk.split("/")[0].strip().lower() for sdk in state["sdks"]]
    if _WEB_SDK in sdks:
        return "web"
    output = _needed(state, "OutputType", project)
    if output != None and output.strip().lower() in _EXECUTABLE_OUTPUTS:
        return "exe"
    return "library"

def _frameworks(state, project):
    """The target frameworks: TargetFrameworks, or TargetFramework, under every Condition."""
    for name in ["TargetFrameworks", "TargetFramework"]:
        found = []
        for definition in state["properties"].get(name.lower(), []):
            if definition.problem != None:
                fail("%s.\nproject_info(%s) needs %s" % (definition.problem, project, name))
            found += [item.strip() for item in definition.text.split(";") if item.strip() and item.strip() not in found]
        if found:
            return sorted(found)
    return []

def _wildcard_index(names):
    """The index of the first name with a wildcard; -1 when none has one."""
    for index in range(len(names)):
        if "*" in names[index] or "?" in names[index]:
            return index
    return -1

def _item_path(text, project, where, reference):
    """The path that one include of an item names, as struct(path, pattern, where, reference): `path` is the `//` path
    of the file, or of the directory before the first wildcard, and `pattern` the include as a pattern from the root.
    Relative paths are from the project's directory, as MSBuild reads them, also in an imported file. None for an
    absolute path of another item than a `ProjectReference`, which is outside the workspace."""
    path = text.strip().replace("\\", "/")
    if ":" in path or (path.startswith("/") and not path.startswith("//")):
        if reference:
            fail("%s: %r is an absolute path, outside the workspace" % (where, text))
        return None
    prefix, body = "", path
    if path.startswith("//"):
        prefix, body = "//", path.removeprefix("//")
    names = body.split("/")
    wildcard = _wildcard_index(names)
    if wildcard >= 0 and reference:
        fail("%s: the module does not expand the wildcard in %r; %s" % (where, text, _BY_HAND))
    fixed, rest = names, []
    if wildcard >= 0:
        fixed, rest = names[:wildcard], names[wildcard:]
    found = join(dirname(project), prefix + "/".join(fixed))
    if found == None:
        fail("%s: %r goes above the root of the workspace" % (where, text))
    pattern = found
    if rest:
        pattern = _with_slash(found) + "/".join(rest)
    return struct(path = found, pattern = pattern, where = where, reference = reference)

def _include(element):
    """What an item names as a path: its Include, or the HintPath of a Reference, whose Include is an assembly name."""
    if element["tag"] != "Reference":
        return element["attrs"].get("Include")
    hints = [child["text"] for child in element["children"] if child["tag"] == "HintPath"]
    if not hints:
        return element["attrs"].get("HintPath")
    return hints[0]

def _items(state, project):
    """The paths that the items of `project` name, under every Condition, as `_item_path()` gives them, sorted by
    pattern: `ProjectReference`s, and the files of every item that names files, such as `Compile`, `None`, `Content` or
    `EmbeddedResource`. An item whose Include is a name, such as a `PackageReference`, names no path."""
    found = {}
    for entry in state["entries"]:
        tag = entry.element["tag"]
        if entry.kind != "item" or tag in _NAME_ITEMS:
            continue
        include = _include(entry.element)
        if include == None:
            continue
        where = _where(entry.file, entry.element)
        value = _expand(include, state["properties"], entry.file, project, where)
        if value.outside:
            continue
        if value.problem != None:
            fail("%s; %s" % (value.problem, _BY_HAND))
        for text in [part for part in value.text.split(";") if part.strip()]:
            item = _item_path(text, project, where, tag == "ProjectReference")
            if item != None and (item.pattern, item.reference) not in found:
                found[(item.pattern, item.reference)] = item
    return [found[key] for key in sorted(found.keys())]

def _references(state, project):
    """The projects that `project` references, as struct(path, where), sorted by path."""
    return [item for item in _items(state, project) if item.reference]

def _is_within(path, directory):
    """Whether the `//` path `path` is `directory` or below it."""
    return directory == "//" or path == directory or path.startswith(directory + "/")

def _packages_below(directory):
    """The labels of the packages at or below the `//` directory `directory`."""
    found = files([join(directory, "**/tasks.star"), join(directory, "**/tasks.yaml")])
    return [package_of(dirname(file)) for file in found]

def project_info(path):
    """What a .NET project file says, with the `Directory.Build.props` and `Directory.Build.targets` files that MSBuild
    reads for it, as a struct:

    - `path`: the project file, as a `//` path;
    - `name`: its name without the extension;
    - `kind`: `test` when IsTestProject is true, or when it references Microsoft.NET.Test.Sdk without a Condition;
      else `web` for the SDK Microsoft.NET.Sdk.Web; else `exe` when OutputType is Exe or WinExe; else `library`;
    - `packable`: IsPackable; when no file sets it, as the SDK does: true for a library and an executable, false for a
      test project and a web project;
    - `frameworks`: TargetFrameworks or TargetFramework, under every Condition, sorted;
    - `references`: the `//` paths of the projects that it references, under every Condition, sorted;
    - `includes`: the files that its other items name outside its package directory, such as
      `<Compile Include="../../libs/orders/src/*.cs" />`, as patterns from the root, sorted;
    - `files`: the MSBuild files that the module read, the project file among them.

    `path` is relative to the package, or to the root after `//`."""
    project = workspace_path(path)
    state = _evaluation(project)
    kind = _kind(state, project)
    packable = _flag(state, "IsPackable", project)
    if packable == None:
        packable = kind in ["library", "exe"]
    own = package_of(project)
    return struct(
        path = project,
        name = _stem(basename(project)),
        kind = kind,
        packable = packable,
        frameworks = _frameworks(state, project),
        references = [item.path for item in _references(state, project)],
        includes = sorted([item.pattern for item in _items(state, project) if not item.reference and not _is_mine(item, own)]),
        files = state["files"],
    )

def _is_mine(item, own):
    """Whether an item's files are in the project's own package directory."""
    return own != None and own != "//" and _is_within(item.path, own)

def dependencies(path):
    """The labels of the packages that a .NET project or solution needs directly, sorted, without its own package; for
    `package(deps = dependencies("App.slnx"))`.

    For a project, they are the packages of the projects that it references, and of the files that its other items name
    outside its package directory, such as `<Compile Include="../../libs/orders/src/*.cs" />`: for a wildcard, the
    package that holds the directory before the first wildcard, and every package below that directory. An item's files
    that no package holds are not a dependency; the standard parts take them as sources. Items count under every
    Condition. Azad finds the dependencies of those packages through the packages themselves (decision 0028), so each
    package names only its own.

    For a solution, a `.slnx` or a `.sln` file, they are the packages that hold its projects, and the dependencies of
    each of its `.csproj`, `.fsproj` and `.vbproj` projects. The solution, not the directory, says which projects a
    build has.

    A project that no package holds, and a path that the module cannot resolve, are errors that name the file and the
    element.

    `path` is relative to the package, or to the root after `//`."""
    file = workspace_path(path)
    if file.endswith(_XML_SOLUTION) or file.endswith(_SOLUTION):
        return _solution_dependencies(file)
    return _project_dependencies(file)

def _project_dependencies(project):
    """The labels of the packages that `project` needs directly, without its own package."""
    state = _evaluation(project)
    own = package_of(project)
    labels = []
    for item in _items(state, project):
        found = []
        if item.reference:
            found = [_holding_package(item.path, item.where)]
        elif not _is_mine(item, own):
            found = [package_of(item.path)] + _wildcard_packages(item)
        labels += [label for label in found if label != None and label != own and label not in labels]
    return sorted(labels)

def _holding_package(path, where):
    """The label of the package that holds the project `path`, which `where` names."""
    label = package_of(path)
    if label == None:
        fail("%s: no package holds %s: no directory at or above it has a tasks.star or a tasks.yaml; add one, or %s" % (
            where,
            path,
            _BY_HAND,
        ))
    return label

def _solution_dependencies(solution):
    """The labels of the packages that the projects of `solution` are in and need, without the solution's own package."""
    own = package_of(solution)
    labels = []
    for project in _solution_projects(solution):
        found = [_holding_package(project.path, project.where)]
        if [extension for extension in _PROJECT_EXTENSIONS if project.path.endswith(extension)]:
            found += _project_dependencies(project.path)
        labels += [label for label in found if label != own and label not in labels]
    return sorted(labels)

def _solution_projects(solution):
    """The projects of a solution, as struct(path, where), in the order of the file."""
    if solution.endswith(_XML_SOLUTION):
        elements = _project_elements(xml.decode(read(solution)))
        paths = [(_where(solution, element), element["attrs"]["Path"]) for element in elements]
    else:
        paths = _sln_projects(solution)
    found = []
    for where, path in paths:
        resolved = join(dirname(solution), path.strip().replace("\\", "/"))
        if resolved == None:
            fail("%s: %r goes above the root of the workspace" % (where, path))
        found.append(struct(path = resolved, where = where))
    return found

def _project_elements(element):
    """The `<Project Path="...">` elements of a `.slnx` file, at any depth below `<Folder>` elements."""
    found, queue = [], [element]
    for _ in range(_STEPS):
        if not queue:
            return found
        current = queue.pop(0)
        queue += current["children"]
        if current["tag"] == _PROJECT and "Path" in current["attrs"]:
            found.append(current)
    fail("dotnet: a solution has more than %d elements" % _STEPS)

def _sln_projects(solution):
    """The projects of a `.sln` file, as (where, path), without its solution folders: each project is a line
    `Project("{type}") = "Name", "path", "{id}"`."""
    found = []
    for number, line in enumerate(read(solution).splitlines()):
        line = line.strip()
        if not line.startswith(_PROJECT + "(") or "=" not in line:
            continue
        kind = line.split("=", 1)[0]
        fields = [field.strip().strip('"') for field in line.split("=", 1)[1].split(",")]
        if _SOLUTION_FOLDER in kind.upper() or len(fields) < 2:
            continue
        found.append(("%s:%d" % (solution, number + 1), fields[1]))
    return found

def _wildcard_packages(item):
    """The packages below the directory of an item's wildcard, whose files the wildcard can match."""
    if item.pattern == item.path:
        return []
    return _packages_below(item.path)

def _closure(project):
    """The `//` paths of `project` and of every project that it references, directly or through others, with the
    items of each that name files that no package holds, as struct(projects, loose)."""
    found, loose, queue = [], [], [project]
    for _ in range(_STEPS):
        if not queue:
            return struct(projects = found, loose = loose)
        current = queue.pop(0)
        if current in found:
            continue
        found.append(current)
        items = _items(_evaluation(current), current)
        queue += [item.path for item in items if item.reference]
        loose += [item.pattern for item in items if not item.reference and _is_loose(item) and item.pattern not in loose]
    fail("dotnet: the references of %s are more than %d" % (project, _STEPS))

def _is_loose(item):
    """Whether no package holds the files of an item."""
    return package_of(item.path) == None and not _wildcard_packages(item)

def _sources(project):
    """The sources of the tasks of `project`: its package; the settings files above it and above the projects that it
    builds, which no package that it depends on holds; and the files that the items of these projects name and that no
    package holds."""
    closure = _closure(project)
    packages = [package_of(path) for path in closure.projects]
    settings = []
    for directory in sorted({dirname(path): True for path in closure.projects}.keys()):
        for candidate in ancestors(directory):
            pattern = join(candidate, "{%s}" % ",".join(_SETTINGS_FILES))
            settings += [file for file in files([pattern]) if file not in settings and package_of(file) not in packages]
    return ["**"] + sorted(settings + [pattern for pattern in closure.loose if pattern not in settings])

def _task(name, verb, project, sdk, sources, doc, generates = []):
    target = relative_to(project, workspace_path("."))
    return task(
        name = name,
        cmd = ["dotnet"] + verb + [target],
        sources = sources,
        exclude = _BUILD_OUTPUT,
        generates = generates,
        image = sdk,
        network = True,
        caches = _NUGET_CACHE,
        dependencies = True,
        doc = doc,
    )

def _build(project, sdk, sources):
    build = _task("dotnet-build", ["build"], project, sdk, sources, "Build the .NET project %s" % basename(project))
    return part(build = build, ci = ["build"])

def _test(project, sdk, sources):
    test = _task("dotnet-test", ["test"], project, sdk, sources, "Run the tests of the .NET project %s" % basename(project))
    return part(test = test, ci = ["test"])

def _format(project, sdk, sources):
    name = basename(project)
    check = _task("dotnet-format-check", ["format", "--verify-no-changes"], project, sdk, sources, "Check the format of %s" % name)
    fmt = _task("dotnet-format", ["format"], project, sdk, sources, "Format %s" % name)
    return part(fmt = fmt, fmt_check = check, ci = ["fmt-check"])

def _pack(project, sdk, sources):
    doc = "Pack %s into %s/" % (basename(project), _PACK_OUTPUT)
    pack = _task("dotnet-pack", ["pack", "--output", _PACK_OUTPUT], project, sdk, sources, doc, generates = [_PACK_OUTPUT])
    return part(pack = pack)

def build_part(project, sdk = None):
    """The part `build`, which CI checks: `dotnet build <project>`.

    Each task of this module runs in the package's directory, in `sdk`, an image with the .NET SDK, such as
    `mcr.microsoft.com/dotnet/sdk`; without one, it runs on the host also in an isolated run. Its sources are the
    package's files without `bin/` and `obj/`, and the settings files that the SDK, NuGet and MSBuild read above the
    project and above the projects that it builds (`Directory.Build.props`, `Directory.Build.targets`,
    `Directory.Packages.props`, `global.json` and `NuGet.config`); it reads its package's dependencies
    (`dependencies = True`), has the network for the restore of packages, and keeps NuGet's packages in a cache volume
    at `/root/.nuget/packages`. `project` is relative to the package, or to the root after `//`."""
    project = workspace_path(project)
    return _build(project, sdk, _sources(project))

def test_part(project, sdk = None):
    """The part `test`, which CI checks: `dotnet test <project>`, as `build_part()` describes the tasks."""
    project = workspace_path(project)
    return _test(project, sdk, _sources(project))

def format_part(project, sdk = None):
    """The parts `fmt`, `dotnet format <project>`, and `fmt-check`, which CI checks:
    `dotnet format --verify-no-changes <project>`, as `build_part()` describes the tasks."""
    project = workspace_path(project)
    return _format(project, sdk, _sources(project))

def pack_part(project, sdk = None):
    """The part `pack`: `dotnet pack --output nupkg <project>`, which generates `nupkg/` in the package, as
    `build_part()` describes the tasks."""
    project = workspace_path(project)
    return _pack(project, sdk, _sources(project))

def _standard(project, sdk = None):
    project = workspace_path(project)
    info = project_info(project)
    sources = _sources(project)
    parts = [_format(project, sdk, sources)]
    if info.kind == "test":
        parts.append(_test(project, sdk, sources))
    else:
        parts.append(_build(project, sdk, sources))
    if info.packable:
        parts.append(_pack(project, sdk, sources))
    return parts

standard = module(
    name = "std/dotnet.standard",
    params = {"project": "string", "sdk": "image?"},
    returns = "parts",
    implementation = _standard,
)
