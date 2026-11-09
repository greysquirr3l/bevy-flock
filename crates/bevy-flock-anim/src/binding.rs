//! Driving a game's `AnimationPlayer` from the simulation, without a frame-rate dependency.
//!
//! The contract (ADR 0052) is that this crate names the parameters and the game supplies the graph.
//! A [`ClipBindings`] component is where the two meet: it holds the game's handle to its
//! [`LocomotionProfile`] and the `AnimationNodeIndex` of the idle, walk, run and (optionally)
//! turn-in-place nodes in *its* graph. This module reads [`Locomotion`] once per tick, asks the
//! profile for four weights, and writes them through [`ActiveAnimation::set_weight`].
//!
//! # What the profile expects
//!
//! Three clip nodes, authored one cycle long, plus an optional fourth:
//!
//! ```text
//! graph
//!  ├── idle        a loop that reads as standing: breathing, weight shifts
//!  ├── walk        one full gait cycle, left foot to left foot
//!  ├── run         one full gait cycle, faster and longer
//!  └── turn        a pivot on the spot, played when a stationary body turns
//! ```
//!
//! `LocomotionProfile` is an [`Asset`](bevy_asset::Asset); the node indices are not, because an asset cannot name a
//! node in a graph it has never seen. A game builds the graph, adds the clips, resolves the four
//! labels to `AnimationNodeIndex` values and stores them in [`ClipBindings`].
//!
//! # Why the weights never accumulate
//!
//! [`apply_locomotion_blend`] is a pure write: every tick it recomputes the four weights from
//! `Locomotion` and assigns them. Running it three times in a frame, or three hundred, leaves the
//! same pose, which is what makes the animation independent of frame rate rather than merely
//! similar at two frame rates. Nothing in this path reads `Time`.

use bevy_flock_ecs::adapters::primary::cognition::NpcTier;
use bevy_flock_core::application::SimClock;
use bevy_flock_ecs::prelude::Locomotion;
use bevy_animation::graph::{
    AnimationGraph, AnimationGraphHandle, AnimationNodeIndex, AnimationNodeType,
};
use bevy_animation::{ActiveAnimation, AnimationClip, AnimationPlayer};
use bevy_app::{App, Plugin, Update};
use bevy_asset::{AssetApp, Assets, Handle};
use bevy_ecs::prelude::{Commands, Component, Entity, Local, Query, Res};
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::SystemParam;
use bevy_transform::prelude::Transform;

use crate::lod::AnimationFocus;
use crate::profile::{ClipSlot, LocomotionProfile};
use crate::root::{RootMotionTrace, apply_root_motion};
use crate::slide::slide_per_tick;
use crate::state::AnimationState;

/// The four nodes of a game's animation graph that a crowd drives.
///
/// Insert one per animated NPC, next to its [`AnimationPlayer`]. The component is deliberately a
/// *binding* and not a configuration: it is the game's answer to "which node is walking", and it
/// belongs on the entity rather than in a resource so that two crowds can use two graphs.
///
/// # Example
///
/// ```
/// use bevy_animation::graph::AnimationNodeIndex;
/// use bevy_asset::Handle;
/// use bevy_flock_anim::{ClipBindings, LocomotionProfile};
///
/// # let profile: Handle<LocomotionProfile> = Handle::default();
/// let bindings = ClipBindings::new(
///     profile,
///     AnimationNodeIndex::new(0), // idle
///     AnimationNodeIndex::new(1), // walk
///     AnimationNodeIndex::new(2), // run
/// )
/// .with_turn_in_place(AnimationNodeIndex::new(3));
///
/// assert!(bindings.node(bevy_flock_anim::ClipSlot::Run).is_some());
/// ```
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ClipBindings {
    profile: Handle<LocomotionProfile>,
    idle: AnimationNodeIndex,
    walk: AnimationNodeIndex,
    run: AnimationNodeIndex,
    turn_in_place: Option<AnimationNodeIndex>,
}

impl ClipBindings {
    /// Bind the three clips every crowd needs.
    #[must_use]
    pub const fn new(
        profile: Handle<LocomotionProfile>,
        idle: AnimationNodeIndex,
        walk: AnimationNodeIndex,
        run: AnimationNodeIndex,
    ) -> Self {
        Self {
            profile,
            idle,
            walk,
            run,
            turn_in_place: None,
        }
    }

    /// Bind the optional turn-in-place clip.
    ///
    /// Without it, a stationary body that turns keeps its idle weight and simply does not pivot;
    /// with it, the pivot takes its share of the standing pose. Games whose clips have no pivot
    /// leave this out and lose nothing.
    #[must_use]
    pub const fn with_turn_in_place(mut self, node: AnimationNodeIndex) -> Self {
        self.turn_in_place = Some(node);
        self
    }

    /// The profile this binding reads its numbers from.
    #[must_use]
    pub const fn profile(&self) -> &Handle<LocomotionProfile> {
        &self.profile
    }

    /// The node bound to one slot, if the game bound it.
    #[must_use]
    pub const fn node(&self, slot: ClipSlot) -> Option<AnimationNodeIndex> {
        match slot {
            ClipSlot::Idle => Some(self.idle),
            ClipSlot::Walk => Some(self.walk),
            ClipSlot::Run => Some(self.run),
            ClipSlot::TurnInPlace => self.turn_in_place,
        }
    }

