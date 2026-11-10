# Work order: rebuild wiring, cell aggregates, and removal

Written 2026-11-10 for an agent taking over from a session that ran out of credits. Read this whole file before touching
anything, then read the four files it names. Nothing here is speculative about the code: every function, field and number
was checked against the tree on `feat/p2-borrow-budget` at commit `ffaa406`.

## Where the project is

`bevy-flock` is a Rust/Bevy crowd and NPC library at version 0.1.0, four crates in one workspace:

| crate | what it holds |
| --- | --- |
| `crates/bevy-flock-core` | `domain` (spatial grid, ids, memory, saves), `behavior`, `application` (crowd, scheduler). No Bevy engine dependency. |
| `crates/bevy-flock-ecs` | `adapters` (plugin, band, dialogue, gaze, gesture, grouping), `boids`. Depends on `bevy_ecs`, `bevy_app`, `bevy_math`. |
| `crates/bevy-flock-anim` | animation layers, skinned avatar playback. |
| `src/` | the facade crate `bevy-flock`: 176 lines of re-exports, no logic. |

The immediate goal is not a new feature. It is finishing three pieces of work that were designed, measured and left
half-wired, in this order, because the second and third need the first:

1. **Wire the incremental rebuild into the plugin.** The patch exists, is tested, and is used only by benchmarks. The
   plugin still rebuilds from scratch every tick.
2. **`SpatialGrid::remove`.** There is no removal API at all, which is why a single death forces a full rebuild.
3. **Per-cell aggregates.** Opt-in statistics a crowd library will want, with a design that keeps determinism.

## Ground rules, in the order they will bite you

**Toolchain and repository.**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/ubuntu/bevy-flock
cargo test --offline --workspace --all-features      # the gate
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
python3 scripts/lint_audit.py && python3 scripts/lint_audit.py --self-test
python3 scripts/check_docs_links.py
python3 scripts/publish_check.py
(cd site && ./node_modules/.bin/astro build)
```

Use `--offline`; the dependency cache is complete and the network is not reliable. The host is a six-core aarch64 VM with
16 GB of RAM and a small disk: when `df -h /home/ubuntu` shows under about 3 GB free, `rm -rf target/debug/incremental
target/debug/examples` before blaming the compiler for a link error.

**The lint policy is not advisory.** `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment` and
the rest of the workspace table are denied **including in tests**. Six allows are permitted
(`module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, `struct_excessive_bools`,
`multiple_crate_versions`) and every other exception has to be a documented, sanctioned entry in
`scripts/lint_audit.py`. In practice that means `slice.get(i)`, `let Some(x) = ... else`, `Entry::or_default()`, and
`crate::testkit::OrDie` instead of `expect`. If you catch yourself writing `#[allow(...)]` or `#[expect(..., reason)]`,
stop and find the shape the lint is asking for; the audit will fail the build and the reviewer will reject it.

**Text patches drift.** `spatial.rs` is about three thousand lines and `cargo fmt` rewrites it. Long literal anchors fail
after a format pass. Read the exact region with `sed -n`, then replace **by line range** with a small script, then compile.
Two sessions in a row lost time to an anchor that looked identical and was not. Never apply a two-hundred-line patch
script blindly: every stage has to compile before the next one starts.

**Commits and pull requests.** Author `Nick Campbell <s0ma@protonmail.com>`, one logical change per commit, a message that
says what was wrong rather than what the diff does. Branch from `origin/main`, then `gh pr create --base main`, then merge
with `gh pr merge --merge --delete-branch`. Do not push to `main`.

**Prose.** No em dashes anywhere in docs, comments or messages; they are checked. No personal names from other games.
Numbers are quoted with their host, and a claim of a speedup needs a before/after measurement on the same machine using
alternating binaries (see *Measurement protocol*).

## The state of the grid, precisely

`crates/bevy-flock-core/src/domain/spatial.rs` is a counting sort over positions, laid out in four arrays:

- `entries: Vec<GridEntry>` holds `id: NpcId`, `position: Vec3`, `velocity: Vec3` (32 bytes), in **insertion order**.
- `cells: Vec<u32>` is the cell of each entry, indexed by entry index. It is the patch's memory of last tick.
- `cell_start: Vec<u32>` has one slot per cell plus one; `cell_start[c]` is the first slot of cell `c`'s run and
  `cell_start[cells]` is the total **capacity**, which is larger than the population by the slack.
- `cell_len: Vec<u32>` is the used length of each run, so a run occupies `cell_start[c] .. cell_start[c] + cell_len[c]`
  and the rest of the region to `cell_start[c + 1]` is spare.
