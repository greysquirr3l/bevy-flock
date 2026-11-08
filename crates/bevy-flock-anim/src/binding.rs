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
//! `LocomotionProfile` is an [`Asset`]; the node indices are not, because an asset cannot name a
//! node in a graph it has never seen. A game builds the graph, adds the clips, resolves the four
//! labels to `AnimationNodeIndex` values and stores them in [`ClipBindings`].
//!
//! # Why the weights never accumulate
//!
//! [`apply_locomotion_blend`] is a pure write: every tick it recomputes the four weights from
//! `Locomotion` and assigns them. Running it three times in a frame, or three hundred, leaves the
//! same pose, which is what makes the animation independent of frame rate rather than merely
//! similar at two frame rates. Nothing in this path reads `Time`.

use bevy_animation::AnimationPlayer;
use bevy_animation::graph::AnimationNodeIndex;
use bevy_app::{App, Plugin, Update};
use bevy_asset::{AssetApp, Assets, Handle};
use bevy_ecs::prelude::{Component, Local, Query, Res};
use bevy_flock::prelude::Locomotion;

use crate::profile::{ClipSlot, LocomotionProfile};

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
#[derive(Debug, Default, Clone, Copy)]
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

/// Apply the locomotion blend to every body that has a player and a binding.
///
/// Runs in `FlockSet::Integration` after the locomotion intent has been derived, so it always sees
/// this tick's speed and turn rate. Writes weights and playback rates; starts a clip at zero weight
/// the first time it is seen, so a body does not pop into a full-weight pose on the frame its clip
/// begins.
///
/// Animations the binding does not name — a gesture, a hit reaction, anything the game started
/// itself — keep whatever weight their own system gave them.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn apply_locomotion_blend(
    profiles: Res<Assets<LocomotionProfile>>,
    mut bodies: Query<(&Locomotion, &ClipBindings, &mut AnimationPlayer)>,
    mut frame: Local<BlendFrame>,
) {
    for (motion, bindings, mut player) in &mut bodies {
        let Some(profile) = profiles.get(bindings.profile()) else {
            continue;
        };
        frame.fill(profile, motion.speed(), motion.turn_rate());

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
            .add_systems(Update, apply_locomotion_blend);
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
    use crate::profile::{ClipSlot, LocomotionProfile};
    use bevy_animation::AnimationPlayer;
    use bevy_animation::graph::AnimationNodeIndex;
    use bevy_asset::{Assets, Handle};
    use bevy_ecs::prelude::{Entity, World};
    use bevy_ecs::system::RunSystemOnce;
    use bevy_flock::prelude::Locomotion;
    use bevy_math::Vec2;
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

        let nodes = [index(0), index(1), index(2), index(3)];
        let bindings =
            ClipBindings::new(handle, nodes[0], nodes[1], nodes[2]).with_turn_in_place(nodes[3]);
        let entity = world
            .spawn((Locomotion::default(), bindings, AnimationPlayer::default()))
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

    /// Run the binding system, failing the test rather than ignoring an error.
    fn run(world: &mut World) {
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
        let bindings = ClipBindings::new(Handle::default(), index(0), index(1), index(2));
        let entity = world
            .spawn((Locomotion::default(), bindings, AnimationPlayer::default()))
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
}
