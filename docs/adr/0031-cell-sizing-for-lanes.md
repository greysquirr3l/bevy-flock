# ADR 0031 — Cell sizing for lane occupancy, and the transposed road as the default

**Status:** Accepted (2026-11-04)
**Related:** ADR 0003 (the budgeted dense grid), ADR 0026 (cell-major crowd kernel), ADR 0030
(transposed SIMD)

## The question

ADR 0030 shipped the transposed kernel behind a feature and named its limit: a block needs four
agents in a cell, the grid sizes cells for *queries* (ADR 0003), and at 10,000 agents a bucket holds
about two. Two things follow, and both are measurable:

1. Is the feature worth having on by default, or is it a specialist option?
2. What cell size does the kernel actually want, and is the grid's budget giving it?

`examples/grid_sweep.rs` answers both. One-kilometre volume, spiral spawn at 1.6 m spacing, five
cell sizes, two populations, one steering pass each, each road measured in its own build.

## The sweep

10,000 agents:

| cell (m) | buckets | agents/bucket | in blocks | scalar | transposed | change |
| --- | --- | --- | --- | --- | --- | --- |
| 5.0 | 3,274 | 3.1 | 38% | 7.63 ms | **5.56 ms** | −27% |
| 7.0 | 1,706 | 5.9 | 68% | 6.82 ms | **4.28 ms** | −37% |
| 9.0 | 1,046 | 9.6 | 81% | 6.71 ms | **4.31 ms** | −36% |
| 13.5 | 478 | 20.9 | 92% | 7.05 ms | **5.32 ms** | −25% |
| 20.0 | 224 | 44.6 | 96% | 8.40 ms | **8.00 ms** | −5% |

100,000 agents:

| cell (m) | buckets | agents/bucket | in blocks | scalar | transposed | change |
| --- | --- | --- | --- | --- | --- | --- |
| 5.0 | 31,596 | 3.2 | 50% | 84.35 ms | **60.49 ms** | −28% |
| 7.0 | 16,495 | 6.1 | 70% | 75.88 ms | **52.59 ms** | −31% |
| 9.0 | 10,049 | 10.0 | 82% | 74.97 ms | **52.63 ms** | −30% |
| 13.5 | 4,511 | 22.2 | 93% | 79.38 ms | **64.21 ms** | −19% |
| 20.0 | 2,074 | 48.2 | 97% | 96.59 ms | **95.53 ms** | −1% |

"In blocks" is the share of agents that fill a four-lane block; the rest take the scalar road.
End to end, through the plugin at 100,000 agents with the default grid: **79.07 ms** a frame with
the transposed road, against the 90.0 ms recorded for the scalar road in ADR 0028's sign-off — a
**−12% frame**, which is smaller than the kernel's −19% because the frame is not only steering.

## What the numbers say

**Three agents per bucket still pays.** At 5 m only 38–50% of agents fill a block, and the
transposed road still wins 27–28%: the win is not gated on perfect occupancy, because the block
that does form amortises its window across four agents and the scalar tail is the same code the
scalar road runs. That removes the reason to keep the feature off — there is no density where it
lost, in five cell sizes and two populations.

**Four per bucket is the minimum, not the optimum.** The kernel's best cell size here is 7–9 m,
which is 6–10 agents per occupied bucket, 1.5–2.5 blocks. Two effects pull in opposite directions:

- *Too fine* (5 m): half the agents fall to the scalar tail, and the bucket sweep grows — 31,596
  buckets to walk instead of 10,049. Cheaper cells, more of them.
- *Too coarse* (20 m): blocks fill almost perfectly (97%) and it is the **slowest** configuration on
  both roads. Work per agent grows with the number of agents in the cell, not with the number that
  fill lanes, so 48 agents a bucket means 48 distance tests each however the lanes are arranged.

The optimum sits where a bucket holds a few blocks: 8–12 agents.

**The production grid is about one and a half cells too coarse.** ADR 0003's budget (405,000 cells)
pins a one-kilometre volume to 13.5 m, which is 19–25% better than scalar and 8–20% worse than 9 m.
Getting 9 m needs about 1.4 million cells — 16 MB of grid arrays rather than 4.9 MB.

## The decision

**The transposed road is on by default.** `default = ["simd"]`. It won at every cell size and both
populations, including the sparse case ADR 0030 was cautious about; the earlier caution was
reasoned from the occupancy fraction rather than measured, and the measurement disagrees. A game
that wants the `wide` dependency gone can set `default-features = false`; the scalar road is the
same code it always was.

**Cell size stays a grid decision, and the recommendation is documented rather than enforced.** The
kernel's preference (6–10 agents per occupied cell) is now in `docs/performance.md` and here, but
`MAX_CELLS` is unchanged. Raising the budget is not obviously right: the grid's budget exists to
bound *query* cost on the ECS path (ADR 0003), and tripling the cell count triples the per-query
cell arithmetic and the memory traffic that path pays every tick. Buying 8–20% of the kernel at an
unmeasured cost to the more frequently called path would be trading a measured number for a guess —
exactly what ADR 0029 was rejected for.

