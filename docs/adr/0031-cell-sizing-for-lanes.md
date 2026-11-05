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

## What would settle it

A sweep of the *ECS* path over the same cell sizes — `flock_frame` at 9 m versus 13.5 m — so the
grid budget can be raised on evidence from both sides. That is a plan task, not a paragraph here.

## Reproducing

```text
cargo run --release --example grid_sweep
cargo run --release --example grid_sweep --no-default-features
cargo bench --bench crowd -- crowd_kernel
```