- `order: Vec<u32>` is sized to the capacity and holds entry indices in run order.
- `cell_cursor: Vec<u32>` is a placement cursor, primed from `cell_start` and left equal to it after a completed scan.

Passes: `clear`, `count`, `prefix` (bases), `place`, all in `build_measured()`, with `PassCosts` reporting each. At one
million bodies over 421,876 cells on six threads the last measured split was clear 0.74 ms, count 1.73 ms, prefix 0.11 ms,
place 1.96 ms, total 4.56 ms. `count` is flat from two to six threads (2.04 to 1.73), which is the single most important
fact in this document: it is memory-bound, so thread work cannot fix it.

`build_incremental()` compares each entry's cell against `cells`, and when at most `PATCH_MAX_MOVERS` entries changed,
`patch_movers()` moves them: removals first, then insertions, each run keeping its order, and a full run borrows a slot
from the next run that has one (bounded by `BORROW_MAX_ELEMENTS` and `BORROW_MAX_RUNS`). It returns
`RebuildOutcome::{Unchanged, Patched, Rebuilt { cause: RebuildCause }}` where the causes are `Population`,
`TooManyMovers`, `StaleTable`, `NarrowRun`.

Three invariants are checked by `check_invariants()` and asserted in tests: runs are contiguous inside their cells, a run
is sorted by identity **when it was built from identity-ordered entries**, and `cell_cursor[c] == cell_start[c] +
cell_len[c]` for an idle grid.

**The trap that cost an hour already:** `order` is sized to capacity, not population, so `for slot in grid.order` walks
spare slots holding `0`. Any test or query that iterates the array must walk `cell_start[c] .. cell_start[c] +
cell_len[c]`. A query that reads the full region will return phantom neighbours, and the failure looks like a corrupt
grid rather than an off-by-one.

## Task A: wire the patch into the plugin

**Why it matters, and why it will look underwhelming.** `rebuild_spatial_grid` in `crates/bevy-flock-ecs/src/boids.rs`
calls `grid.0.clear()`, inserts every query row, then `grid.0.build()`. The incremental path is never called outside
benchmarks. Measured, that build is 0.97 ms of a 44.05 ms frame at 100,000 agents on this VM, so a perfect patch buys
about 2%. The reasons to do it anyway are that it is the prerequisite for removal, that a game which queries the grid but
steers a subset is rebuild-dominated, and that "the library has an incremental path it does not use" is a bad answer in a
review. Do not write a PR claiming a frame win at 100,000; measure, and report whatever it is.

**The blocker.** The patch remembers one cell per *entry index*, and the system hands it a freshly built entry list every
tick, so `cells.len() != entries.len()` can never be false. Wiring it means the grid's `entries` become persistent and
each NPC owns a slot.

1. Add a slot component. `NpcGridSlot(pub u32)` in a new `crates/bevy-flock-ecs/src/adapters/primary/grid_door.rs` (a
   door is the thing an NPC walks through to get into the index); register it in the spawn path where `Npc` and
   `NpcVelocity` are inserted, and keep a free list so a despawned slot is reused. A `Vec<Option<Entity>>` in a resource
   is enough to start; do not build a second ECS.
2. Replace the body of `rebuild_spatial_grid` with: for each `(slot, transform, npc, velocity)` write
   `entries_mut()[slot] = GridEntry { .. }`, then `build_incremental()`. `entries_mut()` already exists.
3. Keep a fallback: if the slot table is not dense and sized to the entries, or if any NPC has no slot this tick, do what
   the system does today (`clear`, insert in spawn order, `build`). Correctness first; the fast path is an optimisation.
4. Announce the outcome. An `Event` or a resource holding the last `RebuildOutcome` gives a game a way to see its own
   rebuild rate, and gives the benchmark something to assert. `FlockStats` already aggregates tick statistics; add
   `patched_ticks` and `rebuilt_ticks` there rather than inventing a second place.

**Tests that have to exist before this merges.** Headless `App`, no renderer, using the patterns already in
`crates/bevy-flock-ecs/src/boids.rs`:

- A crowd that does not move reports `Unchanged` and the query results are bit-identical to a full rebuild.
- Ten NPCs crossing a cell boundary in one tick report `Patched`, and every query returns the same neighbours as a grid
  built from the same positions by `clear` + `insert` + `build`.
