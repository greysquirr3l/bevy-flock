# Architecture

This document explains why the crate is shaped the way it is. It describes the
code that exists, and marks the parts that are specified in `plan.toml` but not
yet built.

## The one rule

**The simulation does not know about rendering, physics, storage, networking or
time.**

Everything follows from that. A server can run a crowd with
`default-features = false`. A test can run 10,000 NPCs with no window. A replay
can reproduce a bug on a machine with no GPU. And a renderer that observes state
can never accidentally become a source of truth.

## Layers

```
domain/        pure: value objects, math, indexes. No Bevy types, no clock.
application/   the logical clock (SimClock). Orchestration state.
adapters/
  primary/     Bevy-facing: components, systems, plugin, commands.
  secondary/   driven adapters: indexes and stores the simulation owns.
behavior/      trees, state machines, blackboard, registry. Engine-agnostic.
boids.rs       the ECS systems that turn the domain math into frame work.
```

The compile-time enforcement is simple and worth stating: `domain/` imports
nothing from `bevy_ecs`, `bevy_app`, `bevy_transform` or `bevy_math`'s ECS
surface. It uses `Vec3` from `bevy_math` because that is a maths type, not an
engine type. If you find yourself wanting `Transform` in `domain/`, the value
belongs in an adapter.

## The frame

```
FlockSet::Spawn         drain the spawn queue against the per-frame budget
FlockSet::Perception    rebuild the spatial grid from current positions
FlockSet::Cognition     brains think, dispatched by the virtual-cost scheduler
FlockSet::Steering      each NPC computes its desired velocity
FlockSet::Integration   accelerate toward it, integrate position
FlockSet::AdvanceTick   clock.tick += 1
```

Sets are chained, so a game can insert its own systems with
`.in_set(FlockSet::Cognition)` and be correctly ordered without knowing anything
about this crate's internals.

## Determinism model

Three mechanisms, each with a test that fails if it is removed:

1. **Logical ticks.** `SimClock` holds a `u64`. No simulation code reads
   `Instant`, `SystemTime` or `Time<Virtual>`.
2. **Per-entity RNG streams.** `RngStream::for_npc(seed, id)` mixes the seed and
   the identifier through SplitMix64 to pick a PCG32 stream. Adding an NPC cannot
   perturb another's sequence.
3. **Ordered results.** The spatial grid sorts agents by `(cell id, insertion
   index)` into one flat array, and every query sorts by `(distance, NpcId)`.
   `HashMap` iteration order never touches simulation state.

Transcendental maths goes through `libm` (ADR 0004), because `std`'s `sin`,
`cos` and `atan2` delegate to the platform libm and differ in the last bits
between operating systems. `f32` is used for storage with `u64` ticks and
documented epsilons at comparison points.

Parallel work, when it lands, may only write per-entity components. Any
cross-entity reduction must be order-independent or sorted before accumulation.

## What is canonical and what is derived

Canonical: identity, temperament, needs, mood, position, velocity, targets,
blackboard, group membership.

Derived: the spatial grid, flow-field caches, LOD tier assignment, statistics.

Snapshots serialise canonical state only and rebuild derived state on load. This
is why the rollback test in the plan can catch hidden state: anything not in the
snapshot is, by definition, a bug waiting for a save/load cycle.

## Budgets

Two budgets, both explicit resources rather than magic numbers:

- `FlockConfig::spawn_budget` — entities created per frame. 100,000 NPCs at 4,096
  per frame arrive over 25 frames instead of one 400 ms hitch.
- The think budget (planned, ADR 0009) — **virtual** nanoseconds per frame. Each
  logic unit declares a cost, the sum is charged per think, and a start-time fair
  queue dispatches the minimum tag first. No wall clock enters the decision, so a
  replay dispatches the same NPCs on a machine four times slower; the maximum
  think interval is bounded by `ceil(cost * total_weight / (budget * weight)) + 1`
  ticks rather than enforced by a promotion rule.

## Failure philosophy

- Public constructors are fallible (`PopulationSize::try_new`,
  `SteeringParams::validate`, `SpatialGridConfig::try_new`); invalid values cannot
  be constructed, so systems cannot receive them.
- Errors carry the field, the value and the valid range.
- No `unwrap`, `expect`, `panic!` or panicking index in library code. `tests` use
  them freely.
- Where an invariant cannot be expressed in the type system (a leaf id that does
  not exist, a malformed authored tree), the engine fails the node or the
  registration, records it, and keeps simulating.

## Extension surface

`behavior::registry` is the only place a game plugs in:

| Trait | Scope | Purpose |
| --- | --- | --- |
| `LeafBehavior` | one NPC | an action inside a tree |
| `GroupDirector` | a group | squad-level orders |
| `WorldDirector` | everyone | festivals, patrols, panics |
| `NpcLogic` | one NPC | full takeover; the built-in scheduler skips it |

All are object-safe and `Send + Sync`, so they live in one `FlockRegistry`
without monomorphising the engine per game. Duplicate identifiers are rejected at
registration, which in practice means the first `app.update()` after a plugin
registers twice.

## Deliberate non-goals

No renderer. No physics solver. No networking protocol. No scripting language.
Each of those is a project in its own right, and every one of them would break
the one rule at the top of this document.
