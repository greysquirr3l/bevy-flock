# bevy-flock-ecs

The Bevy-facing half of [`bevy-flock`](https://crates.io/crates/bevy-flock): the plugin, the components,
the systems and the steering integration, on top of the engine-free simulation core.

```toml
[dependencies]
bevy-flock-ecs = "0.1"
```

```rust
use bevy_app::App;
use bevy_flock_ecs::prelude::*;

let mut app = App::new();
app.add_plugins(FlockPlugin::default());
```

It exists so that the simulation does not have to know an engine exists, and so that a headless build can
take the core and never compile `bevy_ecs`. If you are writing a game, depend on `bevy-flock` instead: it
re-exports this crate at `bevy_flock::adapters` and `bevy_flock::boids`, so nothing in your code changes,
and it adds the prelude and the animation layer on top.

The systems here are the ones a game actually schedules: tiered cognition through `ThinkScheduler`,
perception via `PerceptionSettings`, gestures on the blackboard, gaze targets, locomotion state, the
social-memory bridge, obstacle navigation, and the debug overlay that explains why an NPC did something.

See [ADR 0062](https://github.com/greysquirr3l/bevy-flock/blob/main/docs/dev/adr/0062-three-crate-layering.md)
for the dependency direction and why the animation layer sits above this crate rather than beside it.
