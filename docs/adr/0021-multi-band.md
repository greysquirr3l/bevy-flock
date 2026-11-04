# ADR 0021: Multi-band scaling past 250,000 active entities

- Status: Proposed
- Date: 2026-11-04
- Relates to: ADR 0017, ADR 0018, ADR 0020

## Context

The band plugin meets gate 8: **3.37 ms** for 100,000 inhabitants with 5,000 manifested,
and **3.37 ms** for 250,000 — flat, because the frame now depends on the band, not the world
(`benches/band.rs`).

Two ceilings appear past that. **Neighbours**: the packed kernel is bound by neighbours, not
agents (10.92 ms for 5,000 in a 128 m square). **Radius**: `radius = sqrt(target / (density
* PI))` grows with the square root of the target, so 250,000 manifested at the default
density needs a 1788 m radius, and the cell grid must cover it — 800,000 cells at 4 m, or
9.6 MB, advected every stride.

Raising density instead returns the neighbour problem. Neither knob alone scales.

## Decision

Population is partitioned into **concentric bands with independent fidelity**, not one band
with a bigger radius:

| Band | Radius (default) | Steering | Cadence | Population |
| --- | --- | --- | --- | --- |
| Near | 252 m | packed kernel, every frame | 60 Hz | 5,000 |
| Mid | 800 m | packed kernel, every 3rd frame | 20 Hz | 15,000 |
| Far | beyond mid | continuum cells | advected every 8th tick | unbounded |

Three rules make this work rather than merely exist:

1. **Cadence replaces count.** A mid-band agent steered every third frame costs a third as
   much and is indistinguishable at 800 m. Fidelity is a rate, not a population.
2. **Radius stays fixed per band.** Each band keeps its density target, so raising the
   population grows the *far* band's cells rather than the near band's neighbour count. The
   near band never exceeds 5,000 agents whatever the world holds.
3. **Bands hand off with hysteresis, twice.** Near→mid at 252 m × 1.15, mid→far at 800 m ×
   1.15, each with its own ration. A single boundary would just move the oscillation.

Budget: near 2 ms, mid 1 ms, far 0.5 ms, total 3.5 ms for a million inhabitants.

## The gate

1,000,000 inhabitants, 5,000 near, 15,000 mid, rest far: frame under 6 ms, and the near
band's agent count unchanged from the 100,000 case — the flatness must survive the
partition, or the bands have only moved the cliff.

## Rejected

- **One band with a huge radius.** 250k needs 1788 m; the grid alone is 9.6 MB and every
  agent still wants neighbours.
- **Denser near band.** Returns 10.92 ms per 5,000 exactly as measured.
- **Thin agents uniformly (a global stride).** Makes the player's neighbours stutter, which
  is the one place fidelity is visible.
- **More than three bands.** Each boundary is hysteresis plus a ration plus a handoff bug;
  three is where the cost curve stops paying for them.

## Consequences

Population becomes unbounded in the only sense that matters — a frame that does not grow —
while individual presence stays capped at 20,000. The far band is density, so nothing there
can be asked a question, which is ADR 0018's trade applied twice.
