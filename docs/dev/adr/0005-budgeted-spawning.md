# ADR 0005 — Budgeted spawning from a queue of ranges

- Status: Accepted
- Date: 2026-11-04

## Context

A game asking for 100,000 NPCs at startup gets one of two disasters if spawning
is naive: a multi-second freeze while 100,000 entities are created, or a
`Vec` holding 100,000 pre-generated identities that exists only to be drained
over the next twenty-five frames.

There is also a correctness requirement hiding in the feature list: the same
seed must produce the same individuals regardless of how many frames the spawn
took. If spawning is spread across frames, identity allocation must not depend on
frame boundaries.

## Decision

Spawn requests go into a `SpawnQueue` holding *ranges*, not identities. Each frame,
`drain_spawn_queue` pops at most `spawn_budget` identities, generates each NPC's
personality from `RngStream::for_npc(seed, id)`, and spawns it.

Identity allocation is a pure function of the batch's start index and its mode:
`Unique` yields `NpcId::unique(next)`, `Recycled(slots)` yields
`NpcId::recycled(next % slots)`.

## Consequences

- Memory for pending spawns is one queue entry per request, not one per NPC.
- A 100,000-NPC crowd costs 25 frames at the default 4,096 budget instead of a
  single 400 ms hitch, and the budget is a config field rather than a constant.
- Because identity depends only on the index within the batch, the *n*-th NPC
  spawned is the same individual regardless of how the frames fell. The
  determinism suite asserts exactly this: the same seed produces identical
  temperaments across a budgeted run and an unbudgeted one.
- `FlockStats::spawn_complete()` exists so a game can gate "the level is ready"
  on the queue draining, instead of guessing.

## Alternatives rejected

- **Spawn everything in one frame** — the freeze, and no way to show progress.
- **Pre-generate all blueprints** — turns a spawn budget problem into a memory
  problem and makes the queue the largest allocation in the program.
- **Round-robin over frames without an index contract** — the individual an
  identity belongs to would depend on frame timing, breaking determinism.
