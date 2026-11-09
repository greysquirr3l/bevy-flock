---
title: Behaviour that varies
description: Trees, state machines, archetypes, and how to make a crowd feel like individuals.
---

A crowd where everyone acts the same is a screensaver. Three mechanisms make individuals differ, and they cost
different amounts, so use the cheapest one that gets the effect.

## 1. Temperament

Every NPC carries `NpcTemperament`: a handful of traits that scale steering weights and need decay. Two NPCs with the
same tree but different temperaments already behave differently, one crowds, one keeps distance, one's needs drain
faster. This is the cheapest variety you can buy and the one most crowds actually need.

## 2. Archetypes

`Archetype` is the role: `Guard`, `Civilian`, `Merchant` and friends. An archetype supplies a default behaviour tree
*and* the virtual cost of one think, which is what lets the scheduler budget honestly, a guard that runs a complex tree
consumes more of the per-tick budget than a civilian that wanders.

```rust
use bevy_flock::prelude::*;

let mut registry = FlockRegistry::new();
let leaves = registry.register_standard_leaves();
let guard = Archetype::Guard
    .logic(&leaves, |id| registry.leaf_cost(id));
```

`register_standard_leaves` gives you the built-in leaves; `Archetype::logic` builds the tree an archetype ships with.
When you want your own, implement `LeafBehavior` and register it, the cost closure then reports *your* cost, not a guess.

## 3. Trees and machines

`BehaviorTree` is a node graph of combinators over leaves:

| combinator | meaning |
| --- | --- |
| `sequence` | run children in order, fail on the first failure |
| `selector` | try children in order, succeed on the first success |
| `parallel` | run children together with a `ParallelPolicy` |
| `invert` | flip success and failure |
| `cooldown` / `timeout` | rate-limit and deadline a subtree |
| `repeat_until_fail` | loop a subtree |

Leaves read and write a `Blackboard`, and return a `Status`. A `TreeCursor` is the per-NPC evaluation position, which is
what makes evaluation resumable across ticks instead of restarting from the root every frame.

`StateMachine` is for the cases where a tree is the wrong shape: explicit modes with explicit transitions, where "which
state am I in" is a question you want to answer by reading one field.

## Where to look

- `examples/custom_behavior.rs`, a leaf you write yourself, registered and costed
- `examples/tavern_dialog.rs`, dialogue driven from the same blackboard
- `docs/dev/adr/`, ADR 0005 (trees), 0009 (cost stride), 0017 (tiers)

## The trap

Do not give every NPC a unique tree. A tree is data, but a thousand distinct trees is a thousand cache misses and a
budget you cannot predict. Vary temperament, vary archetype, and let the scheduler pick who thinks, that is what the
tier system is for.
