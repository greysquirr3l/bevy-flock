# Cross-engine benchmark notes

These are rough measurements from one Apple M3 Pro with 11 available threads. Each run used 100 warm-up ticks and 1,000 measured ticks, with a 16.6 ms frame budget. The [benchmark guide](../docs/dev/tuning/benchmarks/cross-engine.md) describes the harnesses and how to rerun them.

This is one host. Every other number in the repository comes from a six-core aarch64 sandbox VM, and the two sets do not divide into each other: a row from here and a row from there are different machines with different clocks, so each is quoted with its host and neither is used to correct the other.

| Agents | Engine | Version | Median | p95 | p99 | Max | Over 16.6 ms |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 10,000 | Bevy | 0.1.0 | 2.217 ms | 2.779 ms | 2.980 ms | 3.625 ms | 0% |
| 10,000 | Unreal | 5.8.2 | 0.937 ms | 2.359 ms | 2.660 ms | 3.679 ms | 0% |
| 10,000 | Godot grid | 4.7.2 | 4.674 ms | 5.322 ms | 5.762 ms | 7.037 ms | 0% |
| 100,000 | Bevy | 0.1.0 | 16.528 ms | 18.240 ms | 19.674 ms | 139.185 ms | 47.8% |
| 100,000 | Unreal | 5.8.2 | 3.496 ms | 5.567 ms | 6.653 ms | 9.343 ms | 0% |
| 100,000 | Godot grid | 4.7.2 | 38.054 ms | 41.445 ms | 44.352 ms | 179.952 ms | 100% |

At 10,000 agents, Unreal's median was about 2.4 times lower than Bevy's. At 100,000, it was about 4.7 times lower. Bevy's median was about 2.1 times lower than Godot's at 10,000 and 2.3 times lower at 100,000. At 100,000, Bevy's median barely fits the budget, but nearly half its measured ticks exceed it.

Treat these as rough system comparisons, not an engine ranking. The measured work differs:

- Bevy times `App::update` with the full `FlockPlugin` schedule, and it steers and integrates **every** agent. The
  [harness](../examples/cross_bench.rs) uses the `CityAmbient` preset, and that preset is a spawn distribution:
  `FlockPlugin` installs the memory bridge and `BoidsPlugin`, not the manifest band, so nothing is culled in this
  row. Per-agent cost stays flat across the two sizes (222 ns at 10,000, 165 ns at 100,000), which is what a loop
  over the whole population looks like when density does the only adjusting. The 5,000-manifested figure quoted
  elsewhere belongs to the banded continuum (`BandPlugin` plus a camera anchor) and is a different workload; it
  cannot be subtracted from this row, and the 47.8% budget misses at 100,000 are the all-steered number.
- Unreal times a custom `UTickableWorldSubsystem`, not Mass Entity. Its [loop](unreal/Source/CrossBench/CrossBenchSubsystem.cpp) rebuilds a 3D counting-sort grid, seeks a shared goal, checks at most eight same-cell neighbors, and integrates every agent.
- Godot times `_physics_process` in 2D. Its [grid mode](godot/cross_bench.gd) rebuilds a dictionary grid, then seeks and integrates every agent. The step does not use that grid for neighbor avoidance.
- The engines do not generate identical positions. Bevy's crowd preset and the other harnesses use different spawn distributions, which also changes cell occupancy and neighbor work.

The Unreal workload is a small SoA loop, not a comparison with Unreal's crowd stack. Bevy's result includes its plugin schedule, while Unreal measures only its subsystem tick. The benchmark guide discusses these differences.

## Which harness answers which question

Seven files in `examples/` and one script produce the numbers in this document and in the performance journal, and they
answer different questions. Reach for the one whose question matches yours; the "roughly" column is one row on the six-core VM, so it is a
sense of scale rather than a promise.

