"""Modules for tests (decision 0014)."""

# The parameters `files` and `image` hide the built-in functions with those names inside `_split`.
_files = files
_image = image

_NAME_CHARACTERS = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-."

def _safe(text):
    """The text as part of an action name; a character that a name cannot hold becomes '_'."""
    return "".join([c if c in _NAME_CHARACTERS else "_" for c in text.elems()])

def _split(name, files, command, image, inputs = []):
    """One action per file that matches `files`. Each one runs `command` in /src, with {file} replaced by the file's
    path, and sees only that file and the files of `inputs`. The action `name` collects their reports."""
    root = _image(image)
    reports = {}
    for file in _files(files):
        test = action(
            name = name + "." + _safe(file),
            cmd = ["sh", "-c", "cd /src && " + command.replace("{file}", file) + " && mkdir -p /work/report && echo '" + file + ": passed' > /work/report/result"],
            root = root,
            inputs = {"/src": glob([file] + inputs)},
            outputs = ["report"],
        )
        reports["/reports/" + _safe(file)] = test.out("report")
    if not reports:
        fail("std/test.split: no file matches " + str(files))
    return action(
        name = name,
        cmd = ["sh", "-c", "mkdir -p reports && cat /reports/*/result > reports/summary"],
        root = root,
        inputs = reports,
        outputs = ["reports"],
    )

split = module(
    name = "std/test.split",
    params = {
        "files": "patterns",
        "command": "string",
        "image": "string",
        "inputs": "patterns?",
    },
    outputs = ["reports"],
    implementation = _split,
)
