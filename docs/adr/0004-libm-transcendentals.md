# ADR 0004 — `libm` instead of `std` for `sin`, `cos`, `sqrt`, `atan2`

- Status: Accepted
- Date: 2026-11-04

## Context

`f32::sin` in `std` delegates to the platform's libm. Two consequences:

1. Results differ in the last bits between Linux, macOS and Windows, because
   each ships a different implementation with different rounding.
2. Results can differ between versions of the same OS.

For a crate that promises "the same seed produces the same world", either fact is
fatal: a replay recorded on Linux diverges from a run on Windows within a few
hundred ticks, and the divergence is invisible until positions have drifted
enough to change a neighbour decision.

## Decision

Every transcendental used in simulation logic comes from the `libm` crate:
`libm::sinf`, `libm::cosf`, `libm::sqrtf`, `libm::atan2f`. `libm` is a pure-Rust
port of the MUSL implementations, so the bits are identical everywhere.

## Consequences

- Cross-platform determinism for the maths that matters. This is the single
  decision that makes the guarantee true rather than aspirational.
- `libm` calls are not inlined or vectorised the way platform intrinsics can be;
  a `sinf` costs a few nanoseconds more. In a crowd, these calls are in spawn
  placement and look-at maths, not the inner steering loop, which uses only
  multiply, add and one `sqrt`.
- One more dependency, `no_std`-compatible, with no transitive deps.
- The rule is stated in `ARCHITECTURE.md` and enforced by review: a `f32::sin`
  in `domain/` is a bug, not a style preference.

## Alternatives rejected

- **Platform `std` maths** — fails the guarantee, non-negotiably.
- **Fixed-point trigonometry** — deterministic and fast, but changes every API
  signature and loses the precision that steering needs at small separations.
- **`micromath`** — faster on embedded, less accurate, and its `atan2` error is
  large enough to visibly change steering behaviour.
