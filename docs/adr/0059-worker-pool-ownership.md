# 0059 — Who owns the buffers a parked worker writes

- Status: Accepted
- Phase: 5, `persistent-worker-pool` (T05)
- Supersedes nothing; amends the plan's assumption that T05 is a straight swap

## Context

The rebuild pays three `std::thread::scope` dispatches per tick — clear, count, place — and phase 5 priced one at
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
needs `&[cells]`. Even without persistence, that is why the current design has three scopes — each scope
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
  enforces — every worker must be idle when `rebuild` returns, and a panic in a worker must not leave a slice aliased.
  A bug here is not a wrong number; it is undefined behaviour, and the crate's lints (`cast_ptr_alignment`,
  `raw_borrows_via_references`) exist precisely because this class of bug has already bitten it once.
- **Mitigations that would be required:** `Drop` that joins every worker; a panic guard that poisons the pool instead
  of unwinding mid-borrow; the parity harness run twice per test; Miri over the parity test with the pool enabled
  (Miri models threads); and a `# Safety` section per unsafe block naming the invariant.

### B. Interior mutability

`Arc<Mutex<Buffers>>` shared with each worker: safe, no laundering, threads can be `'static`.

- **Why it fails at this shape:** the hot loops need *contiguous* `&mut [u32]` over a worker's range. One mutex
  means one worker at a time — strictly worse than the status quo, since the passes would serialise behind a lock they
  currently parallelise for free.
- The variant that does parallelise is one lock per block (`Vec<Mutex<[u32]>>`), which is safe and would let 103 blocks
  be worked on concurrently. But a worker's range spans many blocks and a contiguous window becomes a chain of
  separately-locked slices: the scan's forward walk, `place`'s cursor advance and `count`'s window sum all become
  per-block lock/unlock with a bounds check at every seam. At 4096 blocks × 3 phases that is ~12k lock acquisitions
  per rebuild — ~0.3 ms of uncontended `Mutex` traffic — plus the loss of the contiguous-slice code paths the SIMD
  kernels take. It buys safety with the thing being optimised.
- It also changes the data layout (`Vec<Vec<u32>>` or a fixed array of blocks), which would leak into `cell_start`
  being one array for the rest of the crate: `cell_window`, `place`, and the byte-equality tests all assume a single
  contiguous table.

### C. Safe, and one scope cheaper: fuse the passes that do not conflict

The aliasing table above has exactly one bad row pair: `clear` (writes `cells`) against `count` (reads `cells`). Every
other pair is compatible — `count`, the block scan and `place` all read `cells` and write *disjoint ranges* of
`cell_start`, `cell_cursor` and `order`, which is expressible in safe Rust as one `&mut` slice per worker, split once
before the scope.

So: **two scopes instead of three.** Scope 1 is `clear`. Scope 2 carries `count` → barrier → block scan → barrier →
`place`, with each worker holding its own disjoint slices throughout and the O(blocks) block bases computed between
barriers (a worker can derive its own base by summing `block_totals[..lo]`, which is `O(blocks)` of shared reads and
needs no shared mutable state).

- **What it buys:** one scope, 0.53 ms, less the two barrier rounds it adds (~0.10 ms). Rebuild ≈ **6.2 ms**.
- **What it costs:** a restructure of `build_measured`, no `unsafe`, no API change, and no change to what any pass
  computes — the byte-equality matrix covers it exactly as it covers the block split.

### D. `bevy_tasks::ComputeTaskPool`

13 µs a round — 4x cheaper than a barrier, 40x cheaper than a scope — and already a dependency of the ECS half. But
its `scope` boxes a task per spawn: 9 allocations per round, against the zero steady-frame allocations that
`tests/crowd_no_alloc.rs` and `tests/spatial_batch_no_alloc.rs` assert. Rejected on the no-allocation promise, which
is a stated feature of the crate rather than an incidental property.

## Decision

**Adopt C now. Defer A behind a feature flag, and only if the remaining 0.53 ms is worth an `unsafe` module.**

The reasoning is about what each option risks rather than what each one wins. C is a restructure of code that already
exists, checked by tests that already exist, and it takes two thirds of the dispatch tax. A takes the last third —
0.53 ms, or 8% of the rebuild — in exchange for a hand-written aliasing guarantee in a crate whose lints were
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
