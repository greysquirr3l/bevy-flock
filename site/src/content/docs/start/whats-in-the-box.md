---
title: What is in the box
description: The modules you will actually touch, in the order you will touch them.
---

The crate is wide but the surface you use daily is small. This page is the honest inventory.

## The five things you will use

| item | what it is |
| --- | --- |
| `FlockPlugin` | the plugin; takes a `FlockConfig` |
| `FlockConfig` / `CrowdPreset` | population, spread, budgets, steering weights |
| `Npc`, `NpcVelocity`, `NpcNeeds`, `NpcTemperament` | the components on each agent |
| `BehaviorTree` and its combinators | what an NPC decides |
| `SpatialGrid` | neighbour queries when you need them yourself |

## The things you will use eventually

- **`StateMachine`** when a tree is the wrong shape for a mode change.
- **`MemoryStore`** (feature `memory`) when NPCs should remember a player rather than react to the current frame.
- **`FlockRegistry`** when you want your own leaves in the scheduler's cost model.
- **`SimClock`** whenever you need to know which tick you are in.

## The things you should not touch

`adapters` is where the ECS plumbing lives: systems, resources, the components. It is public because the crate is a
library and hiding it would only make debugging harder, but its shape is not a promise — if you find yourself matching on
internals, open an issue instead.

## Modules by layer

```text
bevy_flock
├── adapters        systems, components, plugin wiring   (engine side)
├── boids           steering systems                     (engine side)
├── prelude         one import for the common case
└── re-exports of bevy-flock-core:
    ├── domain      spatial grid, steering kernels, cognition, continuum, saves
    ├── behavior    trees, state machines, registry, archetypes
    └── application the clock, the crowd budget, the scheduler
```

That layering is enforced by a test: `core` may not name a Bevy engine crate, and the boundary is checked in the
dependency graph rather than in review.
