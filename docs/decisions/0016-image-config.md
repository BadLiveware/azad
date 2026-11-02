# 0016: The environment of an image is the base of an action's environment

Status: accepted, 2026-10-31. It changes decision 0008 and the alias objects of decision 0012.

## Context

An OCI (Open Container Initiative) image has a configuration beside its layers.
Its `Env` holds variables that the tools of the image need, such as `PATH=/usr/local/bin:...` in `python:3.13-slim`.
Azad applied the layers and ignored the configuration, so an action without `env` started with an empty environment.
`cmd = ["python", ...]` with a Python image failed with "the sandbox could not be set up: bwrap: execvp python: No such file or directory".
`docker run` starts the command with the image's `Env`, and users expect the same.

The variables must be known without the layers.
On a new CI (continuous integration) runner whose remote cache has every result, a run that runs nothing pulls nothing (decision 0012).
The action digest holds the environment, so the runner must know the image's `Env` before it can look up a result.

## Decision

- The puller also downloads the image configuration, a small JSON document named by a digest in the manifest, and checks its digest.
- An image record is a canonical object of type `image`: the tree of the layers and the `Env` of the configuration, as variables.
  A later `Env` entry for a name replaces an earlier one; an entry without `=` is an error.
- The store remembers an image record, not a bare tree, for each image and platform.
  The alias name starts with `image-record`, so that an alias of decision 0008, which starts with `image` and names a tree, is never read as a record.
- The remote cache keeps a signed image entry, `image-<hash of the name>`, in place of the alias entry of decision 0012.
  The entry holds the record itself, as a result entry holds its result, so one small object gives the tree and the environment.
  The tree is pulled only when an action that runs needs it, and the pull must give the same record.
- When the engine resolves an action whose root is an image, the environment of the action is the image's `Env`, with each variable of the action's own `env` in place of the image's value.
  The result is part of the action and of its digest, so `azad ir` shows it, and a change of the image's `Env` changes the digest.
  An image that is an input, not the root, gives no variables.
- The GHA (GitHub Actions) frontend no longer sets `PATH`: the image of the job gives it.
- When bwrap cannot find the command, the error says so: "the command python is not found in the PATH of the action: ...".
  bwrap reports this only as text (`bwrap: execvp <command>: No such file or directory`), so the sandbox reads that line.

Other fields of the configuration do not apply:

- `WorkingDir`: an action starts in `/work`, and its outputs are below `/work`.
  This is part of the contract of every action, and the same for every root.
- `User`: the sandbox runs as the user who runs Azad, and trees do not keep file owners (decision 0008).
  A user name or ID from the image has no meaning there.
- `Entrypoint` and `Cmd`: the command of an action is complete, as with `docker run --entrypoint`.

## Consequences

- The digest of every action whose root is an image changes once, also when its `env` already set every variable.
- A store pulls each image again once, because it does not read the aliases of decision 0008.
  A remote cache writes new image entries; old alias objects are not read and expire.
- An action's `env` can be shorter: `PATH` and the variables of the image's tools come from the image.
- When the binary exists but its interpreter or dynamic loader does not, exec also fails with "No such file or directory", and the error says that the command is not found.
