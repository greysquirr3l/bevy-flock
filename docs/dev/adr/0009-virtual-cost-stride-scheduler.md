# ADR 0009 — Cognition is scheduled against a virtual cost in integer nanoseconds

- Status: Accepted, planner implemented in `application::schedule`
- Date: 2026-11-04
- Supersedes: the wall-clock reading of ADR 0008's predecessor task ("nanosecond
  budget measured with an injectable clock")

## Context

10,000 NPCs with behaviour trees, and 100,000 more behind them, cannot all think
every tick. Something has to decide who thinks now, and the decision has two
requirements that pull in opposite directions:

1. **Bounded cost.** The frame must not blow up because a designer added an
   expensive behaviour.
2. **Replayable.** The same seed and the same command log must produce the same
   world. This crate already pays for that property with PCG32 streams, logical
   ticks, `libm` and ordered queries; a scheduler that reads `Instant::now()`
   would throw all of it away.

The obvious implementation fails requirement 2. A budget expressed in real
nanoseconds is measured against a machine that is faster or slower than the one
that recorded the replay, so a different set of NPCs thinks on tick 4,112 and the
world diverges. "Use an injectable clock in tests" does not help: tests are not
replays.

The other obvious implementation — tiered ticks, `Near` every tick, `Far` every
16th — satisfies both requirements and caps quality: a tier promises a *cadence*,
not a *cost*, so one expensive behaviour added to the `Near` tier blows the frame
and the fix is always "make the far tier dumber". The interesting NPCs end up
being the ones the player can see, which is the opposite of a crowd.

## Decision

**Cost is virtual, time is virtual, and the queue is fair.**

1. Every registered logic unit declares a `VirtualCost` in virtual nanoseconds
   (`LeafBehavior::cost_hint`, already in `tree.rs`). A tree's cost is the sum of
   its leaves' declared costs, stored per NPC and recomputed when its logic
   changes.
2. Each tick has a budget `ThinkBudget { virtual_nanos: u64 }`. The scheduler
   dispatches NPCs until the spent virtual cost reaches the budget.
3. Selection is **start-time fair queueing** (SFQ) over integer tags, with a total
   tie-break order `(tag, NpcId)`. No floats, no heap instability, no wall clock.
4. Detail tiers stop being an input. They are an *output*: tier is derived from
   measured think share and distance for display and for culling, and it feeds
   weight, not cadence. Hysteresis applies to the weight.

## The mechanism

Each NPC carries a weight `w ∈ 1..=1000` (`ThinkWeight`, permille of importance)
and a start tag `s`. Per tick, with virtual clock `vt` (the tag of the last
dispatched NPC):

```text
candidate: min over ready NPCs of (max(s_i, vt))        tie-break: NpcId
dispatch:  tag_i = max(s_i, vt) + (cost_i * K) / w_i     K = 1024
           s_i   = tag_i
           vt    = tag_i
           spent += cost_i
stop when  spent >= budget
```

Three details matter and each is a bug avoided:

- **`max(s_i, vt)` is what makes it fair.** Without it an NPC that idled while
  others thought accumulates a low tag and then monopolises the queue. This is
  the difference between SFQ and naive stride scheduling, and it is why SFQ has a
  fairness bound and stride scheduling alone does not.
- **`K / w_i` keeps tags integral.** Weights are permille, `K = 1024`, so the
  division is exact for every weight in range and no float ever enters a decision.
- **`(tag, NpcId)` is a total order.** Two NPCs with equal tags resolve the same
  way on every machine, every run, every thread count.

Overshoot is bounded by one dispatch: the budget is checked *before* each think, so
a frame can exceed it by at most the cost of the single most expensive registered
tree — a number the game can read at startup.

## Why this is replayable

The dispatched set is a pure function of `(ready set, tags, weights, declared
costs, budget)`. All of those are integers in simulation state, and all of them
are either in the snapshot or derived from it. Two machines with a 4× difference
in real speed dispatch the same NPCs in the same order, because neither machine's
speed appears in the computation.

Real time is still measured — in the stats resource only, never in a decision —
so a game can see that its budget of 2,000,000 vns costs it 1.7 ms on this
hardware today.

## The starvation bound

With total weight `W = Σ w_i`, budget `B` vns and uniform think cost `c`, an NPC
of weight `w` receives `B·w/(c·W)` dispatches per tick, so its maximum wait is

```text
max_wait_ticks = ceil(c · W / (B · w)) + 1
```

The `+1` is the partial round in which it is queued but the budget ran out. That
is a *bound*, not a hope: it is the property tiering cannot state, and it is
asserted by a test rather than a comment — 10,000 NPCs, mixed weights, 1,000
ticks, no NPC exceeding the computed bound.