- A tick with more movers than `PATCH_MAX_MOVERS` reports `Rebuilt { cause: TooManyMovers }` and is still identical.
- A spawn and a despawn in the same tick fall back to `Population` **until Task B lands**, and then must be `Patched`.
  Leave the test in place with the stricter expectation commented as the next step, or write it to accept both causes and
  tighten it in Task B. Do not leave a test that asserts today's limitation as if it were the design.

**Where the measurement goes.** `examples/incremental_build` already reports still, mover and full-formation ticks; add a
plugin-mode row to `examples/cross_bench` only if the frame actually moves. Everything else is a harness number, and the
benchmark README already says so.

## Task B: `SpatialGrid::remove`

There is no removal API. A despawned body is absent from the next query, the system rebuilds, and `RebuildCause::Population`
is the whole story. That is correct and expensive: O(population) for one death.

**Signature.** Operate on the entry index, not on `NpcId`. The grid deliberately has no id-to-index table (it would be a
hash map's memory for a sort's job), and the ECS side has the slot table from Task A, so it already knows the index:

```rust
pub fn remove_slot(&mut self, slot: usize) -> Option<Removed>
```

`Removed` should carry the removed entry and, when the slot was filled by the last entry, what moved where:
`{ entry: GridEntry, moved: Option<(NpcId, usize)> }`. The caller must be told, because its `Entity -> NpcGridSlot`
mapping is now wrong and a silent swap is exactly how a replay stops reproducing.

**Mechanics, in the slotted layout.** Find the entry's run position by scanning `order[start .. start + len]` for the
entry index (runs are short; this is not the hot path). Shift the run's tail left by one, which keeps the order and
therefore keeps sortedness if the run was sorted, decrement `cell_len[c]` and `cell_cursor[c]`. The vacated slot stays
inside the cell's region as spare capacity, so removal *increases* slack and never invalidates `cell_start`.

Then swap-compact the entry array: move the last entry into `slot`, update `cells[slot]`, and rewrite the moved entry's
index inside its own run (`order` holds indices, so the moved body's slot in `order` now points at `slot`). Report it.
If `slot` was already the last entry, nothing moves.

**Do not** shift the whole `entries` array down. It is O(n) per death and it renumbers every index the patch remembers.

**Compaction.** Population only ever shrinks the used length, never the capacity, so a crowd that halves keeps the array
and the window table. Add `pub fn compact(&mut self)` that rebuilds the tables from the surviving entries (a
`build_tables` call, which is what `RebuildCause::Population` already does) and record a threshold in the docs: a game
should compact when `cell_start[cells] > 1.5 * entries.len()`. Do not compact automatically inside `remove_slot`; that
would turn an O(run) removal into an O(n) one on an unpredictable tick.

**Tests.** Each must compare against the oracle (rebuild from the survivors) rather than against a hand-written
expectation:

- Remove the only body in a cell; the cell is empty, its run length is zero, and the window table is unchanged elsewhere.
- Remove the first, middle and last body of a three-body run; the survivors keep their order and their cells.
- Remove an entry that is not the last one, so the swap happens; the moved body is reported and is findable at its new
  index, and a query returns the same ids as the oracle.
- Remove, then insert into the same cell in the same tick: the freed slot should be borrowed rather than a rebuild, which
  is the whole point.
- Remove until the population is a quarter of capacity, `compact()`, and assert the tables match a fresh build of the
  survivors and that the capacity is now the smaller one.
- `check_invariants()` after every one of the above.

## Task C: per-cell aggregates

A crowd library gets asked for density, a local centre of mass, and a local mean heading, because LOD and coarse steering
want them and recomputing them per query is what makes a query expensive.

**The design decision that matters is where they live.** Two shapes were considered:

- *Always-on in the grid.* `Vec<Vec3>` for position sums and another for velocity sums is 24 bytes per cell, so 10 MB at
  421,876 cells, on top of a 1.7 MB window table. That is a 6x memory increase for every user, including the ones that
  only want neighbours.
- *Opt-in and computed during placement.* A `SpatialGridConfig::with_cell_stats(true)` allocates the two vectors, and
  `place` accumulates into `cell_sum[c]` and `cell_vel_sum[c]` as it writes each body. The data is already in registers,
  so the cost is six adds and six stores per body rather than a second pass, and the accessors divide by `cell_len`.

Take the second. It is the only version that does not tax the neighbour query.

**Determinism, which is the part that will be got wrong.** Summing floats in parallel is not associative, so accumulating
across workers in arrival order would make the result depend on the worker count and break the byte-parity contract that
`every_thread_count_builds_identical_windows` enforces. Placement writes each cell's run in entry order (see
`arrival_order_decides_the_order_inside_a_cell_and_nothing_else`), and that order is the same for any worker split, so
accumulating *during placement* is deterministic by construction. Pin it: extend the parity test to compare
`cell_sum`/`cell_vel_sum` bit for bit across the existing worker and block matrix, and to compare an opt-in grid against
one built with `with_cell_stats(false)` for the neighbour results.

