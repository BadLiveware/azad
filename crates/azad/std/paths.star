"""Paths of the workspace in the form that `workspace_path()`, `files()` and `package_of()` give: `//` and the path
from the root, such as `//libs/core/Core.csproj`; `//` is the root (decision 0031)."""

_ROOT = "//"

def names(path):
    """The names in a `//` path, from the root: `["libs", "core"]` of `//libs/core`, and `[]` of `//`."""
    return [name for name in path.removeprefix(_ROOT).split("/") if name]

def from_names(parts):
    """The `//` path of names from the root: `//libs/core` of `["libs", "core"]`."""
    return _ROOT + "/".join(parts)

def dirname(path):
    """The directory of a `//` path: `//libs/core` of `//libs/core/Core.csproj`; `//` of `//` itself."""
    return from_names(names(path)[:-1])

def basename(path):
    """The last name in a `//` path: `Core.csproj` of `//libs/core/Core.csproj`; empty for `//`."""
    parts = names(path)
    if not parts:
        return ""
    return parts[-1]

def join(dir, relative):
    """The `//` path of `relative` from the directory `dir`, without `.` and `..`; a `relative` that starts with `//`
    is from the root. None when the path goes above the root."""
    parts = names(dir)
    if relative.startswith(_ROOT):
        parts = []
    for name in relative.removeprefix(_ROOT).split("/"):
        if name == "" or name == ".":
            continue
        if name != "..":
            parts.append(name)
            continue
        if not parts:
            return None
        parts.pop()
    return from_names(parts)

def relative_to(path, start):
    """`path` relative to the directory `start`, both `//` paths: `../core/Core.csproj` of `//libs/core/Core.csproj`
    from `//libs/web`; `.` when they are the same."""
    to, of = names(path), names(start)
    shared = 0
    for index in range(min(len(to), len(of))):
        if to[index] != of[index]:
            break
        shared = index + 1
    parts = [".."] * (len(of) - shared) + to[shared:]
    if not parts:
        return "."
    return "/".join(parts)

def ancestors(dir):
    """The directory `dir` and each directory above it, up to the root, the nearest first, as `//` paths."""
    parts = names(dir)
    return [from_names(parts[:count]) for count in range(len(parts), -1, -1)]

def nearest(file, dir):
    """The `//` path of the nearest file named `file` in `dir` or a directory above it, up to the root; None when there
    is none. Each directory is read on its own, not the workspace below it."""
    for directory in ancestors(dir):
        found = files([join(directory, file)])
        if found:
            return found[0]
    return None
