# Animation

The simulation moves bodies. Animation makes them look like they are walking rather than sliding, and
the whole design of `bevy-flock-anim` follows from one refusal: it will not own your skeleton.

You keep your rig, your clips, your `AnimationGraph` and your asset pipeline. The crate owns the
numbers a crowd needs — how fast a stride is, when a body is turning on the spot, how far away a body
can be before animating it is a waste — and applies them through your graph. That is the contract in
ADR 0052, and this chapter is how you adopt it.

## The three interfaces

| Interface | Where it lives | Who writes it | What it is |
| --- | --- | --- | --- |
| `Locomotion` | `bevy-flock` (core) | the simulation | speed, turn rate, acceleration and distance travelled, derived from the velocity integration wrote |
| `LocomotionProfile` | `bevy-flock-anim` | you, as data | validated authored numbers: walking speed, running speed, stride, turn band, LOD distances |
| `ClipSlot` | `bevy-flock-anim` | the crate | four names — idle, walk, run, turn-in-place — that you map onto nodes in *your* graph |

The fourth thing is yours too: `ClipBindings`, the map from `ClipSlot` to `AnimationNodeIndex`. Only
your code knows how your graph was built, so the crate refuses to guess.

```rust
use bevy_animation::graph::AnimationNodeIndex;
use bevy_flock_anim::{ClipBindings, LocomotionProfile};

let profile = LocomotionProfile::try_new(
    1.4,   // walking speed, m/s
    4.2,   // running speed, m/s
    1.5,   // stride length, m — the artwork's number, not ours
    0.6,   // turn band, radians of turn rate that counts as turning
    2.0,   // turn-in-place threshold, radians per second
)?;
let bindings = ClipBindings::new(profile_handle, idle_node, walk_node, run_node)
    .with_turn_in_place(turn_node);
```

Spawn the profile as an asset and give each body the binding, a `Locomotion` and Bevy's own
`AnimationPlayer`. `AnimPlugin` does the rest, once per tick.

## The tick is the clock

Every weight the crate writes is a pure function of `Locomotion`. `LocomotionProfile::blend` reads no
clock, holds no state, allocates nothing, and takes `(speed, turn_rate)` in and four weights out. That
is why a replay at 20 fps and a replay at 200 fps produce the same pose at the same tick, and why the
book can promise it: `replay_at_two_frame_rates_gives_identical_weights`.

Where a transition cannot be a pure function — starting and stopping looks like a snap if the weight
jumps — the crate spends *ticks*, not seconds:

```rust
use bevy_flock_anim::BlendWindows;

let profile = profile.with_windows(BlendWindows::try_new(6, 8)?); // six ticks up, eight down
```

The ease is one fixed fraction of the window per simulation tick, interpolating from the pose the
window opened in, so a six-tick start is half committed at tick three and exactly arrived at tick six
at any frame rate. Zero ticks, the default, is the blend itself.

## What you must not do

Do not multiply a weight by `time.delta_secs()`. Do not smooth a weight towards its target. Do not
advance a clip by a frame delta. Every one of those puts the frame rate back inside the guarantee and
gives you a crowd whose legs move at different speeds on different monitors. `cycles_per_second` exists
so you can *set* a playback rate from a per-tick speed and let the animation system apply its own
delta; that is the only place a delta is allowed, and it is Bevy's.

## Culling follows the scheduler, not the camera

Bodies in the ambient band are not animated. Far ones step at an interval; near ones every tick. The
input is the tier the virtual-cost scheduler already computed, plus one `AnimationFocus` you set where
the camera is:

```rust
app.insert_resource(AnimationFocus::at(camera_translation));
```

`BlendLod` on the profile decides the distances and the step cadence, because a rig with a long stride
can be stepped further out than one with a short stride before the freeze reads as a bug. No focus
resource means no culling: an app that never says where the camera is keeps every body animating.

Coming back is the part that pops if you get it wrong. A resumed body gets this tick's weights *and*
its walk and run clips seeked to the phase the distance it walked implies, so it is mid-stride rather
than mid-step. The test that proves it drives a body 300 m out and back and compares every node's
weight against a control that was never culled.

## Root motion: decide, do not discover

Some clips translate their own root. The default is that the simulation owns movement and a reported
offset is dropped; opt in per profile when the clip is the authority:

```rust
use bevy_flock_anim::RootMotion;

let mocap = profile.with_root_motion(RootMotion::Clip);
```

Extraction is yours, because the crate does not know which joint is your root. Read the root joint in
your bridge and report it; the crate consumes the report exactly once, quantises it to a micrometre,
and (in `Clip` mode) advances the body's walk phase by the distance so the next tick's blend does not
drag the legs back. `RootMotionTrace` counts what happened and digests the applied offsets, so two runs
of the same clips can be compared bit for bit. [ADR 0053](../../dev/adr/0053-root-motion-contract.md) has the
reasoning; the short version is that the default is the safe one and the opt-in is explicit because
root motion is the most common way an animation layer breaks a simulation.

## Gaze and gestures

Gaze aims a two-bone chain at whatever cognition already selected, never at the camera directly: the
aim comes from the stimulus set through `GazeAim`, so replacing perception does not break the face.
Gestures are a channel on the blackboard — a behaviour leaf asks for one, `bevy-flock-anim` plays the
clip at a weight the gesture's urgency sets, and the blend leaves it alone.

## The envelope, and what to do with it

At 2,000 bodies the blend costs about 0.09 ms per tick and the gaze solve about 0.18 ms on the
reference machine in `docs/dev/tuning/performance.md`; at 10,000 the blend is 0.94 ms with the LOD band on and
0.28 ms with the camera outside the plaza. The plan's 0.3 ms ceiling for blend plus gaze at 2,000 is
*not* met — the measured pair is 0.55 ms — and the same document says so with the per-case table and
names the two causes: per-bone sparse-set lookups in the gaze solve and hash-map node lookups in the
blend. If you need the budget today, cull harder: the band is what turns 0.94 ms into 0.28 ms.

Nothing in the per-frame path allocates after warm-up, and that is a test rather than a claim:
`tests/no_alloc.rs` counts allocations over 600 ticks of the blend and 600 of the reconciler with a
counting allocator, and fails on the first `Vec`.

## When your rig does not match

The crate cannot fix a bad rig, and the failures look alike:

- **The body slides with its legs still.** Your stride is wrong, not the blend. Measure the distance a
  walk cycle covers in one loop and put that in `try_new`; a stride of 1.5 m on a clip that travels
  0.9 m gives you exactly this.
- **The body moonwalks.** Your clip's forward is `+Z` and Bevy's is `-Z`, or the graph node indices are
  swapped. Check `ClipSlot::Run` before suspecting the blend.
- **The body pops on return from culling.** Something is resetting the player on re-entry. The blend
  seeks; if yours restarts at zero, the pop is yours.
- **The body slides sideways at every start.** You have windows but no root motion, so the legs carry
  half the motion while the other half slides. That is the trade `BlendWindows` makes; measure it with
  `AnimationState::slide()` and shorten the window if the number offends you.
- **Everything is a T-pose.** The binding's nodes are not in the graph the player is playing. That is
  one line of your code and the most common first mistake; the example in
  `playground/src/skinned/avatar.rs` does it correctly.
