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

## Animating fewer bones when nobody is looking

A crowd of ten thousand drawn bodies is ten thousand skeletons, and the bones cost more than the
weights. `apply_locomotion_blend` therefore gates itself per body, from two inputs it already has:

| Level | What happens | When |
|---|---|---|
| `Full` | weights and rates written every tick | inside `near` (40 m by default), or inside the band for a body the scheduler is thinking about |
| `Stepped` | written every `far_step_ticks` (6) | inside the band, `Reflex` or `Reactive` tier |
| `Paused` | `pause_all`, nothing written | beyond `far` (120 m) |

The tier is the simulation's own `NpcTier`, not a second distance test, so a body the scheduler is
about to promote is already animating before it can be seen moving. The distance is measured from
`AnimationFocus`, which the game sets to whatever it is watching; set nothing and nothing is culled.

Both numbers are profile values, not constants — `BlendLod::try_new` validates an authored band and
`BlendLod::disabled()` turns culling off entirely:

```rust
use bevy_flock_anim::{AnimationFocus, BlendLod, LocomotionProfile};

// A tighter band than the default, for a game whose camera never leaves the street.
let profile = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)?
    .with_lod(BlendLod::try_new(25.0, 90.0, 4)?);

// Once per frame, from the camera: the band measures from this point, and nothing is culled until
// a focus is set.
app.insert_resource(AnimationFocus::at(camera_position));
```

Coming back is where a naive cull pops. A resumed body gets this tick's weights *and* its walk and
run clips seeked to the phase its own distance walked implies, so it takes the step it would have
taken. That is `AnimationLod::resumes` in the telemetry, and two tests in `binding.rs`: one compares
the resumed weights against a control that was never culled, one checks the seek time.

Measured at 10,000 reflex-tier bodies with the camera at the plaza edge: 0.305 ms per binding pass
against 0.601 ms unculled, 49% saved, and 70% with the camera outside the plaza. The table, the
method and the caveats are in `docs/dev/tuning/performance.md`.

## Starting and stopping

`LocomotionProfile::windows` takes a `BlendWindows`: how many ticks a body spends picking its stride
up and putting it down. The ease is one fixed step per simulation tick, interpolating from the pose
the window opened in, so a six-tick start is half committed after three ticks and exactly arrived
after six — and a replay at 20 fps passes through the poses a replay at 200 fps does.

```rust
let profile = LocomotionProfile::try_new(1.4, 4.2, 1.5, 0.6, 2.0)?
    .with_windows(BlendWindows::try_new(6, 8)?);
```

Zero ticks, the default, is the blend itself. Turn-in-place needs no window: it comes out of idle and
is unreachable while the body is walking, which `turn_in_place_is_unreachable_at_walking_speed`
asserts rather than trusts.

Slide is measured while you author the window. `AnimationState::slide()` reports the peak and total
metres per tick a body has travelled that its stride clips did not account for, and the playground
prints the peak in its headless summary. The number is not decoration: a straight walk reports under
a millimetre over 300 ticks, and a body half way into its start reports the half of its motion its
legs are not carrying.
## Root motion, decided rather than hoped for

A walk cycle that translates its own root is a rigging convenience, and getting it wrong is the same
bug in both directions: the body travels twice as far as its legs, or the legs cycle in place while
the steering slides it. So the profile states who owns movement, and the default is the one that keeps
a deterministic replay intact:

```rust
use bevy_flock_anim::{LocomotionProfile, RootMotion};

// The simulation owns movement (the default): any reported clip translation is dropped on arrival.
let simulation = LocomotionProfile::try_new(1.4, 4.2, 1.5, 0.6, 2.0)?;
assert!(!simulation.root_motion().extracts());

// The clip owns movement: translation moves the body and advances its walk phase.
let clip = simulation.with_root_motion(RootMotion::Clip);
```

Extraction stays outside the crate, because Bevy owns the skeleton: your animation bridge reads the
root joint and calls `RootOffset::report(world_translation)`. What the crate owns is the decision —
apply exactly once, or drop — plus `RootMotionTrace`, which counts what happened and digests the
applied offsets so two runs can be compared. Each tick's offset is snapped to a micrometre
(`QUANTUM_METRES`) before it is applied, so a thousand inexact `f32` sums produce the same transform
twice: `two_runs_of_the_same_clips_produce_the_same_transform_and_digest` runs 240 non-round offsets
through the system and compares the bits.

Consuming the report rather than reading it is what prevents double movement when a game leaves the
bridge writing while the simulation also integrates: the offset exists for exactly one tick, and in
simulation mode that tick ends with it dropped (`simulation_mode_drops_the_offset_and_leaves_the_body_where_steering_put_it`).
## Licence

MIT OR Apache-2.0.
