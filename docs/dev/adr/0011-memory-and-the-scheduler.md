# ADR 0011 — Social memory enters the scheduler as cost and weight, never as a tier

- Status: Accepted
- Date: 2026-11-04
- Relates to: ADR 0009 (virtual-cost stride scheduler), ADR 0010 (social memory)

## Context

ADR 0009 schedules cognition by declared virtual cost and a fair queue; ADR 0010
gives an NPC a bounded, integer memory of the subjects it has met. Both are
accepted. The question this record answers is what happens where they meet, because
two systems that each look deterministic can compose into a feedback loop that is
neither bounded nor replayable.

Three couplings are unavoidable, and each has an obvious-but-wrong implementation:

1. **A think that reads memory costs more than one that does not.** Charge it badly
   and the budget stops meaning anything.
2. **An NPC that remembers you arguably deserves to think sooner.** Wire that
   straight into tiering and the population of thinkers becomes self-reinforcing.
3. **Memory is written by perception and read by cognition.** Mix the two budgets
   and neither can be reasoned about.

The loop is easy to state: grudge → higher weight → dispatched more often → a
higher measured think share → promoted to a nearer tier → stays eligible → meets
the subject more often → stronger grudge. Nothing in that chain is wrong on its
own; together they have no fixed point, and the NPC that the player once shoved
becomes the only NPC in the world that thinks.

## Decision

**Cost is declared per leaf. Weight is boosted per relationship. Eligibility
follows the scheduler, and the scheduler never consults memory.**

Four rules, in the order they bind.

### 1. Memory reads are ordinary leaves, and they declare what they cost

`HasGrudge`, `RemembersSubject` and `FamiliarityAbove` are `LeafBehavior`
implementations like any other. They read three integers from the blackboard that
the perception stage already wrote; they do not reach into the store during a
tick, and they declare a cost measured by `benches/cost_model.rs` the way every
other leaf does.

A store read costs about 8.5 ns (ADR 0010), so those leaves declare roughly 8 vns
against a predicate's 2. That makes a guard with a grudge branch go from 27 vns to
about 44 — a number that must be re-pinned in `Archetype::logic`'s cost table, and
which the ±20% calibration gate then defends.

The scheduler needs no new code for this. Cost is already per-NPC and recomputed
when logic changes (ADR 0009, decision 1), so a crowd whose guards can hold grudges
is simply a crowd of more expensive guards, and SFQ allocates their thinking
proportionally. **There is no per-feature surcharge, and there must never be one:**
a flat "memory tax" would charge the NPC who has met nobody for the store the NPC
next to it is using.

### 2. Writes belong to perception; reads belong to cognition; the budgets do not mix

`remember` (7.7 ns) is called from the perception stage, charged to the frame, and
bounded by the encounter rate limiter — one encounter per (NPC, subject) pair per
configured interval. In the worst realistic case, five hundred NPCs in radius,
four subjects, a thirty-tick limiter, that is tens of writes per tick: under a
microsecond.

`recall_decayed` (8.5 ns) is called inside a think, and its cost is already in the
leaf's declaration from rule 1.

`ThinkBudget` therefore measures *thinking*, not housekeeping. Two operations are
explicitly outside it:

- **`sweep`** (288 µs worst case over 16k rows) is a maintenance pass on a
  seconds-scale cadence, in its own stage, never charged to `ThinkBudget`. A
  scheduler that paid for sweeping would ration cognition to pay for bookkeeping.
- **Encounter detection** is a radius query against subjects, which the spatial
  index already answers, and it runs in the perception stage.

### 3. The scheduler owns tiers and eligibility; memory follows

ADR 0009 makes tier an *output* — derived from measured think share and distance,
with hysteresis, feeding weight. ADR 0010 makes eligibility the thing that sizes
the store. The dependency therefore runs one way only:

```text
scheduler state + distance  →  tier  →  eligible  →  enrolled in the store
```

Memory may not enrol an NPC, may not keep an NPC enrolled, and may not feed back
into tier. If a game wants a relationship to survive an NPC leaving the near tier,
the mechanism is `MemoryArchive` (park on release, restore on return), not a
retention that fights the scheduler for eligibility.

Two consequences worth stating plainly:

- **Cost starvation is not tier exit.** An NPC that did not get dispatched this
  tick is not less interested in the player; releasing its memory because the
  budget was tight would make a busy frame silently erase grudges, and the same
  replay at a different budget would erase different ones. Release is driven by
  distance-driven tier, and by nothing else.
- **Promotion is evaluated in the maintenance pass, not in a think.** A think stays
  a reader. If a think could promote a relationship, then thinking more would
  produce more promoted relationships, which is the loop this ADR exists to close,
  arriving by another door.

### 4. A relationship raises weight, bounded, and never raises cost

ADR 0009 gives each NPC `w ∈ 1..=1000` permille and derives the tag increment from
`(cost · K) / w` with `K = 1024`. That is the whole hook needed:

```text
w_effective = min(W_MAX, w · (1024 + boost) / 1024)      when a remembered subject
                                                        is within the memory radius
```

