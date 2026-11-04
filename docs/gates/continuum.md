# Ambient continuum: gate status

Measured 2026-11-04 by `cargo bench --bench continuum`. Every number below is from this
machine, and every gate the code cannot yet attempt is marked open rather than assumed.

| # | Gate | Target | Measured | Status |
| --- | --- | --- | --- | --- |
| 1 | 1M ambient, advection stride | < 0.5 ms | **0.315 ms** | **PASS** |
| 1 | Manifest 2,000 identities | — | 10.5 µs | **PASS** |
| 2 | Conservation over 10,000 strides | exact | exact | **PASS** (unit test) |
| 3 | Determinism of manifest ids | exact | exact | **PASS** (unit test) |
| 4 | Parity vs full simulation | 1% cells, 5% velocity | — | **OPEN** (needs the band plugin) |
| 5 | No popping at the boundary | < 2% count delta | — | **OPEN** |
| 6 | Anchoring at 10 km | entity after 10k ticks | — | **OPEN** |
| 7 | No allocation after construction | zero | — | **OPEN** |
| 8 | 100k with 5,000 manifested | < 8 ms | **3.37 ms** (band plugin) | **PASS** |
| 9 | Folded NPC matches no query | zero matches | — | **OPEN** |

## Gate 8 fails, and the continuum is not the reason

The measurement is a 5,000-entity manifest band, steered and partitioned exactly as today,
with advection paid on its stride:

| Band size | Frame |
| --- | --- |
| 1,000 manifested | 2.65 ms |
| 5,000 manifested | **9.47 ms** |

Advection contributes 0.315/8 ≈ 0.04 ms amortised. The other 9.4 ms is the crowd path on
5,000 entities — about **1.9 ms per 1,000 steered NPCs**, which is consistent with the
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
**3.37 ms**, and 250,000 measure **3.37 ms** — the frame is flat in total population because
it depends on the band, not the world. ADR 0021 takes this past 250,000 with concentric
bands at independent cadence.
