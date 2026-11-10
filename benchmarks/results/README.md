# Archived benchmark results

The JSONL files here preserve benchmark output that was previously kept under ignored `target/` paths. Each line is one complete JSON result record.

## 2026-11-09, Apple M3 Pro

- [`m3-pro/300-ticks/bevy-flock.jsonl`](2026-11-09/m3-pro/300-ticks/bevy-flock.jsonl) contains the `--workload all` run at 10,000 and 100,000 agents, with 100 warm-up ticks and 300 measured ticks.
- [`m3-pro/1000-ticks/bevy-flock.jsonl`](2026-11-09/m3-pro/1000-ticks/bevy-flock.jsonl), [`unreal.jsonl`](2026-11-09/m3-pro/1000-ticks/unreal.jsonl), and [`godot-grid.jsonl`](2026-11-09/m3-pro/1000-ticks/godot-grid.jsonl) contain the three-engine comparison, with 100 warm-up ticks and 1,000 measured ticks.
- [`m3-pro/1000-ticks/godot-grid-runner.jsonl`](2026-11-09/m3-pro/1000-ticks/godot-grid-runner.jsonl) preserves the separate Godot result emitted by the cross-engine runner. Its measurements differ from `godot-grid.jsonl`, so keep these runs distinct.

The runs are observations from one machine, not a controlled comparison across hardware. Their JSON `notes` fields describe the measured work. See the [benchmark guide](../../docs/dev/tuning/benchmarks/cross-engine.md) and [comparison notes](../README.md) for workload caveats.
