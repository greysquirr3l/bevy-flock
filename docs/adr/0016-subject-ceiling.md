# 0016 — Sixty-four subjects, and a footprint games can budget from

- Status: Accepted
- Date: 2026-11-04
- Amends: ADR 0010 (the flat slot array), ADR 0015 (which documented the cap)
- Relates to: ADR 0013 (subject identifiers in the save format)

## Context

`MAX_SUBJECTS` was sixteen. The number was honest when the store was a sketch of a
feature, and it has been wrong since the first game-shaped requirement arrived: a
player, a party of four, a faction with a captain and three officers, a rival party,
and a pet is nineteen before anybody has met anyone twice.

The cap did not announce itself. It is a clamp inside `MemoryStore::new`, so a game
asking for twenty-four subjects gets sixteen and a store that silently refuses the
eighth relationship — `remember` returns `false`, which a caller is free to ignore.
ADR 0015's diagnostic found it the only way such a thing is ever found: by simulating
a crowd, seeing that half the encounters vanished, and tracing the loss back to a
constant. A ceiling that rejects data quietly is worse than one that refuses to
compile.

Two facts decide how large the number should be.

**It is a ceiling, not a stride.** `MemoryStore::new(capacity, subjects)` sizes the
slot array to the subjects *it was asked for*. A game using four pays for four
whether the constant is sixteen or sixty-four. Raising the ceiling costs nothing to
anybody who does not use it; it only stops lying to those who do.

**What a game asks for, it pays for once.** The slot array is `capacity × subjects ×
16` bytes, allocated at construction. There is no growth and no reallocation, which
is what makes `recall` a pointer computation rather than a lookup.

## Decision

**Raise the ceiling to sixty-four, publish the footprint, and measure what the stride
costs.**

### The footprint, measured

`MemoryStore::footprint()` reports what a store actually holds. A test pins the model
`rows × (subjects × 16 + 8)` against it, with the 8 bytes being row bookkeeping that
does not scale with subjects:

| Subjects | 10,000 rows | 60,000 rows | Row stride |
| --- | --- | --- | --- |
| 1 | 0.23 MiB | 1.4 MiB | 16 B (quarter of a line) |
| 4 | 0.69 MiB | 4.1 MiB | 64 B (1 line) |
| 16 (before) | 2.52 MiB | 15.1 MiB | 256 B (4 lines) |
| 32 | 4.96 MiB | 29.8 MiB | 512 B (8 lines) |
| 64 (after) | 9.84 MiB | 59.0 MiB | 1024 B (16 lines) |

The quadrupling is real and it is the whole cost. Sixty-four subjects at 60,000 rows
is 59 MiB — the size of a modest texture — and it buys a cast list long enough that a
game stops designing around the store. Sixteen subjects at the same capacity was 15
MiB, so the change costs 44 MiB *for a game that asks for all of it* and nothing for
one that does not.

### What the stride costs at runtime, measured

`benches/memory.rs`, `memory_stride`, 4,096 rows:

| Case | 16 subjects | 64 subjects | |
| --- | --- | --- | --- |
| sweep, rows holding nothing | 5.0 µs | 4.9 µs | unchanged |
| sweep, one relationship per row | 71.1 µs | 232.5 µs | 3.3× |

The first row is the surprise and the reason this was measured rather than assumed: a
sweep short-circuits rows that hold nothing, so the population a crowded store mostly
consists of — NPCs who have met nobody — costs the same at sixty-four subjects as at
sixteen. The stride is only walked for rows that hold something, and there the cost is
the 3.3× the cache arithmetic predicts (not 4×, because the per-row overhead does not
scale).

At 4,096 populated rows, 232 µs is affordable on a sweep cadence and would not be
affordable per frame. That was already true at 71 µs; the sweep has always belonged on
a timer.

### Why sixty-four rather than more

- **A row still fits in sixteen cache lines.** At 64 subjects a row is 1 KiB, which a
  sequential scan streams comfortably. At 256 it is 4 KiB and the scan becomes a page
  walk per NPC.
- **`SubjectId` is a `u16` throughout**, including in the save format's registry and
  records (ADR 0013). Sixty-four keeps the identifier dense and small; a thousand
  would invite `u32` and double the save size of every relationship.
- **It covers the design cases named in the issue**: a player, a party, a faction, a
  rival faction, a handful of recurring NPCs, and room for the ones a designer adds
  later.
- **Beyond sixty-four, a second store is the better answer**, because at that point
  the relationships are no longer "who this NPC knows" but "what this NPC knows about
  a region", and those have different access patterns.

### What does not change

The save format is untouched: subject identifiers were already `u16` and the registry
count was already `u16`. A version 2 file written by a sixteen-subject build loads in
a sixty-four-subject build with no translation, because identifiers and keys are
independent of the ceiling.

Existing behaviour is untouched for any game that passes a subject count of sixteen or
fewer: the store sizes to the request, so its memory, its stride and its sweep cost are
exactly what they were.

## Consequences

- **A game can now hold a cast list worth having** without designing around a store
  that would silently drop the seventeenth relationship.
- **The silent-drop failure mode still exists** for counts above the ceiling, so the
  clamp stays and the doc comment now states the number and the reason. A future
  `ConfigError::TooManySubjects` would be better than a clamp; it is deliberately not
  in this change, because it would break callers who pass a large number precisely to
  mean "the maximum".
- **Sixty-four subjects is now the documented worst case for memory**, and
  `docs/performance.md` carries the table so nobody discovers 59 MiB in a profiler.
- **The sweep is more expensive where it matters and free where it does not**, which
  is the argument for keeping it off the frame path rather than an argument against
  the ceiling.
- **ADR 0015's example now simulates sixty-four subjects**, which changes its numbers;
  the tool sized itself from `MAX_SUBJECTS` precisely so it could not go stale.

## Alternatives rejected

- **Keep sixteen and let games run two stores.** Splits the capacity budget, doubles
  the sweep, and makes "who does this NPC remember" a question with two answers.
- **A sparse map per row.** Memory becomes proportional to relationships rather than
  to the ceiling, which is attractive at 1% occupancy — and `recall`, the hot path
  called per think, becomes a hash lookup with an unpredictable cache footprint. The
  flat array's whole justification is that recall is `base + subject × 16`.
- **Columnar storage: one array per subject.** Shrinks memory for unused subjects and
  turns a row scan into a strided gather across up to sixty-four arrays. The sweep
  would win; recall would lose. Recall is the hot path.
- **Grow the array on demand.** Reallocating a 59 MiB array mid-frame is a hitch
  nobody can budget for, and the store's contract — no allocation after construction —
  is what makes it usable inside a frame at all.
- **Make the ceiling a const generic parameter.** `MemoryStore<64>` would push the
  number into every type signature in the public API to save a clamp, and a game that
  changed its mind would have three types instead of one.
- **Raise it to 256 "for safety".** A row of 4 KiB and a save format that wants
  `u32` identifiers, in exchange for headroom nobody asked for.
