---
title: Behaviour that varies
description: Three ways to make individuals differ, cheapest first.
---

A crowd where everyone acts the same is a screensaver. Three mechanisms make individuals differ, and they cost
different amounts. Reach for the cheapest one that produces the effect you want.

## Temperament first

Every NPC carries `NpcTemperament`: traits that scale steering weights and need decay. Two NPCs with the same tree but
different temperaments already behave differently, one crowds, one keeps distance, one's needs drain faster.

This is the cheapest variety available and the one most crowds actually need. Before writing a leaf, check whether the
effect you want is a weight.

## Archetypes second

An archetype is the role. It supplies a default tree and the virtual cost of one think, which is what lets the scheduler
spend a budget honestly: a guard running a complex tree consumes more of the tick than a civilian wandering.

```rust
use bevy_flock::prelude::*;

let mut registry = FlockRegistry::new();
let leaves = registry.register_standard_leaves();
let guard = Archetype::Guard
    .logic(&leaves, |id| registry.leaf_cost(id));
```

## Trees last

When the effect is genuinely a decision rather than a weight, add a leaf. In order:

1. Implement `LeafBehavior`. It reads and writes a `Blackboard` and returns a `Status`.
2. Register it with `FlockRegistry` and declare its cost. The scheduler needs the number to budget; a guess here shows up
   later as a frame that occasionally spikes.
3. Compose it into a tree with the combinators below, or attach it to an archetype.
4. Test it at the population you ship, not at ten agents. A leaf that costs 2 µs is free at 100 and is 2 ms at a
   million.

| combinator | behaviour |
| --- | --- |
| `sequence` | run children in order, stop on the first failure |
| `selector` | try children in order, stop on the first success |
| `parallel` | run children together under a `ParallelPolicy` |
| `invert` | flip success and failure |
| `cooldown`, `timeout` | rate-limit and deadline a subtree |
| `repeat_until_fail` | loop a subtree |

Trees are evaluated through a `TreeCursor`, which is the per-NPC position in the graph, so evaluation resumes across
ticks instead of restarting from the root every frame.

:::caution[One tree, many NPCs]
Do not give every NPC a unique tree. A thousand distinct trees is a thousand cache misses and a budget you cannot
predict. Vary temperament, vary archetype, and let the tier system decide who thinks.
:::

`StateMachine` is for the cases where a tree is the wrong shape: explicit modes with explicit transitions, where "which
state is this NPC in" should be answerable by reading one field.

## Runnable

- `examples/custom_behavior.rs`, a leaf you write, register and cost
- `examples/tavern_dialog.rs`, dialogue driven from the same blackboard
