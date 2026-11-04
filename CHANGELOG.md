# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A complete behaviour-tree node set: `Sequence`, `Selector`, `Parallel` with
  `RequireAll`/`RequireAny`, `Inverter`, `RepeatUntilFail`, `Cooldown`, `Timeout`,
  `Condition` and `Action`. Compiled to a flat node array; a tick allocates
  nothing, which `tests/tree_no_alloc.rs` measures rather than assumes.
- Abort semantics that reach running leaves deepest-first, with subtree ranges in
  the compiled layout so a timeout can cancel the work underneath it.
- `NodeSpec::from_ron` behind the `serde` feature, so a designer can author a tree
  without recompiling; a golden test asserts it evaluates identically to the
  builder equivalent.
- `VirtualCost`, the declared-cost newtype the think scheduler will spend
  (ADR 0009), plus `LeafBehavior::cost_hint` and `FlockRegistry::tree_cost`.
- `StandardLeaves`: sixteen built-in leaves (actions and predicates) that are
  stateless by construction, so one registered instance serves the whole crowd,
  each with a cost calibrated against `benches/brain.rs`.
- `Archetype::logic`: a behaviour tree per archetype with a pinned declared cost —
  guard 27 vns, merchant 46, urchin 36, noble 35, wanderer 35.
- `NpcBrain`: a component holding a shared tree, a per-NPC cursor and the cached
  cost, ready for the scheduler task to drive.

- Social memory behind the `memory` feature (ADR 0010): `MemoryStore`, a flat
  array of 16-byte slots indexed by eligibility and subject, with lazy integer
  decay, a LIFO free list, and exact `implant`/`release` round-tripping.
- `MemoryArchive`, a bounded cold tier that evicts the faintest record by lowest
  total familiarity then lowest `NpcId`, so eviction is a pure function of what is
  stored.
- `DecayPolicy` with half-life in ticks and a `NONE` variant for grudges that never
  fade; memory decays to exactly zero rather than to a permanent sliver.
- `benches/memory.rs`: remember 7.7 ns, recall 5.5 ns, decayed recall 8.5 ns, sweep
  6.9 ns per NPC over 16,384 eligible NPCs.

### Changed

- `LeafBehavior::tick` and `abort` take `&self`: a leaf is a shared function over
  the blackboard, not per-NPC state. This makes the "one instance, whole crowd"
  model explicit instead of a footgun.
- `VirtualCost::DEFAULT` is 40 vns — several times the priciest built-in leaf —
  rather than an arbitrary 1,000.

### Documentation

- ADR 0011: how social memory integrates with the virtual-cost stride scheduler —
  cost as declared leaves, weight as a clamped boost, eligibility following the
  scheduler, and the feedback loop those rules close.

### Planned

The budgeted think scheduler, the utility scorer, the continuum ambient tier,
dialogue graphs and barks, snapshots and replay, rollback, and the render, `avian`
and `replicon` adapters are specified in `plan.toml` and not implemented yet.

## [0.1.0] — 2026-11-04

The simulation foundation.

### Added

- `FlockPlugin` with `FlockConfig`, four `CrowdPreset`s, and budgeted spawning
  driven by `SpawnQueue` (unique and recycled identity modes, 1 to 10,000+ NPCs,
  spawn budget per frame).
- `FlockSet` scheduling stages (`Spawn`, `Perception`, `Cognition`, `Steering`,
  `Integration`, `AdvanceTick`), chained so a game can join any of them.
- Boids steering with separation, alignment, cohesion, seek and arrive combined
  by priority-weighted truncation rather than a weighted sum.
- `SpatialGrid`: a uniform grid over a fixed volume with radius and k-nearest
  queries, deterministic result ordering, and no steady-state allocation.
- `RngStream`: PCG32 with per-NPC stream derivation from `(seed, NpcId)`,
  verified against the reference implementation and pinned by a golden vector.
- `Temperament` (Big Five plus bravery, sociability, industriousness),
  `Archetype` presets, `Needs` with decay and satisfaction, and a two-axis
  `Mood`.
- `BehaviorTree` with resume semantics, abort, a depth cap, and a flat compiled
  layout; `StateMachine` with dwell times and ordered guards; `Blackboard` with
  fixed capacity and compile-checked keys.
- `FlockRegistry` with the `LeafBehavior`, `GroupDirector`, `WorldDirector` and
  `NpcLogic` extension traits, and duplicate-identifier rejection at
  registration.
- Criterion benchmarks for whole frames at 1, 1,000, 10,000 and 100,000 NPCs,
  plus the spatial query and steering inner loops.
- Determinism tests that hash full NPC state in `NpcId` order and assert that
  identical seeds produce identical worlds, and that a larger population keeps
  the shared prefix of individuals.

### Notes

- MSRV is 1.97.1, matching Bevy 0.20.
- `glam` is pinned to the 0.33 line; `cargo tree -d -i glam` should never show
  two versions.
