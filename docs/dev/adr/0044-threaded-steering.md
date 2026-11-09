# ADR 0044: Thread the steering kernel across the ECS task pool

**Status:** Accepted
**Date:** 2026-11-05
**Supersedes in part:** ADR 0031 (which chose the wide road as the default for every frame)

## Context

Every performance decision in this crate until now traded *lanes*: four agents at a time through
`wide::f32x4` (ADR 0023, ADR 0030), a transposed layout so those four loads are contiguous, and a
cell budget tuned so the occupancy matches the lane width (ADR 0031). That work is finished and
measured; the wide road is 19–31% faster than the scalar one at every density tested.

What none of it touches is the other five cores. A profile of the 100,000-agent frame on a six-vCPU
host puts 84% of samples inside `Crowd::steer`, 57.6% of them in the function body, with the grid
rebuild, the gather and the scatter together under 10%. The scan for one agent reads a snapshot of
positions and velocities and writes nothing but that agent's desired velocity. It is a pure map over
agents, and it was being run on one thread.

Two facts had to be established before the map could be split.

The first is that `simd` is a default feature, so `Crowd::steer` was taking `steer_blocks` and never
reaching the scalar code where threading was easiest to add.

The second is that `bevy_tasks` 0.20 has a `multi_threaded` feature that is *not* on by default, and
`bevy_app` depends on the crate with `default-features = false`. Without that feature
`TaskPool::default()` builds a single-threaded stub and `thread_num()` returns 1, so the first
version of this change measured a flat line and looked like a failed optimisation when in fact the
pool had no workers. That is worth writing down: a task pool that silently has one thread is
indistinguishable from code that does not use it.

## Decision

A frame is cut into contiguous slices of agents and the slices are handed to the ECS compute pool.

1. `Crowd` grows a [`SteeringRoad`] with four variants — `Auto`, `Wide`, `Threads`, `Scalar` — and
   `steer()` dispatches on it. `Auto` ships: threads when the crowd and the pool are worth it, the
   wide road otherwise. The road is set from `BoidsConfig::with_road`, so a game can pin it.
2. The per-agent step is factored into `steer_agent`, called by both the scalar loop and the
   threaded one, so the two cannot drift apart. All roads assert bit equality with `steer_scalar`,
   which remains the definition of the arithmetic.
3. A frame is threaded only above `MIN_PARALLEL_AGENTS = 1_024` agents and with at least
   `MIN_PARALLEL_CHUNK = 512` agents per slice, with eight slices per thread so one dense cell
   holding a whole village cannot become the critical path.
4. The pool is borrowed with `ComputeTaskPool::try_get()` and, when a host has not added
   `DefaultPlugins`, created lazily with `get_or_init(TaskPool::default)`. Lazily creating it is
   what makes "add `FlockPlugin` and it uses your cores" true without inserting `TaskPoolPlugin`
   and colliding with a host that adds `DefaultPlugins` afterwards.
5. The `threads` cargo feature enables `bevy_tasks/multi_threaded` and is on by default.
   `default-features = false` gives a single-threaded build for a replay harness or a runner with
   one core, with no behavioural difference beyond speed.

`Crowd::set_sequential` stays as the diagnostic switch: it is for reproducing a reported frame
time, not for correctness, because the result is identical either way.

## Evidence

All four roads on the same host (6 vCPU, `--locked`, release, 20 samples, `--warm-up-time 1
--measurement-time 3`), benchmark id `flock_frame_road`, one `App::update()` per iteration:

| Road | 10,000 agents | 100,000 agents |
| --- | --- | --- |
| `wide` — the old default | 8.48 ms | 84.05 ms |
| `scalar` — reference, one thread | 9.73 ms | 92.99 ms |
| `threads` — reference, six threads | 4.31 ms | 30.00 ms |
| `auto` — what ships | **4.23 ms** | **29.61 ms** |

Against the road this supersedes, `Auto` is 2.0× faster at 10,000 agents and 2.8× at 100,000. The
frame-level figures in `docs/dev/tuning/performance.md` move accordingly. Ten thousand agents now cost a
quarter of a 60 Hz budget; a hundred thousand still miss it at 29.6 ms, which is the honest state of
the 1M-entity ambition: the bands and the ambient continuum spread that population, they do not make
one crowd of 100,000 fit in 16.6 ms.

Two observations from the numbers worth keeping:

- Threads beat lanes at every measured size, so the wide road is no longer the default and is
  retained because it is the fastest single-threaded road and because `default-features = false`
  builds use it.
- The gain is 2.0× on six threads, not 6×. The scan is memory-bound: the neighbour arrays are read
  by every slice and the shared bandwidth is the ceiling. A frame that wants more must read less,
  not run wider.

## What this does not do

The wide road is still single-threaded, and the two wins are therefore additive in principle: four
lanes across six threads. They are not combined here because the block kernel writes four
non-contiguous agent slots per block, and splitting those writes across threads needs either a
slot-indexed output buffer (which changes `Crowd::desired` and every consumer, ADR 0026's contract)
or per-thread scratch buffers merged after the pass (which allocates unless the buffers are cached
per thread and resized in place). Both are contained changes and both are measurable; neither was
worth doing in the same change as the first introduction of threads, when a regression would have
had two possible causes.

The honest expectation for that follow-up is 2–3× on top of the wide road at 100,000 agents, not 6×,
for the bandwidth reason above.

## Consequences

- Determinism is untouched: per-agent arithmetic is identical, slices are disjoint, and the report
  counters are summed in slice order. `a_parallel_frame_matches_the_scalar_one` and
  `tests/determinism.rs` enforce it.
- A steady frame still allocates nothing: `tests/crowd_no_alloc.rs` holds the threaded road to the
  same standard as the scalar one.
- The crate gains `bevy_tasks` as a direct dependency (already in the graph transitively) and, under
  the `threads` feature, two small crates (`async-channel`, `concurrent-queue`) that Bevy's own
  engine enables for every game using `DefaultPlugins`.
- A headless host that adds only `FlockPlugin` now creates a task pool on its first large frame.
  That is a side effect worth documenting in the plugin's own docs, and it is why the road and the
  pool can be pinned.
- `MIN_PARALLEL_AGENTS` is a heuristic, not a measurement: the crossover was not measured below
  10,000 agents because the wide road is already under a millisecond there.

## See also

- ADR 0009 — virtual-cost stride scheduler: the same principle, applied to *when* work runs rather
  than *where*.
- ADR 0026 — the cell-major kernel that defined the per-agent step this splits.
- ADR 0030, ADR 0031 — the transposed wide road and the cell sizing this supersedes as the default.
- `docs/dev/tuning/research/competitive-analysis.md` — the task-pool guidance this follows, and the gaps that
  remain.
