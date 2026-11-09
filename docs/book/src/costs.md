# What it costs

Every number here comes from `docs/dev/tuning/performance.md`, which names the host, the toolchain and
the lockfile. The point of this chapter is not the absolute milliseconds — those belong to a
machine — but the *ratios* and the *shape*, which belong to the design.

## The headline table

| Benchmark | Time | Per agent |
| --- | --- | --- |
| `flock_frame/1` | 0.99 ms | — |
| `flock_frame/1000` | 1.73 ms | 1.7 µs |
| `flock_frame/10000` | **9.18 ms** | 0.92 µs |
| `flock_frame_large/100000` | 90.0 ms | 0.90 µs |
| `spatial_query/radius_8m/1000` | 138 ns | — |
| `spatial_query/radius_8m/100000` | 552 ns | — |
| `steering_step/32_neighbours` | 152 ns | — |

Three things to read from it:

1. **10,000 agents fit a 60 Hz frame with 1.8× margin.** 9.18 ms against 16.6 ms. Before the
   kernel was rewritten the same crowd cost 18.2 ms, and 92% of the frame was steering.
2. **Per-agent cost is flat.** 1.7 µs at a thousand, 0.92 µs at ten thousand, 0.90 µs at a
   hundred thousand. A bigger crowd does not make any one agent more expensive, which is what
   makes bands possible at all.
3. **The fixed cost is the grid, not the crowd.** `flock_frame/1` is 0.99 ms, and almost all
   of it is the index rebuild at the default 1 km volume. Match the volume to your playable
   area: 200 m across is about 20 µs.

## The crowd at scale

| Configuration | Frame |
| --- | --- |
| 100,000 total, 5,000 manifested | 3.72 ms |
| 1,000,000 total, 5,000 manifested | 3.78 ms |

Ten times the population for 1.6% more frame, because cost tracks what is *manifested*.

## Cognition

`benches/tiers.rs`, 10,000 NPCs, 14 ms budget, against a 4.23 ms baseline frame with the same
crowd and no brains:

| Composition | Frame | Delta | Who thought |
| --- | --- | --- | --- |
| All reflex (200 ns) | 6.40 ms | 2.17 ms | all 10,000, every tick |
| All reactive (2 µs) | 4.85 ms | 0.62 ms | ~1,750 per tick |
| All deliberative (8 µs) | 4.77 ms | 0.54 ms | ~262 per tick |
| All social (30 µs) | 4.68 ms | 0.45 ms | ~23 per tick |
| 95/4/1 mix | 6.42 ms | 2.19 ms | ~9,700 / 175 / 13 |

The expensive tiers are the *cheap* frames, because their reserved shares buy so few of them.
Frame cost is bounded by the budget; the tier decides who is served and how long they wait.
Reflex is the only composition where everybody thinks every tick, and the only one that costs
2 ms — which is why the reflex ceiling is ~64,000 per tick at 14 ms, measured at ~217 ns per
reflex think end to end in the ECS rather than the ~93,000 that dividing the budget by the
declared 150 ns would suggest. Query traversal, blackboard access and change detection are
real costs no declared hint includes.

The declared costs in the leaf catalogue are calibrated to within 20% of the measured
per-leaf cost — `benches/brain.rs` measures, `docs/dev/tuning/performance.md` records, and ADR 0009
explains why a virtual cost rather than wall time is what the scheduler spends.

## The three rejected designs

Worth knowing, because each looks reasonable and each was measured:

| Design | 10,000 agents | Why it lost |
| --- | --- | --- |
| 15.6 million dense cells | 54.4 ms | prefix sum over every cell, every tick, whatever the population |
| No cells, sort and binary search | 78.3 ms | `O(n log n)` build plus a search per query |
| Packed SIMD (lanes are neighbours) | no gain | gather cost swamped the vector work (ADR 0029) |

The shipped design is a dense index with a hard cell budget, coarsened to keep cells
populated, plus a transposed SIMD road (lanes are *agents*, ADR 0030) which is 21-29% faster
in the kernel and 20.4% on the 1M band frame.

## Reproducing any of this

```bash
cargo bench --locked --bench crowd -- flock_frame
cargo bench --locked --bench tiers -- tier_frame
cargo bench --locked --bench band  --features simd -- band_frame
```

Rules that make the numbers mean something:

- **`--locked`.** The lockfile is committed for this reason; a different `wide` or `criterion`
  is a different measurement.
- **Discard the first run** after a rebuild: page cache and CPU governor.
- **Compare ratios, not absolutes.** A shared cloud vCPU is not your dev machine, and neither
  is a player's laptop.
- **The gate is a ratio too.** `scripts/scaling_regression.py` compares speedup curves against
  the committed baseline with a tolerance, because absolute times move with the host and
  ratios do not.

## The budget you should set

A crowd is a fraction of a frame, and the fraction is your choice:

| Frame budget | Crowd | Thinking | Notes |
| --- | --- | --- | --- |
| 16.6 ms (60 Hz) | 6 ms | 1-2 ms | leaves room for rendering and gameplay |
| 8.3 ms (120 Hz) | 2 ms | 0.5 ms | bands, not a bigger crowd |
| 33 ms (30 Hz) | 12 ms | 4 ms | the comfortable maximum for a full simulation |

Past those, the answer is not a faster kernel: it is fewer manifested entities. That is what
the bands are for, and it is why the per-agent number being flat matters more than the
headline number being small.
