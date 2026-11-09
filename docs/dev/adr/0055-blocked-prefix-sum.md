# ADR 0055: the blocked prefix sum over cell blocks

- **Status:** Accepted
- **Date:** 2026-11-09
- **Supersedes:** nothing
- **Related:** ADR 0004 (determinism), ADR 0054 (what a parallel build may not change), ADR 0033 (execution groups)

## Context

Phase 3 took the spatial rebuild from one worker to six and ended at 2.94x on the rebuild against 6.36x on the
query batch. Its write-up attributed the difference to the prefix sum over the cell array, "the prefix sum, the
cell clear and the cursor restore are serial by construction", and phase 4 was planned around replacing that
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
| total | 23.14 ms | 8.85 ms |, |

The scan is 0.85 ms, not the multi-millisecond serialisation it was assumed to be, because the grid caps its
cell array: the 1 km fixture holds 421,876 cells (75 per axis, an effective 13.3 m), not the 15.6 million that a
dense 8 m grid over the same volume would imply. The serial tail is `prefix + restore = 1.55 ms`, which by
Amdahl caps the build at **4.5x at six workers** no matter how good the parallel half becomes. The measured 2.6x
is 58% of that ceiling.

So this ADR still accepts a blocked scan, but on honest grounds: it removes one of the two serial passes, it is
a prerequisite for deleting the cursor restore (ADR 0055 §4), and it recovers about 0.7 ms of the 6.3 ms still
available. It is not the phase's headline any more, the clear's 30 ns per cell and the count pass's *negative*
scaling are.

## Decision

### 1. Decompose over the flattened cell index, not a 3D tiling

A block is a contiguous half-open range `[start, end)` of the flattened cell index. A 3D tiling would visit
cells in a different order, and cell order is exactly what decides the order of every window and of `order`
itself, the property ADR 0054 promises and `tests/spatial_parity.rs` measures. Blocks therefore never
reorder; they only decide *who computes* which range.

### 2. Two passes plus a scan over block totals

For `b` blocks over `n` cells:

1. **Count, in parallel.** Each block sums its own cell counts into `block_total[b]`. Work: O(n / workers).
2. **Scan the block totals, serially.** `block_base[b]` is the exclusive prefix sum of `block_total`, plus the
   carried total. Work: O(b) on one thread, the term that has to be small, and is, because `b` is a few
   hundred rather than a few hundred thousand.
3. **Scan inside each block, in parallel.** Each block writes its own exclusive prefix sums starting from
   `block_base[b]`. Work: O(n / workers).

Total single-threaded work falls from O(n) to O(b).

The worked arithmetic, for the 1 km fixture at 8 m: n = 421,876 cells and b = 1,024 blocks, so a block is
412 cells (`n / b`, with the first `n mod b` = 164 blocks taking 413). The serial step is 1,023 additions over
`u32` block totals instead of 421,876 additions over the cell array, a factor of 412 less serial work, and
1,023 additions is under a microsecond on any core this library supports. Two block-total arrays of `b`
`u32`s cost 8 KB, which is not worth a paragraph of justification against a 1.7 MB cell array.

The remainder rule is part of the contract, not an implementation detail: the first `n mod b` blocks take one
extra cell and the rest take `n / b`. Two implementers who differ on that produce different `block_base`
values, and therefore different `cell_start`, while both claiming to follow this ADR.

### 3. The serial path stays as the reference

`blocks == 1` routes to the existing scan, which is also what a grid smaller than `PARALLEL_MIN_CELLS` uses. The
blocked path is therefore testable *against* the serial one rather than merely asserted equal, and removing the
reference would turn the parity test into a tautology.

### 4. The cursor restore disappears rather than parallelising

`restore_cell_start` exists so that `place` does not have to re-read the window starts: it copies `cell_start`
into the write cursors. With per-block bases, each block knows its own write base and the copy has no reason to
exist. Deleting 0.68 ms of work beats distributing it across six workers, and the blocked scan is what makes the
deletion safe, `cell_start` and the cursor array can no longer drift apart because only one of them is written.