    /// Which slot a node belongs to, for a system iterating the player's active animations.
    ///
    /// Anything this binding does not know — a gesture, a hit reaction, a clip the game started
    /// itself — returns `None` and is left alone.
    #[must_use]
    pub fn slot_of(&self, node: AnimationNodeIndex) -> Option<ClipSlot> {
        ClipSlot::ALL
            .into_iter()
            .find(|slot| self.node(*slot) == Some(node))
    }

    /// Every bound slot and its node, in [`ClipSlot::ALL`] order.
    pub fn slots(&self) -> impl Iterator<Item = (ClipSlot, AnimationNodeIndex)> + '_ {
        ClipSlot::ALL
            .into_iter()
            .filter_map(|slot| self.node(slot).map(|node| (slot, node)))
    }
}

/// The per-frame scratch a system reuses instead of allocating.
///
/// Four weights and four playback rates, filled once per entity and consumed by the node loop. It
/// lives in a `Local`, so a crowd of ten thousand bodies shares one.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct BlendFrame {
    idle: f32,
    walk: f32,
    run: f32,
    turn_in_place: f32,
}

impl BlendFrame {
    /// Fill the scratch from a profile and one body's motion.
    pub fn fill(&mut self, profile: &LocomotionProfile, speed: f32, turn_rate: f32) {
        let blend = profile.blend(speed, turn_rate);
        self.idle = blend.weight(ClipSlot::Idle);
        self.walk = blend.weight(ClipSlot::Walk);
        self.run = blend.weight(ClipSlot::Run);
        self.turn_in_place = blend.weight(ClipSlot::TurnInPlace);
    }

    /// Move `step` of the way from this pose to `target`.
    ///
    /// The settle window is one fixed step per simulation tick rather than a frame delta, so the
    /// sequence of poses a replay passes through depends on the tick count and nothing else.
    #[must_use]
    pub fn eased_toward(self, target: Self, step: f32) -> Self {
        let step = step.clamp(0.0, 1.0);
        let mix = |from: f32, to: f32| from + (to - from) * step;
        Self {
            idle: mix(self.idle, target.idle),
            walk: mix(self.walk, target.walk),
            run: mix(self.run, target.run),
            turn_in_place: mix(self.turn_in_place, target.turn_in_place),
        }
    }
    /// Scale the stride slots by `factor`, handing the remainder to idle.
    ///
    /// This is where a settle window lands: a body three ticks into a six-tick start is drawn with
    /// half its stride weight and half of it standing. Idle and turn-in-place are untouched, so a
    /// body that starts moving while pivoting keeps its pivot.
    pub fn scale_stride(&mut self, factor: f32) {
        let factor = factor.clamp(0.0, 1.0);
        let stride = self.walk + self.run;
        let kept = stride * factor;
        self.idle += stride - kept;
        self.walk *= factor;
        self.run *= factor;
    }
    /// The weight for one slot.
    #[must_use]
    pub const fn weight(&self, slot: ClipSlot) -> f32 {
        match slot {
            ClipSlot::Idle => self.idle,
            ClipSlot::Walk => self.walk,
            ClipSlot::Run => self.run,
            ClipSlot::TurnInPlace => self.turn_in_place,
        }
    }

    /// The playback rate for one slot, in cycles per second.
    ///
    /// Walk and run share the profile's stride: a body covering 1.5 m a cycle should play one cycle
    /// per 1.5 m, whatever its speed. Idle and turn-in-place play at their authored rate, because a
    /// breathing loop and a pivot are not measured in distance.
    #[must_use]
    pub fn rate(profile: &LocomotionProfile, slot: ClipSlot, speed: f32) -> f32 {
        match slot {
            ClipSlot::Walk | ClipSlot::Run => profile.cycles_per_second(speed),
            ClipSlot::Idle | ClipSlot::TurnInPlace => 1.0,
        }
    }
}

/// The assets and the camera focus the binding reads, in one parameter.
///
/// Bundled rather than passed one by one so that a reader can see at a glance how much of the world
/// this system touches: two asset libraries (optional, because a unit test has no plugin), the focus,
/// and the clock that turns a tick into a duration.
#[derive(SystemParam)]
pub struct BlendContext<'w> {
    /// The profiles the bodies bind.
    pub profiles: Res<'w, Assets<LocomotionProfile>>,
    /// Animation graphs, when the game has them loaded.
    pub graphs: Option<Res<'w, Assets<AnimationGraph>>>,
    /// Animation clips, when the game has them loaded.
    pub clips: Option<Res<'w, Assets<AnimationClip>>>,
    /// Where the camera is, which is what the LOD band measures against.
    pub focus: Res<'w, AnimationFocus>,
    /// The simulation clock, absent in a world without the flock plugin.
    pub clock: Option<Res<'w, SimClock>>,
}

