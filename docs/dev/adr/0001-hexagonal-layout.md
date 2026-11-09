# ADR 0001, Collapsed hexagonal layout

- Status: Accepted
- Date: 2026-11-04

## Context

A crowd engine that imports Bevy everywhere cannot be tested without a world, run
on a headless server without pretending, or reasoned about without reading
everything. The usual Bevy crate shape, one `plugin.rs` and one `components.rs` , 
gets to ten thousand lines without anyone deciding to build a monolith.

## Decision

Adopt a hexagonal layout with four areas, collapsed to what actually earns its
keep:

- `domain/`, pure simulation state and mathematics. No `bevy_ecs`, no
  `bevy_app`, no `bevy_transform`, no clock, no I/O.
- `application/`, the logical clock and orchestration.
- `adapters/primary/`, components, systems, plugin, command extension: what a
  game calls.
- `adapters/secondary/`, indexes and stores the simulation owns.
- `behavior/`, trees, state machines, blackboard, registry: engine-agnostic by
  construction, because they are pure data structures over domain types.

Rendering is deliberately *not* an adapter. A renderer observes simulation state
and cannot drive it.

## Consequences

- Domain logic is testable with no `App`, which is why the determinism suite runs
  in milliseconds rather than minutes.
- One extra module indirection for new contributors; the module docs in
  `lib.rs`, `adapters/mod.rs` and `ARCHITECTURE.md` are where they land.
- Ports are traits declared in `behavior/registry.rs` and implemented by the
  game. There is exactly one port that matters today (`LeafBehavior`), so the
  abstraction is not speculative.

## Alternatives rejected

- **Single `lib.rs` with `mod plugin; mod components; mod systems;`**, the shape
  every Bevy crate starts with; collapses under its own weight at scale and gives
  no place to put determinism guarantees.
- **Full ports-and-adapters with a trait per dependency**, three traits with one
  implementation each, all of which exist to be mocked by tests that do not need
  mocking. Rejected as ceremony.
- **Separate crates per layer**, compile-time enforcement for free, but it
  fragments the docs, multiplies the publish surface, and makes `cargo doc` for a
  game developer a tour of six crates.
