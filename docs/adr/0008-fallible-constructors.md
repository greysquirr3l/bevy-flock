# ADR 0008 — Parse, don't validate, and no panic paths in library code

- Status: Accepted
- Date: 2026-11-04

## Context

Crowd code receives numbers from config files, designers, and other systems. A
zero-length radius, a NaN weight, or a population of zero produces a simulation
that looks fine for ten minutes and then divides by zero, freezes, or silently
spawns nobody.

Two styles are common in the Bevy ecosystem: clamp silently, or panic loudly.
Both are wrong. Clamping hides a config bug; panicking turns a config bug into a
crash in a shipped game.

## Decision

1. **Invalid values cannot be constructed.** `PopulationSize::try_new(0)` fails
   and `PopulationSize` has no other public constructor except `clamped`, which
   is documented as being for provably-in-range literals.
2. **Every error names the field, the value, and the valid range.**
3. **No `unwrap`, `expect`, `panic!`, `todo!` or panicking index in library
   code.** Where an invariant cannot be expressed in the type system — an
   authored tree that is empty, a leaf id that does not exist — the engine fails
   the evaluation or the registration, records it, and keeps simulating.
4. **Tests may panic freely.** The crate-level `cfg_attr(test, expect(...))`
   says so once, rather than sprinkling `#[expect]` over every test module.
5. **Fallible where the failure is the caller's, infallible where it is
   provably impossible.** `BoidsConfig` stores a `SpatialGridConfig` that was
   already validated, so the plugin can build a grid without a failure path; the
   `SpatialGrid::empty()` degenerate grid exists so that the plugin's
   construction has no panic path at all, even if someone hands it a
   hand-constructed config.

## Consequences

- A misconfigured crowd fails loudly at startup with a message that says what to
  fix, instead of at tick 12,000 with a NaN position.
- Callers write `?` or `expect` at the boundary where they know the value is
  good; the library never decides for them.
- The lint configuration enforces the policy mechanically
  (`clippy::unwrap_used`, `expect_used`, `panic`, `indexing_slicing` denied), and
  CI runs it, so the policy cannot rot.

## Alternatives rejected

- **Clamping silently** — the reason this ADR exists; it hides the bug.
- **Panicking constructors** — a config typo becomes a crash in a shipped game,
  and Bevy's plugin `build` cannot return a `Result`, so an invalid config would
  take down `App::new()` for a reason unrelated to the game.
- **`Result` on everything, including infallible cases** — noise that trains
  callers to ignore the error.
