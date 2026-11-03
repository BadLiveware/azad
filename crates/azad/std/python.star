"""Python projects that uv manages (decision 0031): the packages of the projects that a `pyproject.toml` takes from
the workspace.

A dependency comes from the workspace when its entry in `[tool.uv.sources]` has `path = ...`, a file or a directory, or
`workspace = true`, a member of the uv workspace: the nearest `pyproject.toml` at or above the project that has
`[tool.uv.workspace]`, whose `members` and `exclude` globs name the directories of its members. A member's name is
`[project].name`, compared after PEP 503 normalization. Git, URL and index sources are outside the workspace.
A member also gets the sources of the workspace's root, for the dependencies that it declares, as uv gives them.

The safety rule is over-approximation: an extra dependency only costs a rebuild, but a missing one breaks isolated
builds and makes host runs skip work that they must do. A path that no package holds, and a workspace member that does
not exist, are errors that name the file and the entry; the module never guesses."""

load("@std//paths.star", "ancestors", "dirname", "join")

_PYPROJECT = "pyproject.toml"
_SEPARATORS = "-_."

# The characters that end the name of a requirement (PEP 508), such as `core>=1.0; python_version < "3.12"`.
_NAME_ENDS = " ;[<>=!~(@,"

_BY_HAND = "declare the dependency in package(deps = ...) by hand, and leave python.dependencies() out"

def normalize(name):
    """A name of a Python project as PEP 503 compares it: lower case, with each run of `-`, `_` and `.` as one `-`."""
    normalized = ""
    previous = False
    for char in name.lower().elems():
        separator = char in _SEPARATORS
        if separator and previous:
            continue
        if separator:
            normalized += "-"
        else:
            normalized += char
        previous = separator
    return normalized

def _requirement_name(requirement):
    """The name in a requirement (PEP 508), normalized."""
    end = len(requirement)
    for char in _NAME_ENDS.elems():
        found = requirement.find(char)
        if found >= 0 and found < end:
            end = found
    return normalize(requirement[:end].strip())

def _declared(data):
    """The normalized names of the dependencies that a `pyproject.toml` declares, of every kind."""
    project = data.get("project", {})
    requirements = list(project.get("dependencies", []))
    for group in project.get("optional-dependencies", {}).values():
        requirements += group
    for group in data.get("dependency-groups", {}).values():
        requirements += group
    requirements += data.get("tool", {}).get("uv", {}).get("dev-dependencies", [])
    return [_requirement_name(requirement) for requirement in requirements if type(requirement) == "string"]

def _sources(file, data):
    """The entries of `[tool.uv.sources]` of a `pyproject.toml`, as struct(file, name, source), one for each source
    of an entry that is a list of them."""
    found = []
    for name, value in data.get("tool", {}).get("uv", {}).get("sources", {}).items():
        values = value
        if type(value) != "list":
            values = [value]
        found += [struct(file = file, name = name, source = source) for source in values if type(source) == "dict"]
    return found

def _workspace_root(project, data):
    """The root of the uv workspace of `project`, as struct(path, data); None when no `pyproject.toml` at or above it
    has `[tool.uv.workspace]`."""
    for directory in ancestors(dirname(project)):
        candidate = join(directory, _PYPROJECT)
        found = data
        if candidate != project:
            if not files([candidate]):
                continue
            found = toml.decode(read(candidate))
        if "workspace" in found.get("tool", {}).get("uv", {}):
            return struct(path = candidate, data = found)
    return None

def _members(root):
    """The members of a uv workspace, by normalized name, as `//` directories."""
    workspace = root.data["tool"]["uv"]["workspace"]
    base = dirname(root.path)

    def patterns(key):
        found = []
        for glob in workspace.get(key, []):
            directory = join(base, glob)
            if directory == None:
                fail("%s: [tool.uv.workspace] %s has %r, which goes above the root of the workspace" % (root.path, key, glob))
            found.append(join(directory, _PYPROJECT))
        return found

    members = {}
    for path in [root.path] + files(patterns("members"), exclude = patterns("exclude")):
        data = root.data
        if path != root.path:
            data = toml.decode(read(path))
        name = data.get("project", {}).get("name")
        if name:
            members[normalize(name)] = dirname(path)
    return members

def _target(entry, root, members):
    """The `//` path that a source names in the workspace; None for a source outside it."""
    where = "%s: [tool.uv.sources] %s" % (entry.file, entry.name)
    if "path" in entry.source:
        if entry.source["path"].startswith("/"):
            fail("%s: the path %r is absolute, outside the workspace" % (where, entry.source["path"]))
        found = join(dirname(entry.file), entry.source["path"])
        if found == None:
            fail("%s: the path %r goes above the root of the workspace" % (where, entry.source["path"]))
        return found
    if entry.source.get("workspace") != True:
        return None
    if root == None:
        fail("%s is a member of a uv workspace, but no pyproject.toml at or above %s has [tool.uv.workspace]" % (where, entry.file))
    found = members.get(normalize(entry.name))
    if found == None:
        fail("%s: the uv workspace at %s has no member named %r; its members are %s" % (
            where,
            root.path,
            entry.name,
            sorted(members.keys()),
        ))
    return found

def dependencies(pyproject = _PYPROJECT):
    """The labels of the packages that hold the projects that a `pyproject.toml` takes from the workspace, sorted,
    without its own package; for `package(deps = dependencies())`.

    `pyproject` is relative to the package, or to the root after `//`."""
    project = workspace_path(pyproject)
    data = toml.decode(read(project))
    root = _workspace_root(project, data)
    entries = _sources(project, data)
    if root != None and root.path != project:
        declared = _declared(data)
        entries += [entry for entry in _sources(root.path, root.data) if normalize(entry.name) in declared]
    members = {}
    if root != None and [entry for entry in entries if entry.source.get("workspace") == True]:
        members = _members(root)
    own = package_of(project)
    labels = []
    for entry in entries:
        target = _target(entry, root, members)
        if target == None:
            continue
        label = package_of(target)
        if label == None:
            fail("%s: [tool.uv.sources] %s names %s, which no package holds: no directory at or above it has a tasks.star or a tasks.yaml; add one, or %s" % (
                entry.file,
                entry.name,
                target,
                _BY_HAND,
            ))
        if label != own and label not in labels:
            labels.append(label)
    return sorted(labels)
