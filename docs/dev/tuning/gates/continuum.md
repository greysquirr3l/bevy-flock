# Ambient continuum: gate status

Measured 2026-11-04 by `cargo bench --bench continuum`. Every number below is from this
machine, and every gate the code cannot yet attempt is marked open rather than assumed.

| # | Gate | Target | Measured | Status |
| --- | --- | --- | --- | --- |
| 1 | 1M ambient, advection stride | < 0.5 ms | **0.315 ms** | **PASS** |
| 1 | Manifest 2,000 identities |, | 10.5 µs | **PASS** |
| 2 | Conservation over 10,000 strides | exact | exact | **PASS** (unit test) |
| 3 | Determinism of manifest ids | exact | exact | **PASS** (unit test) |
| 4 | Parity vs full simulation | 1% cells, 5% velocity | passes | **PASS** | `tests/band_continuity.rs` |
| 5 | No popping at the boundary | < 2% count delta | passes | **PASS** | `tests/band_continuity.rs` |
| 6 | Anchoring at 10 km | entity after 10k ticks | passes | **PASS** | `tests/band_continuity.rs` |
| 7 | No allocation after construction | zero | zero | **PASS** | `tests/band_continuity.rs`, `tests/tree_no_alloc.rs` |
| 8 | 100k with 5,000 manifested | < 8 ms | **3.37 ms** (band plugin) | **PASS** |
| 9 | Folded NPC matches no query | zero matches | passes | **PASS** | `tests/band_continuity.rs` |

## Gate 8 fails, and the continuum is not the reason

The measurement is a 5,000-entity manifest band, steered and partitioned exactly as today,
with advection paid on its stride:

| Band size | Frame |
| --- | --- |
| 1,000 manifested | 2.65 ms |
| 5,000 manifested | **9.47 ms** |

Advection contributes 0.315/8 ≈ 0.04 ms amortised. The other 9.4 ms is the crowd path on
5,000 entities, about **1.9 ms per 1,000 steered NPCs**, which is consistent with the
earlier `flock_frame` numbers (1,000 → 1.0 ms, 10,000 → 18.4 ms).

So the 5,000-manifested target cannot be met by folding harder. Folding 95,000 agents out
of the ECS is necessary and is not sufficient: the band that remains already exceeds the
budget on its own. Two ways forward, in order of preference:

1. **A smaller band.** 2,000 manifested is 3.8 ms and meets the gate with margin. The
   manifest band is a design parameter (ADR 0018); 2,000 is a legitimate value, and it
   supersedes the round number in the gate.
2. **A packed steering batch.** The same structure-of-arrays trick already planned for the
   reflex tier, applied to steering: contiguous positions/velocities, no change detection,
   no per-entity component fetch. At ~25 ns per neighbour query this is what would make
   5,000 steerable in under 2 ms.

Until one of those lands, gate 8 is a fail, and the honest headline is that **the crowd
path, not the scheduler and not the continuum, is the binding constraint at every scale
this library has measured.**

## What is real today

`domain::continuum` ships: twelve-byte cells, exact integer conservation, deterministic
manifestation, O(1) folding, alarm that decays by stride, and a flow-field seam the
navigation task will fill. Eleven unit tests cover it, including 10,000 strides of
wandering-wind advection that conserve a population to the person.

## Update: the packed steering kernel (2026-11-04)

`application::packed` steers, integrates and advances over contiguous arrays:

| Agents | Steer + integrate + advance |
| --- | --- |
| 1,000 | 249 µs |
| 5,000 | **1.50 ms** |
| 50,000 | 137 ms |

Against 9.47 ms for the same 5,000 through the ECS crowd path, the arithmetic is no longer
the constraint: replacing the steering half leaves ample room under 8 ms.

The honesty is in the density column:

| 5,000 agents in | Frame |
| --- | --- |
| 512 m square | 1.42 ms |
| 256 m square | 3.65 ms |
| 128 m square | 10.92 ms |

The kernel is bound by neighbour count, not agent count. A dense band destroys the saving,
which is why ADR 0020 derives the manifest radius from a target density instead of fixing a
count. Gate 8 stays **OPEN** until a band plugin produces a measured frame; the mechanism
below it is now measured.

