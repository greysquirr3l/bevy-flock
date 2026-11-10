# 0063, Removing the pass hand-offs, and then removing the passes

- Status: Proposed
- Phase: 6, follow-on to `persistent-worker-pool` (phase 5 T05); supersedes nothing, extends 0059
- Depends on: 0057 (fused count and scan), 0059 (who owns the buffers a parked worker writes), 0060 (the 5x gate)

## Context

A million-body rebuild on six warm workers is 4.283 ms, split `clear` 0.642, `count` 1.638, `prefix` 0.106, `place`
1.911. It pays **three `std::thread::scope` dispatches** per rebuild: `fill_cells_parallel`, `count_cells_parallel` and
`scan_and_place_parallel`. 0059 measured an empty scope at 0.53 ms and then corrected the in-situ value to about **0.2 ms**,
because a scope with work in it overlaps thread creation with the first threads' progress. Three of them is roughly 0.6 ms,
14% of the rebuild, spent handing off rather than computing.

The bandwidth work this phase just finished rules out the other explanation for the remaining time: the rebuild accounts
for about 41 MB per pass, 9.6 GB/s at 4.283 ms, against a measured copy ceiling of 119 GiB/s on the same host. It uses 7%
of the memory system. What is left is dependent latency, and a hand-off is pure dependent latency: nothing in the second
pass can start until the slowest worker finishes the first.

0059's aliasing table still governs what can be fused:

| phase | reads | writes |
| --- | --- | --- |
| fill | `entries` | `cells`, `cell_start[all]` (reset) |
| count | `cells`, `entries` | `cell_start[own windows]`, `block_totals[own blocks]` |
| scan | `cell_start[own windows]`, `block_totals[< own block]` | `cell_start[own windows]`, `cell_cursor[own windows]` |
| place | `cells`, `cell_start[own boundaries]` | `cell_cursor[own windows]`, `order[own run]` |

## Decision

Two changes, in order, each with its own gate.

### P1, collapse count, scan and place into one dispatch with no barrier

The table has exactly one irreconcilable pair: `fill` writes `cells` while every later phase reads all of it. Everything
below that line is disjoint per worker if the block partition is applied to the histogram, the cursor and the output run
at once, which it already is.

So the rebuild becomes **two scopes**:

1. **Scope A, fill.** Each worker computes `cell_of` for its own body range into its own slice of `cells`, and zeroes its
   own slice of `cell_start` and `cell_cursor`. No barrier inside; the scope join is the barrier.
2. **Scope B, count then scan then place, per worker, in sequence, with no barrier at all.**
   - `count` increments the worker's own `cell_start` window from its own `cells` slice and stores its blocks' totals into
     a shared `&[AtomicU32] block_totals` with `Relaxed`.
   - `scan` needs its block's base, which is the sum of every preceding block's total. Instead of a global barrier and a
     serial `prefix_bases` pass, it uses **decoupled look-back**: walk back over `block_totals`, `Acquire`-loading each
     slot, until a slot is published; sum what it finds and publish its own inclusive prefix so later blocks stop early.
     Every value is an exact `u32`, so the published order cannot change the result and byte-identity is preserved.
   - `place` then runs on the same worker with its cursor already primed, exactly as it does today.

0059's outcome already proved the point that makes this legal: a worker's starts depend only on counts below its own block,
which it has itself, plus the base, so there is nothing to synchronise after the base arrives. Look-back replaces the
barrier with a data dependency on a per-block word.

`block_totals` must be `AtomicU32` rather than `u32`. That is 412 bytes of shared state for the default 103 blocks, and the
look-back path is O(1) on the common case because the previous block is almost always already published by the time a
worker gets to it.

**Cost model.** One scope removed at 0.2 ms, the serial base pass (~0.03 ms of the 0.106 `prefix` row) removed, plus the
overlap the barrier was forbidding: `count`'s tail and `scan`'s head now run concurrently on different workers. Expect
4.283 → **4.00 to 4.05 ms**, 1.06x, and the 6-worker speedup against one thread to move 2.91x → ~3.10x.

