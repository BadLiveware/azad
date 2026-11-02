"""Azad's next release, which Azad computes from its own history (decision 0030).

The release workflow runs `//release:version` with the full history and the tags, and reads `release/out/version`:
empty when no commit since the latest release is a feature, a fix or breaking. This is a package of its own, so that
the goals that CI runs by label never walk the history.
"""

load("@std//version.star", "next_release")

def _text(version):
    if version == None:
        return ""
    return str(version)

task(
    name = "version",
    cmd = ["sh", "-c", 'mkdir -p out && printf "%s" "$AZAD_RELEASE" > out/version'],
    env = {"AZAD_RELEASE": _text(next_release())},
    generates = ["out"],
    doc = "Write the version of the release that this commit makes, or nothing, to out/version",
)
