# The cross-engine crowd benchmark

Comparing crowd systems across engines is normally done by quoting two blog posts at each other. This document exists so
that it can be done by running the same fixture and pasting one JSON line per engine into one table.

Nothing here claims an engine is slower than another. The workloads are close but not identical, and the document says
where they diverge instead of pretending they do not.

## What is being measured

One number per engine: **the median wall time of one steady-state simulation tick**, at a given agent count, on a stated
machine. Not frames per second (which hides the tail), not a kernel microbenchmark (which hides the schedule), not the
spawn burst.

Six statistics, always together — `median`, `p95`, `p99`, `max`, `over_budget_pct`, `ns_per_agent`. A run that reports only
the median is not a result; the interesting number for a crowd is the tail, because a crowd that holds 5 ms median and
50 ms at p99 is a crowd that hitches.

Frame budget is **16.6 ms** (60 Hz) in every harness.

## W1 — fixed-tick crowd

The workload both engines run:

1. Spawn *N* unique agents with a deterministic seed, spread over a 1 km cube (or the 2D equivalent for a 2D harness).
2. Drain the spawn queue, then run **100 warm-up ticks** that are not measured.
3. Run **1000 measured ticks**. Each tick: advance every agent, rebuild whatever spatial index the system uses, resolve
   local avoidance.
4. Record the wall time of each measured tick. Sort. Report the six statistics.

**What counts as the tick:** the engine's own loop entry point — `App::update` here, `_physics_process` in Godot, `Tick`
in Unreal. Not a hand-rolled call to the steering kernel. The point is to compare what a game would actually pay at that
entry point.

**What is excluded everywhere:** rendering, skeletal animation, audio, networking, physics bodies, file I/O. Every harness
in this document excludes the same list, which is what makes the numbers comparable at all — as soon as one side includes
its renderer, the table stops meaning anything.

**What is included and differs:** `bevy-flock` pays for its own spatial rebuild inside the tick; Godot's shared-field
harness pays for its field update; Unreal's Mass harness pays for its processors. Each engine pays for its own approach,
which is the point.

## The JSON schema

One line per configuration, so `scripts/compare_benchmarks.py` can read a directory of files and print a table:

```json
{"engine":"bevy-flock","engine_version":"0.1.0","workload":"W1-fixed-tick","agents":10000,
 "ticks":1000,"warmup":100,"budget_ms":16.6,"median_ms":5.467,"p95_ms":5.650,
 "p99_ms":5.805,"max_ms":5.874,"over_budget_pct":0.00,"ns_per_agent":547,
 "machine":"unknown cpu, 6 threads","notes":"App::update over the plugin schedule; no rendering"}
```

Required: `engine`, `engine_version`, `workload`, `agents`, `ticks`, `budget_ms`, `median_ms`, `p95_ms`, `p99_ms`,
`max_ms`, `over_budget_pct`, `machine`, `notes`. Optional: `warmup`, `ns_per_agent`, `commit`.

`machine` is not decoration. A table without it is a table of rumours: the published Godot result below is on a 2018
Ryzen 5 2600X, and this sandbox is a six-core aarch64 VM, so their 5.02 ms and our 5.47 ms are not evidence about the
engines until they run on one machine. The full machine string, the core count and the governor state all belong in it.

`notes` is where an author admits what their harness includes that the others do not.

## Running the bevy-flock side

```text
cargo run --release --example cross_bench -- --agents 10000,100000,1000000 --ticks 1000 --json
```

One JSON line per agent count. Use `--ticks` and `--warmup` to match the other harnesses exactly; the defaults are the
fixture's 1000 and 100.

## Running the Godot side

`benchmarks/godot/cross_bench.gd` is a self-contained `Node` script implementing the same fixture in pure GDScript:
`PackedVector2Array` positions, a shared goal, a per-tick loop that advances every agent, times each measured tick and
prints the same JSON line.

1. New Godot 4.6 project, copy `cross_bench.gd` in, attach it to the scene root.
2. Set `agents` in the inspector (`10_000`), run with the scene focused, not in-editor-debug (debug builds are not
   results; export a release template or run `--headless` with a release build).
3. `godot --headless --quit-after 1200 res://main.tscn` prints the JSON line to stdout.

Report the engine version and the CPU exactly as Godot's own `OS.get_processor_name()` returns them — do not retype it.

## Running the Unreal side

Mass Entity is the fair comparison, and the fixture is small enough to build in C++ without a project template of its own:

1. `UCrossBenchSubsystem : UTickableWorldSubsystem`, or a `AActor` with `PrimaryActorTick`.
2. Hold the crowd in `TArray<FVector>` (positions) and `TArray<FVector>` (velocities) — SoA, as Mass would — not an array
   of actors. An array of `AActor*` measures UObject overhead, not crowd simulation.
3. Per tick: integrate toward the shared goal, run the engine's broadphase (Mass's `FMassEntityQuery` or a
   `TArray<FBox>` grid) and local avoidance, then record `FPlatformTime::Seconds()` deltas.
4. 100 warm-up ticks, 1000 measured, same six statistics, same JSON keys. Use the same seed for spawn.
5. `UnrealEditor-Cmd.exe Project.uproject -game -nullrhi -ExecCmds="crossbench" -log` for a headless run; a PIE run with
   the editor attached is not a result.

If the intent is to test Unreal's *shipping* crowd stack rather than a harness someone wrote in an afternoon, say so in
`notes` and quote the engine's own stat captures (`stat unit`, `stat MassEntity`) alongside.

## Reading the result

The published numbers in `docs/performance.md` are the reference points:

| system | workload | published | ns per agent |
| --- | --- | ---: | ---: |
| Godot 4.6, shared flow field (Ryzen 5 2600X, GDScript) | 10,000 2D agents | 5.022 ms median | 502 |
| same | 20,000 | 9.541 ms | 477 |
| UE5 Mass Entity (community claim) | 100,000 agents, collision + pathfinding | ~10 ms | ~100 |
| bevy-flock, this harness, this sandbox | 10,000 | 5.467 ms median | 547 |
| bevy-flock, this harness, this sandbox | 100,000 | 45.958 ms median | 460 |

Two rules for anyone extending that table:

- **Never compare across machines.** Re-run the other engine's harness on the machine in question, or label the row
  "vendor-published, different machine".
- **Never quote `ns_per_agent` alone.** It is a convenience for reading the scaling shape; at 10k the per-agent cost is
  dominated by index rebuilds that amortise later, and at 1M by memory bandwidth. The six statistics are the result, and
  `ns_per_agent` is a gloss on one of them.

## Why this exists

`docs/performance.md` can say what this crate costs. It cannot say whether that is good. A number with no peer is a
number with no meaning, and the only honest fix is a fixture both sides run.
