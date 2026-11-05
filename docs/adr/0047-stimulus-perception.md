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

The pass is O(live records × observers-due), not O(observers × entities). Measured
(`cargo bench --bench crowd -- perception`):

| Case | Time | Per unit |
| --- | ---: | ---: |
| 10,000 observers, 128 live records | 21.7 ms | 2.2 µs observer |
| 512 observers, 4,096 live records, 40 m square | 35.8 ms | 70 µs observer |
| 512 observers, 4,096 live records, 400 m square | 9.3 ms | 18 µs observer |

At the default six-tick interval the first row is ~3.6 ms a frame for a 10,000-NPC crowd that
perceives on every tick of its band, which is inside a mid-band budget and well outside a reflex
one — hence the interval, which is the scheduler's knob in miniature.

Three optimisations are already in, and they are worth separating because they say which costs are
left:

1. The squared-distance reject skips the square root for out-of-range records.
2. The cone test compares against `cos · |offset|` instead of normalising, saving a square root and
   a divide per record.
3. The result set is a **bounded insertion** rather than collect-sort-truncate. `max_results` is
   six by default, and a dense buffer can put thousands of records inside the hearing radius;
   keeping the best six as we go costs one comparison per slot and no growth of a vector the
   caller never sees. This is the largest of the three: 75.7 ms → 35.8 ms on the dense case.

Together they took the dense case from 87.1 ms to 35.8 ms.

## The linear cost, and the three steps that remove it

Per observer, the pass is O(R) in the number of *live records*, whether those records are on top
of the observer or on the other side of the level. The table above is the proof: the same 4,096
records cost 35.8 ms when they are crammed into a 40 m square and 9.3 ms when they are spread over
400 m, because in the spread case almost every record fails the range test. That 9.3 ms — 4.4 ns
per record per observer — is the **scan tax**: pure cost of looking at records that are nowhere
near. An index removes the tax, not the work, and that is the whole of the future plan.

The steps are ordered by benefit per unit of risk, and each is a separate decision:

1. **Observer-side early-out, no new data structure.** The buffer maintains the union sphere of its
   live records on insert and sweep — one add, one full recompute per tick, O(R) and vectorised by
   nothing in particular. An observer outside that sphere returns an empty list in O(1). In a
   100,000-NPC level where forty things are happening in one corner, this is the difference
   between 10,000 observers paying and the few hundred near the action paying, and it costs one
   bounding-sphere test per observer. This is the step to take first.
2. **A bucketed index over the records.** A uniform grid whose cell is
   `max(sight_radius, hearing_radius) / 2`; each record is inserted into every cell its own radius
   overlaps, so a 45 m shout at a 22.5 m cell spans at most 5×5 cells and a 3 m footstep spans one.
   An observer scans the cells its limit overlaps plus one ring, then runs every exact predicate
   unchanged — range, cone, occlusion and strength are still computed per candidate, so the answer
   is identical, only the candidate set is smaller. Cost is O(cells) to clear and rebuild per
   perception tick, amortised across every observer due that tick, which is why it is worth
   building once per tick rather than per observer. Gains where the ratio of query radius to world
   extent is small: 4,096 records spread over 400 m with a 45 m hearing radius drops from 4,096
   candidates per observer to roughly 50.
3. **Band-scoped observer sets.** Perception is mid-band work, so only observers in the bands that
   consume it should pay, and each band gets its own interval. This is scheduler integration, not
   index work, and it is where the 100,000 case is actually won: the crowd at reflex tier does not
   need a stimulus list, it needs to not walk into a wall.

### What every step must preserve

- **Bit-identical output.** Any index must produce exactly what the linear scan produces, held by a
  parity test over a randomised buffer (`index_matches_the_linear_scan`), in the same spirit as
  `the_bounded_insert_keeps_exactly_the_best_slots`, which guards the top-k swap already made.
- **A canonical candidate order.** The comparator is a total order (`strength`, `distance`, `kind`,
  `position.x`, `position.z`, all `total_cmp`), so bucket traversal order cannot change the result
  — but only as long as ties are broken by a field and not by arrival. A test that shuffles the
  buffer and compares must pass.
- **No steady-frame allocation.** The bucket array and the union sphere are sized from `capacity`
  and reused; a `Vec` rebuilt per tick would be a regression the allocator test would catch.
- **The exact predicates stay exact.** No approximation of the cone, no early acceptance. An index
  is allowed to skip candidates, never to add or soften one.

### What reopens this decision

The plan carries a `stimulus-spatial-index` task, and the trigger is a measurement rather than a
feeling: reopen when the dense case of `crowd_perception` exceeds 25% of a 16.6 ms frame at the
reference population, or when a game reports more than 256 live records in a level. Until then the
capacity and the interval are the two knobs, and a game should size its buffer to what its world
actually does rather than to the worst case.

### Rejected for the index

- **A k-d tree or BVH.** Rebuild cost per tick, and worse constants than a uniform grid for
  point-like records with a handful of radius classes.
- **A hash map keyed by cell.** Allocation per rebuild, and iteration order that would have to be
  sorted back into a canonical order to stay deterministic — paying for the sort twice.
- **Skipping the exact predicates once a record is bucketed.** Cheaper, and it changes answers
  near cell boundaries, which is the one thing a stealth game cannot tolerate.

## Consequences

- A guard can lose sight of a target and search: `PerceivedStimuli` empties once the sighting
  expires, without any special case in the tree.
- Emitters are entities with a `StimulusSource` and a `Transform`; anything can be a stimulus,
  including another NPC's shout.
- `PerceivedStimuli` is copied into the observer, so a tree reads a stable snapshot for the whole
  think rather than live state that changes underneath it.
- The buffer is shared. A stimulus is not addressed to an observer: filters (team, channel) are
  the game's job at the leaf, which keeps the hot loop free of per-game predicates.
- `perceive` allocates nothing: the result is a `SmallVec<[Perceived; 8]>` filled by bounded
  insertion, so six slots at the default `max_results` never leave the stack.
- The cost is linear in live records per observer until the index in "The linear cost" lands; the
  buffer's `capacity` and the perception `interval` are the two knobs that keep it affordable
  meanwhile.

## Alternatives rejected

- **Per-NPC entity scans with the spatial grid.** Correct, and the cost grows with population ×
  density; it is the thing this module exists to avoid. The grid is still the right answer for a
  game that needs exact per-pair occlusion and can afford it.
- **A physics-based visibility system.** A dependency, a scheduler and a class of float
  nondeterminism for a hook that a game can supply in ten lines.
- **Unbounded per-observer history.** Alertness decay belongs in the tree or in social memory;
  the buffer holds what is happening, not what happened.