/// Apply the locomotion blend to every body that has a player and a binding.
///
/// Runs in `FlockSet::Integration` after the locomotion intent has been derived, so it always sees
/// this tick's speed and turn rate. Writes weights and playback rates; starts a clip at zero weight
/// the first time it is seen, so a body does not pop into a full-weight pose on the frame its clip
/// begins.
///
/// Animations the binding does not name — a gesture, a hit reaction, anything the game started
/// itself — keep whatever weight their own system gave them.
pub fn apply_locomotion_blend(
    ctx: BlendContext,
    mut bodies: Query<(
        Entity,
        &Locomotion,
        &ClipBindings,
        &Transform,
        &mut AnimationPlayer,
        Option<&NpcTier>,
        Option<&AnimationGraphHandle>,
        Option<&mut AnimationState>,
    )>,
    mut commands: Commands,
    mut frame: Local<BlendFrame>,
    mut fallback_tick: Local<u64>,
) {
    // The cadence a stepped body blends on is counted in simulation ticks, so a paused body stays
    // paused across a replay exactly as long as it did the first time. Without the plugin (a unit
    // test, a tool) the counter stands in for the tick, one per pass.
    // A body that has not moved this far is standing, which is what starts and stops the window.
    // Half a centimetre a tick is under a centimetre a second at any sane tick rate.
    const MOVING: f32 = 0.005;
    // Slide is metres per tick, so the measurement needs the tick, never a frame time.
    let delta_seconds = ctx
        .clock
        .as_ref()
        .map_or(1.0 / 60.0, |clock| clock.delta_seconds());
    let tick = ctx.clock.map_or_else(
        || {
            *fallback_tick = fallback_tick.saturating_add(1);
            *fallback_tick
        },
        |clock| clock.tick.get(),
    );

    for (entity, motion, bindings, transform, mut player, tier, graph, state) in &mut bodies {
        let Some(profile) = ctx.profiles.get(bindings.profile()) else {
            continue;
        };
        let band = profile.lod();
        let level = band.level(
            ctx.focus.distance_to(transform.translation),
            tier.map(|tier| tier.0),
        );
        let evaluated = level.evaluates(tick, band.far_step_ticks());

        // The body's bookkeeping is read as a copy, advanced, and written back once at the end of
        // the pass: a body seen for the first time is inserted with the values this tick produced,
        // not with defaults that the next tick would have to re-derive.
        let first_sight = state.is_none();
        let mut state_here = state.as_deref().copied().unwrap_or_default();
        let resumed = state_here.lod_mut().record(level, evaluated);

        if level.pauses_player() {
            // Paused is the only level that stops the bones: Bevy skips a paused player's targets,
            // so the saving is real work not done, not a write that happens to be cheap.
            player.pause_all();
        } else if resumed || evaluated {
            if resumed {
                player.resume_all();
                // Coming back is where a naive cull pops: the weights are this tick's, but the clip
                // is still where it froze. Put walk and run at the phase the distance walked implies.
                seek_to_walked_phase(
                    &mut player,
                    bindings,
                    graph,
                    ctx.graphs.as_deref(),
                    ctx.clips.as_deref(),
                    motion,
                    profile,
                );
            }
            frame.fill(profile, motion.speed(), motion.turn_rate());
            // Start and stop over the profile's window, counted in ticks: the pose a body is drawn
            // in walks toward the pose the simulation asks for, which is what makes a crowd leave
            // and arrive without a snap.
            let moving = motion.speed() > MOVING;
            *frame = state_here
                .settle_mut()
                .advance(*frame, moving, profile.windows());
            let slide = slide_per_tick(profile, &frame, motion.speed(), delta_seconds);
            state_here.slide_mut().observe(slide);

            // First activation only: a clip that is not playing gets started at zero weight, so the
            // tick it starts is not a tick it is visible at full strength.
            for (_, node) in bindings.slots() {
                if !player.is_playing_animation(node) {
                    player.play(node).set_weight(0.0);
                }
            }

            for (node, animation) in player.playing_animations_mut() {
                let Some(slot) = bindings.slot_of(*node) else {
                    continue;
                };
                animation.set_weight(frame.weight(slot));
                animation.set_speed(BlendFrame::rate(profile, slot, motion.speed()));
            }
        }

        if first_sight {
            commands.entity(entity).insert(state_here);
        } else if let Some(mut state) = state {
            *state = state_here;
        }
    }
}

/// Put the walk and run clips where the body's own distance walked says they should be.
///
/// The phase comes from [`Locomotion::phase`] — metres travelled modulo the stride — so a body that
/// walked twelve metres during a pause resumes on the same foot it would have been on. When the
/// game has not bound a graph handle, or the node is not a clip with a known duration, this does
/// nothing: a wrong seek is worse than a late one.
fn seek_to_walked_phase(
    player: &mut AnimationPlayer,
    bindings: &ClipBindings,
    graph: Option<&AnimationGraphHandle>,
    graphs: Option<&Assets<AnimationGraph>>,
    clips: Option<&Assets<AnimationClip>>,
    motion: &Locomotion,
    profile: &LocomotionProfile,
) {
    let phase = motion.phase(profile.stride_metres());
    let (Some(graph), Some(graphs), Some(clips)) = (graph, graphs, clips) else {
        return;
    };
    let Some(asset) = graphs.get(&graph.0) else {
        return;
    };
    for slot in [ClipSlot::Walk, ClipSlot::Run] {
        let Some(node) = bindings.node(slot) else {
            continue;
        };
        let Some(entry) = asset.get(node) else {
            continue;
        };
        let AnimationNodeType::Clip(handle) = &entry.node_type else {
            continue;
        };
        let Some(clip) = clips.get(handle) else {
            continue;
        };
        if let Some(animation) = player.animation_mut(node) {
            seek(animation, phase, clip.duration());
        }
    }
}

