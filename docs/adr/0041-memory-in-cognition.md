# 0041 — Memory reaches a tree through three integers, not a store handle

- **Status:** Accepted
- **Date:** 2026-11-05
- **Supersedes:** nothing
- **Related:** ADR 0009 (virtual-cost scheduler), ADR 0010 (social memory),
  ADR 0011 (memory and the scheduler), ADR 0013 (subject registry),
  ADR 0019 (anchor exemption), ADR 0035 (behaviour-tree node set)

## The question

The store from ADR 0010 answers "what does this NPC remember about that person" in
`O(subjects)` per row. A behaviour tree asks a different question, several times per
tick, from arbitrary branches: *do I know you, do I like you, did you hurt me?* Handing
the tree a `&MemoryStore` would answer it correctly and ruin two earlier decisions — the
tree's tick would stop being allocation-free and slot-bounded (ADR 0035), and leaves
would need to know about decay, ticks and subject identity to interpret what they read.

## The decision

The bridge publishes the strongest memory an NPC holds as **three blackboard integers**,
once per tick:

| Key | Meaning | Range |
|---|---|---|
| `KEY_MEMORY_VALENCE` (40) | How the subject feels, signed | `i16` |
| `KEY_MEMORY_FAMILIARITY` (41) | Decayed familiarity | `u16` |
| `KEY_MEMORY_FLAGS` (42) | Encounter-kind bits | `u16` |

Leaves read them through `Remembered::read`, a four-instruction function with three
rules worth stating:

1. **Familiarity gates the view.** A blackboard with a valence but no familiarity is not
   a memory. Otherwise a decayed grudge would leave a stranger who inexplicably dislikes
   everyone, which is exactly the sort of "the town is subtly wrong" failure the
   diagnostics module exists to catch.
2. **Flags beat intensity for hostility.** `Attacked` and `Robbed` survive a decay that
   has softened the valence to neutral: a grudge is an event, not a feeling.
3. **Absence means stranger.** No view is `None`, not zeroes. A tree can distinguish "I
   have never met you" from "I am indifferent to you", and the shopkeeper's two lines
   differ accordingly.

Publication happens in `FlockSet::Perception`, after `observe_players` and before
cognition, in the same frame. A tree that runs in `FlockSet::Cognition` therefore reads a
view that describes this tick, not the previous one.

## Why the leaves are not feature-gated

The store is behind `memory`; the leaves are not. That asymmetry is the point: a game can
author a tree that asks "do I know you?" and ship it with the feature off, where the
answer is always `Failure` and the NPC treats everyone as a stranger. Turning the feature
on changes behaviour without touching the tree, the archetypes or the save format. Gating
the leaves would have forced every downstream game to fork its trees per feature set.

## Enrolment is by proximity and budget

`enrol_the_present` shortlists the NPCs that have seen a player within `publish_radius`,
sorts them with `domain::memory::enrolment_order` — the same comparator `select_enrolment`
uses, exported so the ECS path cannot drift from the pure one — and creates at most
`enrol_budget` rows per tick. Two consequences, both deliberate:

- **Rows are a resource, granted nearest-first.** A player who teleports into a packed
  square gets the people around them remembered first, over several ticks, rather than a
  thousand-row spike in one frame.
- **The store can be full and that is not an error.** `MemoryStore::enrol` returns `None`,
  the loop stops, and `MemoryBridgeStats::budget_bound` reports that the crowd outgrew its
  memory. The alternative — evicting the oldest row — would delete the relationship the
  player spent the longest building.

## Clearing is not forgetting

When the player leaves, `forget_the_absent` removes the three keys and leaves the row
alone. A view is a claim about the present; a row is a claim about the past. Conflating
them would make a shopkeeper greet a customer who left ten minutes ago, or make walking
away and back erase a friendship.

`MemoryBridgeStats::cleared` counts the views removed, which is how a game tells "the
town forgot me" apart from "the town never knew me".

## Cost

Measured on the reference host (see `docs/memory.md` and `docs/performance.md`):

- **Publication:** three slot writes per enrolled NPC per tick. At the default capacity
  (`population / 10`, clamped to 16,384 rows) that is at most 49k writes per tick, against
  a steering kernel that touches millions of floats.
- **Enrolment:** one `O(candidates)` pass and a sort, bounded by the budget. Zero work for
  an NPC without `NpcBlackboard`, which is why a crowd that thinks with nothing pays
  nothing.
- **A tree read:** one slot lookup. Cheaper than the branch it guards.

## What this does not do

- **No dialogue memory.** The view is a summary, not a transcript. A game that wants "you
  promised me a sword" keeps that in its own dialog state and reads the summary for tone.
- **No cross-save translation.** Identifier stability is ADR 0013's problem; the bridge
  publishes whatever the row holds after the registry has been applied at load.
- **No per-subject detail.** The strongest subject wins. A game that needs "which of the
  three players in front of me is the one who robbed me" must use the store directly —
  and accept the `O(subjects)` cost on the NPCs that matter.
