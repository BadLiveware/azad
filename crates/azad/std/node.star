"""Node.js projects that pnpm, npm or Yarn manage (decision 0031): the packages of the projects that a `package.json`
takes from the workspace.

A dependency, a dev dependency or an optional dependency comes from the workspace when its version is `file:<path>` or
`link:<path>`, or `workspace:<range>`, which names a package of the workspace, or `workspace:<name>@<range>` for an
alias. The packages of the workspace are those of the nearest `pnpm-workspace.yaml` (its `packages` globs, with `!`
before an exclude), or of the nearest `package.json` with `workspaces` (a list of globs, or `{"packages": [...]}`), at
or above the manifest; directories named `node_modules` are not packages, and the root is one.

The safety rule is over-approximation: an extra dependency only costs a rebuild, but a missing one breaks isolated
builds and makes host runs skip work that they must do. So a dependency whose name is the name of a package of the
workspace counts with any version, because npm and Yarn link it when the version matches, and pnpm when
`link-workspace-packages` is set. A path that no package holds, and a `workspace:` name that the workspace does not
have, are errors that name the file and the entry; the module never guesses."""

load("@std//paths.star", "ancestors", "dirname", "join")

_MANIFEST = "package.json"
_PNPM_WORKSPACE = "pnpm-workspace.yaml"
_FIELDS = ["dependencies", "devDependencies", "optionalDependencies"]
_PATH_PROTOCOLS = ["file:", "link:"]
_WORKSPACE_PROTOCOL = "workspace:"
_EXCLUDE = "!"
_NODE_MODULES = "**/node_modules/**"

_BY_HAND = "declare the dependency in package(deps = ...) by hand, and leave node.dependencies() out"

def _globs(directory, globs, where):
    """The patterns of the manifests that workspace globs of `directory` name, as (include, exclude)."""
    include, exclude = [], [join(directory, _NODE_MODULES)]
    for glob in globs:
        negated = glob.startswith(_EXCLUDE)
        found = join(directory, glob.removeprefix(_EXCLUDE))
        if found == None:
            fail("%s: the glob %r goes above the root of the workspace" % (where, glob))
        if negated:
            exclude.append(join(found, _MANIFEST))
        else:
            include.append(join(found, _MANIFEST))
    return include, exclude

def _packages(directory, globs, where):
    """The packages of a workspace, by name, as `//` directories: the root's and those that `globs` name."""
    include, exclude = _globs(directory, globs, where)
    packages = {}
    for manifest in files([join(directory, _MANIFEST)]) + files(include, exclude = exclude):
        name = json.decode(read(manifest)).get("name")
        if type(name) == "string" and name:
            packages[name] = dirname(manifest)
    return packages

def _workspace_globs(manifest, data, directory):
    """The workspace globs that `directory` declares, as (file, globs); None when it declares none."""
    pnpm = join(directory, _PNPM_WORKSPACE)
    if files([pnpm]):
        declared = yaml.decode(read(pnpm)) or {}
        return (pnpm, declared.get("packages", []))
    candidate = join(directory, _MANIFEST)
    found = data
    if candidate != manifest:
        if not files([candidate]):
            return None
        found = json.decode(read(candidate))
    workspaces = found.get("workspaces")
    if type(workspaces) == "dict":
        workspaces = workspaces.get("packages")
    if not workspaces:
        return None
    return (candidate, workspaces)

def _workspace(manifest, data):
    """The packages of the workspace of `manifest`, by name, as `//` directories, with the file that declares it, as
    struct(file, packages); None when no file at or above it declares a workspace."""
    for directory in ancestors(dirname(manifest)):
        declared = _workspace_globs(manifest, data, directory)
        if declared != None:
            file, globs = declared
            return struct(file = file, packages = _packages(directory, globs, file))
    return None

def _alias(name, spec):
    """The name of the package that `workspace:<spec>` names: `<name>@<range>` for an alias, else the dependency's."""
    at = spec.rfind("@")
    if at > 0:
        return spec[:at]
    return name

def _target(manifest, where, name, version, workspace):
    """The `//` path that a dependency names in the workspace; None for one outside it."""
    for protocol in _PATH_PROTOCOLS:
        if version.startswith(protocol):
            return _path(manifest, where, version.removeprefix(protocol))
    if version.startswith(_WORKSPACE_PROTOCOL):
        spec = version.removeprefix(_WORKSPACE_PROTOCOL)
        if spec.startswith(".") or spec.startswith("/"):
            return _path(manifest, where, spec)
        if workspace == None:
            fail("%s is %r, but no %s or %s with workspaces at or above %s declares a workspace" % (
                where,
                version,
                _PNPM_WORKSPACE,
                _MANIFEST,
                manifest,
            ))
        package = workspace.packages.get(_alias(name, spec))
        if package == None:
            fail("%s is %r, but the workspace of %s has no package named %r; it has %s" % (
                where,
                version,
                workspace.file,
                _alias(name, spec),
                sorted(workspace.packages.keys()),
            ))
        return package
    if workspace != None:
        return workspace.packages.get(name)
    return None

def _path(manifest, where, path):
    if path.startswith("/"):
        fail("%s: the path %r is absolute, outside the workspace" % (where, path))
    found = join(dirname(manifest), path)
    if found == None:
        fail("%s: the path %r goes above the root of the workspace" % (where, path))
    return found

def dependencies(manifest = _MANIFEST):
    """The labels of the packages that hold the projects that a `package.json` takes from the workspace, sorted,
    without its own package; for `package(deps = dependencies())`.

    `manifest` is relative to the package, or to the root after `//`."""
    path = workspace_path(manifest)
    data = json.decode(read(path))
    entries = []
    for field in _FIELDS:
        entries += [(field, name, version) for name, version in data.get(field, {}).items() if type(version) == "string"]
    if not entries:
        return []
    workspace = _workspace(path, data)
    own = package_of(path)
    labels = []
    for field, name, version in entries:
        where = "%s: %s[%r]" % (path, field, name)
        target = _target(path, where, name, version, workspace)
        if target == None:
            continue
        label = package_of(target)
        if label == None:
            fail("%s names %s, which no package holds: no directory at or above it has a tasks.star or a tasks.yaml; add one, or %s" % (
                where,
                target,
                _BY_HAND,
            ))
        if label != own and label not in labels:
            labels.append(label)
    return sorted(labels)
