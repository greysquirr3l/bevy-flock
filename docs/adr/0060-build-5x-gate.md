# 0060 — Is the 5x build gate reachable, and what would it take

- Status: Proposed — decides what T06 may claim
- Phase: 5, `build-5x-gate` (T06)
- Depends on: 0055 (blocked scan), 0057 (fused count/scan), 0059 (one dispatch for scan and place)

## Context

The phase 5 plan inherited a target: a 1M-body rebuild at six workers in **4.72 ms**, which is 5x against the serial
build. After T03, T04 and T05 the rebuild is **6.29 ms** — 3.65x — and the dispatch it was chartered to remove is
gone. T06 has to decide whether to chase the remainder, re-derive it, or declare it met under a different rule.

That decision needs the per-pass scaling curve rather than one number at full width, because two of these passes fail
the target for different reasons and one of them is nearly at its floor.

## Measurement

`examples/build_passes.rs` grew a `--workers 1,2,3,4,5,6` knob for this; 1M bodies, `cell = 8`, 421,875 cells,
medians of three runs of seven rebuilds each, on the six-core sandbox (`aarch64`, no GPU, nothing else resident).

| pass | 1w | 2w | 3w | 4w | 5w | 6w | speedup at 6w |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `clear` | 13.034 | 6.799 | 4.588 | 3.521 | 2.882 | **2.463** | **5.29x** |
| `count` | 2.088 | 2.109 | 2.003 | 1.863 | 1.747 | **1.730** | **1.21x** |
| `prefix` | 0.424 | 0.294 | 0.190 | 0.150 | 0.127 | **0.112** | 3.77x |
| `place` | 7.396 | 3.128 | 2.668 | 2.272 | 2.068 | **1.984** | 3.73x |
| **total** | 22.943 | 12.330 | 9.448 | 7.806 | 6.824 | **6.289** | **3.65x** |

The gate needs 4.59 ms at six workers. The gap is **1.70 ms**, and it is not spread evenly: `clear` and `place` are
4.45 ms of the 6.29, `count` is 1.73, `prefix` is 0.11.

## Where the time actually goes

### `clear` is compute-bound, and it is doing three divisions per body

`fill_cells_*` calls `cell_of_parts` once per body, and that is `(position - min) / cell_size` — three f32 divisions —
followed by three `clamp_axis` calls, each with a finiteness test and two comparisons. A million bodies is three
million divides and three million branchy clamps.

Two independent readings say that is the cost and not the memory traffic. First, it scales at 5.29x on six threads,
which is what a compute-bound pass looks like when it partitions cleanly (compare `count`, which is memory-bound and
manages 1.21x). Second, the traffic it does move — 16 MB of `entries` read, 4 MB of `cells` written — comes to 8.1 GB/s
against the 14 GB/s that `count`'s scan demonstrates this machine can sustain, so the bandwidth is not the constraint.

The fix is not speculative: replace the division by a precomputed `inv_cell_size` multiply, make the clamps
branchless (`min`/`max` with the finiteness test hoisted into one comparison), and widen the kernel over four bodies
the way the steering kernels already are (ADR 0023/0024). Estimated 2.46 → **1.2–1.5 ms**.

### `count` and `place` are memory-bound on a full rescan each

Both partition by *cell range*, so every worker walks the whole 1M-entry `cells` list and filters. At six workers that
is six 4 MB scans — 24 MB — to place 4 MB of bodies. `count` manages 1.21x and `place` 3.73x, and the difference
between them is that `place`'s writes are to its own `order` run while `count`'s increments scatter into a window table
that is only L1-sized per worker but reached at random.

This redundancy is intrinsic to the cell-partitioned design, and that is the whole reason it cannot simply be tuned
away:

- **Body-partitioned with atomics** removes the rescan and costs a million atomic increments (2.4 per cell). For
  `count` the sums are exact integers, so atomics are still deterministic — but `place` needs the *order* within a
  cell, and atomic order is not reproducible. That is disqualifying for the byte-parity promise.
- **A bucketing pass** (counting-sort bodies by block, so each worker walks only its own) costs a scattered 1M-entry
  write and read — approximately the rescan it replaces.
