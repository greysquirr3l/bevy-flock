# Getting started

## A crowd in five lines

```rust
use bevy::prelude::*;
use bevy_flock::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(FlockPlugin::new(
            FlockConfig::preset(CrowdPreset::QuietVillage),
        ))
        .run();
}
```

`FlockPlugin` inserts the configuration as a resource, spawns the population over the first
frames (budgeted, so a large crowd does not stall the first tick), and registers the systems
that steer it. There is nothing else to add, and nothing to draw: the plugin moves
`Transform`s and leaves rendering to you.

If you have no renderer at all, a server, a test, a benchmark, use `App::new()` and add
only `FlockPlugin`. The crate never asks for a window, and every benchmark in
`docs/dev/tuning/performance.md` was measured headless.

## Sizing the crowd

```rust
use bevy_flock::prelude::*;

let size = PopulationSize::try_new(10_000)?; // 1..=10_000, checked
let config = FlockConfig::preset(CrowdPreset::CityAmbient).with_population(size);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`PopulationSize::try_new` is fallible on purpose: a population of zero or above
`MAX_POPULATION` is a bug in the game's configuration, and the crate refuses it at the point
where it can still be reported instead of clamping it into a silent surprise. Above 10,000,
see [Crowds at scale](crowds-at-scale.md), the answer is not "more entities", it is bands
and density.

Two ways to fill those slots:

| Mode | What it means | When to use it |
| --- | --- | --- |
| `PopulationMode::Unique` | every slot is a distinct identity for the life of the world | named NPCs, persistent relationships, saves |
| `PopulationMode::Recycled(SlotCount)` | a fixed pool of identities, reused lowest-slot first | a crowd that is scenery, not cast |

Recycling is not an optimisation of memory: a recycled identity keeps its row in the memory
store, which is exactly what makes "the same face" possible in a crowd of a thousand.

## Placing them

```rust
use bevy_flock::prelude::*;

let config = FlockConfig::preset(CrowdPreset::MarketplaceBustle)
    .with_placement(Placement::uniform_volume(
        Vec3::splat(-60.0),
        Vec3::splat(60.0),
    ))?
    .with_population(PopulationSize::try_new(400)?)
    .with_seed(SimSeed::new(0xBEEF));
# Ok::<(), Box<dyn std::error::Error>>(())
```

The constructors spell out the shape, so the code reads as what it does:

| Constructor | Shape | Typical use |
| --- | --- | --- |
| `Placement::spiral(spacing)` | golden-angle disc around the origin | a compact crowd, index-ordered |
| `Placement::uniform_rect(min, max)` | box | a flat plaza |
| `Placement::uniform_disc(centre, radius)` | circle | a market square |
| `Placement::uniform_volume(min, max)` | 3D box | a city block with height |
| `Placement::uniform_sphere(centre, radius)` | 3D ball | an arena, a swarm |
| `Placement::fixed(positions)` | your list | hand-authored spawns, tests |

`with_placement` returns a `Result`, because a placement is validated against the grid: a
crowd spread over ten kilometres with four-metre cells is a configuration error, and the
error says which axis was too wide.

Placement is deterministic: positions come from the seeded stream, in spawn order, so the
same seed puts the same person in the same place. `Placement::position_for(npc, seed)`
computes a single position without spawning anything, which is what a test asserts against.

## Choosing a preset

| Preset | Population | Character |
| --- | --- | --- |
| `QuietVillage` | a few dozen | slow, loose spacing, plenty of personal room |
| `MarketplaceBustle` | a few hundred | milling, destination-driven, chatty |
| `CityAmbient` | two thousand | dense, purposeful, indifferent |
| `BattleHorde` | thousands | tight spacing, high speed, aggression in the weights |

A preset is a starting point, not a mode: `CrowdPreset::config()` gives you the whole
`FlockConfig` to edit, and every field is public with a builder.

## Turning on the mind

Steering is the default. Thinking is a budget, and it is a resource rather than a config
field because it is tuned at runtime:

```rust
use bevy_flock::prelude::*;

let mut app = App::new();
app.add_plugins(FlockPlugin::new(FlockConfig::preset(CrowdPreset::CityAmbient)))
    .insert_resource(ThinkSchedule::new(2_000, VirtualCost::from_micros(900)));
```

`ThinkSchedule::DEFAULT_BUDGET` is 14 ms of *virtual* cost, not wall time. The scheduler
spends a queue of per-NPC costs against that number, which makes a frame's thinking
reproducible on any machine (see [The model](the-model.md)). Nine hundred microseconds of
virtual cost does not buy two thousand deliberating NPCs; it buys a few dozen deliberating,
a few hundred reacting and the rest running on reflex. That is the design, not a
limitation, it is also what a real crowd looks like.

`TierConfig` and `WeightConfig` are the other two knobs: how distance picks a tier, and how
close an NPC has to be before it is worth promoting.

## A tuned 10,000-agent crowd

This is the configuration the performance table measures:

```rust
use bevy_flock::prelude::*;

let mut app = App::new();
app.add_plugins(FlockPlugin::new(
    FlockConfig::preset(CrowdPreset::CityAmbient)
        .with_placement(Placement::uniform_volume(
            Vec3::splat(-100.0),
            Vec3::splat(100.0),
        ))?
        .with_population(PopulationSize::try_new(10_000)?)
        .with_seed(SimSeed::new(1)),
))
.insert_resource(ThinkSchedule::new(10_000, VirtualCost::from_micros(1_200)));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Checklist before you ship it:

1. **Match the volume to the playable area.** The grid rebuild is a fixed cost sized from
   the configured volume: a 1 km box costs about 1.0 ms per frame even for one agent, a
   200 m box about 20 µs.
2. **Set the budget from the frame, not from the crowd.** Budget 10-15% of the frame and let
   the scheduler decide who gets it.
3. **Pin the seed in your tests.** A crowd is a simulation; a test that does not pin the seed
   is a test of the weather.
4. **Measure on your target.** The numbers in this book come from a 6-vCPU ARM host; yours
   will differ in absolute terms and should agree in ratios ([What it costs](costs.md)).

## Next

- [The model](the-model.md), what a tick actually does, and in what order.
- [Behaviour](behaviour.md), give them something to want.
- `examples/hello_flock.rs` first, then `examples/horde_100k.rs` for the moment the crowd
  stops being a list.
