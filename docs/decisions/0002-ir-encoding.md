# 0002: The Action IR uses deterministic CBOR

Status: accepted, 2026-10-30.

## Context

The action digest is the hash of the encoded action, so the encoding must give exactly one sequence of bytes for one value.
Protobuf does not promise this; REAPI adds rules of its own on top of it, and implementations differ.
CBOR (Concise Binary Object Representation) defines a deterministic encoding in RFC 8949, section 4.2.

## Decision

The Action IR is encoded as CBOR with the core deterministic encoding requirements of RFC 8949, section 4.2.1:

- integers and lengths in their shortest form;
- definite lengths only;
- map keys sorted by the bytes of their encoding.

In addition:

- no floating-point values, so NaN and negative zero cannot occur;
- no tags, except the ones that a later decision defines;
- every encoded value starts with a schema version.

The decoder refuses input that is not in this form, so a receiver that decodes and encodes again gets the same bytes and the same digest.

## Consequences

- The IR has no wire compatibility with REAPI; remote execution through REAPI needs a translation layer.
- We own a small encoder, or we check a library's output against the rules in tests.
- Golden tests fix the bytes of known values, so an accidental change of the encoding fails a test.
