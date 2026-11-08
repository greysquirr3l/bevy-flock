//! Playing a gesture over the locomotion blend, and getting out of its way afterwards.
//!
//! The core adjudicates gestures: a leaf asks, the channel decides, and [`NpcGestures::cue`] carries
//! the answer for as many ticks as the gesture lasts. This module is the consumer. It starts the
//! clip once per request — keyed on the cue's serial, not on the tick — writes the cue's weight while
//! the gesture runs, and stops the clip when the cue goes away.
//!
//! # Why the locomotion weights do not need restoring
//!
//! [`crate::apply_locomotion_blend`] is a pure write: every tick it recomputes and assigns the
//! weights of the nodes it names. A gesture node is not one of those nodes, so the two systems never
//! fight, and a gesture cannot leave a locomotion weight anywhere but where the blend put it. That
//! is the whole reason [`crate::ClipBindings`] leaves unnamed animations alone, and it is what the
//! `a_gesture_leaves_the_locomotion_weights_exactly_as_the_blend_wrote_them` test pins.

use bevy_animation::AnimationPlayer;
use bevy_animation::graph::AnimationNodeIndex;
use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::{Component, Query};
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_flock::prelude::{GestureKind, NpcGestures};

use crate::binding::ClipBindings;

/// Which clip node in the game's graph plays each gesture.
///
/// A slot per [`GestureKind`], absent when the game has no clip for that gesture — a crowd with no
/// bark animation simply never barks visually, and the channel's refusals never reach the player.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GestureClips {
    nod: Option<AnimationNodeIndex>,
    wave: Option<AnimationNodeIndex>,
    bark: Option<AnimationNodeIndex>,
    flinch: Option<AnimationNodeIndex>,
}

impl GestureClips {
    /// A body with no gesture clips bound.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            nod: None,
            wave: None,
            bark: None,
            flinch: None,
        }
    }

    /// Bind a node to one gesture. Chain the calls.
    ///
    /// A field per kind rather than an array indexed by [`GestureKind::ordinal`]: the kind is a
    /// closed enum, so the compiler can check the mapping instead of trusting an ordinal to stay in
    /// range when someone reorders the variants.
    #[must_use]
    pub const fn with(mut self, kind: GestureKind, node: AnimationNodeIndex) -> Self {
        match kind {
            GestureKind::Nod => self.nod = Some(node),
            GestureKind::Wave => self.wave = Some(node),
            GestureKind::Bark => self.bark = Some(node),
            GestureKind::Flinch => self.flinch = Some(node),
        }
        self
    }

    /// The node bound to one gesture, if the game bound it.
    #[must_use]
    pub const fn node(&self, kind: GestureKind) -> Option<AnimationNodeIndex> {
        match kind {
            GestureKind::Nod => self.nod,
            GestureKind::Wave => self.wave,
            GestureKind::Bark => self.bark,
            GestureKind::Flinch => self.flinch,
        }
    }
}

/// What a body is playing, so a gesture is started once per request rather than once per tick.
///
/// The cue is present on every tick of the gesture; a systems that started the clip whenever it saw
/// a cue would restart it every frame and show only the gesture's first frame forever. The serial the
/// core stamps on the cue is what makes "once" possible without a clock here.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GesturePlayback {
    current: Option<(GestureKind, u32)>,
}

impl GesturePlayback {
    /// A body that has never gestured.
    #[must_use]
    pub const fn new() -> Self {
        Self { current: None }
    }

    /// The gesture being played and its serial, if any.
    #[must_use]
    pub const fn current(self) -> Option<(GestureKind, u32)> {
        self.current
    }
}

/// Start, weight and stop gesture clips from the cues the core writes.
///
/// Ordered after [`crate::apply_locomotion_blend`] so a gesture's weight lands on the same tick as
/// the pose underneath it. The two write disjoint nodes; doing them in that order keeps the reader's
/// model of the frame — blend first, then the thing on top — the same as the schedule's.
pub fn play_gestures(
    mut bodies: Query<(
        &NpcGestures,
        &GestureClips,
        &ClipBindings,
        &mut GesturePlayback,
        &mut AnimationPlayer,
    )>,
) {
    for (gestures, clips, bindings, mut playback, mut player) in &mut bodies {
        let Some(cue) = gestures.cue() else {
            // The gesture is over: stop the clip it was playing and nothing else. The locomotion
            // nodes are the blend's to write and were never touched here.
            if let Some((kind, _)) = playback.current.take()
                && let Some(node) = clips.node(kind)
                && bindings.slot_of(node).is_none()
            {
                player.stop(node);
            }
            continue;
        };

        let Some(node) = clips.node(cue.kind()) else {
            continue;
        };
        // A game that bound a locomotion clip as a gesture would otherwise have the two systems
        // overwrite one node's weight every tick, which reads as a flicker nobody can trace.
        if bindings.slot_of(node).is_some() {
            continue;
        }

        if playback.current != Some((cue.kind(), cue.serial())) {
            player.play(node).set_weight(0.0);
            playback.current = Some((cue.kind(), cue.serial()));
        }
        for (active, animation) in player.playing_animations_mut() {
            if *active == node {
                animation.set_weight(cue.weight());
            }
        }
    }
}

