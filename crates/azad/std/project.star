"""A project's goals from parts (decision 0025): each part puts tasks in roles, such as `fmt` or `test`, and names the
roles that CI checks; `project()` makes a goal for each role, which requires the tasks of every part in it, and the
goal `ci`. A project changes what it does by its choice of parts, and two kinds of project in one directory are two
lists of parts."""

_CI = "ci"
_CI_EVENTS = ["push", "pull_request"]

# What `azad list` says of a role's goal; each task keeps its own doc.
_ROLE_DOCS = {
    "fmt": "Format the files",
    "fmt-check": "Check that the files are formatted",
    "lint": "Lint the code",
    "typecheck": "Check the types",
    "test": "Run the tests",
    "build": "Build",
    "pack": "Make the packages",
}

def part(ci = [], **roles):
    """Tasks in roles, such as `part(fmt = format, fmt_check = check, ci = ["fmt-check"])`.

    A role takes a task, a list of tasks, or None or an empty list, which leave the role out, so that a part can
    choose its roles with a condition or compute them. A `_` in a role's name is a `-`: `fmt_check` is the role `fmt-check`. `ci` names the roles of the
    part that the goal `ci` requires."""
    tasks = {}
    for name, value in roles.items():
        role = name.replace("_", "-")
        if value == None or value == []:
            continue
        if type(value) == "list":
            tasks[role] = value
        else:
            tasks[role] = [value]
    for role in ci:
        if role not in tasks:
            fail("std/project.part: `ci` names %r, which the part has no task for; its roles are %r" % (role, sorted(tasks.keys())))
    return struct(roles = tasks, ci = ci)

def project(*parts, on = _CI_EVENTS):
    """A goal for each role of `parts`, which requires the tasks of every part in that role, and the goal `ci`, which
    requires the tasks of the roles that each part names in its `ci`, and which the orchestrator runs on the events
    `on`.

    Each argument is a part, or a list of parts, such as a preset that a module gives. A goal and a task of a package
    cannot have the same name, so name the tasks of parts after their tool, such as `dotnet-fmt`."""
    roles = {}
    checks = []
    for part in _parts(parts):
        for role, tasks in part.roles.items():
            roles[role] = roles.get(role, []) + [task for task in tasks if task not in roles.get(role, [])]
        for role in part.ci:
            checks += [task for task in part.roles[role] if task not in checks]
    for role, tasks in roles.items():
        goal(role, require = tasks, doc = _ROLE_DOCS.get(role, role))
    if checks:
        goal(_CI, require = checks, doc = "What CI checks", on = on)

def _parts(arguments):
    """The parts of `project()`'s arguments, with each list of parts in its place."""
    found = []
    for argument in arguments:
        items = argument
        if type(argument) != "list":
            items = [argument]
        for item in items:
            if type(item) != "struct" or not hasattr(item, "roles"):
                fail("std/project.project: an argument is a part() or a list of parts, not %r" % (item,))
            found.append(item)
    return found