/// Seek one animation to `phase` of its clip, clamped into the clip's own range.
fn seek(animation: &mut ActiveAnimation, phase: f32, duration: f32) -> &mut ActiveAnimation {
    let phase = if phase.is_finite() {
        phase.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let duration = if duration.is_finite() && duration > 0.0 {
        duration
    } else {
        0.0
    };
    // The last frame of a looping clip is the first frame of the next cycle, so the seek stops one
    // tick short of the duration rather than landing exactly on its end.
    let at = (phase * duration).min((duration - f32::EPSILON).max(0.0));
    animation.set_seek_time(at)
}

/// Adds the animation binding: the profile asset type and the system that drives the players.
///
/// Requires `bevy_asset::AssetPlugin` (or any plugin that installs the asset server), because
/// [`LocomotionProfile`] is an asset.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnimPlugin;

impl Plugin for AnimPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<LocomotionProfile>()
            .init_resource::<AnimationFocus>()
            .init_resource::<RootMotionTrace>()
            // The reconciler runs after the blend so a bridge that reports an offset during the
            // animation pass has it consumed on the same tick it was produced.
            .add_systems(
                Update,
                (
                    apply_locomotion_blend,
                    apply_root_motion,
                )
                    .chain(),
            );
    }
}

/// A profile asset for use in tests and fixtures: the numbers in this crate's documentation.
///
/// Kept next to the binding because every test in this module needs one and a second copy in the
/// integration tests would eventually disagree with this one.
#[cfg(test)]
pub(crate) fn fixture_profile() -> LocomotionProfile {
    LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)
        .unwrap_or_else(|_| unreachable!("the fixture profile is valid"))
}

#[cfg(test)]
mod tests {
    use super::{BlendFrame, ClipBindings, apply_locomotion_blend, fixture_profile};
    use bevy_flock_ecs::adapters::primary::cognition::NpcTier;
    use crate::lod::{AnimationFocus, AnimationLod, BlendLod, LodLevel};
    use crate::profile::{ClipSlot, LocomotionProfile};
    use bevy_flock_core::domain::cognition::CognitionTier;
    use bevy_flock_ecs::prelude::Locomotion;
    use bevy_animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
    use bevy_animation::{AnimationClip, AnimationPlayer};
    use bevy_asset::{Assets, Handle};
    use bevy_ecs::prelude::{Entity, World};
    use bevy_ecs::system::RunSystemOnce;
    use bevy_math::{Vec2, Vec3};
    use bevy_transform::prelude::Transform;
    use proptest::prelude::{prop_assert, proptest};

    const TICK: f32 = 1.0 / 60.0;

    fn index(raw: usize) -> AnimationNodeIndex {
        AnimationNodeIndex::new(raw)
    }

    /// A world with one profile asset and one animated body. Returns the node indices in
    /// [`ClipSlot::ALL`] order and the entity that carries the player.
    fn world_with_body() -> (World, [AnimationNodeIndex; 4], Entity) {
        let mut world = World::new();
        let mut assets = Assets::<LocomotionProfile>::default();
        let handle = assets.add(fixture_profile());
        world.insert_resource(assets);
        world.insert_resource(bevy_flock_core::application::SimClock::default());
        world.init_resource::<AnimationFocus>();

        let nodes = [index(0), index(1), index(2), index(3)];
        let bindings =
            ClipBindings::new(handle, nodes[0], nodes[1], nodes[2]).with_turn_in_place(nodes[3]);
        let entity = world
            .spawn((
                Locomotion::default(),
                Transform::default(),
                bindings,
                AnimationPlayer::default(),
            ))
            .id();
        (world, nodes, entity)
    }

    fn weight_of(world: &World, entity: Entity, node: AnimationNodeIndex) -> f32 {
        let mut found = 0.0_f32;
        if let Some(player) = world.get::<AnimationPlayer>(entity) {
            for (active, animation) in player.playing_animations() {
                if *active == node {
                    found = animation.weight();
                }
            }
        }
        found
    }

    fn speed_of(world: &World, entity: Entity, node: AnimationNodeIndex) -> f32 {
        let mut found = 0.0_f32;
        if let Some(player) = world.get::<AnimationPlayer>(entity) {
            for (active, animation) in player.playing_animations() {
                if *active == node {
                    found = animation.speed();
                }
            }
        }
        found
    }

    /// Run the binding system on the next simulation tick, failing the test rather than ignoring an
    /// error. Advancing the clock here is what makes the cadence tests mean anything: a system run
    /// through `run_system_once` gets fresh `Local` state every call, so the tick has to live in the
    /// world, exactly as it does in a game.
    fn run(world: &mut World) {
        if let Some(mut clock) = world.get_resource_mut::<bevy_flock_core::application::SimClock>() {
            clock.advance();
        }
        if world.run_system_once(apply_locomotion_blend).is_err() {
            unreachable!("the binding system runs on any world");
        }
    }