/// Adds the gesture playback. Every body needs [`GestureClips`], [`GesturePlayback`] and
/// [`ClipBindings`]; the last one is what keeps a gesture off the locomotion nodes.
#[derive(Debug, Default, Clone, Copy)]
pub struct GesturePlugin;

impl Plugin for GesturePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            play_gestures.after(crate::binding::apply_locomotion_blend),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{GestureClips, GesturePlayback, play_gestures};
    use crate::binding::{ClipBindings, apply_locomotion_blend, fixture_profile};
    use crate::profile::LocomotionProfile;
    use bevy_animation::AnimationPlayer;
    use bevy_animation::graph::AnimationNodeIndex;
    use bevy_asset::Assets;
    use bevy_ecs::prelude::{Entity, World};
    use bevy_ecs::system::RunSystemOnce;
    use bevy_flock::adapters::primary::cognition::NpcBlackboard;
    use bevy_flock::prelude::{
        GestureChannel, GestureConfig, GestureKind, Locomotion, Npc, NpcGestures, NpcId, SimClock,
        advance_gestures, collect_gesture_requests, request_gesture,
    };

    /// The node a gesture plays on, and a node the blend owns, chosen apart so a test can tell
    /// which system wrote what.
    fn wave_node() -> AnimationNodeIndex {
        AnimationNodeIndex::new(5)
    }

    fn idle_node() -> AnimationNodeIndex {
        AnimationNodeIndex::new(0)
    }

    fn weight_of(world: &World, entity: Entity, node: AnimationNodeIndex) -> Option<f32> {
        world.get::<AnimationPlayer>(entity).and_then(|player| {
            player
                .playing_animations()
                .find(|(active, _)| **active == node)
                .map(|(_, playing)| playing.weight())
        })
    }

    struct Fixture {
        world: World,
        body: Entity,
    }

    /// A world with the core's gesture channel in it and one animated body.
    fn fixture() -> Fixture {
        let mut world = World::new();
        world.insert_resource(SimClock::default());
        world.insert_resource(GestureConfig::default());
        world.insert_resource(GestureChannel::new(8));
        let mut assets = Assets::<LocomotionProfile>::default();
        let profile = assets.add(fixture_profile());
        world.insert_resource(assets);

        let body = world
            .spawn((
                Npc(NpcId::unique(1)),
                NpcBlackboard::default(),
                NpcGestures::new(),
                Locomotion::default(),
                ClipBindings::new(
                    profile,
                    idle_node(),
                    AnimationNodeIndex::new(1),
                    AnimationNodeIndex::new(2),
                ),
                GestureClips::new().with(GestureKind::Wave, wave_node()),
                GesturePlayback::new(),
                AnimationPlayer::default(),
            ))
            .id();
        Fixture { world, body }
    }

    impl Fixture {
        /// Ask for a gesture the way a behaviour leaf does, then let the core adjudicate it.
        fn request(&mut self, kind: GestureKind) {
            if let Some(mut board) = self.world.get_mut::<NpcBlackboard>(self.body) {
                request_gesture(&mut board.0, kind);
            } else {
                unreachable!("the fixture body has a blackboard");
            }
            self.adjudicate();
        }

        /// Run the two core systems that turn a request into a cue.
        fn adjudicate(&mut self) {
            if self
                .world
                .run_system_once(collect_gesture_requests)
                .is_err()
                || self.world.run_system_once(advance_gestures).is_err()
            {
                unreachable!("the gesture channel runs on any world");
            }
        }

        fn run(&mut self) {
            if self.world.run_system_once(play_gestures).is_err() {
                unreachable!("the gesture playback runs on any world");
            }
        }

        fn cue(&self) -> Option<(GestureKind, f32, u32)> {
            self.world
                .get::<NpcGestures>(self.body)
                .and_then(NpcGestures::cue)
                .map(|cue| (cue.kind(), cue.weight(), cue.serial()))
        }
    }

    #[test]
    fn a_cue_starts_its_clip_once_and_writes_the_cue_s_weight() {
        let mut fixture = fixture();
        fixture.request(GestureKind::Wave);
        let Some((kind, weight, _)) = fixture.cue() else {
            unreachable!("the channel accepted a wave with an empty channel");
        };
        assert_eq!(kind, GestureKind::Wave);
        fixture.run();
        let written = weight_of(&fixture.world, fixture.body, wave_node());
        assert!(
            written.is_some_and(|value| (value - weight).abs() < 1.0e-6),
            "the clip weight was {written:?}, the cue said {weight}"
        );

        // A second tick of the same gesture must not restart it: one playing animation, same weight.
        fixture.adjudicate();
        fixture.run();
        let count = fixture
            .world
            .get::<AnimationPlayer>(fixture.body)
            .map(|player| player.playing_animations().count());
        assert_eq!(count, Some(1), "the clip was duplicated");
        assert!(
            fixture
                .world
                .get::<GesturePlayback>(fixture.body)
                .is_some_and(|playback| playback.current().is_some())
        );
    }

    #[test]
    fn the_clip_stops_when_the_cue_goes_away() {
        let mut fixture = fixture();
        fixture.request(GestureKind::Wave);
        fixture.run();
        assert!(weight_of(&fixture.world, fixture.body, wave_node()).is_some());

        // Run the core forward until the gesture is over: the cue disappears, and the playback stops
        // the clip rather than leaving it at its last weight forever.
        for _ in 0..64 {
            if fixture.cue().is_none() {
                break;
            }
            if let Some(mut clock) = fixture.world.get_resource_mut::<SimClock>() {
                clock.tick = clock.tick.next();
            }
            fixture.adjudicate();
        }
        assert!(fixture.cue().is_none(), "the gesture never finished");
        fixture.run();
        assert!(
            weight_of(&fixture.world, fixture.body, wave_node()).is_none(),
            "the gesture clip outlived its cue"
        );
        assert!(
            fixture
                .world
                .get::<GesturePlayback>(fixture.body)
                .is_some_and(|playback| playback.current().is_none())
        );
    }

    #[test]
    fn a_gesture_leaves_the_locomotion_weights_exactly_as_the_blend_wrote_them() {
        let mut fixture = fixture();
        // Walking, so the blend has something to write that is not the idle clip.
        if let Some(mut locomotion) = fixture.world.get_mut::<Locomotion>(fixture.body) {
            // Two metres a second, one tick's worth of it: a walk the blend will weight above idle.
            locomotion.advance(
                bevy_math::Vec2::new(0.0, 2.0),
                bevy_math::Vec2::new(0.0, 2.0),
                0.05,
            );
        }
        if fixture
            .world
            .run_system_once(apply_locomotion_blend)
            .is_err()
        {
            unreachable!("the blend runs on any world");
        }
        let before: Vec<_> = [
            idle_node(),
            AnimationNodeIndex::new(1),
            AnimationNodeIndex::new(2),
        ]
        .into_iter()
        .map(|node| (node, weight_of(&fixture.world, fixture.body, node)))
        .collect();

        fixture.request(GestureKind::Wave);
        fixture.run();
        fixture.run();
        // And after the gesture is over, the weights the blend owns are the ones it wrote: bit for
        // bit, because the gesture never had the handle.
        for _ in 0..64 {
            if fixture.cue().is_none() {
                break;
            }
            if let Some(mut clock) = fixture.world.get_resource_mut::<SimClock>() {
                clock.tick = clock.tick.next();
            }
            fixture.adjudicate();
        }
        fixture.run();
        for (node, weight) in before {
            assert_eq!(
                weight_of(&fixture.world, fixture.body, node),
                weight,
                "node {node:?} moved"
            );
        }
    }

    #[test]
    fn a_gesture_bound_to_a_locomotion_node_is_refused() {
        let mut fixture = fixture();
        // Bind the idle slot as if it were a wave. The playback must refuse it rather than fight the
        // blend for that node on every tick.
        if let Some(mut clips) = fixture.world.get_mut::<GestureClips>(fixture.body) {
            *clips = GestureClips::new().with(GestureKind::Wave, idle_node());
        }
        fixture.request(GestureKind::Wave);
        fixture.run();
        if fixture
            .world
            .run_system_once(apply_locomotion_blend)
            .is_err()
        {
            unreachable!("the blend runs on any world");
        }
        // Standing still: the idle clip is the blend's, at the blend's weight, with no gesture on it.
        let idle = weight_of(&fixture.world, fixture.body, idle_node());
        assert!(
            idle.is_some_and(|value| (value - 1.0).abs() < 1.0e-6),
            "the idle clip ended at {idle:?}"
        );
    }

    #[test]
    fn a_body_with_no_clip_for_the_gesture_plays_nothing() {
        let mut fixture = fixture();
        if let Some(mut clips) = fixture.world.get_mut::<GestureClips>(fixture.body) {
            *clips = GestureClips::new();
        }
        fixture.request(GestureKind::Bark);
        fixture.run();
        let count = fixture
            .world
            .get::<AnimationPlayer>(fixture.body)
            .map(|player| player.playing_animations().count());
        assert_eq!(count, Some(0));
    }

    #[test]
    fn clips_are_bound_per_kind_and_absent_ones_stay_absent() {
        let clips = GestureClips::new()
            .with(GestureKind::Nod, AnimationNodeIndex::new(3))
            .with(GestureKind::Flinch, AnimationNodeIndex::new(4));
        assert_eq!(
            clips.node(GestureKind::Nod),
            Some(AnimationNodeIndex::new(3))
        );
        assert_eq!(
            clips.node(GestureKind::Flinch),
            Some(AnimationNodeIndex::new(4))
        );
        assert_eq!(clips.node(GestureKind::Wave), None);
        assert_eq!(clips.node(GestureKind::Bark), None);
    }
}
