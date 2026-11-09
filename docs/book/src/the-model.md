# The model

Everything in this crate follows from four decisions. If you understand them, the rest is
detail.

1. **A tick is ordered, and the order is a schedule, not a hope.** Six sets, in sequence.
2. **Thinking is a budget spent on a queue, not a callback per NPC.**
3. **Distance decides how hard an NPC thinks.** Four tiers, promoted and demoted.
4. **Randomness is a stream per identity, never a global generator.**

## One tick, six sets

```text
Spawn ──▶ Perception ──▶ Cognition ──▶ Steering ──▶ Integration ──▶ AdvanceTick
```

`FlockSet` is a `SystemSet`, so a game inserts its own systems with the same ordering and
gets the same guarantees:

| Set | What happens | What it costs |
| --- | --- | --- |
| `Spawn` | queued identities become entities, within a per-frame budget | proportional to the budget, not the crowd |
| `Perception` | the spatial index is rebuilt; who noticed whom is refreshed; memory views are published | `O(n)` rebuild, `O(k)` queries |
| `Cognition` | the scheduler picks who thinks, and those NPCs tick their trees | bounded by the budget |
| `Steering` | desired velocities from neighbours | the hot path: `O(n · k)` |
| `Integration` | velocities become positions | `O(n)` |
| `AdvanceTick` | the logical clock moves | constant |

Perception before cognition is not cosmetic: a brain that reads stale neighbours decides
from a world that no longer exists, and a crowd of brains doing that once per frame produces
the "everyone turns at once" artefact. Steering after cognition means a decision made this
tick moves the body this tick, which is what makes a bark land on the frame it was spoken.

## Thinking is a budget

A naive crowd ticks every brain every frame: 10,000 brains × even 2 µs is 20 ms, and that is
before the crowd moves. So thinking is a resource:

```rust
let schedule = ThinkSchedule::new(capacity, VirtualCost::from_micros(900));
```

- **Virtual cost, not wall time.** Every leaf declares what it costs
  (`LeafBehavior::cost`), and the scheduler spends *that* number. A frame's thinking is then
  a property of the world, not of the machine, so two runs of the same seed make the same
  decisions even if one ran on a busy host. `docs/dev/tuning/performance.md` records the calibration
  that keeps the declared numbers within 20% of measured.
- **A queue, not a loop.** Participants are ordered by a stride schedule, so no NPC starves
  and no NPC hogs: each has a pass value that advances only when it thinks, and the lowest
  pass goes first. A crowd of ten thousand therefore gets even attention rather than "the
  first thousand think and the rest are furniture".
- **At most one think per NPC per tick.** The queue advances a participant's pass when it
  thinks, so a cheap brain cannot be picked twice in a frame and crowd out an expensive one.

The consequence to internalise: **the budget decides how many NPCs think, and the scheduler
decides which ones.** If a scene needs everyone to react, the answer is cheaper tiers, not a
bigger budget.

## Four tiers

| Tier | What runs | Roughly |
| --- | --- | --- |
| `Reflex` | fixed rules, no tree, no allocation | everyone |
| `Reactive` | a small tree over a handful of leaves | hundreds |
| `Deliberative` | the full compiled tree | dozens |
| `Social` | deliberation plus memory, dialogue, relationships | a handful |

Distance and engagement pick the tier (`TierPolicy`), each tier gets a guaranteed share of
the budget (`BudgetShares`), and promotions are rate-limited (`promotions_per_tick`, default
16) so that a player rounding a corner into a packed square does not promote four hundred
NPCs in one frame and drop it. Demotion has hysteresis, a wider band than promotion, so an
NPC standing exactly on a boundary does not oscillate between tiers and pay both costs
forever.

This is the single biggest lever on cost, and it is also the most honest model of a crowd:
the people near you are thinking about you, the people across the square are walking.

## Determinism, precisely

Same seed + same binary + same tick sequence ⇒ same world. That is a testable claim, and
`tests/determinism.rs` tests it with a whole-world digest.

How it is achieved:

- **A stream per identity.** `RngStream` is PCG32 seeded from `(sim_seed, identity, purpose)`,
  so NPC 431's decisions do not depend on how many other NPCs spawned before it, or on the
  order the ECS happened to iterate. Reference vectors in `domain::rng` pin the generator
  itself.
- **Ordered iteration.** The grid, the spawn queue and the scheduler all produce a
  deterministic order: bucket order within a cell, index order within a bucket, lowest pass
  first in the queue.
- **Integer time.** The clock advances in ticks; nothing reads `Instant::now`. A frame that
  took 40 ms and a frame that took 4 ms produce the same decisions.
- **Transcendentals through `libm`.** `sin`, `cos` and friends are the platform's; the crate
  uses `libm` so a wasm build and a native build agree to the last bit (ADR 0004).
- **Budgets are virtual.** See above: spending wall time would break the property the moment
  another process stole a core.

What is *not* guaranteed: bit-identical floats across compilers and optimisation levels. The
digest test runs in one profile; a game that needs cross-build replay should hash its inputs,
not its transforms.

## Where to look in the code

| Concept | File |
| --- | --- |
| The schedule and plugin | `src/adapters/primary/plugin.rs` |
| The scheduler and tiers | `src/application/schedule.rs`, `src/application/tiered.rs` |
| The tier policy | `src/domain/cognition.rs`, `src/domain/weight.rs` |
| The hot path | `src/application/crowd.rs` |
| The index | `src/domain/spatial.rs` |
| The streams | `src/domain/rng.rs` |