It also degrades in the direction a crowd should: when the budget shrinks, every
NPC's interval grows proportionally, instead of the far half of the world going
silent.

## The calibration gate

A declared cost is a lie that ages: hardware changes, a leaf grows a path query,
and the model drifts while the frame budget quietly stops meaning what it says.

So the model is checked, in CI, on a fixed runner:

- `benches/cost_model.rs` measures every registered built-in leaf.
- `tests/cost_model.rs` runs the same measurement as an assertion: **a leaf whose
  measured cost deviates from its declared cost by more than 20% fails the
  build**, with the measured number in the message so the fix is a one-line edit
  to the declaration plus an ADR note if the cost change was intentional.
- Only *relative* costs are load-bearing (they set the fairness ratios), so the
  gate compares each leaf against the reference runner's calibrated value rather
  than demanding absolute nanoseconds on a laptop. Drift on other hardware is
  expected and harmless; drift in the *model* is the failure being caught.

## Consequences

- Replay, rollback and golden digests survive budgeting. This is the whole point.
- A game tunes one number (`virtual_nanos`) and one weight policy, and the tier
  distribution falls out of it, visible in `ThinkStats`.
- Adding an expensive behaviour is a *gradual* cost increase across the crowd
  rather than a cliff at a tier boundary — and the calibration gate tells you
  immediately how much you just spent.
- Per-NPC state grows by `ThinkWeight` + `tag` + `cost` (16 bytes).
- Declared costs must be maintained. The gate makes that a build failure rather
  than a mystery slowdown, which is the best available answer.
- Social memory enters this scheduler as per-leaf cost and a bounded weight boost,
  never as eligibility — see ADR 0011 for the four rules and the feedback loop
  they close.
- The scheduler is single-threaded by construction. Parallel dispatch would need
  the tag arithmetic to be order-independent; deferred until a profile demands
  it (see ADR 0001's note on parallel reductions).

## Alternatives rejected

- **Wall-clock budget with an injectable clock** — deterministic in tests, not in
  replays, which is where it matters. The whole determinism budget of this crate
  would have been spent to save a cost model.
- **Think-every-N-frames tiers** — the industry default, predictable, and a
  quality ceiling: cost is unmodelled, so an expensive behaviour is a cliff.
- **Priority queue with force-promotion at `max_staleness`** — starvation is
  prevented by a second policy bolted on top, and the two policies can disagree
  (promoted NPCs starve the ones just below the threshold). SFQ needs no
  promotion rule because the bound is structural.
- **Random subset each tick** — cheap, unbiased in expectation, and unbounded per
  individual: an NPC can wait arbitrarily long, which is exactly what a game with
  a quest NPC cannot tolerate.
- **Floating-point weights and tags** — drift and platform variance in the one
  place determinism is hardest to debug.

## Implementation notes (2026-11-04)

The planner is `application::schedule::ThinkScheduler`: a min-heap of
`(virtual_finish_time, id)` with the id breaking ties, so the plan is reproducible
without a sort. It is free of Bevy and of every clock — `plan` takes a budget and a
cost function and returns the participants to run, which is what makes the fairness
properties testable without a world.

Two things the implementation settled that this record did not.

**A participant thinks once per tick; "as often as its cost allows" was wrong.** The
first version advanced a participant's virtual time and put it straight back in the
queue — textbook fair queueing — and produced 1,383 thinks for a single participant in
one tick, because the cost model said the budget could afford them. A think is not a
packet: it produces a status the world reads once. Participants that have already run
are now held out of the queue for the rest of the tick and returned at the same virtual
time, so a cheap NPC runs on most ticks without running many times on one.

**Deciding is not free.** The planner costs ~150 ns per selected participant, so 1.8 ms
to choose 12,124 of 100,000. That is 11% of a 16.6 ms frame spent on the decision, so a
game at that scale must set its virtual budget against what it can afford to spend
deciding, not against the whole frame.

Measured with a 16.6 ms virtual budget and the cost mix from `benches/brain.rs`:

| Participants | Thinks per tick | Served per tick | Worst wait |
| --- | --- | --- | --- |
| 1 | 1 | 100% | 1 tick |
| 1,000 | 1,000 | 100% | 1 tick |
| 10,000 | 8,872 | 88.7% | 3 ticks |
| 100,000 | 12,124 | 12.1% | 45 ticks |

Ten thousand NPCs cannot all think every tick at this cost mix, which is the finding
that justifies the mechanism rather than an assumption behind it: the budget buys
88.7% of them and the rest are served within three ticks, because the queue is ordered
by virtual time rather than by a modulo.
