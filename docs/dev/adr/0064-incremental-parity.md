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

## What step 1 changed in this design

Two amendments, both found by building the harness rather than by reading it.

The invariant audit is a **public method** (`SpatialGrid::check_invariants`) rather than a `debug_assert!` inside the
path. An assertion that allocates a bitset would break the no-allocation tests, which run in debug precisely because that
is where the counts are checkable, and it would run on every tick of every test that touches a grid. The harness calls the
audit after each scripted tick instead, which is stronger than one assertion at the end of a rebuild and costs the
production path nothing.

The harness is `spatial_incremental.rs`, a `#[cfg(test)]` child module of `spatial`, so it can read the buffers directly.
The alternative was three accessors on the public API of a hot type, existing to serve its own tests.

It paid for itself on the first afternoon. The fast path's first version decided "is this grid already built?" from buffer
lengths, and an empty grid passed that test: the constructor sizes the window table, so the length matched, and a cursor
that had never been written looked like a table that happened to be all zeroes. The audit reported `CursorMisplaced` on
the empty-grid script, which is exactly the quiet failure this record exists to catch. The grid now carries a `built`
flag, raised when the passes finish and cleared by `clear()`, and the empty-grid script pins the contract: the first call
rebuilds with `changed: 0`, the second takes the fast path.

## Step 2's layout, settled before writing it

Step 1's fast path only helps a tick where *nothing* moved. A tick where a few bodies moved still rebuilds, at four
percent over a plain rebuild, because a flat `order` array cannot absorb an insert.

`benchmarks/probes/patch_layout.rs` prices the candidates (table in `docs/dev/tuning/performance.md`). Patching the flat
layout is `O(k * n)`: 46.6 ms for 64 movers at a million bodies against a 15.7 ms rebuild, and 729 ms for 1,024 movers.
Patching a slotted layout, one with per-cell slack so an insert moves one run instead of the tail, is under a microsecond
at every `k` tested, including 1,024.

So step 2 is a layout change, and the record it needs is this one: `order` becomes a slotted array, `cell_start` holds
capacity offsets instead of run offsets, a new `cell_len` holds occupancy, and `cell_window` reads `start..start + len`.
That last detail is why this shape and not a delta overlay: the query path keeps slicing one contiguous run, so `radius`
and the neighbour loops do not gain an indirection.

The costs and policies step 2 must carry:

- Capacity is `count + count / 4 + 1`, decided at count time. The probe's `+ 4` floor wastes 6.7 MB at 2.4 bodies per
  cell; `+ 1` is enough for the common arrival.
- An arrival into a full run is an overflow. One is repaired by sliding that run's neighbours; more than a few percent of
  cells overflowing in a tick means the tick was not a patch, and the rebuild is cheaper than the repair.
- Slack is only reclaimed by a rebuild. `RebuildOutcome` gains a third arm for "patched", and the caller that cares about
  memory (a session with a shrinking population) compacts by rebuilding.
- The parity matrix compares `cell_start`, `cell_len` and `order` across worker counts, and the scripts compare runs
  against a rebuild cell by cell rather than byte for byte, because two correct rebuilds may lay slack out differently.

None of that is written yet. It is the next unit of work, and it is deliberately not folded into the fast path's commit:
it changes what `cell_start` means, and every test that asserts on it.

## Order of work

1. Invariants and audit inside the path, with the full-rebuild oracle and the **still** and **one-crosser** scripts. This
   is the smallest thing that can be right.
2. The remaining scripts, then the wide nightly configuration.
3. Threshold tuning, chosen from the harness's patch-versus-full timings rather than guessed.
4. Only then the feature flag comes off.

The flag stays on by default until step 4, because until the wide configuration runs green the crate has measured the
patch path only at a population where runs are short and splices are cheap.

## Step 2 delivered: the slot table and the patch

