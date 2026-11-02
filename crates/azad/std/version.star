"""Versions from the history of the workspace's repository (decision 0021), built only on `git` and `semver`."""

def commit_calver(commit = None, window = 25):
    """`<year>.<month>.<count>` of `commit` (the `HEAD` commit when it is None), such as `2026.10.4812`.

    `count` is how many commits the commit contains, so on a branch that only moves forward the versions follow the
    order of the commits. The year and month come from the latest committer date among the commit and its first
    parents, at most `window` of them: git does not make committer dates grow along history, and a commit with an
    older date than its parent must not sort below the version before it."""
    commit = commit or git.commit()
    chain = commit.first_parents(window)
    latest = max([parent.time for parent in chain])
    date = [parent.date for parent in chain if parent.time == latest][0]
    year, month = date.split("-")[0:2]
    return "%d.%d.%d" % (int(year), int(month), commit.count())

def latest_release(prefix = "v"):
    """The highest release among the tags `<prefix><semver>`, as `(version, tag)`; `(None, None)` when there is none.

    Pre-releases, and tags whose rest is not a semantic version, are left out."""
    releases = []
    for tag in git.tags(prefix + "*"):
        text = tag[len(prefix):]
        if semver.valid(text) and semver.parse(text).pre == "":
            releases.append((semver.parse(text), tag))
    if not releases:
        return (None, None)
    return sorted(releases)[-1]

_BREAKING_FOOTERS = ["BREAKING CHANGE:", "BREAKING-CHANGE:"]

# How many first parents each step of the walk to the latest release reads; it is usually a few commits back.
_WALK_STEPS = [25, 100, 400]

def conventional_commit(message):
    """The Conventional Commits header of `message`, as `(type, breaking)`; `(None, False)` when it has none.

    The header is the subject line, `<type>[(<scope>)][!]: <description>`, such as `feat(cli)!: drop --sandbox`. A
    change is breaking when its header has `!`, or a line of its body starts with `BREAKING CHANGE:`."""
    lines = message.split("\n")
    prefix, colon, description = lines[0].partition(": ")
    kind = prefix.removesuffix("!").partition("(")[0]
    if not colon or not description.strip() or not kind or not kind.isalpha() or not kind.islower():
        return (None, False)
    breaking = prefix.endswith("!") or len([line for line in lines[1:] if line.startswith(tuple(_BREAKING_FOOTERS))]) > 0
    return (kind, breaking)

def next_release(prefix = "v", commit = None, releasing = ["feat", "fix", "perf"], window = 1000):
    """The release that `commit` (the `HEAD` commit when it is None) makes, as `<major>.<yyyymmdd>.<n>`; None when no
    commit since the latest release is one.

    The version is semantic: `major` counts breaking changes, `yyyymmdd` is the UTC date of the commit, and `n` counts
    the releases of that major on that day, from 0. A commit since the latest release (`latest_release(prefix)`) makes
    a release when its Conventional Commits type is in `releasing`, or when it is breaking; a breaking one raises
    `major`. The commits since the latest release are the first parents of `commit` down to the release's commit, at
    most `window` of them, as a branch with squash merges or direct pushes has them."""
    commit = commit or git.commit()
    latest, tag = latest_release(prefix)
    released = None
    if latest:
        released = git.tags(tag)[tag]
    since = _since(commit, released, window)
    if latest and since == None:
        fail("next_release(): %s is not among the %d first parents of %s" % (tag, window, commit.id))
    headers = [conventional_commit(parent.message) for parent in since]
    breaking = len([kind for kind, is_breaking in headers if is_breaking]) > 0
    if not breaking and len([kind for kind, _ in headers if kind in releasing]) == 0:
        return None
    major = 0
    if latest:
        major = latest.major
    if latest and breaking:
        major += 1
    day = commit.date.replace("-", "")
    same_day = [name for name in git.tags("%s%d.%s.*" % (prefix, major, day)) if semver.valid(name[len(prefix):])]
    return semver.parse("%d.%s.%d" % (major, day, len(same_day)))

def _since(commit, released, window):
    """The first parents of `commit` after the commit `released`, `commit` first; all of them, at most `window`, when
    `released` is None. None when `released` is not among them. The walk reads more parents at each step, and stops
    at `released` or at the first commit."""
    chain = []
    for size in [step for step in _WALK_STEPS if step < window] + [window]:
        chain = commit.first_parents(size)
        ids = [parent.id for parent in chain]
        if released in ids:
            return chain[:ids.index(released)]
        if len(chain) < size:
            break
    if released:
        return None
    return chain
