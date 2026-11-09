# The cross-engine crowd benchmark

Comparing crowd systems across engines is normally done by quoting two blog posts at each other. This document exists so
that it can be done by running the same fixture and pasting one JSON line per engine into one table.

Nothing here claims an engine is slower than another. The workloads are close but not identical, and the document says
where they diverge instead of pretending they do not.

## What is being measured

One number per engine: **the median wall time of one steady-state simulation tick**, at a given agent count, on a stated
machine. Not frames per second (which hides the tail), not a kernel microbenchmark (which hides the schedule), not the
spawn burst.

Six statistics, always together, `median`, `p95`, `p99`, `max`, `over_budget_pct`, `ns_per_agent`. A run that reports only
the median is not a result; the interesting number for a crowd is the tail, because a crowd that holds 5 ms median and
50 ms at p99 is a crowd that hitches.

Frame budget is **16.6 ms** (60 Hz) in every harness.

## W1, fixed-tick crowd

The workload both engines run:

1. Spawn *N* unique agents with a deterministic seed, spread over a 1 km cube (or the 2D equivalent for a 2D harness).
2. Drain the spawn queue, then run **100 warm-up ticks** that are not measured.
3. Run **1000 measured ticks**. Each tick: advance every agent, rebuild whatever spatial index the system uses, resolve
   local avoidance.
4. Record the wall time of each measured tick. Sort. Report the six statistics.

**What counts as the tick:** the engine's own loop entry point, `App::update` here, `_physics_process` in Godot, `Tick`
in Unreal. Not a hand-rolled call to the steering kernel. The point is to compare what a game would actually pay at that
entry point.

**What is excluded everywhere:** rendering, skeletal animation, audio, networking, physics bodies, file I/O. Every harness
in this document excludes the same list, which is what makes the numbers comparable at all, as soon as one side includes
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

`benchmarks/godot/` is a committed Godot project, so there is no manual setup and no inspector editing:

```text
godot --headless --path benchmarks/godot -- --agents 10000 --ticks 1000 --warmup 100
```

Every parameter the fixture needs is a command-line flag (`agents`, `ticks`, `warmup`, `budget_ms`, `seed`, `mode`),
because a headless run cannot touch the inspector and a fixture that requires editing a scene is a fixture that two
people will run differently.

`--mode` selects the workload, and the difference is the whole point of the comparison:

- `array` (default): one pass over a `PackedVector2Array` of positions, no spatial index. This is a **lower bound for
  Godot**, not a like-for-like row: the Rust side rebuilds its index inside the tick and this does not.

- `grid`: the same pass plus a uniform-grid rebuild into a `Dictionary` of arrays. This is the like-for-like row, and
  it is what makes the comparison say something about the design rather than about GDScript's loop speed.

The row's `engine` field carries the mode (`godot-array`, `godot-grid`), because two rows called `godot` at the same
agent count, measuring different work, make an ambiguous table.

## Running both sides at once

```text
cargo build --release --example cross_bench
python3 scripts/cross_bench.py --agents 10000,100000
```

The runner passes identical arguments to each harness, writes one JSONL file per engine under `target/cross-bench/`,
and hands them to `scripts/compare_benchmarks.py`. It skips an engine that is not installed and says so, rather than
printing a one-engine "comparison". Unreal is not automated: its harness lives in a UE project, not in this repository.

## Running the Unreal side

`benchmarks/unreal/` is the harness: a `UTickableWorldSubsystem` that runs the same fixture in Unreal's own idiom
(SoA arrays, counting-sort grid rebuild, bounded separation, fixed 60 Hz step) and prints the same JSON line. It is a
subsystem rather than an actor, so installing it is copying a module into a project; there is no map to build and no
blueprint to wire. Its [README](../../../benchmarks/unreal/README.md) has the setup, the flags and the build commands.

```text
UnrealEditor-Cmd "$PWD/CrossBench.uproject" -game -nullrhi -log \
  -crossbenchagents=10000 -crossbenchticks=1000 -crossbenchwarmup=100 \
  -crossbenchmachine="unknown cpu, 6 threads"
```

and through the runner, once that project exists:

```text
python3 scripts/cross_bench.py --agents 10000,100000 --engines unreal,bevy-flock,godot-grid \
  --unreal-cmd 'UnrealEditor-Cmd "$PWD/CrossBench.uproject" -game -nullrhi -log \
    -crossbenchagents={agents} -crossbenchticks={ticks} -crossbenchwarmup={warmup}'
```

Three things about this harness are worth knowing before quoting its row:

