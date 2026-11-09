# Extending it

The crate's job is to be a floor, not a ceiling. Five seams, in the order you will reach
for them.

## 1. A leaf: the smallest unit of behaviour

```rust
use bevy_flock::prelude::*;

/// Guard the door: succeed when the player is close, fail otherwise, never run long.
struct GuardDoor;

impl LeafBehavior for GuardDoor {
    // `&self`, not `&mut self`: one instance is shared by the whole crowd, so per-NPC
    // state belongs on the blackboard, never in a field.
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        let close = ctx
            .blackboard
            .get(KEY_PLAYER_SEEN)
            .is_some_and(|value| value == Value::Bool(true));
        if close { Status::Success } else { Status::Failure }
    }

    fn cost_hint(&self) -> VirtualCost {
        // Honest, not optimistic: the scheduler spends this number, so a leaf that
        // under-reports buys itself a bigger share of the frame than it deserves.
        VirtualCost::from_nanos(80)
    }

    fn label(&self) -> &'static str {
        "guard_door"
    }
}

let id = registry.register_leaf("guard_door", GuardDoor)?;
```

Rules the tests enforce: `tick` must not allocate, must not panic on a missing slot, and
must keep per-NPC state on the blackboard, the trait takes `&self` precisely because one
instance serves every NPC running that leaf. `abort` is where a leaf clears what `tick` set,
so an interrupted conversation does not resume stale.

`tests/tree_no_alloc.rs` counts allocations with a custom global allocator; a leaf that
allocates fails CI rather than quietly costing a frame.

## 2. A director: decisions that are nobody's business

```rust
use bevy_flock::application::director::{Director, DirectorCommands, DirectorInputs, DirectorCommand};

struct Weather;

impl Director for Weather {
    fn decide(&mut self, inputs: &DirectorInputs<'_>, out: &mut DirectorCommands) {
        if inputs.tick().raw() % 7_200 == 0 {
            out.push(DirectorCommand::Mood { value: -0.3, ttl_ticks: 900 });
        }
    }

    fn label(&self) -> &'static str { "weather" }
}

app.insert_resource(DirectorState::new(Box::new(Weather)));
```

The director returns commands; the board applies them. That asymmetry is what makes the
crowd replayable: the trace records what was ordered, and a replay feeding the same commands
produces the same afternoon.

## 3. A port: the engines you already use

Physics, networking and rendering are **ports** (ADR 0040), not dependencies:

| Port | Trait | What the crate provides | What you provide |
| --- | --- | --- | --- |
| Physics | `PhysicsPort` | a kinematic adapter that honours the crowd's velocities | avian, rapier, your own |
| Replication | `ReplicationPort` / `InterestSet` | a fixed-width `NpcNetState`, an interest filter | replicon, lightyear, your protocol |
| Render | `RenderPlan` | sorted, culled instances with variant and tint | sprites, meshes, ECS draw |

```rust
use bevy_flock::prelude::*;

// `build_render_plan` is a system: it fills the `RenderPlan` resource from a `CrowdView`.
let plan = world.resource::<RenderPlan>();
for instance in &plan.instances {
    // instance.variant, instance.tint, instance.transform
}
// `plan.culled` and `plan.truncated` report what the budget dropped.
```

The crate links no renderer, no physics engine and no network stack, which is why
`cargo tree` stays small and why your binary is your choice.

## 4. A navigation source

Movement toward a destination goes through `NavSource`:

```rust
use bevy_flock::domain::nav::NavSource;

// The built-in source is a hand-authored grid; implement `NavSource` for a mesh.
// It answers two questions: is this cell walkable, and what are its neighbours.
```

`domain::flow` caches flow fields per destination with a bounded LRU, so a crowd of five
hundred walking to the same market square does one solve, not five hundred. Swap in your own
`NavSource` to use a navigation mesh; the flow solver does not care where the neighbours came
from.

## 5. An archetype

An archetype binds a temperament, a tree and a declared cost, and is what a spawn actually
instantiates:

```rust
use bevy_flock::prelude::*;

let archetypes = ArchetypeTable::standard(&leaves);
let tree = archetypes.tree(Archetype::Guard);
let cost = archetypes.cost(Archetype::Guard);   // the number the scheduler budgets
```

`docs/adr` and `src/behavior/archetypes.rs` carry the cost table; the benchmark
`benches/brain.rs` is what keeps it honest, and `docs/dev/tuning/performance.md` records the measured
cognition costs that the table is calibrated against.

## What not to do

- **Do not tick a brain outside `FlockSet::Cognition`.** The scheduler is the only thing
  keeping the frame affordable; a second place that ticks trees is a second budget nobody
  measured.
- **Do not read `Instant::now` in a leaf.** It breaks determinism, and the digest test will
  find it.
- **Do not hold `&mut World` in a leaf.** Leaves are pure functions of the blackboard for a
  reason: the moment one writes to the world, the tier system's cost model stops describing
  what the frame does.
- **Do not add a dependency to the core crate for a convenience.** The port pattern exists
  because an optional engine belongs in the game's `Cargo.toml`, not in every downstream
  build.

## The extension registry

`FlockRegistry` is where the seams meet: leaves, trees, directors and providers register
once, by name, and the name is what a log, an overlay and an authored file all use. A duplicate
name is an error, not a silent override, two leaves called `wander` is a bug that would
otherwise only show up as "the wrong one ran".
