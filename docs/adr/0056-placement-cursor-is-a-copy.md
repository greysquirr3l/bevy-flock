# 0056 — The placement cursor is a copy, not the window table

- **Status:** Accepted
- **Date:** 2026-11-09
- **Supersedes:** the cursor-shape half of [0055](0055-blocked-prefix-sum.md) — its scan is unchanged.

## Context

`SpatialGrid::build` is a counting sort: count bodies per cell, turn the counts into window starts, then place
each body into its cell's window. Placement needs a write position per cell that advances as the window fills.

Since phase 3 that position has been the window table itself: `cell_start[cell]` held where the window began, the
placement pass incremented it once per body, and after placement every entry held where the window *ended*. A
final serial pass, `restore_cell_start`, shifted the whole array down by one cell to turn the ends back into
starts, so that a caller reading `cell_start` after a rebuild sees the table the scan produced.

That pass cost **0.68 ms** of a 7.48 ms build at a million bodies — 9% of the rebuild, and the larger half of the
0.98 ms serial tail phase 4 was opened to attack. Its cost was not the 1.7 MB it moved; it was that it read what
it had just written. Each iteration depended on the previous one, so the compiler could not vectorise it and the
CPU could not overlap it, and 421k dependent steps came out at ~1.6 ns each.

## Decision

**Placement advances a copy.** The grid holds `cell_cursor`, one `u32` per cell plus the sentinel, and `place`
reads and advances it. `cell_start` is written by the scan and never touched again.

The copy is a single `memcpy` (`seat_cursors`), not a loop, and it is timed as its own pass — `PassCosts::seat`,
which replaces `PassCosts::restore`.

## Why a copy and not a smarter restore

Three shapes were considered and two were measured:

1. **Shift as it is** — 0.68 ms, dependent chain, serial. Rejected on the measurement above.
2. **Parallelise the shift across blocks.** The blocked scan already knows each block's base, and a block's shift
   only needs the last value of the block before it, so the shift partitions cleanly. Rejected because it moves
   0.68 ms into six shares of a step that should not exist: the base *is* the value the shift is looking for, so
   the block could simply have started from it.
3. **Copy the starts and advance the copy** — 0.11 ms measured. Accepted.

The invariant this buys is the one worth more than the milliseconds: **`cell_start` after a rebuild is the scan's
output, not a repaired side effect of placement**. A test can state that postcondition and check it against an
oracle built from body positions alone (`placement_leaves_the_window_table_alone`); the old shape could only be
checked for internal consistency, because placement had been writing through the table it was checked against.

### The copy stays on one thread, and that is measured

Copying 1.7 MB across six workers was implemented and measured, not assumed away: **0.21 ms**, against 0.11 ms
serial — six `thread::scope` spawns cost more than the copy they share, and the copy is already running at
~15 GB/s. So this is the rebuild's one remaining `O(cells)` single-threaded step, which contradicts a must-have in
`plan.phase4.toml` and is recorded as a deviation rather than dressed up: the condition for revisiting it is a
worker pool that outlives a tick (the same follow-up phase 3 recorded for the scan's spawn cost), at which point
the copy's spawn cost goes to zero and the parallel version wins by construction.

> **Amendment, phase 5.** The paragraph above names the right cause and draws one conclusion too many. Phase 5
> priced the dispatch on its own — an empty `thread::scope` at six workers costs **0.53 ms** — so the parallel copy
> was not losing to the serial copy, it was buying a scope it did not need. A rebuild already pays for a scope to
> place the bodies, and the copy fits inside it: `seat_and_place_parallel` splits `cell_cursor` and `order` across
> the same workers and each copies its own slice of `cell_start`, which nothing writes and therefore stays a shared
> slice. This does not require the worker pool the paragraph above waits for, and it lands sooner than one.
>
> Measured by alternating two prebuilt binaries five times on one machine, because separate runs of this fixture
> vary by more than the effect: **7.010 ms → 6.671 ms, median, five pairs, five wins**. The saving exceeds the
> 0.11 ms the serial copy cost, which the measurement does not explain; the likeliest account is locality — the
> serial copy streamed 1.7 MB that the placement pass then re-read cold, while each worker now copies the slice it
> is about to walk. That is a hypothesis with a number attached, not a finding.
>
> `PassCosts::seat` is therefore zero on the parallel path and the copy is counted in `place`; `serial_share` now
> has one serial pass to report rather than two. The consequences above stand for the serial path, where the copy
> still runs on one thread.

## Consequences

- The rebuild's serial remainder is `prefix` (0.30 ms, bandwidth-bound per [0055](0055-blocked-prefix-sum.md)) plus
  `seat` (0.11 ms, a memcpy). At six workers the serial share went from 13.1% to 5.8%.
- A grid carries one more `Vec<u32>` of `cell_count() + 1` — 1.7 MB at 421,876 cells. It is allocated on the first
  rebuild and reused; a steady frame neither allocates nor memsets it, because a grid's cell count never changes.
- `PassCosts::restore` is renamed to `PassCosts::seat` and `serial_share` counts it. Anyone reading old numbers
  needs to know the row changed meaning, so `docs/performance.md` keeps the T03 table beside the T04 one.
- Placement output is unchanged, byte for byte: the fill still walks each window upwards in body order. The
  equivalence matrix in [0055](0055-blocked-prefix-sum.md) and the parity harness both still compare exactly.
- A caller who keeps a borrowed `cell_start` across a rebuild is now reading a table that cannot change under
  placement — though the borrow checker, not this ADR, is what enforces that.

## Rejected alternatives

- **Store window ends and adjust every query.** The array after the scan already reads as ends, so keeping it that
  way would also remove the restore. Rejected because it inverts the meaning of the crate's public prefix table
  and every window read (`cell_start[cell]..cell_start[cell + 1]`) would have to shift by one index at every call
  site, for the same saving as the copy.
- **Two arrays in parallel, `cell_start` and `cell_end`.** Doubles the per-cell memory to remove a 0.11 ms copy.
- **Recompute cursors per block from the block bases instead of copying.** A block knows its base but not each of
  its cells' starts, which is the rest of the block's prefix; recovering them is the scan again.
