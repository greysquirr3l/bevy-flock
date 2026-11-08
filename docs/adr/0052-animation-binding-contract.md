# ADR 0052: The animation binding contract
**Status:** Accepted
**Date:** 2026-11-08
**Context:** Phase 1 shipped a crowd that moves correctly and looks like a scatter plot. Every NPC
is a box that slides from A to B, because nothing in the crate knows what a body does when it walks.
The obvious fix — add animation — is where crowd libraries usually become unusable: the moment a
library owns a skeleton, it owns an opinion about rigs, clip names, retargeting and asset pipelines,
and a game with its own artwork has to fork it or give up. Unity's crowd tools and Unreal's Mass
both escape this by owning the whole engine. This crate does not have that luxury and should not
want it.

The second trap is subtler. Animation is conventionally driven by frame delta: smooth the weight
towards the target, advance the clip by `time.delta_secs()`, lerp the gaze. Every one of those makes
the visual state depend on the frame rate, and this crate's determinism guarantee (ADR 0004) exists
because a run must replay. A crowd that animates differently at 30 fps than at 144 is a crowd whose
replays are not reproducible in the only sense a player can see.
## Decision
The library names the parameters. The game supplies the clips, the skeleton and the graph. Every
binding is a pure function of simulation state, and the tick — never the frame — is the clock.
### Three interfaces, and nothing else in the contract
| Interface | Crate | Owner | What it is |
|---|---|---|---|
| `Locomotion` | `bevy-flock` (core) | library | A component: planar speed, turn rate, acceleration, stride phase, derived from the steering solution each tick. |
| `LocomotionProfile` | `bevy-flock-anim` | game, as data | Validated authored numbers: walking speed, running speed, stride length, turn band. No clip names, no asset handles, no node indices. |
| `ClipSlot` | `bevy-flock-anim` | library | Four names — idle, walk, run, turn-in-place — that a game maps onto nodes in its own `AnimationGraph`. |
The game writes the fourth thing: a binding from `ClipSlot` to `AnimationNodeIndex`, because only it
knows how its graph was built. Everything else is the library's.
### The blend is a pure function, and that is the whole determinism argument
`LocomotionProfile::blend(speed, turn_rate) -> BlendWeights` is a one-dimensional blend space with
three anchors (standing, walking, running) and a turn band taken out of the standing pose. It reads
no clock, holds no state, allocates nothing, and returns four weights that sum to one. Given the
same `Locomotion` it returns the same weights forever, so a 600-tick replay at 20 fps and at 200 fps
produces identical weight vectors at every tick. That is a test
(`replay_at_two_frame_rates_gives_identical_weights`), not an aspiration.
Where a transition needs to be gradual, it is gradual in *space* (a longer blend band) or in *ticks*
(an integer counter), never in seconds.
### Turn-in-place takes from idle and from nowhere else
A body at walking speed steering through ninety degrees walks the turn. A body standing still
pivoting plays a turn clip. The weight comes out of idle's share, so walk and run are untouched and
the sum stays one. Without this rule a hard-steering walker pivots on the spot while travelling,
which reads as a bug to anyone who has watched a crowd.
### The library stops at cycles per second
`cycles_per_second(speed)` is `speed / stride_metres`. A game multiplies by its own frame delta to
set `ActiveAnimation::speed`. This is the one place the library is deliberately less useful than it
could be, because accepting a delta would put the clock back inside the guarantee.
### Culling is the scheduler's tier, not a second distance test
Bodies in the ambient band are not animated. Far bodies step at a configured interval; near ones
every frame. The input is the tier the virtual-cost scheduler already computes (ADR 0009), because a
second distance metric would inevitably disagree with the first, and an NPC would then be drawn but
not animated, or animated but not drawn.
On return from culling, the clip restarts at `stride_phase` rather than at zero, so a body that
comes back into view is mid-stride rather than mid-step. That is the reason `Locomotion` carries a
phase and not only a speed.
### Root motion: the simulation owns movement by default
A clip may carry its own translation. The default is to ignore it: the steering solution is
authoritative, the spatial index stays correct, and the replay stays deterministic. A profile may
opt into extraction, in which case the offset is quantised per tick, added to the transform, and the
steering target advanced to match — never both added, because that is how a crowd ends up moving at
double speed for reasons nobody can reproduce on their machine. The default exists because root
motion is the single most common way an animation layer breaks a simulation.
### Gaze comes from perception, not from the camera
`GazeTarget` carries the entity a cognition system already selected from the stimulus set, plus a
weight. The library solves a two-bone analytic IK split across two spine joints, clamped per joint,
and releases back to the animated pose on an integer tick counter when the target is dropped.
Querying the player transform directly would work in a demo and fail the moment a game replaces
perception, which is a supported thing to do (ADR 0008).
### Costs, stated as budgets
Binding plus gaze is budgeted at 0.3 ms for 2,000 animated bodies and 1.5 ms for 10,000 with
culling, measured on the reference machine in `docs/performance.md`. Nothing in the per-frame path
allocates after warm-up; `tests/no_alloc.rs` asserts it with the counting allocator.
## Consequences
**Positive.** A game keeps its artwork, its rig and its graph; the library cannot go stale against
an asset pipeline it does not own. The animation layer inherits the determinism guarantee instead of
quietly undermining it. The blend is exhaustively testable without a GPU, a window or a clip
asset — the property tests run in milliseconds.
**Negative.** The game writes the slot-to-node binding and the clip playback plumbing, which is
perhaps forty lines and the first thing anybody gets wrong. The book chapter exists because of this,
and the example exists to be copied. The library also cannot fix a bad rig: a wrong joint order, a
missing inverse-bindposes array and a clip authored at the wrong scale are all the game's problems,
and the docs say so plainly rather than implying the binding will sort it out.
**Neutral.** `bevy-flock-anim` is a separate crate rather than a feature of the core. A feature
cannot depend on a crate that depends on the feature's own package, and an `anim` feature that
merely toggled a module would drag `bevy_animation` into every server build. Opting in is depending
on the crate.
**Neutral.** Turn-in-place is part of the blend rather than a state. It costs one more weight per
body and removes a state machine from the player's repertoire of bugs.
## Alternatives considered
**Ship a rig and a clip set.** Killed immediately: it makes the crate an art product, ages badly,
and no studio will retarget its characters onto somebody else's skeleton to use a crowd library.
**Drive weights from `Time::delta_secs()` with smoothing.** Killed by the determinism guarantee. It
is also the reason so many crowd systems look subtly wrong on a 30 fps capture.
**A full animation state machine in the library.** Killed by scope and by ownership: games already
have one (Bevy's `AnimationGraph`, or their own). Duplicating it would put the library in the
business of adjudicating between two graphs.
**Ignore root motion entirely.** Tempting and simpler, but it breaks mocap-derived clips, which are
what a real project has. The opt-in exists, defaults off, and documents its cost.
## References
- `crates/bevy-flock-anim/src/profile.rs` — the blend space and its property tests.
- ADR 0004 (determinism), ADR 0008 (pluggable logic), ADR 0009 (virtual-cost scheduler).
- `docs/book/src/animation.md` — the adoption chapter, written after the example.
