# 0017 — Tiered cognition: from reptile to social

- Status: Proposed
- Date: 2026-11-04
- Amends: ADR 0009 (the virtual-cost scheduler), which assumed one kind of think
- Relates to: `domain::weight` (how often), `application::schedule` (who gets the budget)

## Context

ADR 0009 built a fair queue over a population of thinkers and priced them with one
number each: the declared cost of a think. The planner works — 88.7% of 10,000 NPCs per
tick inside a 16.6 ms budget, 12.1% of 100,000 — and the measurements say plainly what
it cannot do. A hundred thousand NPCs, each running a full behaviour tree, is not a
budget problem that a better queue solves. At an average 2.25 µs per think the frame
holds about 7,400 of them; a crowd of 100,000 therefore thinks about once every
thirteen ticks, and the ones the player is looking at are indistinguishable from the
ones they are not, because the only variable is *how often*.

Weight alone is a fader: it changes cadence, not content. A background NPC and a
foreground NPC run the same tree, so the only way to buy frames is to run the important
one less often too. That is the wrong lever — the important one needs *more* thinking,
and the background one needs *cheaper* thinking.

The cockroach and the philosopher do not differ in how often they think. They differ in
what they think with.

## Decision

**Cognition is tiered, and the tier selects the logic while the weight selects the
cadence. Budget is reserved per tier, so a single social NPC cannot eat a street.**

### The tiers

| Tier | Name | What runs | Typical cost | Population |
| --- | --- | --- | --- | --- |
| 0 | Reflex | Steering and a fixed rule set. No tree, no blackboard writes, no allocation. | 50–200 ns | Thousands |
| 1 | Reactive | A small tree: flee, follow, idle. Depth ≤ 3, two or three leaves. | 1–3 µs | Hundreds to low thousands |
| 2 | Deliberative | The full compiled tree with declared costs, needs, mood, perception. | 4–12 µs | Tens to a few hundred |
| 3 | Social | Deliberation plus memory recall, dialogue intent, faction reasoning. | 20–50 µs | Single digits |

The tiers are a *vocabulary* before they are an optimisation: "this NPC is a reflex
agent" is a sentence a designer can write in a spec and a reviewer can check, where
"this NPC has weight 4" is not.

### How an NPC changes tier

Promotion and demotion are functions of state — distance to the nearest focus,
visibility, whether it is in a conversation, whether it is the target of a quest — and
never of elapsed time, because a replay must reach the same tier at the same tick. The
hysteresis rule in `domain::weight` applies unchanged and for the same reason: an NPC on
a boundary must not oscillate, and tier oscillation is worse than weight oscillation
because it changes which logic runs, not merely when.

**Promotion is a cost spike and must be budgeted.** A reflex agent promoted to
deliberative suddenly needs a blackboard, a perception snapshot and a compiled tree. If a
hundred of them are promoted in one tick, the budget is gone before anything thinks.
Therefore promotions are themselves rationed: at most *K* per tick, cheapest first, with
the rest queued. Demotion is free — the state is kept, and a demoted NPC keeps its
blackboard until its tier is re-promoted, which is what stops a player walking back and
forth across a boundary from resetting a conversation.

### Budget reservation

Each tick's virtual budget is divided by tier, not shared:

```
reflex        55%      # the crowd is the reason the engine exists
reactive      25%
deliberative  15%
social         5%      # guaranteed, so a conversation cannot be starved by a parade
```

The shares are defaults and a game overrides them. The property that matters is that
they are *reserved*: the social tier's 5% cannot be spent by the crowd, and the crowd
cannot be starved by a long conversation. A single global queue gives the first and
loses the second — which is exactly the failure a game notices, because it is the
player's conversation that stutters.

Within a tier, the scheduler is unchanged: virtual-cost fair queueing over the
participants in that tier.

### What this does to the numbers

The measured ceiling stands: ~7,400 full deliberation thinks per 16.6 ms frame. Tiering
does not raise that number — it lowers the *average* cost by moving most of the
population to tiers that cost 10–100× less, which is what lets a hundred thousand
inhabitants all do *something* every tick:

| Composition | Average cost | Thinkers per tick |
| --- | --- | --- |
| All deliberative | 2.25 µs (measured mix) | ~7,400 |
| 95% reflex, 4% reactive, 1% deliberative | ~0.15 µs | ~110,000 |

That second row is the claim worth testing, and it is why the ambient tier (a
continuum with no entities at all) is still needed: even at 150 ns, a million
inhabitants is 150 ms of frame.

## Consequences

- **`NpcBrain` is no longer the only kind of participant.** Reflex agents have no tree,
  so the scheduler must accept participants whose cost is a constant and whose "think"
  is a system, not a tree tick. This is the main structural change: the scheduler keeps
  its current API, and the adapter supplies cost and work per tier.
- **The plan's `lod-think-scheduler` task grows a second half**: per-tier queues and the
  promotion ration. The task is not closable until both exist.
- **Diagnostics must report per tier**, or nobody will know why the crowd went quiet:
  `docs/adr/0015`'s telemetry gains a tier breakdown.
- **Costs per tier must be calibrated**, not guessed, exactly as the leaf costs were
  (ADR 0009): a reflex tier that costs 2 µs is a deliberative tier with a worse name.

## Alternatives rejected

- **One tree, throttled by weight.** What exists now. Correct and measured, and it
  cannot serve the foreground better than the background — only less badly than nothing.
- **A continuous cost/weight curve instead of tiers.** Every NPC gets a unique weight,
  ties never happen, and the scheduler's stability (groups of equal weight, deterministic
  tie-break) is lost. It also makes the tuning surface a float per NPC, which no designer
  will maintain.
- **Tier as an output only** (derive a tier from measured share, as ADR 0009 proposed,
  for culling and stats). That is still worth having — it is the diagnostic — but it does
  not reduce the work, which is the point here.
- **Promotion without rationing.** Simple, and it produces the worst possible frame: the
  one where the player rounds a corner and a hundred NPCs are promoted in the same tick.