The rule, then: **if your crowd is dense, size the cell so a bucket holds 6–10 agents and pass a
budget that allows it; if your crowd is sparse or your ECS queries dominate, the default is
already close.** The sweep is the tool for deciding, and it takes a minute.

## Settling it: the ECS-path sweep (done, same day)

The question above is whether a finer grid is affordable on the path that runs every tick, because
the budget in ADR 0003 exists to bound *query* cost. Measured, `flock_frame` group, whole
`App::update()` at steady state, budget raised per case so the cell size survives:

| population | 13.5 m / 2^20 (405k cells) | 9 m / 2^21 (1.4M) | 7 m / 2^22 (2.9M) |
| --- | --- | --- | --- |
| 10,000 | 7.29 ms | 7.29 ms | 7.26 ms |
| 100,000 | 77.0 ms | 77.8 ms | 80.4 ms |

**The frame is insensitive to cell size.** 1.4M cells costs nothing measurable at either
population; 2.9M costs about 1% at 100,000 and nothing at 10,000. On the kernel side the same
change was worth 8-20% of a steering pass — so where did it go?

Two things absorb it, and both are already in the design:

- **Tiering caps how many agents steer** (ADR 0017). At 100,000 agents roughly 5,000 are manifested
  and steered; the rest are ambient and cost a continuum update, not a neighbour scan. A 20% kernel
  win on 5% of the agents is 1% of the frame, which is inside this measurement's noise.
- **The grid rebuild is cheap.** 405k cells to clear and rebuild was already a small share of the
  frame (ADR 0003 sized it that way); tripling it triples a small share.

That is the honest answer to the question ADR 0031 opened, and it changes the recommendation: the
grid budget is **not** the thing standing between the kernel and its optimum. Moving to 1.4M cells
is affordable but buys nothing at these populations, because the population that steers is small.

Occupancy, from the same spiral at 100,000 agents ([figure](../images/occupancy-7m-9m.png)):

| cell | buckets | agents/bucket | in blocks | peak |
| --- | --- | --- | --- | --- |
| 7 m | 16,521 | 6.1 | 69% | 13 |
| 9 m | 10,063 | 9.9 | 81% | 17 |

The spiral makes occupancy almost perfectly uniform, so there are no hotspots for a coarse grid to
smear and no cold cells for a fine one to waste: the choice is purely about agents per bucket.

## The decision after the sweep

- **`MAX_CELLS` stays at 2^20.** Not because 1.4M is expensive — it is free — but because it buys
  nothing measurable in a frame, and a default that allocates 16 MB where 4.9 MB did the same job is
  a default that costs every user something for no one's benefit.
- **The budget is now a per-config knob**: `SpatialGridConfig::max_cells`, set with
  `with_cell_budget`. A game that steers a much larger share of its crowd — tiering tuned up, or a
  game with no ambient tier — is exactly the case where the kernel's 9 m optimum is worth claiming,
  and it can now be claimed without a fork.
- **`BoidsConfig::with_grid`** exposes the whole grid config through the plugin, so this is reachable
  from `FlockConfig` rather than only from the domain types.

## Metrics and follow-up

The sweep used one metric, because one metric decides it: whole-frame time at steady state for a
fixed population, comparing cell sizes at the budget each needs. Everything else was a diagnostic
(buckets, agents per bucket, block share) used to explain a number rather than to justify one.

Re-open this decision if any of these become true, each with its own measurement:

1. **Tiering raises the steered share** (the scheduler's calibration gate moves): re-run
   `flock_frame_cell`; if 9 m is worth more than 3% of a frame, raise the default budget.
2. **A game's volume is smaller than 1 km** — size the volume first (ADR 0003's advice), which makes
   fine cells affordable inside the existing budget without any change here.
3. **The rebuild stops being cheap**: if a future grid rebuild grows superlinearly in cells, the
   2.9M case's 1% becomes the leading edge of something worse.

Timeline: none of these are scheduled, because none of the triggers has fired — but trigger 1 is no
longer something a human has to remember. It is a CI job (`cell-size-gate`), a script
(`scripts/cell_size_gate.py`, 3% warn, 10% fail) and a plan task (`cell-size-reopen-gate`) that
depends on the live `benchmark-suite` work. The thresholds are asymmetric on purpose: criterion on a
shared runner cannot resolve 3% honestly, and a gate that flaps gets disabled, which is worse than no
gate at all. The gate reports; choosing a new default stays a recorded judgement with measured
support behind it.

## Reproducing

```text
cargo run --release --example grid_sweep
cargo run --release --example grid_sweep --no-default-features
cargo bench --bench crowd -- crowd_kernel
```
