# 0047 — Stimulus perception: a bounded buffer, not a per-NPC scan

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** ADR 0009 (virtual cost), ADR 0041 (memory in cognition), ADR 0040 (ports)

## Context

Encounter perception answers "did this NPC meet that player?", which is what social memory needs.
It is not a senses model: it cannot express a guard losing sight of a target and searching, a
villager hearing a scream through a wall, or a stealth game where being seen is a fact with an
age rather than a flag. Those are the questions behaviour trees actually ask, and answering them
per NPC per tick from the entity list is the classic crowd cost — a hundred agents each walking a
thousand others.

## Decision

Stimuli are published once into a bounded, decaying ring buffer; observers read the buffer.

### The record

`Stimulus { kind, position, radius, intensity, emitted, ttl }`, where `kind` is `Sight`, `Sound`,
`Smell` or `Custom(u16)`. Strength at an observer is
`intensity · (1 − distance/radius) · (1 − age/ttl)` — distance falloff multiplied by age falloff,
so a shout heard three seconds ago is a rumour rather than a location. A record is live while
`age <= ttl`; `ttl == 0` is never live, which is how a game says "this tick only".

`kind` decides the rest: `Sight` is narrowed by the field-of-view cone and blocked by the
occluder; everything else is omni-directional and goes round corners. A `Custom` channel is
treated as sound, because a game that invented the channel knows better than this module does.

### The buffer

`StimulusBuffer::new(capacity)` is a fixed-size ring: pushing into a full buffer overwrites the
oldest record and reports it, so a noisy frame cannot grow memory. `retain_live(now)` sweeps
expired records once per tick, preserving ring order.

### Perception

`perceive(buffer, observer, forward, now, query, occluder)` walks the buffer in ring order,
rejects by range, cone and occlusion, sorts by strength then distance then kind then position with
`total_cmp`, and truncates to `max_results`. No hashing, no sampling, no float comparison that can
differ between runs: the same world yields the same list, and the test
`perception_is_deterministic` holds it to that.

### Occlusion

`Occluder` is one trait with one method, `blocks(from, to)`. `OpenAir` blocks nothing and is the
default, so a game without physics pays nothing. A game with physics implements the trait over its
own raycast. This is deliberate: building an occlusion system would have dragged a collision
dependency into a crate that has none, which ADR 0040 exists to prevent.

### Cadence

The ECS system reads the simulation clock and perceives when `tick % interval == 0` (default 6 —
ten times a second at 60 Hz). Perception is mid-band work, and an agent that re-reads the buffer
every tick pays for an answer that changes once a second. The tick counter is the simulation's, so
a replay perceives on exactly the same ticks.

## Measurements

The pass is O(records × observers-due), not O(observers × entities). `docs/performance.md` records
the cost of a 4,096-record buffer with 10,000 observers at the default interval; the design target
is that it stays inside the mid-band budget, which is a fraction of the steering frame.

## Consequences

- A guard can lose sight of a target and search: `PerceivedStimuli` empties once the sighting
  expires, without any special case in the tree.
- Emitters are entities with a `StimulusSource` and a `Transform`; anything can be a stimulus,
  including another NPC's shout.
- `PerceivedStimuli` is copied into the observer, so a tree reads a stable snapshot for the whole
  think rather than live state that changes underneath it.
- The buffer is shared. A stimulus is not addressed to an observer: filters (team, channel) are
  the game's job at the leaf, which keeps the hot loop free of per-game predicates.

## Alternatives rejected

- **Per-NPC entity scans with the spatial grid.** Correct, and the cost grows with population ×
  density; it is the thing this module exists to avoid. The grid is still the right answer for a
  game that needs exact per-pair occlusion and can afford it.
- **A physics-based visibility system.** A dependency, a scheduler and a class of float
  nondeterminism for a hook that a game can supply in ten lines.
- **Unbounded per-observer history.** Alertness decay belongs in the tree or in social memory;
  the buffer holds what is happening, not what happened.
