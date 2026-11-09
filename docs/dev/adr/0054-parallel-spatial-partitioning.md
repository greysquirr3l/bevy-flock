# ADR 0054: what a parallel spatial build may not change

**Status:** Accepted
**Date:** 2026-11-09
**Context:** The spatial grid is the largest single-threaded structure in the frame. It is built once per
tick over whatever population is active, then queried once per manifest body, and every number the crate
publishes about a million inhabitants, 4.75 ms, 6 ms, the 8 ms manifested tier, was measured with one
thread doing both. Phase 3 (plan.phase3.toml) exists to change that.

The risk is not whether threads can be spawned. It is that a threaded build is the first place the
crate's determinism guarantee (ADR 0004) can quietly stop being true, and the failure is silent: a grid
whose cells are filled in scheduling order produces the same *set* of neighbours for a body and a
different *sequence*, so a replay diverges a tick later in a heading, then a decision, then a save file
that hashes differently on the player's machine than in CI. By the time the divergence is visible,
the thread that caused it is a hundred frames behind.

So this ADR fixes the invariants before there is a thread to break them, and names the test that holds
each one. A task that cannot point at its test does not land.

## Decision

### 1. Cell membership is a pure function of position, and the build never re-derives it
A body's cell is `cell_of(position)`, integer arithmetic on a position, defined in
`src/domain/spatial.rs`, unchanged by the build. The build's job is to *group* bodies by that answer. A
parallel build may therefore partition work by cell with no coordination at all, because no thread's
decision can change another's input. Anything that would make the cell depend on the build (an adaptive
cell size, a fit to a bounding box computed during the build) is out of scope for the parallel path and
needs its own ADR.

### 2. Neighbour *membership* is public; neighbour *order* is internal but stable
`SpatialGrid::radius` and `::nearest` return a `Vec<Neighbour>`. Today that sequence is ordered by
`(cell, insertion)` with `NpcId` as a tie-break, and the SIMD parity work (ADR 0024) compares kernel
output element by element, so order is load-bearing even though no doc comment promised it.

The decision: **membership is the contract, order is a stable internal property.** The parallel path
must produce the same order as the serial one, because the parity harness compares sequences and because
the packed kernel's accumulation order, and therefore its float result, depends on it. Making order
public is a separate decision that this ADR explicitly declines: promising it would freeze the bucket
layout into the API.

### 3. The merge order is cell order, not thread order
Each worker counts its cells into a private histogram, prefix-sums locally, and the assembly step
concatenates per-cell runs **in ascending cell index**, then within a cell in entry order. A shared
atomic cursor is banned: it is the one-line shortcut that makes the grid depend on scheduling, and it is
not faster, the measurement in `docs/dev/tuning/performance.md` is what decides, but the invariant comes first.

Corollary: the number of threads must not appear in the output. The test builds the same fixture at 1,
2, 3 and 8 threads and compares the raw buffers, not a summary: `parallel_build_matches_serial_bytes`.

### 4. Per-body queries are chunked by body range, so output offsets are known before the work starts
The neighbour arena is one allocation sized from the total count, and bodies are chunked by range, so
each chunk's output window is computable in advance and the gather is a deterministic scatter. A
per-body `Vec` would make the output depend on allocation order and would also defeat the
zero-allocation claim that `tests/no_alloc.rs` already holds the animation crate to.

### 5. Nothing in the build reads the virtual-cost scheduler
The scheduler assigns per-body costs (ADR 0009) and those costs change every tick. A build that sorted or
partitioned by cost would be idempotent only for a fixed cost vector, so two runs whose scheduler
histories differ by one tie-break would build different grids. Partition by position, sort never.

### 6. Float reduction: the centroid is summed in a fixed order, or it is not summed
Any per-cell aggregate (centroid, average velocity) is accumulated over the cell's entries in entry order
within the cell, and cells are combined in cell order. Summing per-thread partials in completion order is
the same bug as #3 with extra steps, and it is the one that survives a code review because the code
looks parallel.

### 7. The RNG stream is consumed per body, never per grid slot
A body's stream is keyed by `NpcId` (ADR 0001), so it does not matter which thread updates it or in what
order bodies are visited. A build that draws from a shared generator, even deterministically seeded , 
breaks this, because the number of draws per body would depend on the partition.

### 8. Thread count is configuration; a serial path stays
The count comes from configuration, defaults to available parallelism, and is documented as a
performance knob that cannot change behaviour. The serial path remains compiled and tested: it is what the
parity harness compares against, what a wasm build gets, and what a bug report is bisected to.

### 9. A body that is not in the grid stays at its previous cell for exactly one tick
Rebuilds can drop entries (a body streamed out) and add them. A dropped body's stale cell entry is
cleared by the rebuild, and a re-added body is inserted in the same tick it appears, so no frame observes
a half-built grid. Parallel rebuilds must publish with the same all-or-nothing property: the published
grid is either the previous tick's or this tick's, never a mix. That means the parallel path writes into
a second buffer and swaps, or holds a write lock over the publish step alone.

## Consequences

**Positive.** A parallel build becomes a mechanical change with a test per invariant, rather than a
rewrite whose correctness is argued from the absence of bugs so far. The invariants are also a
specification of the serial path, which until now existed only in the code.

**Negative.** Chunking by body range and by cell order costs the freedom to schedule work however is
convenient: load balancing has to happen inside a deterministic skeleton. In practice the per-cell
histogram merge is cheap (it is a prefix sum over cells, not over bodies), and the constraint has already
been paid for by the SIMD work, which needed the same discipline.

**Neutral.** Invariant 2 declines to make neighbour order public. If it later becomes public, the bucket
layout in the contract and the parallel path must preserve it, which it does anyway, because the parity
harness compares sequences.

## Alternatives considered

**Treat membership as the only contract and compare sets in the parity harness.** Rejected: the packed
kernel sums in iteration order, so equal sets with different orders produce different floats and a
diverging replay. The parity harness would then need a tolerance, and a tolerance in a determinism test
is how determinism is lost.

**Atomic slot allocation with a shared cursor.** Rejected by invariant 3. It is the standard lock-free
build and it is not wrong *in general*, it is wrong here because it makes output order a function of
thread scheduling, and this crate's entire published performance record is defended by replay.

**Partition by entity index instead of by cell.** Considered and rejected: an entity's cell requires a
lookup, so a range partition would touch every body's position and scatter writes across all cells. Cell
partitioning needs no coordination precisely because `cell_of` is pure.

**Make the grid build asynchronous and publish a tick later.** Considered as a latency-hiding measure;
rejected for now because it changes what a tick means to a consumer that assumes it queries this tick's
positions. It stays available as an optimisation that does not touch any invariant above.

**Do nothing; the frame still fits.** True at 1M today, and false at 2M, which is the number the next
competitive gap names. The baseline task exists to make the decision measurable rather than assumed.

## References

- `benches/spatial.rs`, the serial baseline this phase is measured against, with build, query and
  kernel separable.
- ADR 0001 (identifiers and streams), ADR 0004 (determinism), ADR 0009 (virtual-cost scheduler),
  ADR 0021 (multi-band scaling), ADR 0024 (SIMD layout and parity), ADR 0031 (cell sizing).
- `plan.phase3.toml`, the tasks that implement this, each naming the test that holds its invariant.
