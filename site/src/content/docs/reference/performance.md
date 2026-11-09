---
title: Performance
description: The numbers, the machine they came from, and the shape rather than the headline.
---

Every number here is a measurement with a named fixture. None of them are rounded up, and the ones that lost are included
because a tuning journal that only records wins is marketing.

**Reference machine:** shared cloud vCPU, x86_64, `--release`, `--offline`. Treat these as shape and ratios; measure your
own machine before quoting them.

## Frame budgets

| what | population | measured | gate |
| --- | --- | --- | --- |
| band frame, wide kernel | 1,000,000 | **3.78 ms** | < 6 ms |
| band frame, scalar | 1,000,000 | 4.75 ms | < 6 ms |
| ambient with 5,000 manifested | 100,000 | 3.37 ms | < 8 ms |
| reflex tier ceiling | 64,000 / tick | at the ceiling | 64,000 |

## The spatial rebuild

Five passes — clear, count, prefix, place — after the phase-5 fusion. At 1,000,000 bodies over 421,876 cells:

| configuration | total | notes |
| --- | --- | --- |
| 1 worker | 6.289 ms | serial reference |
| 6 workers | **4.572 ms** | 1.38× |

The honest read: the build is memory-bound, not thread-bound, and the two optimisations that looked best on paper —
bucketing the scatter and sharded histograms — were measured and rejected. `docs/dev/tuning/performance.md` has the
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

Full journals, including the rejected optimisations and their measurements, are in the repository:
`docs/dev/tuning/performance.md` and `docs/dev/tuning/memory.md`.