    /// Move the fixture's body as if steering had written this velocity.
    fn move_body(world: &mut World, entity: Entity, velocity: Vec2) {
        if let Some(mut motion) = world.get_mut::<Locomotion>(entity) {
            motion.advance(velocity, velocity, TICK);
        }
    }

    #[test]
    fn the_first_run_starts_every_clip_at_zero_weight_then_assigns() {
        let (mut world, nodes, entity) = world_with_body();
        run(&mut world);
        // Standing still: idle carries the pose, and the walk clip exists at zero rather than not
        // existing at all, which is what makes the transition reversible without a graph edit.
        assert!((weight_of(&world, entity, nodes[0]) - 1.0).abs() < 1e-6);
        assert!(weight_of(&world, entity, nodes[1]).abs() < 1e-6);
        assert!(weight_of(&world, entity, nodes[2]).abs() < 1e-6);
    }

    #[test]
    fn weights_are_the_profile_s_blend_of_the_body_s_motion() {
        let (mut world, nodes, entity) = world_with_body();
        let profile = fixture_profile();
        move_body(&mut world, entity, Vec2::new(0.0, 0.7));
        run(&mut world);

        let expected = profile.blend(0.7, 0.0);
        for (slot, node) in ClipSlot::ALL.into_iter().zip(nodes) {
            let actual = weight_of(&world, entity, node);
            assert!(
                (actual - expected.weight(slot)).abs() < 1e-6,
                "{}: {actual} != {}",
                slot.as_str(),
                expected.weight(slot)
            );
        }
    }

