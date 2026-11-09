# ADR 0026 — Cell-major crowd steering

**Status:** Accepted (2026-11-04)
**Supersedes:** the per-agent steering loop in `src/boids.rs` (`steer_flock` as it stood at
v0.1.0-rc.1)
**Related:** ADR 0003 (spatial grid), ADR 0023 (SIMD steering for the band), ADR 0024 (layout
ordering and parity)

## The problem

`flock_frame/10000` — the crowd path a game gets from `FlockPlugin` — measured **18.2 ms**, which
is 55 fps against a 16.6 ms budget. The plan has carried that as a documented miss rather than a
claim, and ADR 0018 pointed at the band path as the 60 Hz route. That is a poor answer for the
common case: ten thousand NPCs that all steer is the simplest thing a customer will try.

A breakdown of the frame (10,000 agents, `CityAmbient` preset, one `update` per frame) said where
it went:

| System | Cost |
| --- | --- |
| `rebuild_spatial_grid` | 1.43 ms |
| `steer_flock` | **17.01 ms** |
| `integrate_flock` | 0.07 ms |

Steering was 92% of the frame, at 1.7 µs per agent, for a neighbourhood that holds ~78 agents at
the default spacing of 1.6 m. Three things in that loop were paying for structure rather than
arithmetic:

1. `SpatialGrid::radius` sorts every neighbour list by distance (`total_cmp` on `f32`, tie-broken
   by id) before returning it. At ~78 neighbours that is a sort per agent per frame.
2. The model walks the buffer **three times** — separation, alignment, cohesion — because each
   behaviour is its own function. Same neighbours, three passes.
3. Each neighbour is copied into a `Vec<Neighbour>` (24 bytes) that is cleared and refilled per
   agent, so the frame streams roughly `78 × 24 B × 10,000 ≈ 19 MB` through a scratch buffer for
   no reason.

## The decision

Steer the crowd the way ADR 0023 steers the band: **agents as arrays, in grid-cell order, with
one pass per agent over the cells in range.**

`src/application/crowd.rs` holds a `Crowd` that is gathered from the ECS once per frame:

- positions, velocities, targets and flags in parallel `Vec`s;
- a counting sort of agent indices into **cell-major order** (an agent's bucket comes from the
  same `SpatialGrid` arithmetic, so the neighbour set is identical to the grid's);
- the same `SpatialGrid` geometry — cell size, dimensions and coarsening — so the two structures
  cannot disagree about who is next to whom.

Then one `scan` per agent accumulates **separation, alignment, cohesion and the neighbour count in
a single pass over the cells in range**, and `desired_for` applies the model to those sums. The
model is not re-derived: it is `desired_velocity`'s arithmetic term for term — priority-weighted
truncation via the existing `accumulate`, the arrival curve, the stop radius, the jitter floor,
the speed clamp.

`steer_flock` becomes gather → `Crowd::steer` → scatter into `NpcDesiredVelocity`. The per-agent
reference implementation survives as `steer_flock_reference`, out of the schedule, because the
parity tests and any caller that wants to inspect neighbours still need it.

## What this buys, and what it costs

| 10,000 agents | Before | After |
| --- | --- | --- |
| `steer_flock` | 17.01 ms | 6.89 ms |
| frame (`flock_frame/10000`) | 18.2 ms | see `docs/dev/tuning/performance.md` |

Measured on the reference host (6 vCPU Neoverse-N1). The win is not the arithmetic: it is the
sort, the second and third passes, and the neighbour copies that stopped happening.

The costs, stated plainly:

- **A second representation of the crowd.** The ECS holds entities; the kernel holds arrays. They
  are reconciled once per frame by the gather and the scatter, which is `O(n)` copies of 16 bytes
  each — an order of magnitude cheaper than the buffer traffic it replaces, but it is not free and
  it is the price of any data-oriented pass under an archetype ECS.
- **The kernel needs the grid's geometry, not its contents.** `configure_from_grid` reads cell
  size, dimensions and the origin from `FlockSpatialGrid`; a caller that builds a grid with a
  different configuration between `Perception` and `Steering` would steer against stale geometry.
  The `FlockSet` ordering makes that a bug in the caller's schedule, not a hazard in the library.
- **A planar assumption.** The crowd lives on the plane `y = 0`, so the vertical cell is computed
  once from the plane height rather than per agent. A game that spreads agents through the volume
  must pass its plane height to `configure_from_grid_on_plane`; otherwise agents in different
  vertical cells are bucketed together, which costs extra candidates (and never misses one,
  because the horizontal window is unchanged and the distance test is still exact).

## One deliberate behaviour change

`SpatialGrid::radius` returns the querying agent **as its own nearest neighbour** at distance
zero. The old loop averaged that self entry into the alignment and cohesion sums, so both were
divided by `n + 1` instead of `n`: a bias toward less cohesion in sparse crowds that vanished in
dense ones. The kernel excludes the agent, which is what the equations always meant.

This is a behaviour change, so it is pinned rather than described:
`the_kernel_does_not_average_an_agent_into_its_own_cohesion` measures the old path's bias and
asserts the kernel matches the self-excluded model. Games that tuned against the old numbers may
see slightly tighter crowds at low density; that is the bug leaving, not a regression.

## Determinism

Unchanged, and stronger than before: the order of the blocks is a property of the cell ids (the
occupancy list is sorted before it is walked), the sort that made the old path reproducible is
gone because nothing iterates in arrival order any more, and the kernel performs no RNG and no
wall-clock reads. `the_same_world_steers_the_same_way_twice` asserts two independent crowds
produce identical output, and `docs/dev/adr/0002` still governs the seeds.

## Tests

- `cells_agree_with_the_spatial_grid` — 500 random points, kernel cell ids against
  `SpatialGrid::cell_of`, including the coarsened grid.
- `the_crowd_agrees_with_the_domain_model` — 400-agent spiral, every agent's desired velocity
  against `desired_velocity` within 1e-3, with targets on a third of them.
- `the_kernel_does_not_average_an_agent_into_its_own_cohesion` — the behaviour change above.
- `ambient_agents_are_neighbours_but_not_steered` — the LOD contract survives the rewrite.
- `a_lone_agent_is_isolated_and_does_not_drift` — the degenerate case.
- `tests/crowd_no_alloc.rs` — a steady 1,000-agent frame allocates **zero** bytes, measured by a
  counting allocator.

## What was not done

The four-lane `wide` path from ADR 0023 was prototyped for this kernel and **not shipped**. With
the sort, the extra passes and the copies gone, the scalar cell-major loop lands comfortably
inside the frame budget, and a four-lane version has to reproduce the sums in the same order to
stay parity-clean — a sharp edge for a win that is no longer needed to make the target. The band
still uses its own SIMD kernel where the numbers justified it. If the crowd needs another 2×, the
place to start is the `Pack`-style block scan in `src/application/packed.rs`, which is the same
layout with lanes already written.
