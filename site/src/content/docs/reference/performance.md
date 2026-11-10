---
title: Performance
description: The numbers, the machine they came from, and the shape rather than the headline.
---

Every number here is a measurement with a named fixture. None of them are rounded up, and the ones that lost are included
because a tuning journal that only records wins is marketing.

**Reference machine:** a six-core aarch64 sandbox VM with no CPU model string exposed, `--release`, `--offline`. The
animation-envelope figures in the journals came from an earlier six-core x86-64 VM, and each section of
`docs/dev/tuning/performance.md` names its own machine. Treat all of it as shape and ratios; measure your own machine
before quoting it.

## Frame budgets

| what | population | measured | gate |
| --- | --- | --- | --- |
| band frame, wide kernel | 1,000,000 | **3.78 ms** | < 6 ms |
| band frame, scalar | 1,000,000 | 4.75 ms | < 6 ms |
| ambient with 5,000 manifested | 100,000 | 3.37 ms | < 8 ms |
| reflex tier ceiling | 64,000 / tick | at the ceiling | 64,000 |

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

The full six-statistic table, the differences that make it a system comparison, and the four measurements that
would tighten it are in `benchmarks/README.md`. `python3 scripts/cross_bench.py --agents 10000,100000` reproduces
our side on your hardware.

Full journals, including the rejected optimisations and their measurements, are in the repository:
`docs/dev/tuning/performance.md` and `docs/dev/tuning/memory.md`.
