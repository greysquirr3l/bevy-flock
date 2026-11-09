# 0059, Who owns the buffers a parked worker writes

- Status: Accepted
- Phase: 5, `persistent-worker-pool` (T05)
- Supersedes nothing; amends the plan's assumption that T05 is a straight swap

## Context

The rebuild pays three `std::thread::scope` dispatches per tick, clear, count, place, and phase 5 priced one at
**0.53 ms** with an empty body (`examples/spawn_cost.rs`, median of 15 samples of 200 dispatches). Three of them is
**1.59 ms of a 6.7 ms rebuild**, 24%, spent before any pass does work.

T05 asks for a pool that parks its workers between rebuilds, so that cost is paid once per grid instead of once per
pass. That is easy to describe and hard to type: every pass writes into buffers the grid owns, and a thread that
outlives a call cannot hold a borrow of them.

The specific problem is aliasing, not lifetimes alone. Within one scope, the passes want:

| pass | reads | writes |
| --- | --- | --- |
| clear | `entries` | `cells[slices of bodies]` |
| count | `cells` (all of it), `entries` | `cell_start[windows]` |
| prefix scan | `cell_start`, `block_bases` | `cell_start`, `cell_cursor` |
| place | `cell_start` (boundaries only), `cells` | `cell_cursor`, `order[runs]` |

Two of those rows are irreconcilable in safe Rust inside a single scope: `clear` needs `&mut cells` while `count`
needs `&[cells]`. Even without persistence, that is why the current design has three scopes, each scope
re-establishes the borrows from scratch, and the borrow checker is what guarantees no pass overlaps another. Removing
the scopes means removing that guarantee, and something has to replace it.

## Options

### A. Hand-rolled unsafe shared slices

Workers are spawned once per grid and parked on a condvar. `rebuild` hands each worker a `'static`-laundered slice of
the buffers (`&mut [u32]` derived from a raw pointer), signals it, waits at a barrier, and takes the borrow back
before returning.

- **What it buys:** the full 1.59 ms. Dispatch becomes three barrier rounds, measured at **51 µs each** by the same
  harness (one scope + `Barrier` per pass), so the rebuild goes 6.7 → ~5.3 ms.
- **What it costs:** one module of `unsafe` whose correctness depends on a discipline the type system no longer
  enforces, every worker must be idle when `rebuild` returns, and a panic in a worker must not leave a slice aliased.
  A bug here is not a wrong number; it is undefined behaviour, and the crate's lints (`cast_ptr_alignment`,
  `raw_borrows_via_references`) exist precisely because this class of bug has already bitten it once.
- **Mitigations that would be required:** `Drop` that joins every worker; a panic guard that poisons the pool instead
  of unwinding mid-borrow; the parity harness run twice per test; Miri over the parity test with the pool enabled
  (Miri models threads); and a `# Safety` section per unsafe block naming the invariant.

### B. Interior mutability

`Arc<Mutex<Buffers>>` shared with each worker: safe, no laundering, threads can be `'static`.

- **Why it fails at this shape:** the hot loops need *contiguous* `&mut [u32]` over a worker's range. One mutex
  means one worker at a time, strictly worse than the status quo, since the passes would serialise behind a lock they
  currently parallelise for free.
- The variant that does parallelise is one lock per block (`Vec<Mutex<[u32]>>`), which is safe and would let 103 blocks
  be worked on concurrently. But a worker's range spans many blocks and a contiguous window becomes a chain of
  separately-locked slices: the scan's forward walk, `place`'s cursor advance and `count`'s window sum all become
  per-block lock/unlock with a bounds check at every seam. At 4096 blocks × 3 phases that is ~12k lock acquisitions
  per rebuild, ~0.3 ms of uncontended `Mutex` traffic, plus the loss of the contiguous-slice code paths the SIMD
  kernels take. It buys safety with the thing being optimised.
- It also changes the data layout (`Vec<Vec<u32>>` or a fixed array of blocks), which would leak into `cell_start`
  being one array for the rest of the crate: `cell_window`, `place`, and the byte-equality tests all assume a single
  contiguous table.

### C. Safe, and one scope cheaper: fuse the passes that do not conflict

The aliasing table above has exactly one bad row pair: `clear` (writes `cells`) against `count` (reads `cells`). Every
other pair is compatible, `count`, the block scan and `place` all read `cells` and write *disjoint ranges* of
`cell_start`, `cell_cursor` and `order`, which is expressible in safe Rust as one `&mut` slice per worker, split once
before the scope.

So: **two scopes instead of three.** Scope 1 is `clear`. Scope 2 carries `count` → barrier → block scan → barrier →
`place`, with each worker holding its own disjoint slices throughout and the O(blocks) block bases computed between
barriers (a worker can derive its own base by summing `block_totals[..lo]`, which is `O(blocks)` of shared reads and
needs no shared mutable state).

- **What it buys:** one scope, 0.53 ms, less the two barrier rounds it adds (~0.10 ms). Rebuild ≈ **6.2 ms**.
- **What it costs:** a restructure of `build_measured`, no `unsafe`, no API change, and no change to what any pass
  computes, the byte-equality matrix covers it exactly as it covers the block split.

