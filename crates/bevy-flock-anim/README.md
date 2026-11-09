# bevy-flock-anim

Blended clip weights, gaze and gesture playback for [`bevy-flock`](https://crates.io/crates/bevy-flock)
crowds: the layer that turns a simulated crowd into one that looks alive.

```toml
[dependencies]
bevy-flock = { version = "0.1", features = ["anim"] }
```

```rust
use bevy_flock::prelude::*;

app.add_plugins((FlockPlugin::default(), AnimPlugin));
```

That is the path a game takes. This crate is where the code lives, and it exists separately for one
reason: it is the only part of the graph that needs `bevy_animation` and `bevy_asset`, and a dedicated
server should never compile either. It depends on `bevy-flock-ecs`, not on the facade, so the facade
can offer it as a feature without a cycle.

What it does:

- **Locomotion blending.** One `LocomotionProfile` of authored numbers (walk speed, run speed, stride,
  turn band) becomes four clip weights, with foot-slide correction against the simulated velocity
  rather than against the animation's own root motion.
- **Gaze.** A limited cone per rig, with the bone sparse-set solve that ADR 0045 profiles.
- **Gestures.** A channel on the blackboard; a behaviour leaf asks for one and this layer plays it,
  without the simulation ever seeing an `AnimationPlayer`.
- **Root motion.** The trace that keeps a bevy-flock root contract: the simulation owns the position,
  and the animation follows.

The layer never owns your skeleton. `ClipBindings` maps four names onto nodes in *your* graph, so a
rigged character you already have is a configuration, not a rewrite. See
[ADR 0062](https://github.com/greysquirr3l/bevy-flock/blob/main/docs/dev/adr/0062-three-crate-layering.md)
for why this is a crate rather than a module in the facade.
