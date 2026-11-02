# 0008: Azad pulls images itself

Status: accepted, 2026-10-30. It changes a consequence of decision 0003.

## Context

Decision 0003 says that an image is pulled as a fixed-output action.
A fixed-output action runs in a sandbox, and a sandbox needs a root image, so the first image cannot be pulled that way.

## Decision

Azad pulls images itself, with the same guarantee as a fixed-output action:

- an image is named by the digest of its manifest or index, never by a tag alone;
- Azad checks the digest of the index, the manifest and every layer that it downloads;
- it selects the manifest for the platform of the action;
- it applies the layers in order, with whiteout files, and stores the result as one tree.

The store remembers which tree each image and platform gave, so an image is pulled and unpacked once.

## Consequences

- Azad has an OCI (Open Container Initiative) registry client: anonymous and bearer-token access, and redirects to blob storage.
- File owners in layers are not kept, and only the executable bit of the mode is kept, as for every tree.
  Device files are skipped; hard links become copies.
- A private registry needs credentials; that is a later step.
