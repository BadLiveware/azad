# 0005: Derivation rules use `datafrog`

Status: accepted, 2026-10-30.

## Context

The core derives what must run from facts: `needed`, `blocked`, `must_run`.
These are Datalog rules: joins and anti-joins over relations, computed to a fixed point.
`datafrog` is a small Rust library for exactly this, and the Rust compiler's borrow checker work (Polonius) uses it.
`crepe` and `ascent` compile rules from macros; plain Rust would hide the rules in loops.

## Decision

The derivation rules are written in Rust as `datafrog` relations and joins.
Explanations ("why did this run?") come from a separate query that walks the rules backward from one fact, and gives the facts and rules that derived it.

## Consequences

- The vision says rules are data, and `datafrog` rules are code.
  That is right for derivation rules, which we write and release with Azad.
  Policy rules, which people edit, need a rule language that compiles to the same operations; that is a later decision.
- The explanation query must stay in step with the rules; a test checks that every derived fact can be explained.
- Each derivation recomputes from the facts of one run; incremental derivation is a later optimization, if a measurement asks for it.
