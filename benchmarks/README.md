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

## What the gap is made of

A product row cannot say which part of it is the schedule and which is the work. `--workload matched` answers that with
two extra rows: the Unreal harness's loop (`benchmarks/unreal`) written twice, once called directly and once from a
single system in `Update` on a bare `App::new()`. Same 1,000 m cube, same 10 m cells, same eight-neighbour bound, same
seek and separation, same 1/60 s step. A test asserts the two produce bit-identical positions, so the ECS row cannot be
paying for a different trajectory.

```text
cargo run --release --example cross_bench -- --workload all --agents 10000,100000 --ticks 300
```

Measured on the same six-core aarch64 VM as the two-engine table, median of three runs:

| agents | matched, called directly | matched, in one ECS system | the whole plugin | ECS plus schedule |
| ---: | ---: | ---: | ---: | ---: |
| 10,000 | 2.062 ms | 2.068 ms | 5.393 ms | +0.006 ms (0.3%) |
| 100,000 | 13.352 ms | 13.360 ms | 44.146 ms | +0.008 ms (0.06%) |

Read it as three statements, in order of how much they can bear:

- **The ECS and its schedule are not the cost.** Eight microseconds per tick at 100,000 agents is 0.06% of the plugin
  row. Whatever the difference against another engine turns out to be, it is not Bevy's dispatch.
- **The plugin is 2.6x the matched loop at 10,000 and 3.3x at 100,000.** That is the price of what the product row
  actually does: a twenty-seven-cell neighbourhood instead of eight same-cell slots, needs, temperament-driven weights,
  the cognition scheduler and social memory. It is also where the optimisation budget belongs.
- **The matched loop is still slower than the Unreal row on comparable hardware.** Ours is bounds-checked per element
  and runs clear, prefix and cursor copy as three passes over a million cells; Unreal's `TArray` compiles to unchecked
  addressing. That is a difference in the loop's implementation, not in the engine around it, and it is measurable
  rather than assumed: run `--workload all` on the machine where you ran Unreal and the two matched rows sit in the same
  table.

The full plugin row here is 44.146 ms, and the laptop's was 16.528 ms for the same workload, which is what a 6-core VM
against an M3 Pro is worth. The matched rows are the ones to compare across hosts, because they contain nothing but the
fixture.

## What to try next

1. **Start from the matched rows when attributing the gap.** `--workload all` runs the fixture twice more: Unreal's
   loop called directly, and the same loop from one system in `Update` on a bare `App::new()`, with a test asserting the
   two are bit-identical. The table is under [What the gap is made of](#what-the-gap-is-made-of). On the sandbox the ECS
   and its schedule were 0.06% of the plugin row at 100,000 agents and the plugin was 3.3x the matched loop, so the gap
   is the workload and the loop's implementation rather than Bevy's dispatch. What is missing is those three rows on the
   M3 Pro, which is one command.
2. **Sweep worker counts on this machine.** [`BoidsConfig::with_workers()`](../crates/bevy-flock-ecs/src/boids.rs) already exposes the grid rebuild's worker count. Compare 1, 2, 4, 6, 8, and 11 workers at both population sizes, and report repeated medians, p95, and budget misses. Existing [worker measurements](../docs/dev/tuning/performance.md) used a six-core host; they do not establish the best setting for this M3 Pro.
3. **Measure the steering query before changing its arithmetic.** A 100,000-agent Time Profiler capture sampled `steer_agent`, `ScanView::scan`, `Crowd::steer_parallel`, and Bevy's task executor. The capture covered a 15-second window during a roughly 19-second process, so it points to areas to investigate but does not assign exact costs to measured ticks. The scan is in [crowd.rs](../crates/bevy-flock-core/src/application/crowd.rs). Count scanned cells and candidate neighbors in a focused run, then test changes against the existing deterministic parity tests.
4. **Account for headless schedule work separately.** Bevy's measured frame includes systems such as render-plan preparation even though the benchmark has no renderer. The profile did sample render-plan sorting, but it was not among the leading paths. Measure its cost in [render.rs](../crates/bevy-flock-ecs/src/adapters/primary/render.rs) before considering a headless-only option, and keep the full-plugin row for the application-level comparison.

The grid itself is not missing: Bevy already has a 3D counting-sort grid with count, prefix, and placement passes in [spatial.rs](../crates/bevy-flock-core/src/domain/spatial.rs). Unreal's fixture uses the same broad grid strategy.

## Experiments not to repeat as default fixes

- **Cell-size tuning:** a whole-frame sweep found little difference between the default and finer grids at 10,000 and 100,000 agents. The result and limits are in [ADR 0031](../docs/dev/adr/0031-cell-sizing-for-lanes.md).
- **Per-worker histograms and bucketing:** profiling and measurements rejected these rebuild alternatives. See the [outcome in ADR 0060](../docs/dev/adr/0060-build-5x-gate.md#outcome).
- **More prefix-scan work:** the count and prefix work has already been fused and measured; the remaining prefix pass was a small part of the rebuild. See [ADR 0057](../docs/dev/adr/0057-fused-count-and-scan.md) and [ADR 0060](../docs/dev/adr/0060-build-5x-gate.md).
- **A persistent unsafe worker pool:** [ADR 0059](../docs/dev/adr/0059-worker-pool-ownership.md) defers it because the remaining dispatch cost does not justify the aliasing and soundness burden. The Bevy task-pool alternative was rejected because it allocates in the steady-state rebuild.

## Reproducing a row

```text
cargo run --release --example cross_bench -- --agents 10000,100000 --ticks 1000 --json
godot --headless --path benchmarks/godot -- --mode=grid --agents=10000 --ticks=1000 --json
# Unreal: copy the module per unreal/README.md, then run with the -crossbench* flags
python3 scripts/compare_benchmarks.py bevy.json godot.json unreal.json
```

The raw JSONL files from this run are in ignored `target/` directories and are not part of the repository.

The Bevy harness reported `unknown cpu` on macOS for this run, because it read `/proc/cpuinfo` unconditionally, and
the label in the table above was corrected by hand. That is fixed in the harness: `/proc/cpuinfo` on Linux,
`sysctl -n machdep.cpu.brand_string` on macOS, `PROCESSOR_IDENTIFIER` on Windows, which is what the Godot harness
(`OS.get_processor_name()`) already did. It mattered more than a label: `scripts/compare_benchmarks.py` refuses to
put two machines in one table, so a row that cannot name its own CPU turns that refusal off. The three rows here
were assembled by hand for that reason, and merging a real set through the comparator is still open.
