---
title: Scaling to a million
description: Which knob matters at which population, and where the tiers and bands start carrying the load.
---

The crate scales in three regimes, and the right knob changes with each. Picking the wrong one is how a crowd engine
becomes a physics demo.

## Up to ~10,000: steering dominates

Rebuild the spatial grid every frame, keep cognition on a modest budget, and the frame belongs to steering. This is the
regime where the wide (SIMD) kernel earns its keep; it is on by default, and it is worth roughly 19–31% of a steering
pass at 100k agents depending on cell size.

If your crowd is much smaller than 10,000, do not enable the wide kernel expecting the headline number, its advantage
scales with density.

## 10,000 → 250,000: the budget is the knob

At this size the question stops being "how fast is steering" and becomes "how many NPCs think this tick". The scheduler
spends a virtual-cost budget per tick across candidates chosen by tier and distance. Raise the budget for a smarter
crowd at a higher cost; lower it for a bigger one at the same cost.

Practical advice: measure at your real population, not at 100k, and watch the *variance* rather than the mean, a budget
that fits on average will still spike.

## Above 250,000: the bands

Past a few hundred thousand, individual NPCs at distance stop existing. The continuum advances a density and velocity
field cheaply, and manifests individuals only where the camera can see them; anchors you pin are always manifested and
never folded.

The guarantees here are the ones that matter: population is conserved exactly, nothing pops at the boundary, and a far
field cannot leak into the near field. They are tested by driving a camera through band transitions for 10,000 ticks and
asserting the invariants every tick.

## The honest ceiling

On the reference machine, a 1,000,000-inhabitant band frame costs **3.78 ms** with the wide kernel and **4.75 ms**
scalar, against a 6 ms gate. A 1M-body spatial rebuild costs **4.572 ms**. Those numbers come with their fixture and
machine in [Performance](/bevy-flock/reference/performance/), treat them as shape and ratios, then measure your own.

## Where to look

- `examples/horde_100k.rs`, `examples/ambient_metropolis.rs`, the two regimes, runnable
- `examples/cross_bench.rs`, the harness used for cross-engine comparison
- `docs/dev/tuning/performance.md`, every measurement, including the optimisations that were tried and rejected