- **Per-worker histograms** work for `count` alone: 6 × 1.7 MB of scratch, zeroed and merged. Estimated 1.73 → 1.0–1.3
  ms, at 10 MB of extra memory and one more merge pass.

### `prefix` no longer matters

0.112 ms at six workers, 1.8% of the rebuild. T03 and T05 between them took it from 0.42 to 0.11. Any further work
here is unmeasurable.

## Verdict

**5x is not reachable by optimising `clear` and `place` alone, and it is only marginally reachable with everything.**

| scenario | total | speedup |
| --- | ---: | ---: |
| today | 6.289 ms | 3.65x |
| `clear` vectorised (2.46 → 1.3) | 5.13 ms | 4.47x |
| `clear` vectorised + `count` by body (1.73 → 1.15) | 4.55 ms | 5.04x |
| …and `place` 10% better | 4.35 ms | 5.27x |

The middle row is the honest target: it lands *on* the gate, which means any run with a slower machine, a busier host,
or a different cell size misses it. A gate that is met by 0.05 ms is not a gate, it is a coin flip.

Two things follow, and they should both be in T06 rather than one chosen on the plan's behalf:

1. **Re-derive the target.** Either 4.5x at six workers (5.1 ms — met by the `clear` fix with margin), or keep 5x but
   measure it at the core count where it is achievable (the parallel passes scale to 8+; this sandbox has six).
2. **Take the `clear` fix regardless.** It is a division replaced by a multiply, it is worth ~1.2 ms — more than every
   dispatch this phase removed — and it needs no design change, no scratch memory and no new unsafety.

`place`'s rescan is the structural item, and it should be scoped as its own task with its own ADR (a bucketed
body order that survives a rebuild, or a placement that consumes one), not folded into a gate.

## What this ADR does not claim

- The 14 GB/s bandwidth figure is inferred from `count`'s scan, not measured with a bandwidth kernel.
- The `clear` estimate is arithmetic over the divisions and clamps, not a prototype. It should be measured before T06
  is allowed to claim anything on top of it.
- No measurement was taken on a machine with more than six cores, which is the configuration where a 5x gate would be
  least contentious.
- The 6.289 ms baseline here and the 6.488 ms in `docs/performance.md` are the same binary on the same machine: the
  former is today's three-run sweep, the latter the alternating-pairs comparison that produced the T05 record. The 3%
  between them is run-to-run spread on this sandbox, and it is the same order as the margin this gate is being argued
  over — which is part of the case for re-deriving it rather than defending it.

## Outcome

The `clear` fix landed: a cached reciprocal instead of three divisions, and a `clamp_axis` that needs no branch per axis.
`clear` went 13.034 → 2.767 ms serially and 2.463 → 0.739 at six workers, taking the rebuild from 6.289 to **4.572 ms**.

Against the pre-fix serial build that is **5.02x**, so the gate as written is met. Against the *new* serial build
(12.229 ms) the same binary is 2.67x, because a third of the serial work stopped existing. This is the clearest possible
argument for the re-derivation this ADR recommended: the ratio falls while the wall clock improves, and which number a
gate names decides whether the work looks like a success.

The estimate in this record was right in direction and wrong by half: it predicted 2.46 → 1.2–1.5 ms from a probe, and
the real change delivered 0.74 ms. The probe was wrong because it called the kernel through a `fn` pointer, which
prevents inlining and inflated every variant it measured except the one dominated by divisions — the one being compared
against. Probes written to price a kernel have to inline it.

**`count` was analysed and the recommended fix was measured and rejected.** Per-worker histograms with a merge are
4.90 ms at six workers against today's 1.66, and fusing `clear` into that structure is 8.90. Today's layout — every
worker scanning all bodies, but incrementing an L2-resident window it owns — is at the machine's bandwidth (24 MB at
14 GB/s) and wins on locality. `count`'s 1.21x is the price of that locality, not a scheduling defect. The remaining
option is a bucketing pass that pays 12 MB instead of 24, which would help `place` more than `count`, and it belongs in
its own record.
