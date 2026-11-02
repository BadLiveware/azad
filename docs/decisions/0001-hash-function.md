# 0001: SHA-256 is the default hash function

Status: accepted, 2026-10-30.

## Context

Every identity in Azad is a digest: blobs, trees and actions.
A change of hash function changes every digest, so every cached result becomes useless.
Tools that speak the Remote Execution API (REAPI), such as Buildbarn, use SHA-256 by default.
BLAKE3 is faster, but few tools accept it.

## Decision

A digest has three parts: the hash function, the hash, and the size in bytes.
Its text form is `sha256:<hex>/<size>`.
SHA-256 is the default.

## Consequences

- A store can hold digests of more than one function, so BLAKE3 can be added later without a migration.
- Two digests are equal only when their functions are equal too; the same content under two functions is two entries.
- The size in the digest lets a reader reject a short or long blob before it hashes it.
