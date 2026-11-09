# ADR 0023, Four-agent SIMD steering behind a cell-major order

- **Status:** Accepted
- **Date:** 2026-11-04
- **Supersedes:** nothing
- **Related:** ADR 0018 (packed kernel), ADR 0020 (band plugin), ADR 0021 (multi-band)

## Context

After ADR 0022's profiling pass the steering kernel was 93% of a 1,000,000-inhabitant frame
in self time. The remaining cost was the neighbour scan: nine buckets, a linked-list walk per
bucket, roughly fifteen to twenty candidates per agent. The obvious next move is to do the
same work for four agents at once, and on paper it is a clean four-fold win, the arithmetic
per candidate is independent per agent.

It is not clean. Two things get in the way, and both are measurable:

1. **Agents are stored in insertion order, and insertion order is not spatial.** Four
   consecutive agents are four unrelated places on the map. The union of their 3x3 cell
   neighbourhoods then covers far more cells than the four scans it replaces, and the block
   does more work than the scalar path would have.
2. **The masking costs money.** A lane that should not see a cell needs a mask, and building
   four-lane masks per cell per block is not free, it dominated at small populations.

## Decision

Vectorise four agents at a time, with the block taken from a **cell-major order** built as a
by-product of the grid, and with two deliberate fast paths.

### The order

`build_grid` already walks every bucket to fill the chains; collecting those indices as it
goes produces a cell-major order for free, a counting sort that is already being paid for.
Blocks are taken from that order, so the four agents in a block are nearly always in the same
cell, and the union box is 3x3 rather than something sprawling. If the box does exceed 12
cells, the block is handed back to the scalar path: the fallback is a rule, not a hope.

### The masks

Each candidate is tested against all four lanes at once:

```text
dx = splat(candidate.x) - position_x      // four distances in two instructions
keep  = (dx*dx + dy*dy <= radius²) & inside & (candidate != identity)
```

`inside` is the per-lane entitlement to look in this cell at all. When all four agents share a
cell, the common case, it is all-ones and is computed once for the block instead of once per
cell. That single check was worth 10% on its own.

### Bit parity, not approximate parity

The scalar path stays in the crate as `steer_one`, and the vectorised path has to reproduce it
**exactly**. Two decisions follow:

- The distance is `dx * dx + dy * dy`, deliberately *not* `mul_add`: a fused multiply-add
  rounds once where the scalar path rounds twice, which is a one-ULP difference in the
  distance and therefore in the result. The instruction is not worth the divergence.
- Masked-out lanes contribute `0.0`, which is exact, so each lane accumulates over exactly the
  neighbours the scalar path would see, in exactly the same order.

`the_vectorised_kernel_agrees_with_the_scalar_one` asserts equality bit for bit, including the
pair, steered and isolated counts. It failed on the first run with a single ULP on one lane,
which is how the `mul_add` was found.

## Measurements

`wide` 1.7 as the vector type (safe API, `Zlib OR Apache-2.0 OR MIT`, no `unsafe` in this
crate), behind an opt-in `simd` feature so the default build stays dependency-free.

| Workload | Scalar | SIMD | Change |
| --- | --- | --- | --- |
| 1,000,000 band frame | 4.75 ms | **3.78 ms** | −20.4% |
| packed steer, 50,000 | 138 ms | **56.5 ms** | −59% |
| packed steer, 5,000 | 1.31 ms | **1.20 ms** | −8.4% |
| packed steer, 1,000 | 0.238 ms | **0.223 ms** | −6.3% |

The win scales with density, which is the expected shape: the benefit comes from sharing one
bucket walk across four agents, and a sparse crowd has little walk to share.

### What did not work, in order

1. **Blocking in index order.** 1.40 ms at 5,000, *slower* than the scalar 1.31 ms, because
   scattered blocks produced sprawling boxes and fell back to scalar anyway.
2. **Blocking in cell order without the same-cell fast path.** 1.36 ms at 5,000 and 62.9 ms at
   50,000, the big crowd was already winning, the small ones were still losing to mask
   construction.
3. **Computing the pair count per candidate**, as a blend plus a four-float store plus a
   reduction. Removing it in favour of summing the lane totals at the end was worth ~10% at
   50,000.

## Consequences

- `simd` is opt-in. A game that wants the 20% enables one feature; a game that cannot take a
  dependency does not have one.
- The pack grows by 4 bytes per agent for the order. At 1,000,000 agents that is 4 MB, against
  the 20 MB the positions and velocities already cost.
- The scalar path is now also the **reference implementation** and is tested against the wide
  path on every run, so neither can rot silently.
- The fallback rule (box larger than 12 cells) means correctness never depends on the order
  being perfect: a game that pushes its own positions between ticks still steers correctly,
  just slower for that block.
