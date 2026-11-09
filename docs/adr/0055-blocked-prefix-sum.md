# ADR 0055: the blocked prefix sum over cell blocks

- **Status:** Accepted
- **Date:** 2026-11-09
- **Supersedes:** nothing
- **Related:** ADR 0004 (determinism), ADR 0054 (what a parallel build may not change), ADR 0033 (execution groups)

## Context

Phase 3 took the spatial rebuild from one worker to six and ended at 2.94x on the rebuild against 6.36x on the
query batch. Its write-up attributed the difference to the prefix sum over the cell array — "the prefix sum, the
cell clear and the cursor restore are serial by construction" — and phase 4 was planned around replacing that
scan with a blocked one, on the prediction that the build would then reach 5x.

T01 priced the passes before anyone wrote the scan, and the attribution was wrong in one important way and
imprecise in another. Measured at 1M bodies on six aarch64 cores (`examples/build_passes.rs`, median of seven):

| Pass | 1 worker | 6 workers | Share of the parallel rebuild |
| --- | ---: | ---: | --- |
| clear | 12.91 ms | 2.46 ms | 27.8% |
| count | 1.68 ms | 2.08 ms | 23.5% |
| prefix | 0.85 ms | 0.87 ms | 9.8% |
| place | 6.99 ms | 2.23 ms | 25.2% |
| restore | 0.68 ms | 0.68 ms | 7.7% |
| total | 23.14 ms | 8.85 ms | — |

The scan is 0.85 ms, not the multi-millisecond serialisation it was assumed to be, because the grid caps its
cell array: the 1 km fixture holds 421,876 cells (75 per axis, an effective 13.3 m), not the 15.6 million that a
dense 8 m grid over the same volume would imply. The serial tail is `prefix + restore = 1.55 ms`, which by
Amdahl caps the build at **4.5x at six workers** no matter how good the parallel half becomes. The measured 2.6x
is 58% of that ceiling.

So this ADR still accepts a blocked scan, but on honest grounds: it removes one of the two serial passes, it is
a prerequisite for deleting the cursor restore (ADR 0055 §4), and it recovers about 0.7 ms of the 6.3 ms still
available. It is not the phase's headline any more — the clear's 30 ns per cell and the count pass's *negative*
scaling are.

## Decision

### 1. Decompose over the flattened cell index, not a 3D tiling

A block is a contiguous half-open range `[start, end)` of the flattened cell index. A 3D tiling would visit
cells in a different order, and cell order is exactly what decides the order of every window and of `order`
itself — the property ADR 0054 promises and `tests/spatial_parity.rs` measures. Blocks therefore never
reorder; they only decide *who computes* which range.

### 2. Two passes plus a scan over block totals

For `b` blocks over `n` cells:

1. **Count, in parallel.** Each block sums its own cell counts into `block_total[b]`. Work: O(n / workers).
2. **Scan the block totals, serially.** `block_base[b]` is the exclusive prefix sum of `block_total`, plus the
   carried total. Work: O(b) on one thread — the term that has to be small, and is, because `b` is a few
   hundred rather than a few hundred thousand.
3. **Scan inside each block, in parallel.** Each block writes its own exclusive prefix sums starting from
   `block_base[b]`. Work: O(n / workers).

Total single-threaded work falls from O(n) to O(b). For this fixture, n = 421,876 and b = 1,024, so the serial
step drops from 421,876 additions to 1,023.

### 3. The serial path stays as the reference

`blocks == 1` routes to the existing scan, which is also what a grid smaller than `PARALLEL_MIN_CELLS` uses. The
blocked path is therefore testable *against* the serial one rather than merely asserted equal, and removing the
reference would turn the parity test into a tautology.

### 4. The cursor restore disappears rather than parallelising

`restore_cell_start` exists so that `place` does not have to re-read the window starts: it copies `cell_start`
into the write cursors. With per-block bases, each block knows its own write base and the copy has no reason to
exist. Deleting 0.68 ms of work beats distributing it across six workers, and the blocked scan is what makes the
deletion safe — `cell_start` and the cursor array can no longer drift apart because only one of them is written.

### 5. No shared cursor, restated

ADR 0054 banned a shared atomic cursor. A blocked scan obeys it for a stronger reason than discipline: each
block writes a disjoint range and the range comes from `block_base`, so there is no contention to lose.

## Acceptance

A task implementing this ADR is not done until:

- `cell_start` and `order` compare **byte for byte** against the serial scan at 1, 2, 3 and 8 workers across
  block counts {1, 128, 1024, 16384}, in a test that compares bytes rather than element-wise equality.
- `tests/spatial_parity.rs` still passes unchanged at 2,000 ticks: the integration-level proof that no
  neighbourhood moved.
- `examples/build_passes.rs` reports the new `prefix` and `restore` costs beside the old ones, so the 0.7 ms
  prediction is falsifiable in public.

## Consequences

- The build's serial floor drops from 1.55 ms to whatever the block scan costs (predicted ≤ 0.1 ms at 1,024
  blocks), which raises the Amdahl ceiling at six workers from 4.5x to about 5.6x *if* the parallel half also
  improves. It will not improve on its own: `count` is 0.8x with workers today, and that has to be fixed
  separately.
- The `blocks` knob becomes configuration that must not change output, joining `workers` in the same rule: a
  setting may change cost, never behaviour.
- The phase's target moves. `build-scaling-gate` now measures against the floor this ADR computes rather than
  the 5x guess, and it has to state which pass stopped it if the total lands above the prediction.

## Alternatives considered

- **Parallel scan with a look-back (Blelloch/Hillis-Steele) over the whole array.** O(n log n) work for O(log n)
  depth; rejected because the scan is 0.85 ms and the extra work would cost more than the depth buys.
- **Atomics per cell.** Rejected by ADR 0054, and would reorder `order` nondeterministically.
- **Smaller cells to make the grid denser and the scan more "worth" parallelising.** This inverts the problem:
  the scan is small *because* the cell budget keeps `n` small, and growing `n` to justify the optimisation is a
  net loss.
- **Do nothing and chase the clear instead.** Tempting given the measurement, and partly what the revised plan
  does — but the restore deletion depends on this decomposition, and 0.68 ms of a 8.85 ms rebuild is worth
  having once the frame is 16 ms.