| harness | question it answers | command | roughly |
| --- | --- | --- | ---: |
| [`cross_bench`](../examples/cross_bench.rs) | Does the engine cost anything, and what does the plugin do on top of a bare loop? | `--workload all --agents 10000,100000 --ticks 300 --json` | 1 s per 60 ticks |
| [`build_passes`](../examples/build_passes.rs) | What do the four rebuild passes cost, and how do they scale with threads, blocks and bodies? | `--bodies 100000,1000000 --workers 1,6` | 0.1 s per row |
| [`patch_tail`](../examples/patch_tail.rs) | How often does the incremental rebuild give up, and what does a tick cost when it does? | `--crossers 512 --slack 25 --floor 2 --arrival cluster` | 0.2 s per row |
| [`incremental_build`](../examples/incremental_build.rs) | What do a still tick, a handful of movers, and a full re-formation cost side by side? | `--bodies 1000000 --crossers 64` | 0.6 s |
| [`bandwidth`](../examples/bandwidth.rs) | What does this machine's memory actually do, so a memory-bound pass can be told from a badly written one? | `--size 512 --threads 1,2,4,6` | 2 s |
| [`spawn_cost`](../examples/spawn_cost.rs) | What does acquiring and releasing a crowd cost, separate from moving it? | `--bodies 10000` | 6 s |
| [`band_ticks`](../examples/band_ticks.rs) | What does the banded continuum cost per tick with a moving anchor? | `--ticks 200` | 0.2 s per 60 ticks |
| [`scaling_sweep.sh`](../scripts/scaling_sweep.sh) with [`scaling_regression.py`](../scripts/scaling_regression.py) | Did this change make a measured row worse than the checked-in baseline? | see [What CI will fail on](#what-ci-will-fail-on) | minutes |

`profile_crowd`, `profile_build`, `profile_band`, `horde_100k` and `ambient_metropolis` are shaped demos rather than
measurements: they exist for a scene that needs a number, not for a table.

## Comparability rules

Four rules, and a table that breaks one of them is not a measurement:

1. **Every row names its host.** The rows here come from an Apple M3 Pro laptop and a six-core aarch64 VM, and
   [`scripts/compare_benchmarks.py`](../scripts/compare_benchmarks.py) refuses to build a table from two machines at once.
   A row that cannot name its own CPU turns that refusal off, which is why the CPU label was fixed rather than annotated.
2. **A ratio across hosts is not a result.** [The section below](#the-sandbox-got-the-third-statement-wrong-and-that-is-worth-keeping)
   is what happens when one is taken anyway: the same fixture reads 7.7x slower on the VM single-threaded and 2.8x slower
   parallel, so dividing one row by the other produced a deficit that did not exist.
3. **A row is the median of a stated number of runs.** `--repeat 3` gives the record a `spread_pct` and the name of the run
   whose median sat in the middle. A row without a repeat count is one run on a shared host.
4. **Rows measured in different sessions are only comparable within their noise.** A container that was idle for one sweep
   and busy for the next produces two of everything; the archived records keep their timestamps for this reason.

## How to read a p99 here

A p99 on this kind of host has three causes, and they are told apart by comparing the p99 to its own row's median before
comparing it to anything else:

- **A rebuild inside the measured loop.** The p99 lands near the rebuild's cost and the patched share says how often: at
  512 crossers with floor 0, the p99 *rises* with the worker count because the residue is a full rebuild. Change the
  slack, not the thread count.
- **Host scheduling.** The p99 sits within about twice the median, and `spread_pct` moves with it. The small-body passes
  are in this bucket: their kernels are 3 to 4 µs, so a 100 µs tick is the scheduler rather than the code.
- **First touch and the allocator.** The worst tick is many times the p99 and appears once per process, which is why
  `cross_bench`'s 100,000-agent row has a 139 ms maximum beside a 19.7 ms p99. Warm-up ticks exist to keep this out of the
  medians, not to make it disappear.

## What the gap is made of

A product row cannot say which part of it is the schedule and which is the work. `--workload all` answers that with two
extra rows: the Unreal harness's loop (`benchmarks/unreal`) written twice, once called directly and once from a single
system in `Update` on a bare `App::new()`. Same 1,000 m cube, same 10 m cells, same eight-neighbour bound, same seek and
separation, same 1/60 s step, same seed. A test asserts the two produce bit-identical positions, so the ECS row cannot be
paying for a different trajectory.

```text
cargo run --release --example cross_bench -- --workload all --agents 10000,100000 --ticks 300 --json
```

On the Apple M3 Pro that produced the three-engine table, 300 measured ticks after 100 warm-ups:

| agents | matched, called directly | in one ECS system | the whole plugin | Unreal, same host | ECS plus schedule |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 | 0.428 ms | 0.429 ms | 2.184 ms | 0.937 ms | +0.001 ms (0.05%) |
| 100,000 | 1.737 ms | 1.734 ms | 15.940 ms | 3.496 ms | -0.003 ms (noise) |

Three statements, in order of how much they can bear:

- **The ECS and its schedule are not the cost.** A tenth of a millisecond at 10,000 and nothing above run-to-run noise at
  100,000. Whatever the gap against another engine is, it is not Bevy's dispatch, and this is measured rather than argued.
- **The fixture's loop is 2.2x faster here than Unreal's, and 2.0x faster at 100,000.** Same host, same work, same bound,
  both read from tables in the same document. This is the row that settles the argument about the engine around the loop.
- **What the plugin does on top of the loop is 5.1x the loop at 10,000 and 9.2x at 100,000**, which is 80.3% and 89.1% of
  the product row. A twenty-seven-cell neighbourhood instead of eight same-cell slots, plus needs, temperament-driven
  weights, the cognition scheduler and social memory. That is the whole of the 4.6x gap against Unreal at 100,000, and it
  is where the optimisation budget belongs.

The three-engine table's product row reads 2.217 and 16.528 ms from the earlier run; this run of it reads 2.184 and
15.940, which is the same number twice. Its 100,000-agent over-budget share is 32% here against 47.8% in the 1,000-tick
run, and that is what 300-tick samples on a shared laptop look like rather than a change in the crate.

### Where the threads go

`--workers 1,2,4,6` pins the grid build's worker count. On the six-core VM it moves the product row by nothing:

| agents | 1 worker | 2 workers | 4 workers | 6 workers |
| ---: | ---: | ---: | ---: | ---: |
| 10,000 | 5.284 ms | 5.435 ms | 5.330 ms | 5.436 ms |
| 100,000 | 44.102 ms | 44.331 ms | 43.983 ms | 44.374 ms |

That is not the build failing to scale. Measured on its own, 1,000,000 bodies over 421,876 cells, seven repeats, from
`examples/build_passes`:

| threads | total | speedup | efficiency | clear | count | prefix | place |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 12.351 ms | 1.00x | 100% | 2.760 ms | 2.042 ms | 0.417 ms | 7.291 ms |
| 2 | 6.970 ms | 1.77x | 88% | 1.552 ms | 2.060 ms | 0.294 ms | 3.082 ms |
| 4 | 5.149 ms | 2.40x | 60% | 0.927 ms | 1.840 ms | 0.158 ms | 2.237 ms |
| 6 | 4.564 ms | 2.73x | 46% | 0.740 ms | 1.731 ms | 0.111 ms | 1.964 ms |

Place and count are where the curve flattens: both are memory-bound scatter and histogram passes, and the serial share of
the build rises to 24.5% at six threads, which caps it near 4x. 2.73x is what six threads are worth here, and it is already
banked.

It does not show in the product row because of arithmetic: at 100,000 agents the build is 1.79 ms at one worker and 0.97 ms
at six, out of a 44 ms row, so pinning its threads can move at most 1.9% of the frame. That is what the sweep measured.

### The pool, not the grid

The rest of the row is steering, and steering never reads the grid flag. `pool_for` in `crowd.rs` takes the host's compute
task pool, creating the default one when a headless app has none, and slices the pass across it above
`MIN_PARALLEL_AGENTS`. The row's thread count is therefore the pool's, which is why the grid sweep above is flat.

`--pool 1,2,4,6` builds the app with a `TaskPoolPlugin` whose compute pool is exactly that many threads, and it re-executes
the binary once per shape, because `ComputeTaskPool` is a process-wide `OnceLock`: the first pool in a process wins, so an
in-process sweep would report the first shape N times. Measured on the same six-core VM, 200 ticks, medians of three runs:

| agents | 1 thread | 2 threads | 4 threads | 6 threads |
| ---: | ---: | ---: | ---: | ---: |
| 10,000 | 11.941 ms | 7.341 ms (1.63x) | 5.578 ms (2.14x) | 5.463 ms (2.19x) |
| 100,000 | 124.823 ms | 68.297 ms (1.83x) | 49.315 ms (2.53x) | 44.052 ms (2.83x) |

So the frame does scale, 2.83x on six threads at 100,000 agents, with efficiency falling 92%, 63%, 47%. The canonical row
in every other table of this document is the six-thread one, which is the pool's own choice and not an authored setting.
Designing for more than six threads means designing for the efficiency column, not the speedup one.

### The sandbox got the third statement wrong, and that is worth keeping

The same three rows on the six-core aarch64 VM, median of three runs, gave 2.062 / 2.068 / 5.393 ms at 10,000 agents and
13.352 / 13.360 / 44.146 ms at 100,000. From those, the first two conclusions hold: the schedule is free, and the plugin
is a multiple of the loop.

The third one inverted. On the VM the matched loop is 13.352 ms at 100,000; divide that by the 2.7x host ratio the plugin
row shows and it lands at 4.9 ms against Unreal's 3.496 ms laptop row, which reads as a 1.4x deficit for the Rust loop.
An earlier revision of this section said exactly that. It was wrong, and wrong in a way worth naming: the bare loop is single-threaded and touches four megabytes of grid three times a tick, so the VM
penalises it 7.7x against the laptop, while the plugin's parallel build absorbs the same weak memory 2.8x. The fixture
rows are not portable between hosts even when the fixture is identical, and dividing a sandbox row by a laptop row to
reach a conclusion about an implementation is what produced the mistake. The correction is one command on the host that
matters, which is the row above.

### The patch's borrow budget

`examples/patch_tail.rs` now measures two arrival patterns, because the patch's failure mode depends on which one a game
has. `--arrival random` scatters a tick's crossers over the whole field; `--arrival cluster --cluster-radius 4` puts them
inside a hotspot four cells wide around one point per tick, which is a crowd flowing into one district.

| bodies | arrival | patched before | median before | patched after | median after |
| --- | --- | --- | --- | --- | --- |
| 100,000 | cluster | 0.0% | 2.581 ms | 100.0% | 0.347 ms |
| 1,000,000 | cluster | 0.0% | 13.981 ms | 100.0% | 1.305 ms |
| 100,000 | random | 99.5% | 0.351 ms | 100.0% | 0.352 ms |
| 1,000,000 | random | 99.0% | 1.344 ms | 100.0% | 1.362 ms |

`--cluster-radius 0` sends every crosser into a single cell, which is beyond the borrow budget and still rebuilds on
purpose: the row exists so the limit is visible rather than inferred.

## What to try next

1. **Start from the matched rows when attributing the gap.** `--workload all` runs the fixture twice more: Unreal's
   loop called directly, and the same loop from one system in `Update` on a bare `App::new()`, with a test asserting the
   two are bit-identical. The table is under [What the gap is made of](#what-the-gap-is-made-of). On the sandbox the ECS
   and its schedule were 0.06% of the plugin row at 100,000 agents and the plugin was 3.3x the matched loop, so the gap
   is the workload and the loop's implementation rather than Bevy's dispatch. What is missing is those three rows on the
   M3 Pro, which is one command.
2. **Sweep the pool, not the grid build, if the frame is the target.** `--workers` pins
   [`BoidsConfig::with_workers()`](../crates/bevy-flock-ecs/src/boids.rs), which reaches only the grid rebuild, and the
   rebuild is 2% of the 100,000-agent row: see [Where the threads go](#where-the-threads-go). The steering pass takes its
   threads from the compute task pool, so measuring it needs `TaskPoolPlugin` with a chosen pool shape in front of
   `FlockPlugin`. That sweep, on the M3 Pro at 1, 2, 4 and 10 threads, is the one worth running next.
3. **Measure the steering query before changing its arithmetic.** A 100,000-agent Time Profiler capture sampled `steer_agent`, `ScanView::scan`, `Crowd::steer_parallel`, and Bevy's task executor. The capture covered a 15-second window during a roughly 19-second process, so it points to areas to investigate but does not assign exact costs to measured ticks. The scan is in [crowd.rs](../crates/bevy-flock-core/src/application/crowd.rs). Count scanned cells and candidate neighbors in a focused run, then test changes against the existing deterministic parity tests.
4. **Account for headless schedule work separately.** Bevy's measured frame includes systems such as render-plan preparation even though the benchmark has no renderer. The profile did sample render-plan sorting, but it was not among the leading paths. Measure its cost in [render.rs](../crates/bevy-flock-ecs/src/adapters/primary/render.rs) before considering a headless-only option, and keep the full-plugin row for the application-level comparison.

The grid itself is not missing: Bevy already has a 3D counting-sort grid with count, prefix, and placement passes in [spatial.rs](../crates/bevy-flock-core/src/domain/spatial.rs). Unreal's fixture uses the same broad grid strategy.

## The patched rebuild's tail

`examples/patch_tail.rs` answers a different question from the cross-engine rows: not how long a frame
takes, but how often the spatial grid's incremental rebuild gives up and starts over. Each row is one
`(bodies, crossers, slack)` cell of a sweep with uniformly random arrivals, which is the pattern the patch
is worst at, and reports the split between patched and rebuilt ticks next to the tick's median, p95, p99 and
worst time.

```text
cargo run --release --example patch_tail
cargo run --release --example patch_tail -- --bodies 1000000 --crossers 64,512,2048 --slack 25,50
cargo run --release --example patch_tail -- --json
```

The p95 is the rebuild share wearing a latency, so read it as "how often", not "how bad". Doubling the slack
from a quarter to a half removes the spike at a million bodies (13.9 ms to 2.4 ms) for 17.8% more table, and
changes nothing at 100,000 for 1% more, because the cells that refuse an arrival there are empty ones. That
finding is why `--floor` exists: the floor is the spare slots a cell gets regardless of occupancy, and at 512
crossers raising it from 0 to 2 takes the patched share from 61% to 99.5% and the p99 from 2.83 ms to 0.58 ms at
100,000 bodies (16.08 ms to 2.49 ms at a million) for 2.7x and 1.7x the table. Rows:
`benchmarks/results/2026-11-10/sandbox-aarch64/patch-tail-floor/`.

`--workers` takes a list as well, so the floor and the thread count can be swept together. At a million bodies the
patch scales to the core count and then back off (6.18 ms at one worker, 1.33 ms at six, 2.00 ms at sixteen on a
six-core host), and the floor is what decides whether that matters for the tail: floor 0's p99 *rises* with workers
(19.0 ms at two, 27.7 ms at sixteen) because the residue is a rebuild, floor 2 holds p99 at 2.4 ms at every count, and
floor 3 leaves no rebuild at all, so the worst tick equals the median (2.46 ms at six, 2.51 ms at sixteen). Rows and
the chart: `benchmarks/results/2026-11-10/sandbox-aarch64/floor-thread-sweep/`. Rows
from the sandbox run are in `benchmarks/results/2026-11-10/sandbox-aarch64/patch-tail/`.

## Experiments not to repeat as default fixes

- **Cell-size tuning:** a whole-frame sweep found little difference between the default and finer grids at 10,000 and 100,000 agents. The result and limits are in [ADR 0031](../docs/dev/adr/0031-cell-sizing-for-lanes.md).
- **Per-worker histograms and bucketing:** profiling and measurements rejected these rebuild alternatives. See the [outcome in ADR 0060](../docs/dev/adr/0060-build-5x-gate.md#outcome).
- **More prefix-scan work:** the count and prefix work has already been fused and measured; the remaining prefix pass was a small part of the rebuild. See [ADR 0057](../docs/dev/adr/0057-fused-count-and-scan.md) and [ADR 0060](../docs/dev/adr/0060-build-5x-gate.md).
- **A persistent unsafe worker pool:** [ADR 0059](../docs/dev/adr/0059-worker-pool-ownership.md) defers it because the remaining dispatch cost does not justify the aliasing and soundness burden. The Bevy task-pool alternative was rejected because it allocates in the steady-state rebuild.

## What CI will fail on

`scaling-regression` runs a fresh sweep against the checked-in baseline and fails above a threshold:

```text
scripts/scaling_sweep.sh /tmp/candidate.json
python3 scripts/scaling_regression.py docs/data/scaling-sweep.json /tmp/candidate.json \
  --fail-pct 25 --warn-pct 12 --json /tmp/scaling-verdict.json
```

A row more than 25% worse than its baseline fails the job, 12% to 25% warns, and the verdict is uploaded as an artifact.
The thresholds, the noise handling and the fixture guards are covered by `--self-test`, which runs without a compiler.

Two things this gate does not do. `bench-smoke` only *compiles* the benches (`--no-run`), so a bench that became slower
does not fail CI. And nothing in CI runs the harnesses above at a million bodies: the archived rows are records, not
assertions. If a change can move a rebuild, run `build_passes` and `patch_tail` against the same host before and after, and
put both files in `results/`.

## What the engine fixtures need from you

The fixture is checked in, but each engine needs one thing before it will run:

- **Godot** is self-contained: `godot --headless --path benchmarks/godot -- --mode=grid --agents=10000 --ticks=1000 --json`
  works from a clean checkout with Godot 4.7 or later, no editor import step. `--mode=basic` runs the version that does
  not use the grid for avoidance, which is the distinction between the two Godot rows.
- **Unreal** is not: copy [benchmarks/unreal/Source/CrossBench](unreal/Source/CrossBench) into a project's `Source/`, add
  the module to that project, build, then run the editor command line the [Unreal README](unreal/README.md) lists.
  `scripts/cross_bench.py` refuses to guess a template path, so the first run names what is missing rather than producing
  a row that is quietly the wrong project.
- **Bevy** needs nothing: `cargo run --release --example cross_bench -- --agents 10000 --ticks 1000 --json`.

The Unreal side has never been executed on this host. It is reviewed code with a fixture behind it, not a measurement, and
the table above says so where it matters.

## Numbers we do not have

Written down so the gaps are not mistaken for measurements:

- **No x86 row at all.** Every number here is aarch64 or Apple silicon.
- **No pair of the same fixture on two hosts of the same architecture**, which is what a portable scaling claim needs.
- **No real GPU numbers for the demo.** The software rasteriser validated on lavapipe is a CPU rasteriser; it says nothing
  about a Metal or D3D12 frame.
- **No Unreal execution**, only the review and the fixture.
- **No measured plugin frame for a rebuild that changes nothing.** The plugin patches its spatial index now
  (PR #62), so a still crowd does not rebuild it, and `flock_frame` measures the frame a game gets. The
  `spatial_build` rows still measure a rebuild from scratch, because that is the operation they name.

## Reproducing a row

```text
# three engines, same fixture
cargo run --release --example cross_bench -- --agents 10000,100000 --ticks 1000 --json
godot --headless --path benchmarks/godot -- --mode=grid --agents=10000 --ticks=1000 --json
# Unreal: copy the module per unreal/README.md, then run with the -crossbench* flags
python3 scripts/compare_benchmarks.py bevy.json godot.json unreal.json

# the four rebuild passes, one row per worker count
# seven repeats are the harness's own constant, and --warm reuses one grid across them
cargo run --release --example build_passes -- --bodies 100000,1000000 --workers 1,2,4,6

# the incremental rebuild, both arrival patterns
cargo run --release --example patch_tail -- --bodies 100000,1000000 --crossers 512 --slack 25 --floor 2 \
  --workers 6 --arrival random,cluster --ticks 200 --json
cargo run --release --example patch_tail -- --arrival cluster --cluster-radius 0

# still, a handful of movers, and a re-formation
cargo run --release --example incremental_build -- --bodies 1000000 --crossers 64 --workers 6

# this machine's memory, so a memory-bound pass is recognisable
cargo run --release --example bandwidth -- --size 512 --threads 1,2,4,6
```

The [archived result records](results/README.md) include this three-engine run and the later matched-workload run. The files are committed under `benchmarks/results/`, not left in ignored `target/` directories.

Re-run the harness with `--repeat 3` to make a row carry its own dispersion: the record then names the run whose
median sat in the middle of the three, and `spread_pct` says how far the medians ranged. Every table above predates
that field and states its run count in prose instead; the archived records under `results/` carry it directly.

The Bevy harness reported `unknown cpu` on macOS for this run, because it read `/proc/cpuinfo` unconditionally, and
the label in the table above was corrected by hand. That is fixed in the harness: `/proc/cpuinfo` on Linux,
`sysctl -n machdep.cpu.brand_string` on macOS, `PROCESSOR_IDENTIFIER` on Windows, which is what the Godot harness
(`OS.get_processor_name()`) already did. Aarch64 kernels that publish only `CPU implementer` and `CPU part` are named
from those identifiers, so the sandbox's own rows say `ARM Neoverse N1` instead of `unknown cpu`; an unrecognised part
keeps its raw ids rather than being guessed at. It mattered more than a label: `scripts/compare_benchmarks.py` refuses to
put two machines in one table, so a row that cannot name its own CPU turns that refusal off. The three rows here
were assembled by hand for that reason, and merging a real set through the comparator is still open.
