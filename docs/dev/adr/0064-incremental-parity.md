# 0064, How an incremental rebuild earns the right to skip passes

- Status: Proposed
- Phase: 6, verification for 0063 P2
- Depends on: 0057 (the fused count and scan), 0063 (dispatch elimination, incremental rebuild)

## Context

0063 P2 ends the rebuild after `fill` on any tick where no body changed cell, and patches runs when a few did. That is the
only path to the phase-5 target, and it is also the first change in this crate whose *common* case does very little. An
incremental rebuild that silently produces a slightly stale grid is worse than a slow one: the crowd keeps moving, queries
keep returning neighbours, and the defect surfaces as an NPC that walks through a wall an hour later.

The full rebuild earns trust by being one code path, checked by a byte-equality matrix against the serial scan at 1, 2, 3
and 8 workers across four block counts. The incremental path cannot inherit that. It has to be checked against the full
rebuild, per tick, with a failure that names the tick it broke on.

## What can go wrong

The failure modes are specific, and each one suggests a check:

| failure | how it shows | what catches it |
| --- | --- | --- |
| a departed body left in `order` | population grows past `bodies`, or a duplicate id in one cell | multiset equality against `0..n` |
| an arriving body spliced at the wrong offset | a cell's run no longer sorted by id, so query tie-breaks drift | per-cell sortedness |
| `cell_cursor[c]` left past the run start | placement writes over the next cell's first slot | `cursor == start + count` for every cell |
| a cell that emptied keeps a stale run | stale neighbours in a query, pop-in at boundaries | `cell_start` rebuilt from `order` and compared |
| the changed-count threshold flips mid-tick | patch and full disagree by one body | differential test at threshold ±1 |
| boundary positions clamped differently | a body in the wrong cell, once, at the volume edge | scripted walk on the clamp planes |
| cell-of recomputed with different rounding | the same body reported in different cells across paths | both paths use `cell_of_parts`, asserted by construction and by test |

## The oracle

The reference is the **existing full rebuild**, from the same positions, in the same process. Not a second implementation
of the incremental path, and not a model: a model would need its own proof. The full path already has one, and its output
is a byte string.

```
for tick in 0..TICKS:
    move bodies (scripted)
    incremental = grid.rebuild_incremental()          # the subject
    reference   = SpatialGrid::new(config).build(&positions)   # the oracle
    assert bytes_eq(incremental.cell_start, reference.cell_start)
    assert bytes_eq(incremental.cell_cursor, reference.cell_cursor)
    assert bytes_eq(incremental.order, reference.order)
```

Three byte comparisons, on arrays whose contents are already the contract of the byte-equality matrix. The cursor is
compared because a patch that advances it too far passes a `cell_start` comparison and fails only when something places.

## Harness

`crates/bevy-flock-core/tests/incremental_parity.rs`, one test per script, driven by a seeded generator so any failure is a
number rather than a mystery.

- **Confined** (default, runs in CI): 20,000 bodies, 10,000 ticks, default cell size, one worker and six. The cost is
  bounded by the tick count, not the population, because the oracle rebuild is the only O(n) work and it is the same work
  the suite already does elsewhere.
- **Wide** (`#[ignore]`, run nightly or on demand): 1,000,000 bodies, 500 ticks, workers {1, 2, 3, 8} × blocks {1, 128,
  1024}. Population is what exercises run splices in dense cells, so it cannot be skipped entirely, but it is too slow for
  every push.

Scripts, because "random motion" will not reach the corners:

1. **Still.** No body moves for 10,000 ticks. Every tick takes the fast path, and the test asserts that it did, so a
   correct-but-never-incremental regression fails here rather than passing quietly.
2. **Every crossing.** Every body is displaced by one cell each tick. The threshold forces the full path; the test asserts
   agreement and that the patch path was *not* used.
3. **One crosser per tick.** Exactly one body crosses each tick, cycling through the population. This is the patch path's
   steady state, the case between the two extremes, and the one an idle crowd actually produces.
4. **Oscillation.** Bodies straddling one boundary move back and forth a sub-cell distance, so they change cell on roughly
   half the ticks. A patch that mishandles a body leaving and returning to the same cell fails here.
5. **Dense cells.** 20,000 bodies confined to a volume of twenty cells, so every run is a thousand long and each splice is
   a thousand-element move. Correctness first; the timing is recorded so the patch's O(run) cost is known rather than
   assumed.
6. **Clamp planes.** Bodies walking along and across the six faces of the volume, where `cell_of` clamps, plus bodies that
   leave and re-enter it.
7. **Threshold edge.** A script that produces exactly the configured change count, and one either side of it, so the
   decision between patch and full rebuild is tested at its boundary and cannot depend on a rounding accident.
8. **Empty and refill.** Every body leaves a cell, and later they all come back. A cell whose run length returns to zero
   and then grows is the case where a stale `cell_cursor` hides.

## Invariants asserted inside the path

The byte comparison is the gate, but a failure should say *what* broke. In debug builds the incremental path runs its own
audit after patching, behind `debug_assert!` so release pays nothing:

- `order` is a permutation of `0..bodies`, checked as a bitset rather than a sort.
- each cell's run is sorted by `NpcId`, which is the tie-break the serial path establishes.
- for every cell, `cell_cursor[c] == cell_start[c] + count(c)`.
- the sums of the patched counts equal the number of bodies.

`debug_assert!` is deliberate: the release path must not pay for assertions that the harness already provides, and the
crate's lint policy forbids the alternative of a test-only `allow`.

## Metamorphic checks that do not need an oracle

Two properties are worth testing because they catch classes of bug the byte comparison only catches when a script happens
to hit them:

- **Path independence.** Two different scripts that end at the same positions must produce byte-identical grids. A patch
  that carries history in its state (a count that drifts, a run that keeps a ghost) fails this even when each tick matched
  its reference.
- **Idempotence.** Rebuilding twice without moving anything must be a no-op the second time, byte for byte, and must take
  the fast path.

## Allocation and cost guards

- The incremental path must not allocate after warm-up. `tests/crowd_no_alloc.rs` and `tests/spatial_batch_no_alloc.rs`
  already assert that for the full path; the harness extends the same accounting counter to a 100-tick incremental run.
- The harness records the per-tick cost as a distribution, not a mean, and asserts that the still-crowd script's median is
  at least 4x cheaper than a full rebuild on the same machine. That is a regression guard on the *premise* of the design,
  not a performance gate on any particular number.

## Failure triage

A divergence at tick 8,124 in a 10,000-tick walk is worthless unless it can be replayed. On mismatch the harness writes a
reproducer next to the failure: the seed, the tick, the config, the positions of the bodies in the cells that differ, and
both byte arrays for the first differing cell block. `--exact <test> -- --nocapture` then replays it, and the dump names
the first divergence rather than the first symptom.

## Order of work

1. Invariants and audit inside the path, with the full-rebuild oracle and the **still** and **one-crosser** scripts. This
   is the smallest thing that can be right.
2. The remaining scripts, then the wide nightly configuration.
3. Threshold tuning, chosen from the harness's patch-versus-full timings rather than guessed.
4. Only then the feature flag comes off.

The flag stays on by default until step 4, because until the wide configuration runs green the crate has measured the
patch path only at a population where runs are short and splices are cheap.
