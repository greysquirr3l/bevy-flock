# bevy-flock-anim

Animation binding for [`bevy-flock`](https://crates.io/crates/bevy-flock): the layer that turns a
simulated crowd into a crowd that looks alive.

The simulation knows where an NPC walks, how fast and how hard it is turning. It has never known what
that should look like. This crate answers exactly one question — *which pose carries how much weight*
— and leaves the skeleton, the clips and the animation graph to the game (ADR 0052).

## What it owns, and what you own

| | |
|---|---|
| **This crate** | The four slot names (`idle`, `walk`, `run`, `turn-in-place`), the validated `LocomotionProfile`, and the pure blend that maps speed and turn rate onto those four weights. |
| **Your game** | The rig, the clips, the `AnimationGraph`, and the forty lines that map each slot to a node and apply the weights. |

That split is why the crate works with an orc, a city crowd or a flock of birds without knowing what
any of them look like.

## Why the blend is a pure function

`LocomotionProfile::blend(speed, turn_rate)` reads no clock and holds no state. The same motion always
produces the same four weights, so a replay animates identically at 20 frames a second and at 200.
Smoothing a weight against a frame delta — the conventional way to write this layer — is how a crowd
ends up looking subtly different on somebody else's machine, and it would quietly break the
determinism guarantee the simulation core is built around.

```rust
use bevy_flock_anim::{ClipSlot, LocomotionProfile};

# fn main() -> Result<(), bevy_flock_anim::ProfileError> {
let profile = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)?;

let blend = profile.blend(0.7, 0.0);
assert!((blend.weight(ClipSlot::Idle) - 0.5).abs() < 1e-5);

// A body standing still and turning pivots instead of walking, and the weight comes out of idle.
let pivot = profile.blend(0.0, 2.0);
assert!((pivot.weight(ClipSlot::TurnInPlace) - 1.0).abs() < 1e-5);
# Ok(())
# }
```

## Status

Phase 2 of the roadmap, which is the shell around a shipped simulation core. This crate carries the
profile, its blend space, and the binding that drives a `AnimationPlayer`: `ClipBindings` maps the
four slots onto nodes in *your* graph, and `apply_locomotion_blend` writes weights and stride-derived
playback rates once per tick. Gaze and gestures land against the same contract, each with its own
tests.

```rust
use bevy_animation::graph::AnimationNodeIndex;
use bevy_flock_anim::{AnimPlugin, ClipBindings, LocomotionProfile};

# fn wire(app: &mut bevy_app::App, graph_nodes: [AnimationNodeIndex; 3]) {
app.add_plugins(AnimPlugin);
// Your graph: three clips, one cycle long each, resolved to node indices when you build it.
let bindings = ClipBindings::new(profile_handle, graph_nodes[0], graph_nodes[1], graph_nodes[2]);
// …then spawn `(Locomotion::default(), bindings, AnimationPlayer::default())` per NPC.
# }
```

## Licence

MIT OR Apache-2.0.