    #[test]
    fn playback_rate_follows_the_stride_not_the_speed() {
        let (mut world, nodes, entity) = world_with_body();
        move_body(&mut world, entity, Vec2::new(0.0, 3.0));
        run(&mut world);
        // 3 m/s over a 1.5 m stride is two cycles a second, on both gait clips.
        assert!((speed_of(&world, entity, nodes[1]) - 2.0).abs() < 1e-5);
        assert!((speed_of(&world, entity, nodes[2]) - 2.0).abs() < 1e-5);
        // A body standing still still breathes.
        let (mut still, still_nodes, still_entity) = world_with_body();
        run(&mut still);
        assert!((speed_of(&still, still_entity, still_nodes[0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn running_the_binding_again_changes_nothing() {
        let (mut world, nodes, entity) = world_with_body();
        move_body(&mut world, entity, Vec2::new(0.0, 3.2));
        run(&mut world);
        let first = weight_of(&world, entity, nodes[2]);
        for _ in 0..10 {
            run(&mut world);
        }
        // Bit-identical, not merely close: the second run recomputes rather than accumulates.
        assert_eq!(
            first.to_bits(),
            weight_of(&world, entity, nodes[2]).to_bits()
        );
        assert!(first > 0.0);
    }

    #[test]
    fn an_animation_the_binding_does_not_name_is_left_alone() {
        let (mut world, nodes, entity) = world_with_body();
        let gesture = index(9);
        if let Some(mut player) = world.get_mut::<AnimationPlayer>(entity) {
            player.play(gesture).set_weight(0.8);
        }
        run(&mut world);
        assert!((weight_of(&world, entity, gesture) - 0.8).abs() < 1e-6);
        assert!((weight_of(&world, entity, nodes[0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_six_hundred_tick_run_gives_the_same_weights_at_any_frame_rate() {
        // The simulation runs at sixty ticks a second in both runs; only the number of times the
        // render path applies the binding changes — once per tick, then ten times.
        let reference = drive(600, 1);
        let fast = drive(600, 10);
        assert_eq!(reference.len(), fast.len());
        for (tick, (slow, quick)) in reference.iter().zip(&fast).enumerate() {
            for (a, b) in slow.iter().zip(quick) {
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "tick {tick}: {a} != {b} at a different application rate"
                );
            }
        }
    }

    /// Run for `ticks`, applying the binding `applications` times per tick, and collect the four
    /// weights after each tick.
    fn drive(ticks: usize, applications: usize) -> Vec<[f32; 4]> {
        let (mut world, nodes, entity) = world_with_body();
        let mut trace = Vec::with_capacity(ticks);
        for step in 0..ticks {
            // A body that walks, accelerates into a run and turns: enough motion that a
            // frame-rate dependency would show up in the weights.
            let phase = f32::from(u16::try_from(step % 240).unwrap_or(0)) / 240.0;
            let speed = 0.2 + 4.0 * phase;
            let heading = Vec2::new(libm::sinf(phase * 3.0), libm::cosf(phase * 3.0));
            move_body(&mut world, entity, heading * speed);
            for _ in 0..applications {
                run(&mut world);
            }
            trace.push([
                weight_of(&world, entity, nodes[0]),
                weight_of(&world, entity, nodes[1]),
                weight_of(&world, entity, nodes[2]),
                weight_of(&world, entity, nodes[3]),
            ]);
        }
        trace
    }

    #[test]
    fn a_body_without_a_loaded_profile_is_skipped_rather_than_panicking() {
        let mut world = World::new();
        world.insert_resource(Assets::<LocomotionProfile>::default());
        world.init_resource::<AnimationFocus>();
        let bindings = ClipBindings::new(Handle::default(), index(0), index(1), index(2));
        let entity = world
            .spawn((
                Locomotion::default(),
                Transform::default(),
                bindings,
                AnimationPlayer::default(),
            ))
            .id();
        run(&mut world);
        // Nothing was started, because there is no profile to take weights from.
        let player = world.get::<AnimationPlayer>(entity);
        assert!(matches!(player, Some(player) if player.playing_animations().count() == 0));
    }

    #[test]
    fn a_binding_without_a_turn_clip_simply_has_no_such_slot() {
        let bindings = ClipBindings::new(Handle::default(), index(0), index(1), index(2));
        assert!(bindings.node(ClipSlot::TurnInPlace).is_none());
        assert_eq!(bindings.slots().count(), 3);
        assert_eq!(bindings.slot_of(index(1)), Some(ClipSlot::Walk));
        assert_eq!(bindings.slot_of(index(7)), None);
    }

    #[test]
    fn the_scratch_is_reused_and_reports_what_it_was_filled_with() {
        let mut frame = BlendFrame::default();
        let profile = fixture_profile();
        frame.fill(&profile, 0.7, 0.0);
        let expected = profile.blend(0.7, 0.0);
        for slot in ClipSlot::ALL {
            assert!((frame.weight(slot) - expected.weight(slot)).abs() < 1e-6);
        }
        frame.fill(&profile, 5.0, 0.0);
        assert!((frame.weight(ClipSlot::Run) - 1.0).abs() < 1e-6);
    }

    proptest! {
        /// The weights a game will actually apply, over the speeds and turn rates a crowd produces:
        /// finite, non-negative and normalised.
        #[test]
        fn every_weight_is_finite_non_negative_and_normalised(
            speed in 0.0_f32..12.0,
            turn in -8.0_f32..8.0,
        ) {
            let profile = fixture_profile();
            let blend = profile.blend(speed, turn);
            let total: f32 = ClipSlot::ALL
                .into_iter()
                .map(|slot| blend.weight(slot))
                .sum();
            for slot in ClipSlot::ALL {
                let weight = blend.weight(slot);
                prop_assert!(weight.is_finite(), "{} at speed {} turn {}", slot.as_str(), speed, turn);
                prop_assert!(weight >= 0.0, "{} was {} at speed {}", slot.as_str(), weight, speed);
            }
            prop_assert!(
                (total - 1.0).abs() < 1e-5,
                "sum {} at speed {} turn {}",
                total,
                speed,
                turn
            );
        }
    }

    /// A body at `position`, with the tier the scheduler assigned it.
    fn spawn_at(
        world: &mut World,
        bindings: ClipBindings,
        position: Vec3,
        tier: Option<CognitionTier>,
    ) -> Entity {
        let mut entity = world.spawn((
            Locomotion::default(),
            Transform::from_translation(position),
            bindings,
            AnimationPlayer::default(),
        ));
        if let Some(tier) = tier {
            entity.insert(NpcTier(tier));
        }
        entity.id()
    }

    fn bindings_for(world: &mut World) -> ClipBindings {
        let mut query = world.query::<&ClipBindings>();
        query
            .iter(world)
            .next()
            .cloned()
            .unwrap_or_else(|| ClipBindings::new(Handle::default(), index(0), index(1), index(2)))
    }

    fn lod_of(world: &World, entity: Entity) -> AnimationLod {
        world
            .get::<crate::state::AnimationState>(entity)
            .map(crate::state::AnimationState::lod)
            .unwrap_or_default()
    }

    #[test]
    fn the_tier_the_scheduler_chose_decides_stepping_inside_the_band() {
        let (mut world, _, _) = world_with_body();
        let bindings = bindings_for(&mut world);
        *world.resource_mut::<AnimationFocus>() = AnimationFocus::at(Vec3::ZERO);
        // Both bodies sit at the same distance: only the tier can separate them.
        let reflex = spawn_at(
            &mut world,
            bindings.clone(),
            Vec3::new(60.0, 0.0, 0.0),
            Some(CognitionTier::Reflex),
        );
        let thinking = spawn_at(
            &mut world,
            bindings,
            Vec3::new(0.0, 0.0, 60.0),
            Some(CognitionTier::Deliberative),
        );
        run(&mut world);
        assert_eq!(
            lod_of(&world, reflex).level(),
            LodLevel::Stepped,
            "a reflex body steps"
        );
        assert_eq!(
            lod_of(&world, thinking).level(),
            LodLevel::Full,
            "a thinking body does not"
        );
        assert_eq!(
            lod_of(&world, reflex).evaluations(),
            0,
            "tick 1 is not on a six-tick cadence"
        );
        for _ in 0..5 {
            run(&mut world);
        }
        assert_eq!(lod_of(&world, reflex).evaluations(), 1, "tick 6 is");
        assert_eq!(
            lod_of(&world, thinking).evaluations(),
            6,
            "the full body blended every tick"
        );
    }

    #[test]
    fn a_body_beyond_far_is_paused_and_never_evaluated() {
        let (mut world, _, body) = world_with_body();
        *world.resource_mut::<AnimationFocus>() = AnimationFocus::at(Vec3::ZERO);
        if let Some(mut transform) = world.get_mut::<Transform>(body) {
            transform.translation = Vec3::new(500.0, 0.0, 0.0);
        }
        run(&mut world);
        assert_eq!(lod_of(&world, body).level(), LodLevel::Paused);
        assert!(
            world
                .get::<AnimationPlayer>(body)
                .is_none_or(|player| player.playing_animations().all(|(_, a)| a.is_paused())),
            "a paused body's clips are paused, not merely left alone"
        );
        for _ in 0..1_000 {
            run(&mut world);
        }
        assert_eq!(
            lod_of(&world, body).evaluations(),
            0,
            "a paused body is not blended"
        );
    }

    #[test]
    fn a_returning_body_resumes_at_the_walked_phase() {
        let (mut world, nodes, body) = world_with_body();
        *world.resource_mut::<AnimationFocus>() = AnimationFocus::at(Vec3::ZERO);
        // A control that is never culled, moving identically.
        let bindings = bindings_for(&mut world);
        let control = spawn_at(&mut world, bindings, Vec3::ZERO, None);
        for entity in [body, control] {
            move_body(&mut world, entity, Vec2::new(0.0, 1.2));
            run(&mut world);
        }
        // Push the camera 300 m away: the body pauses, the control stays where it is.
        if let Some(mut transform) = world.get_mut::<Transform>(body) {
            transform.translation = Vec3::new(300.0, 0.0, 0.0);
        }
        run(&mut world);
        assert_eq!(lod_of(&world, body).level(), LodLevel::Paused);
        // Walk further while it is paused, then bring the camera back.
        for _ in 0..10 {
            move_body(&mut world, body, Vec2::new(0.0, 1.2));
            move_body(&mut world, control, Vec2::new(0.0, 1.2));
            run(&mut world);
        }
        if let Some(mut transform) = world.get_mut::<Transform>(body) {
            transform.translation = Vec3::ZERO;
        }
        run(&mut world);
        assert_eq!(lod_of(&world, body).level(), LodLevel::Full);
        assert_eq!(lod_of(&world, body).resumes(), 1);
        for node in [nodes[0], nodes[1], nodes[2]] {
            let resumed = weight_of(&world, body, node);
            let control = weight_of(&world, control, node);
            assert!(
                (resumed - control).abs() < 1e-6,
                "node {node:?}: resumed {resumed} vs control {control}"
            );
        }
    }

    #[test]
    fn a_disabled_band_animates_a_body_at_any_distance() {
        let (mut world, _, body) = world_with_body();
        let handle = bindings_for(&mut world).profile().clone();
        if let Some(mut assets) = world.get_resource_mut::<Assets<LocomotionProfile>>()
            && let Some(mut profile) = assets.get_mut(&handle)
        {
            *profile = profile.with_lod(BlendLod::disabled());
        }
        *world.resource_mut::<AnimationFocus>() = AnimationFocus::at(Vec3::new(1_000.0, 0.0, 0.0));
        for _ in 0..5 {
            run(&mut world);
        }
        assert_eq!(lod_of(&world, body).level(), LodLevel::Full);
        assert_eq!(lod_of(&world, body).evaluations(), 5);
    }

    #[test]
    fn resuming_seeks_the_walk_clip_to_the_phase_the_distance_implies() {
        let mut world = World::new();
        let mut profiles = Assets::<LocomotionProfile>::default();
        let handle = profiles.add(fixture_profile());
        world.insert_resource(profiles);
        world.init_resource::<AnimationFocus>();
        let mut graphs = Assets::<AnimationGraph>::default();
        let mut clips = Assets::<AnimationClip>::default();
        let mut graph = AnimationGraph::new();
        let root = graph.root;
        let walk_clip = clips.add(AnimationClip::default());
        if let Some(mut clip) = clips.get_mut(&walk_clip) {
            clip.set_duration(2.0);
        }
        let walk = graph.add_clip(walk_clip, 1.0, root);
        let graph_handle = graphs.add(graph);
        world.insert_resource(graphs);
        world.insert_resource(clips);
        let bindings = ClipBindings::new(handle, index(0), walk, index(2));
        let body = world
            .spawn((
                Locomotion::default(),
                Transform::default(),
                bindings,
                AnimationGraphHandle(graph_handle),
                AnimationPlayer::default(),
            ))
            .id();
        *world.resource_mut::<AnimationFocus>() = AnimationFocus::at(Vec3::ZERO);
        // Walk 0.75 m of a 1.5 m stride: the clip should resume halfway through its 2 s.
        move_body(&mut world, body, Vec2::new(0.0, 1.5));
        run(&mut world);
        if let Some(mut transform) = world.get_mut::<Transform>(body) {
            transform.translation = Vec3::new(300.0, 0.0, 0.0);
        }
        run(&mut world);
        if let Some(mut transform) = world.get_mut::<Transform>(body) {
            transform.translation = Vec3::ZERO;
        }
        run(&mut world);
        let profile = fixture_profile();
        let phase = world
            .get::<Locomotion>(body)
            .map_or(0.0, |motion| motion.phase(profile.stride_metres()));
        let seen = world
            .get::<AnimationPlayer>(body)
            .and_then(|player| {
                player
                    .playing_animations()
                    .find(|(node, _)| **node == walk)
                    .map(|(_, animation)| animation.seek_time())
            })
            .unwrap_or(-1.0);
        assert!(
            (seen - phase * 2.0).abs() < 1e-3,
            "seek {seen} is not the walked phase {phase} of a 2 s clip"
        );
    }

    /// A world like [`world_with_body`], but the profile carries a start and stop window.
    fn world_with_windows(start: u32, stop: u32) -> (World, [AnimationNodeIndex; 4], Entity) {
        let mut world = World::new();
        let mut assets = Assets::<LocomotionProfile>::default();
        let windows = crate::settle::BlendWindows::try_new(start, stop)
            .unwrap_or_else(|_| unreachable!("the fixture windows are valid"));
        let handle = assets.add(fixture_profile().with_windows(windows));
        world.insert_resource(assets);
        world.insert_resource(bevy_flock_core::application::SimClock::default());
        world.init_resource::<AnimationFocus>();
        let nodes = [index(0), index(1), index(2), index(3)];
        let bindings =
            ClipBindings::new(handle, nodes[0], nodes[1], nodes[2]).with_turn_in_place(nodes[3]);
        let entity = world
            .spawn((
                Locomotion::default(),
                Transform::default(),
                bindings,
                AnimationPlayer::default(),
            ))
            .id();
        (world, nodes, entity)
    }
    /// Drive one body's motion to a steady speed, the way the integration system would.
    fn steady(world: &mut World, entity: Entity, speed: f32) {
        if let Some(mut motion) = world.get_mut::<Locomotion>(entity) {
            for _ in 0..40 {
                motion.advance(Vec2::new(speed, 0.0), Vec2::new(speed, 0.0), TICK);
            }
        }
    }
    #[test]
    fn a_start_window_commits_the_stride_over_its_ticks() {
        let (mut world, nodes, body) = world_with_windows(6, 8);
        steady(&mut world, body, 1.4);
        run(&mut world);
        let first = weight_of(&world, body, nodes[1]);
        assert!(
            (first - 1.0 / 6.0).abs() < 1e-4,
            "one tick into a six-tick start should be a sixth of the walk, got {first}"
        );
        for _ in 0..5 {
            run(&mut world);
        }
        let settled = weight_of(&world, body, nodes[1]);
        assert!(
            (settled - 1.0).abs() < 1e-4,
            "six ticks should commit the walk, got {settled}"
        );
    }
    #[test]
    fn the_window_counts_ticks_rather_than_seconds() {
        // The same start window, driven at 1 tick per pass and at 10 ticks per pass. If any part of
        // the transition lerped against a delta, the two runs would disagree.
        let (mut slow, slow_nodes, slow_body) = world_with_windows(6, 8);
        let (mut fast, fast_nodes, fast_body) = world_with_windows(6, 8);
        steady(&mut slow, slow_body, 1.4);
        steady(&mut fast, fast_body, 1.4);
        for _ in 0..3 {
            run(&mut slow);
            if let Some(mut clock) = fast.get_resource_mut::<bevy_flock_core::application::SimClock>() {
                for _ in 0..9 {
                    clock.advance();
                }
            }
            run(&mut fast);
        }
        let slow_weight = weight_of(&slow, slow_body, slow_nodes[1]);
        let fast_weight = weight_of(&fast, fast_body, fast_nodes[1]);
        assert!(
            (slow_weight - fast_weight).abs() < 1e-6,
            "the window is tick-counted: {slow_weight} against {fast_weight}"
        );
        assert!(
            (fast_weight - 0.5).abs() < 1e-4,
            "three ticks of six is a half, got {fast_weight}"
        );
    }
    #[test]
    fn a_stop_window_hands_the_walk_back_to_idle() {
        let (mut world, nodes, body) = world_with_windows(0, 8);
        steady(&mut world, body, 1.4);
        run(&mut world);
        assert!((weight_of(&world, body, nodes[1]) - 1.0).abs() < 1e-4);
        steady(&mut world, body, 0.0);
        run(&mut world);
        let just_stopped = weight_of(&world, body, nodes[1]);
        assert!(
            (just_stopped - 7.0 / 8.0).abs() < 1e-4,
            "one tick into an eight-tick stop keeps seven eighths, got {just_stopped}"
        );
        for _ in 0..7 {
            run(&mut world);
        }
        assert!(
            weight_of(&world, body, nodes[1]).abs() < 1e-4,
            "the walk is gone"
        );
        assert!(
            (weight_of(&world, body, nodes[0]) - 1.0).abs() < 1e-4,
            "and idle carries the body"
        );
    }
    #[test]
    fn a_straight_walk_slides_under_a_millimetre_in_three_hundred_ticks() {
        let (mut world, nodes, body) = world_with_body();
        steady(&mut world, body, 1.4);
        for _ in 0..300 {
            run(&mut world);
        }
        let slide = world
            .get::<crate::state::AnimationState>(body)
            .map(crate::state::AnimationState::slide)
            .unwrap_or_default();
        assert_eq!(slide.ticks(), 300, "every tick is measured");
        assert!(
            slide.total_metres() < 1e-3,
            "300 ticks of walking slid {:.4} mm in total",
            slide.total_metres() * 1_000.0
        );
        assert!(
            (weight_of(&world, body, nodes[1]) - 1.0).abs() < 1e-4,
            "and the walk is still the whole pose"
        );
    }
}
