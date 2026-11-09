# 0057, The count pass absorbs the scan's totals, and the scan seats its own cursors
- **Status:** Accepted
- **Date:** 2026-11-09
- **Amends:** the totals half of [0055](0055-blocked-prefix-sum.md) and the seat half of [0056](0056-placement-cursor-is-a-copy.md); both scans are otherwise unchanged.
## Context
`SpatialGrid::build` is four passes and, on the parallel path, three dispatches:

| pass | what it does | 1 worker | 6 workers | scaling |
| --- | --- | ---: | ---: | ---: |
| `clear` | zero the window table, partitioned | 12.96 ms | 2.46 ms | 5.3× |
| `count` | `cell_start[cell] += 1` per body, partitioned | 1.72 ms | 1.76 ms | **0.98×** |
| `prefix` | per-block totals, block bases, block-local scans | 0.43 ms | 0.30 ms | 1.4× |
| `place` | copy starts to cursors, then fill windows, partitioned | 7.19 ms | 2.22 ms | 3.2× |

`prefix` is three things behind one timer, and two of them are avoidable:

**The totals pass re-reads what `count` just wrote.** `prefix_cells_blocked` sums `cell_start[first+1 .. first+1+len]`
for each of 4096 blocks, 1.7 MB read serially, immediately after the pass that produced it, to compute numbers
that pass could have accumulated as it counted. It is the only part of the scan that touches every cell on one
thread, and it exists only because `count` had nowhere to put a running total.

**The seat is a copy of a value the scan already holds.** Since [0056](0056-placement-cursor-is-a-copy.md) the
placement cursor is its own array, filled by `seat_cursors` inside the `place` dispatch. But the block-local scan
computes each cell's window start as an intermediate value on its way to writing the table: it has the number in a
register and then throws it away, and a later pass reads the array back to recover it.

`count`'s 0.98× is the other half of the argument. It is memory-bound, not compute-bound: each worker iterates
the entire `cell_list` and filters for its own range, so six workers read 24 MB of cell indices to count them
into six windows of about 70,000 cells each, 281 KB, at the benchmark's 421,876 cells. The pass is already
paying to touch every entry six times, which is precisely the loop a per-block accumulate belongs in.

## Decision
**`count` accumulates per-block totals as it counts; `prefix` becomes bases plus block-local scans; the scan writes
the cursor table alongside the window table, and `seat` disappears.**

### Arithmetic, so it does not have to be re-derived
Counts live one slot to the right of the cells they count: `count(c) == cell_start[c + 1]`, and `cell_start[0]` is
zero from the clear and is never written again. That offset is why the scan works on `cell_start[1..]`, and why every
read and write below carries a `+ 1`. For block `b` with `(first, len) = block_span(b, cells, blocks)`:

```
count(c)  = cell_start[c + 1]                                    # what `count` wrote
total[b]  = Σ  cell_start[first + i + 1]        for i in 0..len   # accumulated during count
base[b]   = Σ  total[k]                         for k < b         # serial, O(blocks)
running   = base[b]
for i in 0..len:                                                  # block-local, parallel
    running += cell_start[first + i + 1]
    cell_start[first + i + 1]  = running       # window start of cell `first + i + 1`
    cell_cursor[first + i + 1] = running       # the same value, for `place` to advance into
```

`base[0]` is zero, so the first write leaves `cell_start[1] == count(0)`, correct, because cell 0's window begins at
0 and `cell_start[0]` already says so. The last block leaves `cell_start[cells]` holding the total, which is where
the final cell's window ends. This reproduces today's array byte for byte, including the rotation that leaves
`cell_start[cell]` holding where cell `cell`'s window begins.

`cell_cursor[p]` is written from the same register as `cell_start[p]`, so the two tables are equal by construction
immediately after the scan, an invariant a test can assert, and the whole reason the copy in `seat` can go.


### Workers own block-aligned ranges
Each block's total must have exactly one writer, or the pass needs atomics and its output depends on contention
order. So `count` slices by *block range* rather than by equal cell counts: worker `k` owns blocks
`[blocks·k/w, blocks·(k+1)/w)` and the cell span those blocks cover. Blocks are derived from the cell count and
never from the worker count ([0055](0055-blocked-prefix-sum.md) §6), so the slicing changes what a worker is given,
not what the array ends up holding. Byte-identity at every worker count is preserved by the same argument as before:
partitioning is by cell range, and a body's cell is a function of its position.

### What `PassCosts` reports instead
Five rows become four. `seat` is deleted, a field that can only ever be zero is a field that misleads.
`count` covers counting plus totals and says so in its doc. `prefix` covers bases plus the block-local scans, and
`examples/build_passes.rs` loses its `seat` row in the same commit, because the inventory is the thing a reader
compares against and a stale row is worse than no row.

Dispatch count does not change: three before, three after, since [0056](0056-placement-cursor-is-a-copy.md)'s
amendment had already folded the seat into the `place` dispatch. The dispatch is `persistent-worker-pool`'s problem,
not this one's, this ADR removes work, not scopes.

## The two trades, and which is expected to pay
**Expected to pay:** the totals read. 1.7 MB of serial bandwidth leaves the rebuild, and with it the dependent
chain that made the totals pass resist vectorisation. The 0.11 ms-class copy also leaves `place`.

