# Archived benchmark results

The JSONL files here preserve benchmark output that was previously kept under ignored `target/` paths. Each line is one complete JSON result record.

## 2026-11-09, Apple M3 Pro

- [`m3-pro/300-ticks/bevy-flock.jsonl`](2026-11-09/m3-pro/300-ticks/bevy-flock.jsonl) contains the `--workload all` run at 10,000 and 100,000 agents, with 100 warm-up ticks and 300 measured ticks.
- [`m3-pro/1000-ticks/bevy-flock.jsonl`](2026-11-09/m3-pro/1000-ticks/bevy-flock.jsonl), [`unreal.jsonl`](2026-11-09/m3-pro/1000-ticks/unreal.jsonl), and [`godot-grid.jsonl`](2026-11-09/m3-pro/1000-ticks/godot-grid.jsonl) contain the three-engine comparison, with 100 warm-up ticks and 1,000 measured ticks.
- [`m3-pro/1000-ticks/godot-grid-runner.jsonl`](2026-11-09/m3-pro/1000-ticks/godot-grid-runner.jsonl) preserves the separate Godot result emitted by the cross-engine runner. Its measurements differ from `godot-grid.jsonl`, so keep these runs distinct.

## 2026-11-09, six-core aarch64 virtual machine

The harness names this machine from `CPU implementer` and `CPU part`, because the kernel publishes no model line:
`ARM Neoverse N1, 6 threads`.

- [`sandbox-aarch64/pool-sweep/bevy-flock.jsonl`](2026-11-09/sandbox-aarch64/pool-sweep/bevy-flock.jsonl) is the plugin
  workload at 10,000 and 100,000 agents, 200 ticks after 100 warm-ups, once per compute-pool shape (1, 2, 4 and 6
  threads), each row repeated three times. The median of the 100,000-agent row falls from 127.008 ms on one pool thread to
  44.365 ms on six, which is 2.86x, and the spread across the three repeats is 0.14% to 3.18%, so that difference is far
  outside the noise.
- [`sandbox-aarch64/build-passes/bevy-flock-build.jsonl`](2026-11-09/sandbox-aarch64/build-passes/bevy-flock-build.jsonl)
  is the spatial rebuild at 10,000, 100,000 and 1,000,000 bodies, on 1 and 6 workers, with the harness's own seven repeats.
  At one million bodies the total falls from 12.246 ms to 4.535 ms, which is 2.70x. The per-pass figures there carry
  spreads of 8% to 28%: `clear` 2.745 to 0.719 ms, `count` 2.022 to 1.740 ms, `prefix` 0.411 to 0.105 ms, `place` 7.056 to
  1.972 ms.

A caveat the spread column exists to state: the same harness at 10,000 bodies reports passes of 27 to 410 microseconds
with spreads from 2% to 625%. Those figures are too small for this machine to measure reliably, so only the million-body
rows above are worth quoting. The earlier prose tables that reported 10,000-body passes to three decimal places were
overstating what a shared VM can resolve.

The runs are observations from one machine, not a controlled comparison across hardware. Their JSON `notes` fields describe the measured work. See the [benchmark guide](../../docs/dev/tuning/benchmarks/cross-engine.md) and [comparison notes](../README.md) for workload caveats.
