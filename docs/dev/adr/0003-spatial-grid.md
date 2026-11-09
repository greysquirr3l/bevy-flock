# ADR 0003, A sorted-array grid, not a dense one and not a spatial crate

- Status: Accepted (supersedes the first version of this ADR)
- Date: 2026-11-04

## Context

Neighbour queries dominate crowd cost. At 10,000 agents an `O(n²)` scan is 100
million distance checks per tick; at 100,000 it is ten billion. Some index is
mandatory.

Three candidates: an existing Bevy spatial plugin, a
`HashMap<(i32,i32,i32), Vec<Entity>>`, or a flat structure the crate owns.

## Decision

Agents are sorted by cell id into one flat `Vec`, with a parallel `Vec` of the
cell id of each entry. A cell's occupants are the binary-search window over that
array. No per-cell storage exists at all.

Rebuild the whole index every tick rather than updating it incrementally.

## The first attempt, and what the benchmark said

The first implementation was a textbook dense uniform grid: a `Vec<u32>` of
prefix sums with one slot per cell, occupied cells packed by counting sort. It is
the design every article describes, and it is right when the volume is small.

`flock_frame/1` measured **37.6 ms**. A single NPC. The default configuration is a
1 km cube with 4 m cells, 15.6 million cells, so every tick allocated 60 MB and
walked 15.6 million prefix-sum entries to sort three agents. At 10,000 agents the
frame was 54 ms, i.e. the agent work was **17 ms** and the volume bookkeeping was
**37 ms**: 68% of the frame spent on cells nobody was standing in.

The fix is not a smaller default volume; it is not storing cells at all. Sorting
`(cell_id, index)` pairs costs `O(n log n)` in agents, memory is `O(n)`, and the
size of the world costs nothing. After the rewrite: `flock_frame/1` **5.4 µs**
(7000× faster) and `flock_frame/10000` **8.1 ms** (6.7×). The library's own test
suite dropped from 100 s to 7.6 s, because 136 tests were each rebuilding 15.6
million cells.

The numbers are in [`docs/dev/tuning/performance.md`](../tuning/performance.md).

## Consequences

- **Determinism.** A `HashMap`'s iteration order depends on hasher state, which
  differs between runs. Sorting by `(cell_id, insertion_index)` is total, so the
  order is identical on every machine and every rebuild.
- **No allocation in steady state.** The sort buffer, the order array and the cell
  array are reused across rebuilds.
- **Rebuild beats update** at crowd density: one sort over positions against
  incremental insert/remove bookkeeping that can go stale. A stale index produces
  the worst class of bug, an NPC invisible to its own flock until something else
  moves.
- **Cell ids are `u32`, so the id space is bounded** at ~4.29 billion cells. A
  10 km cube with 8 m cells fits (1.95 billion); a 100 km cube with 2 m cells does
  not, and `SpatialGrid::new` rejects it rather than silently wrapping. That is a
  documented limit, not a hidden one.
- Positions outside the configured volume are clamped, not dropped, so an NPC that
  walks off the navmesh stays findable by its neighbours.
- Query cost is two binary searches per cell touched, ~27 cells for a radius
  query: measured at 0.52 µs for 1,000 agents and 0.68 µs for 100,000, which is
  the point, query cost tracks local density, not population.

## Alternatives rejected

- **Dense uniform grid (the first version)**, 37 ms of fixed cost per frame.
- **`bevy_spatial` / `kd-tree` crates**, fine for static geometry; rebuilt per
  tick they are slower than a sort, and their iteration order is unspecified.
- **`HashMap` of cells**, the determinism problem above, plus a hash per insert.
- **Incremental update**, saves a sort and costs correctness.
- **`bevy_ecs` `Entity` as the key instead of `NpcId`**, `Entity` indices are
  recycled and depend on spawn order; `NpcId` is the identity the simulation
  actually uses, and it is what a replay can name.
