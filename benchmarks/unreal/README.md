# The Unreal side of the cross-engine benchmark

The fixture the other two harnesses run, in Unreal's own idiom: N agents in a 1 km cube, one shared goal, a fixed 60 Hz
step, a uniform-grid rebuild every tick, local separation, and one JSON line in the shape
[`cross-engine.md`](../../docs/dev/tuning/benchmarks/cross-engine.md) defines.

It is a `UTickableWorldSubsystem`, which is the whole reason there is no map to set up: adding the module to a project
is the entire installation. No actor to place, no blueprint to wire, no level to open.

**Status:** written against the UE 5.x API, not compiled in this repository's CI, because UE5 is a 100 GB install and
this project's gates run in a sandbox. Expect the first build to surface small version drift, and fix it there rather
than working around it.

## What it measures

Inside the subsystem's `Tick`, around the workload only:

1. `RebuildGrid()`: counting sort into cells, the same arithmetic as `SpatialGrid` in `bevy-flock-core`. Counts land in
   slot `cell + 1`, the prefix scan turns them into starts, the scatter fills `Sorted`.
2. `Step()`: seek the shared goal, separate from at most eight same-cell neighbours, integrate at a fixed `1/60` s, clamp
   to the cube.

Excluded from the measurement: the engine frame, rendering, physics bodies, audio, and the actor/component layer. That
is the same convention the Godot harness uses; the Rust harness measures `App::update`, which includes its plugin
schedule, so read the three `notes` fields together instead of assuming the measured regions are identical.

The grid covers the cube exactly: `RoundToInt(1000 m / 10 m)` gives 100 cells per axis, so 1,000,000 cells and a 4 MB
`CellStart` that is cleared every tick. That clear is a fixed cost at every population, which is why small agent counts
look worse per agent here than they do at 100k.

## Files

| path | what it is |
| --- | --- |
| `Source/CrossBench/CrossBenchSubsystem.h` | the fixture's interface |
| `Source/CrossBench/CrossBenchSubsystem.cpp` | grid rebuild, steering, statistics, JSON line |
| `Source/CrossBench/CrossBench.Build.cs` | module rules: `Core`, `CoreUObject`, `Engine` |
| `Source/CrossBench/CrossBench.cpp` | module registration |
| `Source/CrossBench.Target.cs` | game target |
| `Source/CrossBenchEditor.Target.cs` | editor target, so the project opens normally for anyone who wants to watch it |

## Setting it up

The module name is part of the project name, so the shortest path is a project called `CrossBench`.

1. Unreal Engine 5.4 or newer, then **Games > Blank**, C++, name it `CrossBench`, no starter content.
2. Copy this directory's `Source/` over the generated `CrossBench/Source/`, replacing the generated target files.
3. Regenerate project files (right-click `CrossBench.uproject`, **Generate Visual Studio project files**, or run
   `GenerateProjectFiles.sh` / `.bat`).
4. Build once:

   ```bash
   # macOS
   "$UE/Engine/Build/BatchFiles/Mac/Build.sh" CrossBenchEditor Mac Development -project="$PWD/CrossBench.uproject"
   # Linux
   "$UE/Engine/Build/BatchFiles/Linux/Build.sh" CrossBenchEditor Linux Development -project="$PWD/CrossBench.uproject"
   # Windows
   "%UE%\Engine\Build\BatchFiles\Build.bat" CrossBenchEditor Win64 Development -project="%CD%\CrossBench.uproject"
   ```

For an existing project, add an entry to the `Modules` array in the `.uproject` instead:

```json
{ "Name": "CrossBench", "Type": "Runtime", "LoadingPhase": "Default" }
```

## Running it

`UnrealEditor-Cmd` plus `-game -nullrhi` is a shipping-shaped process with no editor and no renderer. Add
`-stdout -FullStdOutLogOutput` to forward the benchmark record to standard output:

```bash
UnrealEditor-Cmd "$PWD/CrossBench.uproject" -game -nullrhi -log -stdout -FullStdOutLogOutput \
  -crossbenchagents=10000 -crossbenchticks=1000 -crossbenchwarmup=100 \
  -crossbenchmachine="unknown cpu, 6 threads" > unreal.log 2>&1
```

| flag | default | meaning |
| --- | --- | --- |
| `-crossbenchagents=` | 10000 | agents in the crowd |
| `-crossbenchticks=` | 1000 | measured ticks |
| `-crossbenchwarmup=` | 100 | unmeasured ticks first |
| `-crossbenchseed=` | 0xC0FFEE | spawn seed, same constant as the other harnesses |
| `-crossbenchbudget=` | 16.6 | the budget the over-budget share is counted against |
| `-crossbenchmachine=` | detected | machine label, see below |

The benchmark record is a `LogCrossBench` display message on stdout, and the process exits itself:

```text
LogCrossBench: Display: crossbench {"engine":"unreal","engine_version":"5.4.2-...","workload":"W1-fixed-tick","agents":10000,...}
```

If `crossbench starting:` never appears, the subsystem is not ticking: check that the run has `-game` (an editor world
is deliberately not supported) and that the module is in the project's `Modules` array.

## Why the machine flag exists

`scripts/compare_benchmarks.py` refuses a table whose rows disagree about the machine, which is the feature that keeps
this document honest. Unreal reports the real CPU brand; the Rust and Godot harnesses fall back to `unknown cpu` on a
VM whose `/proc/cpuinfo` has no model line. Same machine, three spellings.

`-crossbenchmachine=` is how you make the label agree. Copy the string from the rows already in the table. Do not use it
to merge results from different machines: the moment that label stops identifying one machine, the comparator's refusal
becomes the only thing standing between the table and a rumour.

## Comparing

```bash
python3 scripts/cross_bench.py --agents 10000,100000 --engines unreal \
  --unreal-cmd 'UnrealEditor-Cmd '"$PWD"'/CrossBench.uproject -game -nullrhi -log -stdout -FullStdOutLogOutput \
    -crossbenchagents={agents} -crossbenchticks={ticks} -crossbenchwarmup={warmup} \
    -crossbenchmachine="unknown cpu, 6 threads"'
```

The runner substitutes the agent, tick and warm-up counts, captures the row, and prints the table beside the other
engines. Add `bevy-flock,godot-grid` to `--engines` to get all three in one table, which is the only arrangement in
which the numbers mean anything.

## What this is not

It is not Unreal's shipping crowd stack. Mass Entity would be the fair comparison for "what does Epic's engine cost per
agent", and a harness written in an afternoon is the fair comparison for "what does a straightforward SoA loop cost in
this engine". Both are legitimate questions, and they have different answers. If you want the first, run `stat
MassEntity` and `stat unit` in a Mass scene and put that in `notes` next to the row, rather than mixing the two.
