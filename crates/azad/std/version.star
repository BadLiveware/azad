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
