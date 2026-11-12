# What it costs

Every number here comes from `docs/dev/tuning/performance.md`, which names the host, the toolchain and
the lockfile. The point of this chapter is not the absolute milliseconds, those belong to a
machine, but the *ratios* and the *shape*, which belong to the design.

## The headline table

| Benchmark | Time | Per agent |
| --- | --- | --- |
| `flock_frame/1` | 2.66 µs | |
| `flock_frame/1000` | 299 µs | 0.30 µs |
| `flock_frame/10000` | **1.72 ms** | 0.17 µs |
| `flock_frame_large/100000` | 14.1 ms | 0.14 µs |
| `spatial_query/radius_8m/1000` | 40 ns | |
| `spatial_query/radius_8m/100000` | 160 ns | |
| `steering_step/32_neighbours` | 70 ns | |

Three things to read from it:

1. **10,000 agents fit a 60 Hz frame with 9.6× margin.** 1.72 ms against 16.6 ms. Before the
   kernel was rewritten the same crowd cost 18.2 ms, and 92% of the frame was steering.
2. **Per-agent cost is flat.** 0.30 µs at a thousand, 0.17 µs at ten thousand, 0.14 µs at a
   hundred thousand. A bigger crowd does not make any one agent more expensive, which is what
   makes bands possible at all.
3. **The fixed cost is gone.** `flock_frame/1` is 2.66 µs. A crowd that stands still rebuilds
   the index once and patches nothing after that, so what a frame costs is the work that scales
   with agents and nothing else.

## The crowd at scale

Band frame, wide kernel, on the M3 Pro.

| Configuration | Frame |
| --- | --- |
| 100,000 total, 5,000 manifested | 1.64 ms |
| 250,000 total, 5,000 manifested | 1.56 ms |
| 1,000,000 total, 5,000 manifested | 1.37 ms |

Ten times the population for *less* frame, because cost tracks what is *manifested* and the
manifested count is 5,000 in every row. The rest of the population is a density field, and
the field is not walked per body.

## Cognition

`benches/tiers.rs`, 10,000 NPCs, 14 ms budget, against a 0.62 ms baseline frame with the same
crowd and no brains:

| Composition | Frame | Delta | Who thought |
| --- | --- | --- | --- |
| All reflex (200 ns) | 1.44 ms | 0.82 ms | all 10,000, every tick |
| All reactive (2 µs) | 0.89 ms | 0.27 ms | ~1,750 per tick |
| All deliberative (8 µs) | 0.82 ms | 0.20 ms | ~262 per tick |
| All social (30 µs) | 0.83 ms | 0.21 ms | ~23 per tick |
| 95/4/1 mix | 1.47 ms | 0.85 ms | all 10,000, every tick (9,500 / 400 / 100) |

The expensive tiers are the *cheap* frames, because their reserved shares buy so few of them.
Frame cost is bounded by the budget; the tier decides who is served and how long they wait.
Reflex is the only composition where the whole population thinks every tick, and the only one
that costs most of a millisecond.

A reflex think costs about 52 ns end to end in the ECS — the query traversal and the tree
tick, with no spatial rebuild in the frame to amortise over it — so a crowd of 250,000
reflex NPCs still fits a 14 ms frame, measured rather than extrapolated. 300,000 does not,
at 14.92 ms. The same arithmetic on the declared 150 ns would say ~93,000; the declared cost
is what the scheduler spends, and it is not what the frame pays.

There are two ceilings, and they are different things. The frame is wall clock: how many
NPCs can be *in the world* before a frame costs 14 ms. The budget is virtual: 55% of
14 ms at a 200 ns declared cost buys exactly 38,500 reflex thinks, at every population
above it. Past 38,500 the extra NPCs wait their turn instead of costing more frame, which
is what a reservation is for.

The mix is the case worth reading twice. Its reserved shares buy 38,500 reflex thinks, 1,750
reactive and 262 deliberative, and the crowd is smaller than all three, so every NPC thinks
every tick — the frame costs the same as a crowd of pure reflex. That is the budget working
as intended: a reservation that is not needed is not spent.

The declared costs in the leaf catalogue are calibrated to within 20% of the measured
per-leaf cost, `benches/brain.rs` measures, `docs/dev/tuning/performance.md` records, and ADR 0009
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