- `boost` is integer permille, declared in `MemoryConfig`, clamped, and applied
  only when a subject this NPC remembers is within the memory radius. The check is
  free: the perception pass already computed that distance to decide whether to
  write an encounter.
- The division stays exact because `boost` is chosen from the same permille scale
  as `w`; no float enters a dispatch decision.
- `w_effective` is applied at dispatch time and is **excluded from the share that
  derives tier.** That exclusion is the loop-breaker: an NPC that thinks more often
  because it recognises the player does not thereby climb into a nearer tier and
  stay eligible. It thinks sooner, and then it thinks no sooner than its base
  weight earns.

The starvation bound of ADR 0009 still holds, because it is expressed in terms of
the weights the scheduler actually uses:

```text
max_wait_ticks = ceil(c · W / (B · w_i)) + 1
```

with `W` the total including boosts. Clamping `w_effective` to `W_MAX` is what
keeps a horde of grudges from making `W` meaningless; the bound is then checked
against the boosted weights in the same test that already checks 10,000 NPCs at
mixed weights.

## What this costs

At the ADR 0009 example budget of 2,000,000 vns per tick:

| Crowd | Cost per NPC | Crowd cost | NPCs the budget admits |
| --- | --- | --- | --- |
| Guards, no memory branches | 27 vns | 270,000 | 74,074 |
| Guards with a grudge branch | 44 vns | 440,000 | 45,454 |

So at 10,000 NPCs — the population this crate is built for — a grudge branch costs
nothing in scheduling terms: every guard still thinks every tick, with 78% of the
budget left over. The ceiling moves from 74k NPCs to 45k, and it moves *gradually*,
per NPC, which is exactly the property ADR 0009 chose SFQ for. A tiered scheduler
would have discovered this as a cliff at the tier boundary.

Reading memory inside a think is 8.5 ns against a 45 ns guard think: a fifth of a
think, paid by the NPCs whose logic actually reaches for it.

## Why this stays replayable

Every input to a dispatch decision remains an integer in simulation state:
`w`, `w_effective`, tag, declared cost, budget. `boost` is a function of
`(store contents, subject positions at tick T, config)`, and store contents are a
function of the encounter log. `recall_decayed` is a pure function of stored
integers and `now`, so a replay re-derives identical boosts without any float in
the path.

The store is pre-allocated, so nothing in this feature allocates during play, and
a snapshot of memory is a `copy_from_slice` of 16-byte slots rather than a graph
walk. Rollback and golden digests therefore cover social memory at the same cost
as they cover position.

## Consequences

- **The calibration gate now covers memory leaves.** Their cost is dominated by a
  memory access, so it is noisier than a blackboard read. Declare the warm-store
  number and measure with a warm store; if the gate flakes at ±20%, the fix is a
  better fixture, not a looser tolerance.
- **Every archetype that gains a branch re-pins its declared cost.** That is the
  intended friction: the table is the scheduler's model of the crowd, and it should
  not change by accident.
- **Recomputing a cost must not reset the tag.** A tag reset is a fairness event —
  it hands the NPC a fresh, low tag and lets it jump the queue. If a game swaps
  trees when a relationship crosses a threshold, the swap changes `cost` and leaves
  `s` alone; otherwise an NPC that keeps meeting the player keeps jumping the queue.
- **`W_MAX` is a policy knob with a fairness cost.** Raise it and recognising NPCs
  react faster while everyone else waits fractionally longer; the bound formula
  says exactly how much, which is why the knob is in permille and not "high/low".
- **Memory cannot make a crowd affordable.** If a game wants 100,000 NPCs holding
  grudges, the answer is fewer thinkers per tick, not cheaper memory. This ADR
  does not create budget; it only decides who spends it.
- **Two knobs, one of which is dangerous.** `boost` is safe because it is clamped
  and excluded from tier. Anything that lets memory influence enrolment is not, and
  is a design error rather than a tuning choice.

## Alternatives rejected

- **A flat per-NPC memory surcharge.** Wrong shape: the cost of memory scales with
  encounters, not population, and a surcharge charges the hermit for the
  socialite's store.
- **Letting a relationship keep an NPC eligible.** Unbounded: the eligible set
  becomes "everyone the player has ever met", which is the population-sized store
  ADR 0010 exists to prevent, arrived at by a side door.
- **Charging the scheduler for sweeping.** Turns housekeeping into a reason to
  ration thinking; a busy frame would then spend its budget reclaiming rows instead
  of running NPCs.
- **Expressing interest as a utility consideration instead of a weight.** Utility
  decides *what* an NPC does; weight decides *how often* it thinks. Conflating them
  makes the budget unanalysable, because the thing that changes the answer is then
  inside the cost model rather than beside it.
- **Recomputing tree cost per think from current memory.** Puts a store read into
  the scheduler's inner loop, where it would be paid for every candidate in the
  ready set rather than once per dispatch.
- **Resetting tags when logic changes.** Cheap to implement, and it converts every
  relationship change into a queue-jump.
