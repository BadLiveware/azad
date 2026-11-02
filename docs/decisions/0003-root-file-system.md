# 0003: The root file system of an action is an OCI image

Status: accepted, 2026-10-30.

## Context

An action must see the same tools and libraries on every machine, or the same digest can give different results.
The host's root file system differs between machines and changes with updates, so it is not hermetic.
A Nix closure is hermetic, but it asks every user to use Nix.
OCI (Open Container Initiative) images are content-addressed already, and most tools are published as images.

## Decision

Every action names a root image by the digest of its OCI manifest, and the image is an input like any other.
Azad converts the layers of an image into one Azad tree in the store, so staging uses the same tree mechanism as every other input.
The platform of the image (for example `linux/amd64`) is part of the action, and so of its digest.

## Consequences

- Before image pulls exist (phase 6), tests import a root file system from a local archive or an OCI layout on disk.
- An image is pulled as a fixed-output action, so a tag that moves cannot change a run silently.
- The conversion of layers must apply whiteout files and keep file modes, and its result must be deterministic.
- Results are not shared between platforms.