- **It is not Mass Entity.** A hand-written SoA loop answers "what does a straightforward crowd loop cost in this
  engine". Mass answers "what does Epic's crowd stack cost". If you want the second, run `stat MassEntity` and
  `stat unit` in a Mass scene and put the capture in `notes` rather than blending the two.

- **The measured region is the subsystem tick**, which excludes the engine frame, as the Godot row excludes the rest of
  its frame. The Rust row measures `App::update` and therefore includes its schedule. Read the three `notes` fields
  together.

- **`-crossbenchmachine=` exists because the comparator refuses mixed machine labels.** Unreal reports the real CPU
  brand; the other two fall back to `unknown cpu` on a VM with no model line. Copy the string from the existing rows so
  one machine has one label.

The harness is written against the UE 5.x API but has not been compiled in this repository's CI, because UE5 does not
fit in the sandbox the gates run in. The first build may need small version fixes; that is a five-minute fix in the
project, not a redesign of the fixture.

## A same-machine result

2026-11-09, one machine, one command (`python3 scripts/cross_bench.py --agents 10000,100000`), 1000 measured ticks
and 100 warm-up ticks per row, Godot 4.7.2 and bevy-flock 0.1.0:

| agents | engine | median | p95 | p99 | max | over 16.6 ms | ns/agent |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 | godot-array | 3.044 ms | 3.077 | 3.095 | 3.161 | 0.00% | 304 |
| 10,000 | bevy-flock | 5.661 ms | 5.930 | 6.263 | 12.018 | 0.00% | 566 |
| 10,000 | godot-grid | 13.976 ms | 14.997 | 15.367 | 15.774 | 0.00% | 1398 |
| 100,000 | godot-array | 30.636 ms | 30.871 | 30.937 | 44.163 | 100.00% | 306 |
| 100,000 | bevy-flock | 46.977 ms | 49.440 | 53.532 | 67.496 | 100.00% | 470 |
| 100,000 | godot-grid | 118.071 ms | 135.494 | 142.826 | 163.934 | 100.00% | 1181 |

`machine: unknown cpu, 6 threads` is what both harnesses report on the aarch64 sandbox VM this ran on, whose
`/proc/cpuinfo` has no model line. Both implement the same fallback deliberately: if the two strings differed, the
comparator would refuse the table, and the refusal is the feature.

What it says:

- **Against a bare GDScript loop we are slower**, 1.9x at 10k and 1.5x at 100k in ns per agent. That row does one pass
  over an array and no indexing at all, so this is the price of the work we do that it does not.

- **Against a GDScript loop that rebuilds a spatial structure we are 2.5x faster** at both counts. That is the
  comparison about design rather than about language: the same work, two implementations.

- **The tails are tighter than the ratios suggest.** Our p99 sits 10 to 14% above the median; the Godot grid row is
  10% above at 10k and 21% above at 100k, because a GDScript rebuild pays garbage collection and dictionary growth that
  the counting sort does not.

- **At 100,000 agents every row is over budget on this hardware**, including ours. That is a statement about a 6-core
  VM without SIMD-friendly clocks, not a hidden claim that 100k is playable here.

What it does not say: nothing in this table is a claim that Bevy is faster than Godot, or the reverse. Each row runs a
harness someone wrote in an afternoon, in that engine's own idiom, against a shared fixture.

## Reading the result

The vendor-published numbers are a different machine and stay labelled as such. They are reference points, not
comparisons; the table above is the comparison:

| system | workload | published | ns per agent |
| --- | --- | ---: | ---: |
| Godot 4.x, shared flow field (Ryzen 5 2600X, GDScript), vendor-published | 10,000 2D agents | 5.022 ms median | 502 |
| same | 20,000 | 9.541 ms | 477 |
| UE5 Mass Entity (community claim) | 100,000 agents, collision + pathfinding | ~10 ms | ~100 |
| bevy-flock, this harness, this sandbox (2026-11-09 re-run) | 10,000 | 5.661 ms median | 566 |
| bevy-flock, this harness, this sandbox (2026-11-09 re-run) | 100,000 | 46.977 ms median | 470 |

Two rules for anyone extending that table:

- **Never compare across machines.** Re-run the other engine's harness on the machine in question, or label the row
  "vendor-published, different machine".

- **Never quote `ns_per_agent` alone.** It is a convenience for reading the scaling shape; at 10k the per-agent cost is
  dominated by index rebuilds that amortise later, and at 1M by memory bandwidth. The six statistics are the result, and
  `ns_per_agent` is a gloss on one of them.

## Why this exists

`docs/dev/tuning/performance.md` can say what this crate costs. It cannot say whether that is good. A number with no peer is a
number with no meaning, and the only honest fix is a fixture both sides run.
