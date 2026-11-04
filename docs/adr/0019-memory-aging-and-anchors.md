# ADR 0019: Memory aging for NPCs nobody simulates

- Status: Accepted
- Date: 2026-11-04
- Relates to: ADR 0010 (social memory), ADR 0012 (save format), ADR 0016 (subject
  ceiling), ADR 0018 (ambient continuum)

## Context

A player leaves a village for an hour of game time. The NPCs they wronged are never
simulated again — they are far away, and with ADR 0018 they may be folded into the
continuum entirely. What happens to their memories?

Two properties already held, and one did not.

**Decay was already independent of simulation.** `DecayPolicy::familiarity_at` is a pure
function of `now - last_seen`, evaluated when the memory is *read*. An NPC that has not
ticked in an hour does not hold a stale, warm memory: the moment anything asks, the answer
is an hour old. There was never a staleness bug here.

**Reclamation was not independent of simulation.** A row in `MemoryStore` is freed by
`sweep`, and `sweep` is called by whoever thinks to call it. Per-entity code cannot call it
for an NPC that is not ticked. So the row — and its subject slots, of which there are at
most 64 per NPC (ADR 0016) — sat there until the store filled, at which point `enrol`
returned `None` and *new* NPCs stopped having memories at all. Long absences leaked the one
resource the design is sized by.

**And the obvious fix would have destroyed the feature.** A naive "sweep everything that
has faded" pass reclaims the absent NPC's row — including the grudge against the player the
game promised to remember. Distance plus time equals forgiveness, silently.

## Decision

Sweeping is a **store-level cadence with a caller-supplied exemption**, not a per-entity
responsibility.

```rust
pub struct AgingPolicy { pub cadence: u64, pub min_familiarity: u16 }
pub struct AgingReport { pub swept: bool, pub freed: u32, pub retained: u32 }

impl MemoryStore {
    pub fn sweep_keeping(&mut self, now, decay, min_familiarity, keep: &dyn Fn(NpcId) -> bool) -> u32;
    pub fn age(&mut self, now, decay, policy: AgingPolicy, keep: &dyn Fn(NpcId) -> bool) -> AgingReport;
}
```

Three consequences, each with a test:

1. **Absence cannot leak.** `age` runs on a tick cadence (default 600, ten seconds at
   60 Hz) over the store's rows, so an NPC nobody simulates is still reclaimed. A test
   pushes 5,000 arrivals and departures through a store with 16 rows and asserts it never
   grows.
2. **Anchors survive fading.** `keep` is the anchoring seam from ADR 0018. A protected row
   is never freed: its *warmth* decays to zero through `recall_decayed`, while its *record*
   — the meeting happened, it was an attack, on this date — survives intact.
3. **Warmth and history are different things.** The tested behaviour after 60 game-minutes
   of absence: `recall_decayed` returns nothing worth acting on, `recall` still returns a
   grudge, `last_seen` is unchanged, and `encounters()` still counts one. A game can say
   "you again" without saying it warmly.

The two predicates a caller will pass are the two the library already has: anchored
entities (ADR 0018) and subjects named by a save file (ADR 0014).

## Rejected alternatives

- **Sweep per entity.** Requires simulating the thing you are trying not to simulate.
- **Never sweep; raise the store capacity.** Trades a bounded leak for an unbounded one, and
  the store is sized by eligibility (ADR 0016) precisely so it is not a population-sized
  cost.
- **Let absent memories be forgotten.** Coherent, cheap, and it deletes the feature: the
  grudge that defines an NPC becomes a function of how recently the player visited.
- **Freeze decay while absent.** Makes an hour away cost nothing, so the "long absence"
  mechanic inverts: leaving becomes a way to preserve affection. Decay stays continuous;
  only reclamation is governed.

## Consequences

The memory feature is now safe to combine with the continuum: an NPC can be a density field
for an hour and come back with a faded but intact history. The cost is one store-wide pass
every ten seconds — 16 rows per NPC-eighth-second, which the memory benchmarks already show
is noise next to the per-encounter path.

The remaining hazard is the seam itself: `keep` is a caller-supplied predicate, and a game
that passes `|_| false` gets exactly the leak-free, anchor-less behaviour — every grudge
eventually reclaimed. That is the right default for a crowd and the wrong one for a story,
which is why ADR 0018 makes anchoring a design-visible constant rather than an internal
optimisation.