**Precision, which has to be documented.** `f32` sums lose bits when a cell holds thousands of bodies far from the origin:
the relative error is about `1e-7`, so a cell of a thousand bodies at a coordinate of 1,000 has an absolute error near
0.1 units. That is fine for density and LOD and not fine as a substitute for a position. Say so in the doc comment, and
give the caller `CellStats { centre: Vec3, mean_velocity: Vec3, len: u32 }` for occupied cells only, with a
`cell_stats(&self, cell: u32) -> Option<CellStats>` accessor and a `fill_cell_stats(&self, out: &mut Vec<(u32,
CellStats)>)` for the per-tick case so a caller allocates once.

**Where they should be used, later and separately.** The steering pass's neighbourhood scan is the biggest cost in the
frame (the plugin is 3.3x to 9.2x the matched loop, and the scan is the largest part of it). A cell-level prefilter is the
natural next step: skip cells whose centroid and density cannot contribute. That is a separate change with its own
parity test, not part of this one.

## Sequence and sizing

1. **A**, in two commits: the slot component plus the persistent-entries rewrite with the fallback, then the tests. Expect
   a day, mostly in making the fallback honest.
2. **B**, one commit plus a test commit. The removal itself is an afternoon; the swap-compaction and the reporting are
   the part that needs care.
3. **C**, one commit. The config flag, the accumulation inside placement, the accessors, the parity extension.

Each of the three is a separately reviewable PR against `main`. Do not stack them: `main` moves fast here, and a stack of
three is a rebase per merge.

## Measurement protocol, because a number is a claim

Two binaries, one harness, alternating runs, medians of three:

```bash
git worktree add /tmp/before origin/main
(cd /tmp/before && cargo build --release --offline --example patch_tail)
cargo build --release --offline --example patch_tail
# alternate: before, after, before, after, before, after
```

Record into `benchmarks/results/<date>/<host>/<name>/` as JSONL with `schema_version` and `spread_pct` (both harnesses
emit them with `--json --repeat 3`), then add a row to `docs/dev/tuning/performance.md` and, if it changes a claim, to
`site/src/content/docs/reference/performance.md`. The benchmark README's comparability rules apply: a row names its host,
and a ratio across hosts is not a result.

Never write "5x" about the rebuild. ADR 0060 measured the ceiling at about 1.8x for thread work because `clear` and
`count` are memory-bound, and the honest gate is "the frame does not regress and a moving crowd patches".

## Things that are easy to miss

- **`RebuildCause` has no reference page.** It is new public API, it is mentioned in the site's performance page, and the
  reference section does not describe it. Add it when Task A lands.
- **`FlockStats` should count outcomes.** A game cannot currently tell whether its crowd patches or rebuilds. Two counters
  in the existing stats struct close that.
- **`build_incremental` is documented as honouring `check_invariants` in debug builds.** Confirm that is true before
  relying on it; if it is not, add it under `debug_assertions` and say so.
- **The look-back primitive is parked.** `docs/dev/adr/0063-dispatch-elimination.md` describes removing the per-pass
  barriers with a decoupled look-back, and PR #59 holds the primitive as a tested module that nothing calls. It is not
  needed for any of the three tasks above. Do not merge it into the grid without the aliasing decision that ADR 0063
  records as open, and do not delete the branch without asking.
- **The phase-5 plan's completion records are stale.** T04 to T06 shipped in pieces across several PRs and the plan file
  still describes the original ordering. Reconcile it against `docs/dev/tuning/performance.md` rather than trusting either.
- **The README and site claim numbers from two hosts.** Both say so. If you add a row, say which host it came from.
- **1.0 needs things this sandbox cannot do**: a real GPU capture of the demo (lavapipe is a software rasteriser) and an
  Unreal run of the cross-benchmark harness. Leave them on the checklist as the owner's tasks, with the exact commands
  from `benchmarks/README.md`.

## Definition of done

`cargo test --offline --workspace --all-features` green with the new tests counted, clippy clean at `-D warnings`, the
lint audit and its self-test clean, the docs link gate clean, the site built, three PRs merged, each with a measured
before/after where a number moved, and this file deleted once the last of the three is closed.