### 5. No shared cursor, restated

ADR 0054 banned a shared atomic cursor. A blocked scan obeys it for a stronger reason than discipline: each
block writes a disjoint range and the range comes from `block_base`, so there is no contention to lose.

### 6. The block count derives from the cell count, never from the worker count

`blocks` is computed from `n` alone, `n.div_ceil(CELLS_PER_BLOCK)`, clamped to `[1, MAX_BLOCKS]`, with
`CELLS_PER_BLOCK = 4_096` and `MAX_BLOCKS = 4_096` as the starting constants, and then a caller may override it
through `with_blocks`, exactly as `with_threads` overrides the worker count. It is never derived from `workers`.

This is the rule that keeps the determinism promise honest. If `blocks` followed `workers`, then changing the
thread count would change the decomposition, and the only thing standing between that and a changed snapshot
would be the claim that the scan is associative. The scan is not required to be associative: it is required to
be *identical*, and the cheapest way to guarantee identical is to make the decomposition independent of the
thing being scaled. `workers` decides who computes which block; it never decides where a block starts.

The consequence is that the output depends on `(n, blocks)` and nothing else, which is precisely the property
the acceptance test below can check exhaustively over the block sizes it uses.

### 7. Small grids keep the serial scan

Below `PARALLEL_MIN_CELLS = 65_536` (the constant the build already uses to decide whether to partition at all),
`blocks` is 1 and the serial scan runs, regardless of what the caller asked for. Two reasons: the O(b) serial
step stops being negligible when `b` is a meaningful fraction of `n`, and a 4,000-cell grid is not where anyone's
frame time goes. The override is clamped rather than honoured so that a caller cannot accidentally make a small
grid slower and then report it as a regression in this library.

### 8. What ADR 0054's invariants mean here

Restated against this design, because the ADR is only useful if its promises are checkable in the code it
authorises:

- **Neighbourhood membership** is unchanged: membership is decided by `cell_start` and `count`, and both are
  produced by a scan whose only freedom is *who* adds the numbers.
- **Neighbour order** is unchanged: `order` is filled by `place` in cell order over contiguous windows, and the
  blocked scan produces the same windows.
- **The RNG stream per body** is untouched: no pass in this design draws random numbers, and the order bodies
  are placed is `entries` order, not worker completion order.
- **The centroid float sum** is untouched: it happens in the steering kernel, not the rebuild, and this ADR does
  not move any float addition.
- **No shared mutable cursor**: §5.

## Acceptance

A task implementing this ADR is not done until:

- `cell_start` and `order` compare **byte for byte** against the serial scan at 1, 2, 3 and 8 workers across
  block counts {1, 128, 1024, 4096}, the last is `MAX_BLOCKS`; this ADR first said 16384, which its own
  constant would have clamped, so the matrix and the clamp agree on 4096), in a test that compares bytes rather
  than element-wise equality. Byte
  comparison, not element-wise: element-wise equality would let a difference in the padding between arrays pass.
- `blocks = 1` is asserted to route to the serial scan, so the test above fails loudly if the reference path is
  removed rather than silently comparing the blocked path with itself.
- `tests/spatial_parity.rs` still passes unchanged at 2,000 ticks: the integration-level proof that no
  neighbourhood moved.
- `examples/build_passes.rs` reports the new `prefix` and `restore` costs beside the old ones, so the prediction
  below is falsifiable in public.

### The prediction this ADR is judged by

Written before the implementation, from the T01 measurements (1M bodies, 421,876 cells, six aarch64 cores):

| Quantity | Today | Predicted after |
| --- | ---: | ---: |
| `prefix` at 6 workers | 0.87 ms | ≤ 0.10 ms |
| `restore` at 6 workers | 0.68 ms | 0 ms (deleted, §4) |
| serial floor at 6 workers | 1.55 ms | ≤ 0.10 ms |
| Amdahl ceiling at 6 workers | 4.5x | 5.6x |
| build total at 6 workers | 8.85 ms | 8.85 - 1.45 = 7.40 ms, i.e. 1.2x, if the parallel half is unchanged |