**Why not a parked pool** (0059 option A, still deferred): 0059 measured a condvar-woken pool as *slower* than the scope it
replaced, 6.74 → 7.42 ms, because six wake-ups cost more than one spawn. A pool only pays if it spins between phases rather
than parking, which trades 0.2 ms of hand-off for a few tens of microseconds of spinning on six cores, and that is a
separate decision with a different risk profile (`unsafe` laundering of the slices). P1 does not need it, and doing P1
first makes the pool's remaining prize 0.2 ms, which is not worth an unsafe module yet.

### P2, make the common tick pay for no passes at all

Even with P1 perfect the rebuild is 4 ms for a million bodies, and 55% of the phase-5 budget target is unreachable that
way. The larger prize is not doing the work: **most tick-to-tick motion does not change any body's cell**.

Add a `dirty` path to `SpatialGrid`:

- Keep `cells` from the previous rebuild (it is already a reused buffer).
- `fill` computes the new cell per body and compares. It writes a `changed` bitset and counts per-cell departures and
  arrivals. This is one pass over 1M bodies and 5 MB of traffic, and it already exists as the `clear` row at 0.642 ms.
- If no body changed cell, `cell_start`, `cell_cursor` and `order` are **still valid** and the rebuild ends after fill.
  Determinism is preserved because the order inside each cell is unchanged, and the id tie-break that orders a cell is a
  property of the membership, not of when it was computed.
- If bodies changed, one of two patches: below a threshold (say 1% of bodies or 5% of any cell), remove the departed ids
  from their old runs and splice arrivals in, in id order, which is O(run) per changed cell; above it, run the full P1
  path.

**Cost model in the steady state.** A crowd walking across a 1 km volume at 2 m/s crosses a cell boundary rarely: at 8 m
cells, the crossing rate per body is roughly `speed / cell / 3` per tick, about 0.08 of a body per tick per thousand. The
common tick is then fill alone, **0.642 ms**, which is 6.6x the current rebuild and inside the phase-5 target with room for
the serial tail. The patch path needs its own measurement before that number is claimed for a moving crowd.

**Where the risk is.** The patch path has to keep three invariants that the full rebuild gets for free: every body appears
exactly once in `order`, each cell's run is sorted by id, and `cell_cursor[c]` ends at the run's start rather than at its
end. The byte-equality harness is the tool for this: build with the incremental path, and compare `cell_start`,
`cell_cursor` and `order` against a full rebuild of the same positions after every tick of a scripted 10,000-tick walk. A
one-tick divergence fails there rather than in a game.

## Implementation status

`BlockPrefix` is the primitive P1 needs. It is complete and tested on its own, but it is **not on this branch**:
it lives on `feat/p2-incremental-rebuild` with the incremental rebuild, because it is only usable once that work
opens the door to one dispatch. The description below is of the implementation on that branch:  three-state
packed slots (not ready, aggregate, inclusive prefix), `publish_total`, `base_of`, `resolve`, `total` and a `reset` that
re-arms the buffer without returning its memory. Eight unit tests hold it, including a concurrent one that publishes and
resolves from eight workers over 257 blocks and compares every block against the serial prefix, four seeds deep.

The failure worth recording came out of that test. Resolving a block whose predecessor has not counted yet has to wait,
and waiting with `core::hint::spin_loop` alone **deadlocks as soon as more workers run than there are cores**, because the
worker that owes the aggregate never gets scheduled: eight threads on six cores hung for ten minutes on the first run.
The wait is now 64 spins and then `std::thread::yield_now`. That is not a theoretical case here: the phase-5 sweeps
already drive sixteen workers on a six-core host, so the rebuild has to survive oversubscription, not merely tolerate it.

What is left for P1, in order:

1. Replace the grid's `block_totals: Vec<u32>` with a `BlockPrefix` sized by `block_count()`, and have
   `count_cells_parallel` call `publish_total` for each block in its own window as soon as that block's count is final.
   The serial count keeps its plain sum, since a single worker has no predecessors to wait on.