## Gate 8 closed (2026-11-04)

The band plugin (ADR 0020) is wired: 100,000 inhabitants with 5,000 manifested measure
**3.37 ms**, and 250,000 measure **3.37 ms**, the frame is flat in total population because
it depends on the band, not the world. ADR 0021 takes this past 250,000 with concentric
bands at independent cadence.

## ADR 0021 gate (2026-11-04)

1,000,000 inhabitants, 5,000 near, 15,000 mid, 980,000 as density, measure **5.02 ms**,
under the 6 ms gate, with the near band's agent count unchanged from the 100,000 case.

Two defects surfaced on the way, both invisible without the number:

1. **The mid pack inherited the near band's grid extent** (512 m). Mid agents live from
   400 m outward, so every one of them fell outside the grid and clamped into its edge
   cells, becoming a neighbour of every other mid agent. The frame was 7.6 ms for that
   reason, not for the cadence. `BandConfig::mid_extent` fixes it.
2. **The fixture seeded 15,000 mid agents into 300-800 m**, roughly double the specified
   density. A bench fixture that is denser than the design is a bench that measures the
   fixture.

The table above is the post-harness state: gates 4, 5, 6, 7 and 9 were closed by
`tests/band_continuity.rs`, two of them after the harness found the defects described below.

## The gate harness closed four gates and found two bugs (2026-11-04)

`tests/band_continuity.rs` drives a camera on a sine path across the band edge for 10,000
ticks with five instruments. Gates 4 (parity/conservation), 5 (pop-in), 6 (anchoring) and 7
(allocation rate) pass; gate 9 (fold completeness) passes in isolation.

Two real defects fell out, neither of which any unit test could have found:

1. **Anchors were steered.** `band_steer` queried every `BandAgent`, anchor included, and
   the packed grid clamps a position outside its extent into the edge cell. Gate 6 watched
   an anchor at x = 10,256 become x = 16 in a single tick, a story character teleported
   ten kilometres by the crowd system. Anchors are now excluded from both steering passes.
2. **The field could not cover its own fold radius.** With `extent` 512 and a fold radius of
   290 m, no cell existed beyond 256 m from centre, so an agent that walked out had nowhere
   to fold into and stayed an entity forever. `extent` is now 1,024.

## The churn finding, and its fix (2026-11-04, ADR 0025)

The harness left one finding open: with the default configuration the band manifested 256
agents a tick and folded 256 the next. Population was conserved and every gate passed, which is
exactly why it survived five gate reviews, the *work* was cancelling, not the population. Gate
9 sidestepped it by folding with `target: 0`.

Reproduced and root-caused, it was three defects:

1. **The manifest window was square.** Chebyshev rings reach `radius * sqrt(2)` = 1.41 radius
   into the corners, against a fold bound of 1.15 radius: 1,548 of the 16,641 window cells at
   the default geometry were past the fold radius, so anyone manifested from them was born
   outside the band and folded next tick.
2. **`seed_field` filled the field from the origin** and stopped when the count ran out,
   putting 40,000 people into the first ~150 rows. A camera at the map centre stood in empty
   country, so the square window reached further out for anyone it could find.
3. **The spawn point was never checked against the fold bound**, so `fold_slack: 1.0` would
   still oscillate at the ration.

All three are fixed: the window is a disc bounded by the manifest radius (and the fold radius),
the seeding utility distributes uniformly by Bresenham share, and the spawn uses the vetted
point. `tests/band_churn.rs` pins the behaviour.

| | Before | After |
| --- | --- | --- |
| Live agents, tick 11 | 256 | 3,072 |
| Live agents at target | never reached | 5,000 at tick 20 |
| Folds per tick after convergence | 256 | 0 |
| Manifestations per tick after convergence | 256 | 0 |
| Live agents outside the fold radius | 256 | 0 |

Gate 9 now runs against a full band with a still camera instead of the `target: 0` sidestep, and
the 1,000,000-inhabitant frame is unchanged at 3.80 ms. The harness re-ran gates 4, 5, 6 and 7
after the change: all pass.
