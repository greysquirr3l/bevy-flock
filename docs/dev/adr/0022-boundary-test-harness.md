# ADR 0022: The boundary test harness

- Status: Accepted
- Date: 2026-11-04
- Relates to: ADR 0018 (continuum), ADR 0019 (anchoring), ADR 0020 (band plugin), ADR 0021

## Context

Five gates remain open, and they share one shape: they are about what happens *at the
boundary* over time, not about what any single function returns. Parity, pop-in, anchoring
at distance, allocation and fold completeness cannot be unit-tested honestly, because each
is a property of a running world:

- **Parity (4)**, the continuum's aggregate must agree with full simulation.
- **Pop-in (5)**, a camera crossing the edge at speed must not spend a frame spawning.
- **Anchoring (6)**, an anchored NPC at distance must still be an entity after 10,000 ticks.
- **No allocation (7)**, a steady-state band frame must not touch the allocator.
- **Fold completeness (9)**, a folded NPC must match no query.

Each is also the kind of invariant that fails quietly: a leak shows up as a stutter three
minutes in, a popping boundary as a visual artefact nobody files, an allocation as a
frame spike under load. A harness that drives time is the only thing that catches them.

## Decision

One harness, `BandHarness`, driving a headless `App` through a scripted camera path with
four instruments attached. It lives in `tests/band_continuity.rs` rather than in the
library, because it is a test rig and not a feature.

### The camera

A sine path across the band edge rather than a straight line: `x = centre + A·sin(ωt)`.
A straight line leaves the field, after which nothing folds because nothing is in a cell , 
a test that measures the absence of the thing it is testing. Oscillation crosses the
manifest and fold radii repeatedly, which is where the defects are.

Default: amplitude equal to `fold_radius`, period 600 ticks, 10,000 ticks per run.

### The instruments

| Instrument | Reads | Asserts |
| --- | --- | --- |
| conservation | `field.population() + live agents` every tick | equals the seeded total, exactly |
| pop-in | per-tick change in live agents | ≤ `ration`, both directions |
| anchoring | existence of a `BandAnchor` at 10 km | still present at tick 10,000 |
| allocation | a counting allocator around `app.update()`, counting the thread the frame runs on | zero allocations after warm-up |
| fold completeness | the agent query after a fold | the entity is gone; the cell count rose by one |

### The three rules that make it trustworthy

1. **Warm up before counting.** The first frames spawn, size buffers and populate caches.
   Measuring from tick zero measures startup; every instrument starts after the warm-up
   window and says so.
2. **Assert conservation every tick, not at the end.** A population that drifts by one and
   drifts back is a real bug, and an end-of-run assertion hides it.
3. **Prove the instrument before reading it.** A counter that never engaged reports zero for
   everything, so the harness counts a deliberate allocation before it trusts a reading. The
   counter counts one thread, because the test harness runs these gates on their own
   threads and a process-wide counter would read their allocations as part of the frame
   under test; the harness also records the thread the frame ran on, so a schedule that
   moved the systems elsewhere fails the gate rather than passing it vacuously.

## Consequences

The harness is the first thing in this repository that tests *time*: 10,000 ticks is
166 seconds of simulation, and every gate listed above is an assertion somewhere inside it.
It is deliberately headless and single-threaded, so a failure is reproducible rather than
intermittent.

What it cannot prove is visual quality, that a manifested NPC looks like it was always
there. That is the one part of pop-in that stays a human judgement, and this record says so
rather than pretending a counter covers it.

The allocation instrument had a platform-dependent reading once: a process-wide counter
counted whatever the other four gates happened to be doing, so the same test passed on one
platform and failed on another. That is a property of the test harness, not of the band,
and the counter now counts one thread.
