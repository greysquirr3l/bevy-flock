# 0046, Obstacle-driven navigation: rebuild in slices, publish once

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** ADR 0040 (optional integrations are ports), ADR 0009 (virtual cost), ADR 0048 (links)

## Context

`NavSource` is a port: a game supplies the walkability data, the flow-field cache keys on
`NavVersion`, and a stale field is dropped rather than walked through a wall. What was missing is
the obvious source for a level that *changes*: a crate pushed into a corridor, a door closing, a
bridge collapsing. Until now the game had to notice the change, re-stamp its own bitmap by hand,
and remember to bump the version. Forgetting is a silent bug, the crowd keeps walking through
the crate.

The competitive research flagged this as a gap against Unreal's navmesh rebuild-on-demand and
Unity's NavMesh carving.

## Decision

`domain::obstacle_nav::ObstacleNav` derives a walkability grid from axis-aligned obstacle
footprints, staging the rebuild a slice at a time and publishing atomically.

### Input

`ObstacleFootprint` is a world-space AABB; only `x` and `z` are used, because the nav grid is a
floor plan. `ObstacleFootprint::new` normalises the corners and rejects footprints with no area,
so a zero-sized prop cannot block a cell by accident.

### Staging

`request(&[ObstacleFootprint])` sorts and dedups the set into a canonical order, compares it with
the previous set, and starts a rebuild only if it changed. Two consequences follow, and both are
tested:

- A query whose iteration order changes frame to frame does not restart the rebuild every frame.
- A rebuild already in flight is abandoned when the set changes again: the newer world wins, and
  at most one tick of work is thrown away.

`step()` fills at most `cells_per_tick` cells (4,096 by default, a 64×64 block a frame),
**a whole row at a time**. A half-filled row is a gap that exists for exactly one frame, and a
path through a wall that exists for one frame is still a path an agent can take. When a single
row is wider than the whole budget, one row is granted anyway, or a narrow budget would never
make progress.

### Publication

The staged bitmap is never visible. `source()` returns the last complete `GridNavSource`;
`version()` increments exactly once per commit, on the step that finishes the rebuild. A
flow-field cache keyed on the version therefore sees either the old world or the new one, never a
mixture. The test `a_staged_rebuild_never_exposes_a_half_built_grid` drives a 1-cell budget and
asserts the old source stays readable and unchanged for the whole staging window.

### Budget

The budget is expressed in **cells**, not nanoseconds. A cell of rasterisation is a fixed amount
of work, so cells-per-tick is a deterministic virtual-time budget: the same obstacle set costs
the same number of ticks in a replay as in the original run, which a wall-clock budget cannot
promise. `erode_cells` grows the clearance around every obstacle for agent radius, computed on
the bitmap rather than through `GridNavSource::eroded`, so the published version is exactly the
one the caller configured.

### ECS surface

`ObstacleNavPlugin` plus a `NavObstacle` component on anything that blocks walking. The system
runs in `FlockSet::Perception`, so a version published this tick is visible to steering in the
same frame. It is opt-in: a game with a baked level keeps its own source and never pays for the
resource or the system.

## Measurements

`cargo bench --bench crowd -- perception`, one full publish of a 64×64 grid with 32 obstacles:
**3.7 µs**. A rebuild is therefore invisible next to a steering pass, and the budget exists for the
pathological case (a 512×512 grid, or an obstacle list that changes every frame) rather than for
the common one. The per-tick cost is bounded by `cells_per_tick` writes plus one sort of the
obstacle list; `ObstacleNav::finish` is a test helper, not a production path.

## Consequences

- A game can spawn and despawn obstacle entities and let navigation follow, without a hitch and
  without remembering to bump a version.
- Rebuild work is bounded per frame, so an obstacle-heavy level degrades in latency (more ticks
  to publish) rather than in frame time.
- The manual `NavSource` path is untouched: `GridNavSource`, the flow solver and the cache do not
  know this module exists.

## Alternatives rejected

- **Rebuild eagerly on change detection.** A crate pushed every frame would restart the rebuild
  forever and never publish. Comparing the set is cheaper and gives staging a chance to finish.
- **Rebuild across frames without atomic publication.** Cheaper to write, and it ships a crowd
  that walks through a wall for the duration of the rebuild.
- **A polygon navmesh rebuild.** Re-triangulation cannot be sliced into "ten cells a tick"
  without publishing a partly-old mesh; `domain::navmesh` remains available for games that want
  polygons and can afford an atomic rebuild of their own.
- **A wall-clock budget.** Breaks replay determinism, which is the crate's first principle.