The slotted layout and the patch path landed together, because either alone is a regression. The layout is
`cell_start` (a prefix of capacities), `cell_len` (occupancy) and `cell_cursor` (the scan's scratch), with
`order` sized to the sum of capacities rather than to the population. The patch is two phases over the movers
the tick already found: every departure, then every arrival.

### What the probe decided

The layout was chosen by measurement rather than by preference (`benchmarks/probes/patch_layout.rs`). At
1,000,000 bodies and 64 moves per tick, a flat rebuild is 15.7 ms; the patch is 9.4 µs. The failure mode is a
full run: 64 random movers land in 64 distinct cells and the patch survives, but random arrivals cluster, and a
cluster of two in an empty cell exhausts the one spare slot it owns. That is why the capacity rule is a quarter
of the count plus one — the slack has to absorb a tick's arrivals, not a single body's.

### What the patch costs and when it declines

`O(moves)` to leave the old runs, `O(moves)` to enter the new ones, plus the shift of each affected run's tail.
It declines in three cases, all of which fall back to the rebuild: a mover is not in the run its old cell names
(the table is already inconsistent), a target run has no room left (only a fresh count can lay the slack out
again), or the tick moved more than `PATCH_MAX_MOVERS` bodies, in which case the list is not even kept. An
abandoned patch is safe because `build_tables` recomputes every table from the entries.

### What a patched table is *not*

It is not the table a rebuild would produce. The runs are identical, cell by cell and in order, which is what a
query reads; the *offsets* differ wherever the slack did, because a rebuild derives each capacity from the count
it just measured and a patch keeps the capacity the last rebuild gave the cell. Over many patched ticks the
slack drifts towards the cells that keep receiving bodies and away from the cells that keep losing them, so a
rebuild is also the compaction that resets it. `check_invariants` therefore checks runs and occupancy, not
offsets, and the four-way byte-equality matrix in `spatial.rs` still holds for the full path alone.

### Measured

`examples/incremental_build --workers 6 --crossers 64`, median of five, against a full rebuild of the same
positions:

| bodies | still | 64 crossers | full rebuild | patch vs rebuild |
| --- | --- | --- | --- | --- |
| 100,000 | 0.207 ms | 0.230 ms | 1.870 ms | 8.1x |
| 1,000,000 | 1.161 ms | 1.190 ms | 13.427 ms | 11.3x |

The tail is the fallback, and it is visible: p95 at a million bodies is 13.8 ms against a 1.2 ms median,
because a handful of ticks in the run put two arrivals into the same empty cell. A real crowd's arrivals are
correlated with its departures, which is the case a swap models and the case the tests pin.

### Tests

`spatial_incremental.rs` is the harness ADR 0064 specified, now covering both steps: the still crowd, one
crosser, a body swapping cells for eight ticks against an oracle rebuild, a flood above the mover bound, a
crowded cell with no slack, an entry list that changes length, every worker count from one to eight, an empty
grid, and the audit's three corruptions.

## Amendment: the slack rule is a knob, and the tail says what it buys

`capacity_for` used to be a constant. It is now `capacity_for(count, slack_percent)`, exposed as
`SpatialGrid::with_slack_percent`, defaulting to `DEFAULT_SLACK_PERCENT = 25`. The reason is measurable: whether a
patch survives is decided by the receiving cell's spare slots, so "how much slack" is a real trade between `order`
width and how often a tick falls back to the rebuild. `examples/patch_tail.rs` measures it under uniformly random
arrivals, which is the worst case the patch has, and `benchmarks/results/2026-11-10/sandbox-aarch64/patch-tail/`
holds the raw rows.

Medians of three runs of 200 ticks, six workers, 8-unit cells, arrivals teleported uniformly:

| bodies | crossers | slack | patched | median | p95 | p99 | order slots |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 100,000 | 64 | 25% | 93.5% | 0.229 ms | 2.364 ms | 2.454 ms | 736,066 |
| 100,000 | 64 | 50% | 93.5% | 0.224 ms | 2.393 ms | 2.472 ms | 743,159 |
| 100,000 | 512 | 25% | 61.0% | 0.346 ms | 2.649 ms | 2.843 ms | 736,066 |
| 100,000 | 512 | 50% | 61.0% | 0.341 ms | 2.655 ms | 3.104 ms | 743,159 |
| 1,000,000 | 64 | 25% | 93.5% | 1.197 ms | 13.893 ms | 14.162 ms | 1,685,496 |
| 1,000,000 | 64 | 50% | 95.0% | 1.191 ms | 2.356 ms | 14.120 ms | 1,985,236 |
| 1,000,000 | 512 | 25% | 60.5% | 1.404 ms | 14.189 ms | 15.423 ms | 1,685,496 |
| 1,000,000 | 512 | 50% | 67.5% | 1.394 ms | 14.199 ms | 15.734 ms | 1,985,236 |

Three readings, and the third is the one that matters for the rule.

1. **The tail is bimodal.** A tick either patches (0.2 ms at 100k, 1.2 ms at a million) or rebuilds (2.4 ms, 14 ms).
   p95 is therefore not a distribution's shoulder, it is "how often did this run rebuild". At 64 crossers the
   rebuild share is 6.5% of ticks, which is exactly the p95 sitting at the rebuild's cost.
2. **Doubling the slack is nearly free and nearly useless on a sparse grid.** At 100,000 bodies over 421,876 cells
   the average cell holds a quarter of a body: the runs that refuse an arrival are *empty* cells that own their one
   spare slot, and half of zero is still zero. The rule costs 1.0% more table (736,066 slots to 743,159) and moves the
   p95 by 1%.
3. **Where the grid is dense, the percentage does bite.** At a million bodies the average cell holds 2.4 bodies, so a
   cell's spare slots are one or two at a quarter and three or four at a half. Sixty-four crossers then stop
   rebuilding: p95 falls from 13.9 ms to 2.4 ms, for 17.8% more table (1,685,496 slots to 1,985,236). The p99 stays at
   14 ms, so roughly one tick in twenty still rebuilds, and 512 crossers is beyond either rule's reach.

So the default stays a quarter: it is the cheapest rule that absorbs a light tick, and the 50% variant is a trade a
game makes deliberately when its grid is dense and its tail matters more than 300 KB at a million bodies. The
finding that deserves the next change is not the percentage but the floor: a percentage of zero is zero, so the
failing case at any density is a cell that was empty when the tick began. A rule with a minimum of two spare slots
would address it directly, where no percentage can, and that is a change to argue on its own measurements rather than
fold into this one.

## Amendment: the capacity floor, and why the percentage had to be joined

The previous amendment measured the percentage rule and found the failure it cannot reach: a cell that was empty when
the tick began owns one slot, and a percentage of zero is zero, so two bodies arriving in it together force a rebuild
however high the percentage is set. `DEFAULT_SLACK_FLOOR = 2` adds that floor, `SpatialGrid::with_slack_floor` exposes
it, and a cell's spare slots are now `max(percent of count, floor)`. Zero restores the percentage-only rule exactly,
which is why the previous amendment's numbers remain reproducible.

At 512 crossers, three runs of 200 ticks, six workers, 25% slack:

| bodies | slack | floor | patched | median | p95 | p99 | worst | order slots |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100,000 | 25% | 0 | 61.0% | 0.340 ms | 2.661 ms | 2.834 ms | 3.119 ms | 736,066 |
| 100,000 | 25% | 1 | 96.5% | 0.332 ms | 0.581 ms | 2.697 ms | 3.212 ms | 1,372,112 |
| 100,000 | 25% | 2 | 99.5% | 0.335 ms | 0.420 ms | 0.581 ms | 2.677 ms | 2,008,168 |
| 100,000 | 25% | 4 | 100.0% | 0.339 ms | 0.415 ms | 0.684 ms | 0.972 ms | 3,280,280 |
| 1,000,000 | 25% | 0 | 60.5% | 1.410 ms | 14.548 ms | 16.079 ms | 24.889 ms | 1,685,496 |
| 1,000,000 | 25% | 1 | 96.5% | 1.336 ms | 1.732 ms | 14.042 ms | 14.118 ms | 2,272,269 |
| 1,000,000 | 25% | 2 | 99.0% | 1.337 ms | 1.505 ms | 2.485 ms | 14.152 ms | 2,908,168 |
| 1,000,000 | 25% | 4 | 100.0% | 1.336 ms | 1.476 ms | 2.393 ms | 2.496 ms | 4,180,280 |

The floor is paid per cell rather than per body, so it is expensive on a sparse grid: 1.27 million extra slots at
100,000 bodies, 1.22 million at a million. It is also the only rule that touches this failure mode. Floor 1 fixes the
p95 and leaves the p99 at the rebuild cost; floor 2 fixes the p99, taking it from 2.83 ms to 0.58 ms at 100,000 and
from 16.08 ms to 2.49 ms at a million; floor 4 removes the last 0.5% of rebuilds and the worst tick with them, for
4.5x and 2.5x the table.

Two is therefore the default, as the smallest floor that covers a pair, and four is left to the caller who has measured
that the residual matters to them. The remaining question this does not answer is the clustered arrival: a crowd that
pours into a stairwell is not random, and no per-cell rule handles a run that the tick itself makes too narrow. That
belongs to the patch's budget of movers (P2 step 2) rather than to the capacity rule.

## Amendment: the floor and the thread count, measured together

A 16-thread sweep at a million bodies changes how the two knobs should be described. The patch's own cost is
indifferent to the floor (every floor lands within 1% of the same curve: 6.18 ms at one worker, 1.33 ms at six,
2.00 ms at sixteen on a six-core host) and scales to the core count and no further. What the floor changes is what the
tick that *does not* patch costs, and therefore whether adding workers helps or hurts the tail:

- floor 0: p99 rises with workers after two, 19.0 ms at two and 27.7 ms at sixteen, because the non-patching ticks are
  a rebuild whose scaling is poor and degrades under oversubscription.
- floor 2: p99 is 2.4 ms at every worker count above two. The build's scaling stops mattering because the build stops
  happening.
- floor 3: 100% of ticks patch at every worker count, so the worst tick equals the median (2.46 ms at six workers,
  2.51 ms at sixteen) and oversubscription costs the patch 50% of its throughput instead of costing the frame a 24 ms
  spike.

Floor 2 therefore remains the default, and floor 3 is the documented setting for a game that needs the guarantee that
no tick can rebuild. The general lesson, which the 100,000-body measurement had already hinted at (a floor of four was
needed there for the same guarantee), is that the floor a crowd needs for that guarantee depends on its density, so it
is a measured choice rather than a constant this crate can pick for everyone.
