---
title: Performance
description: The numbers, the machine they came from, and the shape rather than the headline.
---

Every number here is a measurement with a named fixture. None of them are rounded up, and the ones that lost are included
because a tuning journal that only records wins is marketing.

**Reference machine:** the frame-budget rows below were measured on an Apple M3 Pro with 11 threads, macOS, `--release`;
the spatial-rebuild section was measured on a six-core aarch64 sandbox VM with no CPU model string exposed. Each section
names its own machine, and each section of `docs/dev/tuning/performance.md` names its own too. Treat all of it as shape
and ratios; measure your own machine before quoting it.

## Frame budgets

Measured on the M3 Pro, median of five runs each. The scalar row is the build with the `simd` feature off.

| what | population | measured | gate |
| --- | --- | --- | --- |
| band frame, wide kernel | 1,000,000 | **1.37 ms** | < 6 ms |
| band frame, scalar | 1,000,000 | 1.87 ms | < 6 ms |
| ambient with 5,000 manifested | 100,000 | 1.64 ms | < 8 ms |
| reflex tier frame | 250,000 | **13.51 ms** | < 14 ms |
| reflex tier frame | 300,000 | 14.92 ms | over |

## The spatial rebuild

Four passes: clear, count, prefix, place. Since phase 5 the scan and the placement are one dispatch, and the
allocation cursor is primed by the scan instead of copied by a pass of its own. At 1,000,000 bodies over 421,876
cells, on the machine above:

| configuration | total | notes |
| --- | --- | --- |
| one worker | 12.229 ms | serial reference, same binary |
| six workers | **4.572 ms** | 2.67× against that reference, 27% off the build before the fix |

Both of those numbers are the same binary, which is the only way a ratio means anything. The temptation is to divide
the old 6.289 ms by the new 4.572 ms and tell a 1.38× story about threads; 6.289 ms was six workers too, so that
division compares two parallel runs from different weeks.

Against the *pre-fix* serial build (22.943 ms) the same 4.572 ms is 5.02×, which is where the phase's 5× gate came
from, and why ADR 0060 says a gate phrased as a multiple has to name its denominator. Wall clock fell 27% while the
parallel ratio fell, because a third of the serial work stopped existing.

The honest read: the build is memory-bound, not thread-bound, and the two optimisations that looked best on paper,
bucketing the scatter and sharded histograms, were measured and rejected. `docs/dev/tuning/performance.md` has the
scatter probe that killed them, including the numbers that made the decision.

## The incremental rebuild

`SpatialGrid::build_incremental` is the rebuild a running game wants: it moves the bodies that changed cell and leaves
the rest of the table where it is. At 1,000,000 bodies a patched tick costs about 1.2 ms against 14 ms for a full
rebuild, so the question is never "how fast is the patch" but "how often does it give up". It gives up when a cell
receives more arrivals than it has slack for, and the slack is two knobs: `with_slack_percent` (a quarter of a cell's
occupancy by default) and `with_slack_floor` (two spare slots per cell, whatever its occupancy).

Neither is free. The percentage costs a quarter again in the run table; the floor costs two slots per cell, which on a
sparse grid is the larger bill (about 8 MB of table at 100,000 bodies, against 2.9 MB before). What they buy is the
tail: at 512 random crossers a tick, the default pair takes the share of ticks that patch from 61% to 99.5% and p99
from 2.83 ms to 0.58 ms at 100,000 bodies, and from 60.5% to 99.0% with p99 16.08 ms to 2.49 ms at a million.
A cell that is still full does not force a rebuild: the patch borrows a slot from the first run after it that has a
spare, moving the boundaries between them, bounded by 32,768 shifted elements and 64 consecutive full runs. That is what
makes a crowd flowing into one district survive: 512 crossers arriving inside a four-cell hotspot patched 0% of ticks
before and every tick after, taking the median from 2.58 ms to 0.35 ms at 100,000 bodies and from 13.98 ms to 1.31 ms at
a million, with p99 following. `RebuildOutcome::Rebuilt` reports a `RebuildCause` when even that is not enough.
`with_slack_floor(0)` and `with_slack_percent(0)` each restore the rule they replace, so a game can spend what its own
measurements justify. At a million bodies a floor of three patches every tick at every thread count, which is the
setting to reach for when a spike is not merely expensive but unacceptable. `examples/patch_tail.rs` is the harness those numbers come from.

## Memory

| property | scalar | wide |
| --- | --- | --- |
| per agent | 28 B | 32 B |
| 1,000,000 agents | 26.7 MiB | 30.5 MiB |
| steady-state allocations over 60 frames | 0 | 0 |

## What you should measure instead

Your crowd, your hardware, your tick budget. `cargo run --release --example profile_crowd` and
`--example cross_bench` are the harnesses; the comparison script refuses to mix machines, which is the point.

## Against other engines

Three harnesses, one fixture, one laptop: an Apple M3 Pro with 11 threads, 1,000 measured ticks each, the same
16.6 ms budget.

| agents | `bevy-flock` | Unreal 5.8.2 | Godot 4.7.2 grid |
| ---: | ---: | ---: | ---: |
| 10,000 | 2.217 ms | 0.937 ms | 4.674 ms |
| 100,000 | 16.528 ms, 47.8% of ticks over budget | 3.496 ms | 38.054 ms |

Faster than the Godot grid at both sizes, slower than the Unreal loop, and neither half is an engine verdict.
Unreal's row is a hand-written subsystem that rebuilds a counting-sort grid and reads at most eight neighbours,
not Mass Entity. Godot's rebuilds a dictionary grid and then does not query it. Ours is the whole plugin schedule
with every agent steered, no band and no culling, which is why the 100,000 row miss rate is the interesting
number: the banded path exists to cut it, and this row deliberately does not use it.

The full six-statistic table, the differences that make it a system comparison, and the measurements that would
tighten it are in `benchmarks/README.md`. `--workload all` is the one to start with there: it runs the Unreal
fixture's loop bare and inside an ECS system, and separates the schedule's share from the workload's.

That decomposition, on the same laptop: at 100,000 agents the fixture's loop costs 1.737 ms and this crate's full
schedule costs 15.940 ms, with the ECS wrapper adding nothing measurable. The loop is 2.0x faster than Unreal's on the
same hardware, so the 4.6x gap between the two product rows is this crate doing more: a twenty-seven-cell neighbourhood,
needs, temperament, cognition and social memory. That is a deliberate trade, and it is the number to look at before
asking for the fixture's workload instead. `python3 scripts/cross_bench.py --agents 10000,100000` reproduces
our side on your hardware.

Full journals, including the rejected optimisations and their measurements, are in the repository:
`docs/dev/tuning/performance.md` and `docs/dev/tuning/memory.md`.
