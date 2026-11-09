---
title: Concepts
description: The four ideas the crate is built on — the tick, the grid, the tiers, and the bands.
---

Four ideas explain almost all of the crate's behaviour. Read them once and the configuration makes sense.

## The tick

`SimClock` counts ticks, not seconds. Everything in the simulation is a function of the tick number, so a run is
reproducible from `(seed, feature set, platform)` alone — no wall clock, no thread scheduling, no float non-determinism
from `std`'s transcendentals (the crate uses `libm` for that reason).

This is the property that makes saves, replays and CI assertions possible. It also constrains you: anything you add to
the simulation that reads the clock or the OS breaks it, and the crate cannot stop you.

## The grid

`SpatialGrid` is a uniform grid over the simulated volume. Each rebuild sorts bodies into cells, then each NPC's
neighbour queries look at a 27-cell neighbourhood instead of the population — the difference between O(n) and O(n²).

The grid coarsens itself when the configured cell size would need more than `max_cells` slots, so a large world costs
larger cells rather than more memory. `SpatialGridConfig` is where you set the volume, the target cell size and the
budget.

Two properties worth knowing:

- **Rebuild cost is a per-frame decision.** A static crowd rebuilds rarely; a moving one rebuilds every frame or every
  N frames. The [performance page](/bevy-flock/reference/performance/) has the numbers for both.
- **Parallel and serial builds are byte-identical.** Worker count changes speed, never output. That is tested, not
  asserted.

## The tiers

Thinking every NPC every tick is how crowd engines die. `bevy-flock` assigns each NPC a tier by its distance to the
nearest player:

| tier | what runs | roughly |
| --- | --- | --- |
| reflex | steering only | far from everyone |
| ambient | steering plus a cheap decision | the middle distance |
| cognition | full behaviour tree, memory, dialogue | near the player |

The budget is per-tick virtual cost, not a headcount, so expensive archetypes cost more of it than cheap ones. You
control the budget; the scheduler decides who thinks. `Archetype` decides what a think costs, which is why an urban
civilian and a guard can share a budget honestly.

## The bands

Above roughly 250,000 inhabitants, individual NPCs stop existing at distance and become a *field*: a density and velocity
continuum that is cheap to advance and indistinguishable at range. As the camera approaches, the field manifests
individuals; as it leaves, they fold back in.

That is the "a million when you need them" claim. The guarantees that matter — no pop-in at the boundary, anchors never
fold, the population is conserved exactly — are what the continuity test suite drives a camera through for 10,000 ticks.

## Where the tuning lives

The ADRs, the performance journals, the gate ledgers and the benchmark harnesses are in the repository under
`docs/dev/`, not on this site. They are written for whoever changes the engine, not for whoever uses it. Start at
[Where the internals live](/bevy-flock/reference/internals/) if you want the map.
