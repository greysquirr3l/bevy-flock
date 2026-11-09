# 0047, Stimulus perception: a bounded buffer, not a per-NPC scan

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** ADR 0009 (virtual cost), ADR 0041 (memory in cognition), ADR 0040 (ports)

## Context

Encounter perception answers "did this NPC meet that player?", which is what social memory needs.
It is not a senses model: it cannot express a guard losing sight of a target and searching, a
villager hearing a scream through a wall, or a stealth game where being seen is a fact with an
age rather than a flag. Those are the questions behaviour trees actually ask, and answering them
per NPC per tick from the entity list is the classic crowd cost, a hundred agents each walking a
thousand others.

## Decision

Stimuli are published once into a bounded, decaying ring buffer; observers read the buffer.

### The record

`Stimulus { kind, position, radius, intensity, emitted, ttl }`, where `kind` is `Sight`, `Sound`,
`Smell` or `Custom(u16)`. Strength at an observer is
`intensity · (1 − distance/radius) · (1 − age/ttl)`, distance falloff multiplied by age falloff,
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

The ECS system reads the simulation clock and perceives when `tick % interval == 0` (default 6 , 
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
one, hence the interval, which is the scheduler's knob in miniature. The two 4,096-record rows
are the same work at two densities; the rest of this document is about closing the gap between
them.

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

## The linear cost, and what was done about it

Per observer, the pass is O(R) in the number of *live records*, whether those records are on top
of the observer or on the other side of the level. Two measurements make that concrete:

- The same 4,096 records cost 36.4 ms when crammed into a 40 m square and 9.6 ms spread over 400 m,
  because in the spread case almost every record fails the range test.
- That 9.6 ms is the **scan tax**: 18 µs per observer for records that are nowhere near.

Two of the three planned steps shipped with this ADR (the third is scheduler work and stays open):

### 1. The buffer's bounding box

`StimulusBuffer` keeps the axis-aligned box around every live record's *sphere*, updated on `push`
and recomputed on `retain_live`. Min and max are exact operations, so this can never exclude a
record that is visible, it is a conservative prune, not an approximation. An observer outside that
box expanded by its own reach returns an empty list in O(1): in a level where the action is in one
corner, that is most of the crowd, for one comparison each.

### 2. The bucketed index

`StimulusIndex` is a uniform grid over the live records, rebuilt once per perception pass and read
by every observer due that tick.

**The grid.** `origin` is the minimum corner of the buffer's box; `cols`/`rows` are
`ceil(extent / cell) + 1` over x and z; every record sits in the single cell containing its centre.
If the grid would exceed `MAX_CELLS` (2^16) the cell size doubles until it fits, so a level
measured in kilometres cannot allocate a grid measured in metres. Coarsening only ever adds
candidates.

**The layout.** A counting sort into CSR: `starts[i]..starts[i + 1]` is cell `i`'s slice of
`entries`. The fill uses a separate cursor array, writing through `starts` would destroy the
offsets the reader needs, which is exactly the bug the parity test caught on the first attempt.
Entries are filled in ring order, so every cell's slice is in ring order: canonical, reproducible,
and free of hash iteration.

**The candidates.** The window is `observer ± query.max_limit()`, clamped into the grid.

### Why the output is bit-identical to the linear scan

Three facts, and each has a test:

1. **Superset.** A record is perceived only if `dist <= limit`, and `limit = min(query radius,
   record radius) <= query.max_limit()`. Therefore every perceivable record's centre lies inside
   the window, therefore its cell is scanned. Nothing perceivable is skipped. (The window is sized
   from the *query's* maximum reach, not the record's, which is why centre-only insertion is safe
   for a 45 m shout as well as a 3 m footstep, `a_record_with_a_long_radius_is_found_from_a_far_cell`.)
2. **No duplication.** One record lives in one cell, so the visitor sees it at most once and the
   bounded insertion cannot count it twice.
3. **Order irrelevance.** The kept slots are chosen by `ranks`, a total order over exactly the
   fields that appear in `Perceived` (strength, distance, kind, position x, position z, all
   `total_cmp`). The set of candidates determines the answer, so the order they arrive in cannot.
   Two records identical in all five fields are also identical in output, so even a tie that
   straddles `max_results` cannot change the list.

`the_index_agrees_with_the_linear_scan` drives 400 randomised observers against 600 randomised
records and compares both paths element for element, including observers outside the box and
observers with nothing in range; `the_index_agrees_when_records_were_pushed_in_another_order`
publishes the same records in reverse and checks all three lists agree. Predicates are not
duplicated: `consider` is the only place a record is tested, and an index only decides which
records reach it.

### The fallback that keeps it never-slower

When the window contains every live record the index can prune nothing, and measuring showed that
walking copied entries is then 34% *slower* than scanning the buffer. `perceive_indexed` checks
`prunes_nothing` (candidates ≥ live) and delegates to `perceive` in that case. That is what makes
"always call `perceive_indexed`" a safe default: it is exactly `perceive` where there is nothing to
prune, and 2.4× faster where there is.

Deliberately not "does the window cover the grid": the grid is padded by the largest record radius,
so a dense world has empty columns the window need not reach, and the grid-shaped test reported
`false` on precisely the case where the index had nothing to offer. The measurement caught that,
not the reasoning.

### 3. Band-scoped observer sets (open)

Only observers in bands that consume perception should pay, each with its own interval. That is
scheduler integration, not index work, and it is where the 100,000 case is won. It stays in the
plan as `band-scoped-perception`.

### Measured

| Case | Linear | Indexed |
| --- | ---: | ---: |
| 512 observers, 4,096 records, 40 m square | 36.4 ms | 36.4 ms (fallback) |
| 512 observers, 4,096 records, 400 m square | 9.6 ms | 4.0 ms (2.4x) |
| Rebuild of that 4,096-record grid |, | 69 µs per pass |

### Rejected for the index

- **A k-d tree or BVH.** Rebuild cost per tick, and worse constants than a uniform grid for
  point-like records with a handful of radius classes.
- **A hash map keyed by cell.** Allocation per rebuild, and iteration order that would have to be
  sorted back into a canonical order to stay deterministic, paying for the sort twice.
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
