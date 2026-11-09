---
title: A crowd in five lines
description: The smallest program that produces a moving crowd, and what each line bought you.
---

```rust
use bevy_app::App;
use bevy_flock::application::SimClock;
use bevy_flock::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(FlockConfig::preset(
        CrowdPreset::CityAmbient,
    )));
    app.update();
}
```

That is a running crowd: entities spawned, spatial grid built, steering ticking. Run it with
`cargo run --example hello_flock` to see the numbers.

## What each line did

`FlockPlugin::new(config)` registers the crowd resource, the spatial grid, the scheduler and the systems that advance
them. It spawns nothing on its own.

`FlockConfig::preset(CrowdPreset::CityAmbient)` is a starting point, not a mould. The three presets differ in population,
spread and how much thinking is allowed:

| preset | population | shape |
| --- | --- | --- |
| `QuietVillage` | small | sparse, low cognition budget, cheap |
| `CityAmbient` | thousands | dense, ambient tier, the default choice |
| `BattleHorde` | large | tight formation, high steering weight |

Every field is public, so a preset is a struct you copy and change:

```rust
let config = FlockConfig {
    population: 5_000,
    ..FlockConfig::preset(CrowdPreset::CityAmbient)
};
```

## Reading the state back

```rust
let stats = app.flock_stats();
println!("{} alive", stats.population());

let clock = *app.world().resource::<SimClock>();
println!("tick {}", clock.tick);
```

`flock_stats()` is a cheap read of the crowd resource. For per-NPC data, query the components, `Npc`, `NpcVelocity`,
`NpcNeeds`, `NpcTemperament`, exactly as you would query anything else in Bevy.

## Next

- [Concepts](/bevy-flock/start/concepts/), why the crowding looks right and what the tiers are for
- [Behaviour that varies](/bevy-flock/guides/behaviour/), make individuals act differently
