# Performance

Measured, not estimated. Every number here comes from `cargo bench --bench crowd`
on the machine and settings below, and the two designs that lost are documented
alongside the one that won, because a table of "we are fast" is worth less than a
record of what was tried and rejected.

## How to reproduce

```sh
cargo bench --bench crowd
```

Raw criterion output lives in `target/criterion`. The suite measures:

- `flock_frame/N` — a whole `App::update()`: spawn drain, grid rebuild, steering
  and integration, with N NPCs already alive. This is the number that decides
  whether a game hits its frame budget.
- `flock_frame_large/100000` — the same at 100,000 agents, with fewer samples.
- `spatial_query/radius_8m/N` — one radius query against an index of N agents,
  isolated from everything else.
- `steering_step/32_neighbours` — one steering evaluation with 32 neighbours.

## Test machine

| | |
| --- | --- |
| CPU | ARM64, 8 vCPU (cloud sandbox) |
| Toolchain | Rust 1.97.1, `--release` with debug symbols |
| Build | default release (`codegen-units = 16`, no LTO), debug symbols on |
| Criterion | 10 samples, 1 s warm-up, 2–3 s measurement |
| Renderer | none — headless `App::new()`, simulation only |

Absolute numbers on a desktop x86-64 will differ; the ratios between designs is
what this document is for.

## Final numbers

| Benchmark | Time | Per agent |
| --- | --- | --- |
| `flock_frame/1` | 1.00 ms | — |
| `flock_frame/1000` | 2.47 ms | 2.5 µs |
| `flock_frame/10000` | 18.2 ms | 1.8 µs |
| `flock_frame_large/100000` | 192 ms | 1.9 µs |
| `spatial_query/radius_8m/1000` | 138 ns | — |
| `spatial_query/radius_8m/10000` | 185 ns | — |
| `spatial_query/radius_8m/100000` | 552 ns | — |
| `steering_step/32_neighbours` | 152 ns | — |

Two things to read from that table:

- **Per-agent cost is flat.** 1.8–2.5 µs from one agent to a hundred thousand,
  because the index makes each agent's work local.
- **Query cost tracks density, not population.** 138 ns at 1,000 agents and 552 ns
  at 100,000: the index walks the same 27 cells, and the cells are fuller.

The 1.00 ms at N=1 is the grid rebuild's fixed cost at the default 1 km volume
(405,000 cells after coarsening). It is the price of dense cells, which the query
numbers above justify. If your playable area is 200 m across, set the volume to
match and the fixed cost drops to ~20 µs.

## The design that lost: 15.6 million cells

The first index was a textbook dense grid sized from the configured volume: a
1 km cube with 4 m cells is 15.6 million cells. It allocated 60 MB and walked a
15.6 million entry prefix sum **every tick**, whatever the population.

| | |
| --- | --- |
| `flock_frame/1` | **37.6 ms** |
| `flock_frame/1000` | 39.3 ms |
| `flock_frame/10000` | 54.4 ms |
| `flock_frame_large/100000` | 224 ms |

A crowd of one cost 37 ms, i.e. 68% of a 10,000-agent frame was spent maintaining
cells nobody stood in. The library's own test suite took 100 s for the same
reason: 136 tests each rebuilt 15.6 million cells.

## The design that also lost: no cells at all

If per-cell storage is the problem, remove it: sort agents by cell id into one
flat array and binary-search the cell's window on demand. Memory becomes `O(n)`,
build `O(n log n)`, and a crowd of one costs 4.6 µs.

| | |
| --- | --- |
| `flock_frame/1` | **4.6 µs** |
| `flock_frame/1000` | 5.41 ms |
| `flock_frame/10000` | 78.3 ms |
| `flock_frame_large/100000` | 953 ms |
| `spatial_query/radius_8m/10000` | 4.24 µs |

It wins by 8000× at one agent and loses by 4× at a hundred thousand. The reason is
the query shape: an 8 m radius with 4 m cells touches 125 cells, and two binary
searches per cell (~26 steps each) is 3000 comparisons where a dense grid needs
125 array indices. Queries are the hot path, so the dense grid wins.

## The design that shipped

Dense cells, with a **budget of 2^20 cells** and automatic coarsening when a
volume would exceed it. The default 1 km volume coarsens from 4 m to 13.5 m cells
(405,000 cells, 1.6 MB) and rebuilds in about a millisecond.

The budget itself was tuned by measurement:

| Cell budget | 1 NPC | 1,000 | 10,000 | 100,000 |
| --- | --- | --- | --- | --- |
| 2^20 | 1.00 ms | 2.47 ms | **18.2 ms** | **192 ms** |
| 2^18 | 0.30 ms | 2.11 ms | 23.1 ms | 276 ms |

2^18 is three times better for a crowd of one and 27% worse at the population this
crate targets, so the budget is 2^20 and the documentation tells you to size your
volume to your playable area.

## What the numbers do not cover

- **Rendering.** There is no renderer; the frame time above is simulation only.
  Instanced rendering of 10,000 NPCs will cost more than the simulation does.
- **Cognition.** Behaviour trees and the think scheduler are not in these
  benchmarks yet. `steering_step` bounds the per-agent floor, not the cost of
  trees, dialogue or pathfinding on top.
- **wasm.** No wasm numbers yet; the determinism guarantees cover it, the
  performance does not.
- **Threads.** Everything measured is single-threaded. The determinism policy
  (ADR 0002) constrains what can be parallelised and how.
