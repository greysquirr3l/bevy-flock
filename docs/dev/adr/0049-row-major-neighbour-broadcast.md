# 0049 — Row-major neighbour broadcast, and what the lint spine cost

- **Status:** Accepted
- **Date:** 2026-11-05
- **Supersedes:** the neighbour-read half of [0030](0030-transposed-simd.md) (the transposed buffers
  remain the right shape for the *lane* gather; they were the wrong shape for the *broadcast*).
- **Related:** [0024](0024-layout-ordering-and-parity.md), `plan.toml: kernel-bounds-checks`,
  [docs/dev/lint/lint-policy.md](../lint/lint-policy.md)

## Context

`application::crowd` steered four agents at a time out of seven parallel `Vec<f32>`s — `x`, `y`, `z`,
`vx`, `vy`, `vz`, `agent` — one array per component, filled in cell-major order. ADR 0030 chose that
layout for the *gather*: four agents of a cell load one component into four lanes without shuffles.

Then `plan.toml: kernel-bounds-checks` set out to remove the last two `#[expect(clippy::indexing_slicing)]`
attributes in the crate, which covered 62 index expressions in the two steering kernels — 44 in
`application::packed`, 18 in the wide kernel of `application::crowd`. The mechanical conversion is
`buffer[index]` → `buffer.get(index)`, which reads as free: a subscript already emits a bounds check,
so the `Option` arm ought to compile to the same branch.

It did not. Measured with Criterion against a saved baseline on this machine (10,000 agents, the
`crowd_kernel` group):

| Case | `get` per subscript, transposed | Change |
| --- | --- | --- |
| `crowd_kernel/planar` | 2.208 ms | **+9.1%** |
| `crowd_kernel/dense` | 25.61 ms | **+18.5%** |

Both cases have agents packed into a plane, so cells are full and the neighbour loop dominates.
`volumetric`, which spreads agents over 20 m in Y, moved +1.5%. A regression that appears only when
cells are full is a regression in the inner neighbour loop, not in the block setup or the tail.

## What the measurements say

Three variants were built and measured, one at a time:

1. **Per-neighbour `get` on the transposed buffers** (the mechanical conversion): +18.5% on `dense`.
   Seven `Option<&f32>` in a tuple pattern, each with its own exit branch, is not the same thing as
   seven subscripts whose checks the optimizer can merge and hoist.
2. **One row buffer, per-neighbour `get`**: still +19.1%. So it was never the layout — it was the
   *check in the loop*.
3. **One row buffer, one slice per cell**: 21.71 ms on `dense`, **+0.5% against the baseline**, and
   `volumetric` **−2.1%**. This is the shape that ships.

The bucket already knows its range: `begin..end`. Slicing that range once per cell moves the bound
out of the neighbour loop entirely, so the loop body is one contiguous walk over 28-byte rows with no
checks at all — strictly less work than the seven subscripts it replaced.

That also settles the layout question in the other direction. With the check hoisted, the row buffer
is *faster* than seven parallel arrays (`volumetric` −2.1%): the scan reads one line per neighbour
instead of touching seven streams, and `pack_soa` writes one store per agent instead of seven. The
transposed arrays were optimising for the side of the loop that runs once per block, not the side
that runs once per neighbour.

## Decision

- `Soa` holds `rows: Vec<SoaRow>` — 28 bytes per agent, position/velocity/identity together, in
  cell-major order. `SoaRow` is `#[repr(Rust)]` but is layout-stable in practice: three `[f32; 3]`s
  and an `f32`, align 4, no padding.
- The neighbour scan slices the bucket once (`rows.get(begin..end)`) and iterates. No per-neighbour
  bounds check, no indexing, no `Option` in the loop.
- The four lane records are unpacked with a `lane!(0)..lane!(3)` macro: a *literal* index into a
  `[f32; 4]` is exempt from `indexing_slicing` (the length is known), so the tail needs neither a
  loop that has to prove a bound nor a helper that returns ten vectors through the ABI.
- The scan and the write-back live in one function. Splitting the scan out meant returning a
  160-byte struct of ten vectors, which is a stack round-trip per block; measurements at the time
  showed no benefit from the split, and the merged form is what shipped.

## Consequences

- `application::crowd` carries no lint override, and neither does `application::packed`. The deny
  list applies to the kernels like everywhere else.
- Memory: 28 bytes per agent in `Soa`, the same as the seven arrays it replaces. One allocation
  rather than seven.
- `pack_soa` is cheaper: one store per agent instead of seven.
- The wide kernel is now at parity or slightly ahead of where it started (`dense` +0.5%,
  `volumetric` −2.1%, `crowd_avoidance/steer_only/100000` −4.7%), so the lint migration cost nothing
  in the end — but it cost 19% for two iterations, and that is the lesson recorded here: *a bounds
  check moved into an inner loop is not free, and `get` at the top of a hot loop is a different
  change from `get` at the top of a function.*
- Anyone re-profiling the kernel should check `crowd_kernel/dense` first. It is the case that
  saturates buckets, and it is the case that moved when the loop shape changed.