The last row is the honest one: deleting 1.45 ms of a 8.85 ms rebuild cannot produce more than a 1.2x
improvement on its own, and 5.6x is a ceiling on a build whose `count` pass is currently 0.8x with workers.
A task that lands this ADR and reports "no change to total" is not a failure, but it has to report the passes,
and a `prefix` above 0.10 ms at six workers is.

If `prefix` lands above 0.10 ms, sweep `blocks` before distrusting the design. That sweep is part of the
implementing task, not a follow-up.

**What actually happened (T03).** `prefix` landed at **0.28 ms** at six workers, 2.8x over this prediction, and
the sweep the paragraph above prescribes is what identified why: 64 blocks 0.272 ms, 256 blocks 0.279 ms, 1024
blocks 0.290 ms, 4096 blocks 0.304 ms. Flat, so the O(b) step and per-block overhead are not the limit, and the
suggested cause above was wrong. The limit is bandwidth: the scan is a read-modify-write over 421,876 `u32`, and
0.28 ms is about 6 GB/s of effective single-pass bandwidth on the measurement VM. A 0.10 ms target implied
~17 GB/s, which a pass that both reads and writes the same array does not get.

The prediction is kept here rather than edited to match the result, because a prediction that is rewritten after
the fact is not a prediction. The total did land where it said it would, 8.85 ms to 7.48 ms, so the arithmetic
about the serial floor was right and the per-pass estimate for one of its terms was not.

## Consequences

- The build's serial floor drops from 1.55 ms to whatever the block scan costs (predicted ≤ 0.1 ms at 1,024
  blocks), which raises the Amdahl ceiling at six workers from 4.5x to about 5.6x *if* the parallel half also
  improves. It will not improve on its own: `count` is 0.8x with workers today, and that has to be fixed
  separately.
- The `blocks` knob becomes configuration that must not change output, joining `workers` in the same rule: a
  setting may change cost, never behaviour. Unlike `workers`, it is derived from `n` when unset, so a caller who
  never touches it still gets a decomposition that does not depend on the machine.
- This ADR does not fix the build. It removes 1.45 ms of a 8.85 ms rebuild and raises the ceiling from 4.5x to
  5.6x; the pass that stops the build reaching that ceiling is `count`, which is 0.8x with workers today. Anyone
  reading this ADR for a headline number should read `clear-and-count-cost` instead.
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
  does, but the restore deletion depends on this decomposition, and 0.68 ms of a 8.85 ms rebuild is worth
  having once the frame is 16 ms.

## Amendment, the block constant, swept (phase 5 T04)

§6 chose `CELLS_PER_BLOCK = 4096` from the shape of the decomposition rather than from a measurement, and the phase 5
plan recorded a doubt with it: 103 blocks at the benchmark's 421,876 cells is 17 per worker on six cores, which the
task text guessed was too few.

The sweep (two populations × nine block counts, `docs/dev/tuning/performance.md` §*Picking CELLS_PER_BLOCK*) refutes the guess
and confirms the number. The walls are at ~1.3 blocks per worker (8 blocks: `prefix` 0.285 ms against 0.23 in the
middle, a tail five workers watch) and at 103 cells per block (4096 blocks: the per-block overhead shows up in
`count`, 1.98 against 1.85 ms, and costs the total 0.3 ms). Everything from 32 to 2048 blocks is inside the fixture's
noise.

What justifies 4096 specifically is bytes, not blocks: a 16 KB window slice with the cursor slice beside it is 32 KB,
which fits L1d on the cache geometries this crate targets and is why the local scan's forward walk and its totals read
stay in cache. The derivation itself is now a free function (`derived_blocks`) so a test can pin the constant, its
byte size, and the block count at seven cell counts, an edit to the constant fails a test that cites this table
instead of moving silently with it.