2. In `scan_and_place_parallel`, replace `self.prefix_bases()` and the `block_bases` slice with `base_of(lo_block)` and
   `base_of(hi_block)` resolved inside the scope, which is what lets count, scan and place share one dispatch. Retire
   `prefix_bases` and `needs_block_totals_for` along with the array.
3. Move the count into the scan's scope, so `build_measured` pays two spawns per rebuild instead of three.
4. Re-run the byte-equality matrix, which already covers `cell_start`, `cell_cursor` and `order` across workers
   {1, 2, 3, 8} × blocks {1, 128, 1024, MAX_BLOCKS}, then measure with `examples/build_passes --warm`.

## Correction: P1 cannot be finished in safe Rust as written

The look-back primitive works, and it is tested. Wiring it exposed a flaw in the decision above, and the flaw is
structural rather than a matter of effort.

`Splits::take` hands each worker its output slice for `order`, and it needs `begin` and `end`: the worker's first and
last output index. Today those come from `prefix_bases()`, computed serially **before** the scope, which is exactly why
the pre-split is legal. Under look-back, `begin` and `end` are not known until the count has finished, and the count now
happens inside the same dispatch as the place. A worker would have to split `order` at a runtime offset inside its own
closure, which safe Rust cannot express: the split has to happen on the thread that owns the buffer, before the closures
are formed.

Every escape from that is worse than the 0.2 ms it buys:

- **Split runs inside the closure with a raw pointer.** That is 0059's option A arriving through the back door: an
  `unsafe` hand-off module, for one dispatch.
- **Store `order` as `AtomicU32`.** Then runs can be handed over shared. It also puts an atomic load on every neighbour
  read in `cell_window`, which is the query path every frame, to save a hand-off that happens once.
- **Give each block a fixed-capacity run.** Capacity would have to be the population, multiplied by the block count.

So the three-scope floor stands, and P1's assumption that only `fill` against everything-else resists fusion was wrong:
`place` against `scan` resists it too, for a different reason. The primitive stays in the tree, tested, because a parked
pool would put the whole rebuild inside one dispatch where the hand-off is already `unsafe` and the split could be done
by the owner. It is not a reason to introduce that `unsafe` today for 0.2 ms.

P2 is therefore the next move, and it is also the larger prize by an order of magnitude: 0.642 ms against 4.283 ms on the
ticks where no body changes cell, with no threading change at all. Its verification design is ADR 0064.

## Rejected or deferred

- **Per-block mutexes for a shared histogram** (0059 option B): re-rejected. A million-body count touches every cell, and
  a lock per block chain would put a bounds check and an atomic at every seam.
- **`bevy_tasks::ComputeTaskPool`**: still 13 µs a round and still allocates a task per spawn, against the no-allocation
  promise that `tests/crowd_no_alloc.rs` asserts. Rejected on the same grounds as 0059.
- **Sorting entries cell-major before the build** so that fill and count fuse per worker: tempting, and it would remove
  scope A as well, but the sort is O(n log n) and is exactly what the caller does after the build today. It would also
  change the observable order of `bodies()`, which is a published behaviour. Deferred until P1 and P2 are measured.
- **Prefetching the scatter in `place`**: worth trying inside P1's scope, since `place` is 45% of the rebuild and its cost
  is a dependent cursor load per body. Not a design decision, just a kernel to A/B under the same parity harness.

## Gates

P1 is done when: byte-equality holds across workers {1, 2, 3, 8} × blocks {1, 128, 1024, MAX_BLOCKS} including
`cell_cursor`; `cargo test --example bandwidth` and the spatial suite are green; the no-allocation tests still pass; clippy
`-D warnings` is clean with no new `allow`; and `examples/build_passes --warm` shows a total below 4.10 ms at six workers,
recorded in `docs/dev/tuning/performance.md` with the run-to-run spread beside it.

P2 is done when: the 10,000-tick walk matches a full rebuild byte for byte every tick; a 1M crowd at 2 m/s shows a
steady-state rebuild under 0.7 ms in the harness; the patch path is measured on a scripted crowd that crosses boundaries
deliberately (so the threshold is chosen from data); and the incremental path is behind a feature or a config flag, because
it changes the cost model a caller sees rather than only the speed.
