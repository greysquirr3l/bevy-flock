# ADR 0030, The transposed SIMD kernel: lanes are agents

**Status:** Accepted, behind the `simd` feature (2026-11-04)
**Supersedes:** ADR 0029's rejection (which rejected the *lane-per-neighbour* shape, not SIMD)

## The question

ADR 0029 measured lane-per-neighbour vectorisation in the 3D crowd kernel and rejected it: 16–26%
slower, 8% slower even on a crowd built to fill every lane. The diagnosis was not "SIMD is slow" but
"these lanes share nothing", four *neighbours* of one agent, with a masked accumulation throwing
away the work of every rejected neighbour, and a scalar scan that had been rejecting those
neighbours for one compare and a branch.

That ADR ended with the shape that might work, borrowed from the band kernel (ADR 0023): **block
four agents and broadcast each neighbour into all four lanes**, so one load feeds four agents that
are all doing real work. This ADR is that experiment, run and adopted.

## What was built

In `src/application/crowd.rs`, behind the existing `simd` feature (default off):

- **A cell-major SoA copy**, built by the same counting sort that produces `order`, seven arrays of
  `f32` (`x, y, z, vx, vy, vz, agent`). Four agents of a cell are contiguous, so a block loads
  without a gather.
- **Blocks of four agents that share a cell.** Every neighbour in the cell's 3×3×3 neighbourhood is
  broadcast into all four lanes: `keep = d² ≤ r² & id ≠ self`, `weight = keep.select(1, 0)`, and the
  sums and the `1/d²` separation push accumulate per lane. Because all four lanes are in one cell,
  the window is exact, nothing within the radius can be further than one cell away when the radius
  does not exceed the cell size, which the default configuration satisfies.
- **A scalar road for whatever cannot fill a block**: buckets of one to three agents, and the tail
  of a larger bucket. A block containing an ambient-tier agent (a neighbour that is not itself
  steered) also goes down the scalar road, because masking a lane off would waste the block.
- **`steer_scalar`** stays public as the reference implementation, and
  `the_transposed_road_agrees_with_the_scalar_road` holds the two to identical `steered`,
  `isolated` and `pairs` counters and desired velocities within 1e-4 on a volumetric fixture.

## The numbers

10,000 agents, `crowd_kernel` group, medians, same machine:

| Crowd | Scalar | Transposed | Delta |
| --- | --- | --- | --- |
| planar (1.6 m spiral) | 7.36 ms | **5.78 ms** | −22% |
| volumetric (20 m of height) | 6.12 ms | **4.85 ms** | −21% |
| dense (10,000 in a 30 m disc) | 114.9 ms | **81.8 ms** | −29% |

An early standalone prototype reproduced −24% to −30% on the same three crowds with exact parity,
which is why this was worth porting rather than theorising about. The gain is largest where the
kernel is most expensive, which is the right shape for a win: the dense crowd is the case ADR 0029
could not rescue at all.

## Why this works where the other did not

- **One load, four agents.** The neighbour's six components are splatted once and used by four
  lanes; in the rejected shape they were loaded once per lane with no reuse.
- **The window is amortised.** A block walks 27 cells for four agents instead of 27 cells per agent.
- **Rejection is cheap when the lanes are full.** In the dense crowd most lanes pass, so the masked
  arithmetic is nearly all useful work; in the sparse crowd the block is skipped only when *no* lane
  passes, and `keep.any()` makes that one test.

The remaining waste is the SoA pass (seven arrays rewritten per frame) and the scalar tail. Both are
visible in the numbers: −29% when buckets are deep, −21% when they are shallow.

## Consequences

- The `simd` feature now means something for the crowd kernel too; it is **off by default** because
  it adds a per-frame copy that a game at low density does not need, and because a library should not
  decide a game's build flags. Enable it with `features = ["simd"]` and measure.
- `Crowd::steer` dispatches on the feature; `Crowd::steer_scalar` is the reference road and is
  public so a caller can compare their own world.
- CI's `--all-features` job now exercises the transposed kernel and its parity test, so a future
  edit to either road cannot drift silently.
- `docs/dev/tuning/performance.md` publishes the numbers above; the end-to-end `flock_frame` figures are still
  measured with default features, which is what a game gets unless it asks for more.

## What is still not done

The block requires four agents in one cell, and the cell size is chosen for the *grid*, not for the
kernel (ADR 0003): at 10,000 agents it is 13.5 m and a bucket holds about two, so most agents take
the scalar road and the gain above comes from the minority that do not. Sizing the cell for four-per-
bucket would raise the blocking rate and also change the neighbour scan's cost in the ECS path, a
decision for the grid, not the kernel, and one worth taking with a profile of a real game rather than
a synthetic crowd.