**Expected to cost:** a per-block accumulate inside the pass that scales worst. Two mitigations, in order of
preference. First, the accumulate is a register-level `+=` on a slot the worker owns, `block_totals` is 16 KB at
4096 blocks, which is L1-resident, and the inner loop is already walking `cell_list` at a stride the prefetcher
handles. Second, if measuring shows the accumulate is not free, the fallback is for each worker to sum its *own*
window after counting it (hot in L1/L2, parallel) rather than to RMW a block slot per hit. The ADR commits to the
output, not to the mechanism.

**The risk this ADR is honest about:** `count` at 0.98× has almost no headroom, so a fusion that adds a cache miss
per body could make the rebuild *slower* while making the scan faster. [0055](0055-blocked-prefix-sum.md) demoted
its own blocked scan on exactly this kind of measurement, and the same rule applies here: if `build_passes` at one
and six workers does not show the total improving, the fusion is reverted and the measurement is recorded as the
reason. A fused pass that is slower is a result, not a mistake to hide.

## Determinism under a worker pool
Phase 5's `persistent-worker-pool` task replaces `std::thread::scope` with workers that outlive the rebuild. That
change is only safe if the rebuild's output does not depend on *which worker wakes first*, and this ADR is what
makes that checkable:

- Every block total has one writer, fixed by block range, not by scheduling.
- `base` is computed after all counts join, so a late worker delays the pass but cannot change a number.
- The block-local scans write disjoint ranges, and each takes its base from a read-only array.
- A worker that runs no blocks at all, because another stole them, leaves the arrays exactly as the schedule that
  gave it work would have.

The pool must therefore preserve the scope's happens-before at each pass boundary (join before bases, bases before
scan). It may reorder *within* a pass, which is what it does when it steals; it may not reorder across one, which no
implementation should want to.

## Consequences
- The scan's serial part drops from `O(cells)` bytes read plus `O(blocks)` additions to `O(blocks)` additions, which
  is what phase 4 was opened to attack and what phase 5's 4.72 ms target needs.
- One fewer array to keep in sync: `place` advances `cell_cursor` and never reads `cell_start`, so the two cannot
  drift; a test asserts they are equal immediately after the scan, before placement.
- `count`'s doc comment stops being able to say "this pass only counts", which is a real loss of simplicity and the
  reason the inventory keeps a per-pass table rather than a single total.
- If `count` regresses, the fallback (per-worker sums over its own window) preserves the shape of the decision:
  totals stop being a separate read, whichever thread ends up computing them.

## Rejected alternatives
- **Atomics on block totals inside the count loop.** Correct, and the cache line holding a hot block becomes the
  bottleneck for every body in it. It also makes the pass's timing depend on contention order, which is the property
  ADR 0054 exists to keep out of the rebuild.
- **A fourth dispatch that computes totals in parallel over block chunks.** Costs 0.53 ms to save a read that costs
  less than that, and adds a scope to a build whose scopes are already the phase's next target.
- **Make `blocks` proportional to the worker count.** Rejected by [0055](0055-blocked-prefix-sum.md) §6: the output
  must be byte-identical at every worker count, so the block geometry may not be a function of it. This ADR slices
  workers to blocks, never blocks to workers.
- **Keep the seat in `place` and fuse only the totals.** Half the win for none of the risk, and it leaves a copy of a
  value the scan already holds. Worth naming because it is the tempting minimum: the seat is 0.11 ms-class work, and
  the invariant that makes it unnecessary (`cursor == start` after the scan) is the thing worth having.
- **Write the cursor table from `count` instead.** Count does not know window starts; it knows counts. The starts
  only exist after the prefix sum, so the scan is the only pass that can write both.

## Outcome

Implemented as decided, with two amendments the measurement forced and one prediction that did not hold.

**Amendment, a single worker takes the serial scan.** The decision routed on `blocks` alone, which sent a
one-worker rebuild through the blocked scan: it walked the same array in the same order, through a block loop, for
1.02 ms against 0.42 ms. Routing now asks `needs_block_totals_for(workers)`, a blocked scan *and* more than one
worker, and both `count` and `prefix` consult it, which is what lets `count` skip the totals entirely on a serial
rebuild. The block count is still reported, because it is still what the grid was configured for.

**Amendment, the cursor table is not cleared.** The decision said the scan writes both arrays, which it does, but
`count` cleared `cell_cursor` first for tidiness: 1.7 MB of `fill` per rebuild, 0.3 ms, to prepare memory the scan
overwrites in full. Every slot the scan writes, it writes unconditionally; the one slot it skips is index 0, and the
scan sets that. The table is still sized once, so the steady-frame no-allocation promise is unchanged.

**The accumulate was not a cost, and the read it replaced was not a win.** The decision expected `count` to absorb a
new per-block accumulate in a pass that scales 0.98×, and named the fallback (each worker sums its own window after
counting it) if that was not free. That fallback is what shipped, because it is measurably free: `count` at six
workers went 1.734 → 1.772 ms. The other half of the trade did register, `prefix` 0.296 → 0.238 ms, the 1.7 MB totals
read leaving the serial tail, but it is 0.06 ms of a 6.8 ms rebuild, which alternated binaries cannot distinguish
from noise. The total is unchanged (6.758 → 6.814 ms, medians of six alternating pairs).

So the fusion is kept for what it removes rather than for what it saves: a pass, a copy, a field, and the reread
between two phases. The revert clause this ADR wrote for itself is not invoked, because the change did not make the
rebuild worse, but neither did it make it better, and the next task should not be chosen on the assumption that it
did. Full figures in `docs/dev/tuning/performance.md` §*The count pass absorbs the scan's totals*.