### D. `bevy_tasks::ComputeTaskPool`

13 µs a round, 4x cheaper than a barrier, 40x cheaper than a scope, and already a dependency of the ECS half. But
its `scope` boxes a task per spawn: 9 allocations per round, against the zero steady-frame allocations that
`tests/crowd_no_alloc.rs` and `tests/spatial_batch_no_alloc.rs` assert. Rejected on the no-allocation promise, which
is a stated feature of the crate rather than an incidental property.

## Decision

**Adopt C now. Defer A behind a feature flag, and only if the remaining 0.53 ms is worth an `unsafe` module.**

The reasoning is about what each option risks rather than what each one wins. C is a restructure of code that already
exists, checked by tests that already exist, and it takes two thirds of the dispatch tax. A takes the last third , 
0.53 ms, or 8% of the rebuild, in exchange for a hand-written aliasing guarantee in a crate whose lints were
tightened precisely because that class of bug had shipped before. That trade is worth making only with a plan for
proving it (Miri over the parity test, a pool `Drop` that joins, panic poisoning) and a measurement showing the third
scope is the binding constraint. It is not worth making as a side effect of a phase task.

**B is rejected outright**, not deferred: one mutex serialises the passes, and per-block mutexes pay for their safety
in the exact loops the phase is trying to speed up.

## Consequences

- T05 lands as a two-scope rebuild, with the barrier discipline documented at the call site. Measured targets:
  dispatch 1.59 → 1.06 ms, rebuild 6.7 → ~6.2 ms.
- The pool, if it ever happens, is a `pool` feature with a `# Safety` block per unsound operation, a Miri job in CI,
  and the parity harness run twice in the same binary (a pool that leaks state between rebuilds shows up as a
  difference between run one and run two, not as a failure of either).
- The 5x gate has to be re-derived regardless: with dispatch at zero the build would be ~5.1 ms against a 4.72 ms
  target, so `build-5x-gate` measures a decomposition whose remaining costs are `clear` (2.46 ms, 5.3x) and `place`
  (2.26 ms, 3.2x), not the thread hand-off. Naming that is the gate's job; assuming the pool would close it is not.

## Outcome (T05, as built)

The two-scope design was implemented and is faster, but not for the reason this record expected, and the difference
matters more than the 0.2 ms.

**The barrier was wrong and the measurement said so.** The first implementation fused the scan and the placement into
one scope and put a `Barrier` between them, on the reasoning in §C: a worker cannot place until every block below its
own has been scanned. That reasoning is wrong. A worker fills the windows of *its own* cells `[lo, hi)`, and every
start in that range depends only on counts below `hi`, which it has just scanned itself, on top of the base
`prefix_bases` gave it. The base is already the sum of the preceding blocks, so there is nothing to wait for.
Measured, the barrier version was **slower than what it replaced**: 6.74 → 7.42 ms at six workers, with `place` up
27% and `prefix` up 49%. Waking six threads off a condvar cost more than the `thread::scope` it removed.

Without the barrier the same fusion costs nothing to synchronise and the win appears:

| | before (two dispatches) | after (one) |
| --- | ---: | ---: |
| `prefix` (scan) | 0.235 ms | **0.110 ms** |
| `place` | 2.147 ms | 2.044 ms |
| total, six workers | 6.711 ms | **6.488 ms** |
| total, one worker | 22.827 ms | 22.692 ms |

Thirteen alternating pairs against the previous binary, medians; the single-worker column is the control, since it
takes the serial path and only changes by noise.

**The dispatch was overpriced by a factor of two.** The phase 5 harness priced one `thread::scope` at 0.53 ms with an
empty body. In situ it is worth about 0.2 ms: an empty scope pays the full spawn and join with nothing to overlap,
while a scope with work in it overlaps the cost of the last thread's creation with the first threads' work. The
0.53 ms figure should be read as an upper bound on dispatch and never as the saving available from removing one;
`examples/spawn_cost.rs` is what to fix if that number is ever quoted again.

**A second cost hid inside the first attempt.** The scan was written per cell, `split_at_mut_checked(1)` around the
bookkeeping for each of 421,876 cells, which put `prefix` at 0.349 ms against the serial 0.235. The accumulation is
one contiguous pass over a slice, `*slot = slot + running`, exactly as before; the per-cell version was paying for
arithmetic the block-local loop had already hoisted.

**Placement keeps a block-aligned range** because the worker holding that slice of the cursor table has to be the one
advancing it, and the cursor slices are handed out with the same block partition as the scan. The parity test now
compares `cell_cursor` as well as `cell_start` and `order` at 2, 3 and 8 threads: each worker writes the cursor slots
for its own cells, so a boundary slot written by the wrong worker fails there instead of quietly misplacing bodies.

`bevy_tasks` (option D) was not needed; the unsafe pool (option A) is still unclaimed, and the measured value of the
dispatch it would remove is 0.2 ms rather than 0.53, a third of what this record assumed, which makes the case for a
hand-written aliasing invariant weaker than it already was.
