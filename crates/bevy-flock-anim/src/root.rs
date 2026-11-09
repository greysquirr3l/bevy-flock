//! Who moves the body when a clip carries its own translation.
//!
//! A walk cycle that translates its root is a rigging convenience: the animator moves the hips, the
//! engine is supposed to take the hint and drop the translation. Get it wrong in either direction and
//! the bug is the same one — the body travels twice as far as its legs, or the legs cycle in place
//! while the steering slides it.
//!
//! So the choice is explicit, per profile, and the default is the one that keeps a deterministic
//! replay intact:
//!
//! * [`crate::profile::RootMotion::Simulation`] — the simulation owns movement. The clip's translation is **dropped**
//!   on the tick it arrives, so a game that ships a root-motion clip without opting in gets a body
//!   that walks where the steering put it, not twice as far.
//! * [`crate::profile::RootMotion::Clip`] — the clip owns movement. The translation is quantised to
//!   [`QUANTUM_METRES`], added to the body's transform, and added to the locomotion's distance
//!   walked, so the next tick's steering starts from where the clip left the body instead of
//!   dragging it back.
//!
//! The quantisation is the determinism guard. A clip's root channel is `f32` and the sum of a
//! thousand of them is a rounding question; snapping each tick's offset to a micrometre means two
//! runs of the same clips produce the same transform bit for bit, which is what a replay digest
//! compares.
//!
//! Extraction itself stays outside this crate: Bevy owns the skeleton, so the game's animation
//! bridge reads the root joint and calls [`RootOffset::report`]. What this module owns is the
//! decision — apply once, or drop — and the trace that proves which happened.
use bevy_flock_core::domain::num;
use bevy_flock_ecs::prelude::Locomotion;
use bevy_ecs::prelude::{Component, Query, Res, ResMut, Resource};
use bevy_math::{Vec2, Vec3};
use bevy_transform::prelude::Transform;

use crate::binding::ClipBindings;
use bevy_asset::Assets;

use crate::profile::LocomotionProfile;

/// The finest translation a clip can contribute in one tick: one micrometre.
///
/// Small enough to be invisible (a micrometre a tick is 6 cm an hour) and coarse enough that an
/// `f32` sum of a hundred thousand of them stays exact in the range a plaza covers.
pub const QUANTUM_METRES: f32 = 1.0e-6;

/// One tick's worth of a clip's own translation, in world metres.
///
/// The game's animation bridge reports it; [`apply_root_motion`] consumes it. Reporting again before
/// the tick is consumed replaces the value rather than adding to it, because a bridge that runs
/// twice in a frame is a bug this should not average away.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct RootOffset {
    planar: Vec2,
    reported: bool,
}

impl RootOffset {
    /// Report this tick's translation, snapped to the quantum.
    ///
    /// Only the horizontal plane is kept: a clip that lifts its root — a jump, a stumble — does not
    /// get to move the body vertically through the simulation, which has no opinion about floors.
    pub fn report(&mut self, translation: Vec3) {
        self.planar = Vec2::new(quantise(translation.x), quantise(translation.z));
        self.reported = true;
    }

    /// The offset waiting to be consumed, if any.
    #[must_use]
    pub const fn pending(&self) -> Option<Vec2> {
        if self.reported {
            Some(self.planar)
        } else {
            None
        }
    }

    /// Consume the offset, leaving the component empty for the next tick.
    pub const fn take(&mut self) -> Option<Vec2> {
        let pending = self.pending();
        self.reported = false;
        self.planar = Vec2::ZERO;
        pending
    }
}

/// Snap a distance to the nearest micrometre.
#[must_use]
pub fn quantise(metres: f32) -> f32 {
    if !metres.is_finite() {
        return 0.0;
    }
    // Micrometres as an integer, so the division that follows is exact in binary.
    let steps = num::f32_to_i64((metres / QUANTUM_METRES).round());
    num::i64_to_f32(steps) * QUANTUM_METRES
}

/// What the reconciler has done this run, in a form a replay can compare.
///
/// The digest is FNV-1a over every applied offset's micrometre pair, so two runs that moved their
/// bodies by the same clips in the same ticks agree, and a run that applied one extra offset does
/// not. Discarded offsets are counted separately: a game in simulation mode should see a non-zero
/// `discarded` and a zero `digest`, which is the shape of "the clips offered motion and we refused".
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootMotionTrace {
    applied: u64,
    discarded: u64,
    digest: u64,
}

impl Default for RootMotionTrace {
    /// An empty trace: nothing applied, nothing discarded, and the digest at its seed.
    fn default() -> Self {
        Self {
            applied: 0,
            discarded: 0,
            digest: Self::SEED,
        }
    }
}

impl RootMotionTrace {
    /// The FNV-1a offset basis, so an empty run has a defined digest.
    pub const SEED: u64 = 0xcbf2_9ce4_8422_2325;
    /// The FNV-1a prime.
    pub const PRIME: u64 = 0x0000_0100_0000_01b3;

    /// Offsets this run applied to a transform.
    #[must_use]
    pub const fn applied(&self) -> u64 {
        self.applied
    }

    /// Offsets this run dropped because the simulation owns movement.
    #[must_use]
    pub const fn discarded(&self) -> u64 {
        self.discarded
    }

    /// The digest of the applied offsets, seeded so that "nothing applied" is [`Self::SEED`].
    #[must_use]
    pub const fn digest(&self) -> u64 {
        self.digest
    }

    fn record_applied(&mut self, offset: Vec2) {
        self.applied = self.applied.saturating_add(1);
        for micrometres in [
            num::f32_to_i64((offset.x / QUANTUM_METRES).round()),
            num::f32_to_i64((offset.y / QUANTUM_METRES).round()),
        ] {
            for byte in micrometres.to_le_bytes() {
                self.digest ^= u64::from(byte);
                self.digest = self.digest.wrapping_mul(Self::PRIME);
            }
        }
    }

    const fn record_discarded(&mut self) {
        self.discarded = self.discarded.saturating_add(1);
    }
}

/// Apply the clip's translation if the profile says the clip owns movement, and drop it otherwise.
///
/// Runs once per tick after the blend, so the offset a bridge reported this tick is consumed this
/// tick. Consuming it — rather than reading it — is what stops a body moving twice when a game leaves
/// the bridge writing while the simulation also integrates: the offset exists for exactly one tick.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value"
)]
pub fn apply_root_motion(
    profiles: Res<Assets<LocomotionProfile>>,
    mut bodies: Query<(
        &ClipBindings,
        &mut RootOffset,
        &mut Transform,
        &mut Locomotion,
    )>,
    mut trace: ResMut<RootMotionTrace>,
) {
    for (bindings, mut offset, mut transform, mut motion) in &mut bodies {
        // Taken first, so an unbound body still clears its bridge's report instead of carrying it.
        let Some(planar) = offset.take() else {
            continue;
        };
        let Some(profile) = profiles.get(bindings.profile()) else {
            continue;
        };
        if !profile.root_motion().extracts() {
            trace.record_discarded();
            continue;
        }
        transform.translation.x += planar.x;
        transform.translation.z += planar.y;
        // The clip moved the body, so the walk phase has to know about the distance: without this the
        // next tick's blend would put the legs back where the steering thought they were.
        motion.apply_root_offset(planar);
        trace.record_applied(planar);
    }
}

#[cfg(test)]
mod tests {
    use super::{QUANTUM_METRES, RootMotionTrace, RootOffset, apply_root_motion, quantise};
    use crate::binding::ClipBindings;
    use crate::profile::LocomotionProfile;
    use bevy_flock_core::domain::num;
    use bevy_flock_ecs::prelude::Locomotion;
    use bevy_animation::graph::AnimationNodeIndex;
    use bevy_asset::Assets;
    use bevy_ecs::prelude::{Entity, World};
    use bevy_ecs::system::RunSystemOnce;
    use bevy_math::{Vec2, Vec3};
    use bevy_transform::prelude::Transform;

    fn index(raw: usize) -> AnimationNodeIndex {
        AnimationNodeIndex::new(raw)
    }

    fn world(extracts: bool) -> (World, Entity) {
        let mut world = World::new();
        let mut assets = Assets::<LocomotionProfile>::default();
        let mode = if extracts {
            crate::profile::RootMotion::Clip
        } else {
            crate::profile::RootMotion::Simulation
        };
        let profile = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0).map_or_else(
            |_| unreachable!("the fixture profile is valid"),
            |profile| profile.with_root_motion(mode),
        );
        let handle = assets.add(profile);
        world.insert_resource(assets);
        world.insert_resource(RootMotionTrace {
            digest: RootMotionTrace::SEED,
            ..RootMotionTrace::default()
        });
        let nodes = [index(0), index(1), index(2), index(3)];
        let bindings =
            ClipBindings::new(handle, nodes[0], nodes[1], nodes[2]).with_turn_in_place(nodes[3]);
        let entity = world
            .spawn((
                Locomotion::default(),
                Transform::default(),
                RootOffset::default(),
                bindings,
            ))
            .id();
        (world, entity)
    }

    fn run(world: &mut World) {
        world.run_system_once(apply_root_motion).ok();
    }

    fn report(world: &mut World, entity: Entity, translation: Vec3) {
        if let Some(mut offset) = world.get_mut::<RootOffset>(entity) {
            offset.report(translation);
        }
    }

    fn transform_of(world: &World, entity: Entity) -> Transform {
        world.get::<Transform>(entity).copied().unwrap_or_default()
    }

    fn trace(world: &World) -> RootMotionTrace {
        world
            .get_resource::<RootMotionTrace>()
            .copied()
            .unwrap_or_default()
    }

    #[test]
    fn the_default_is_the_simulation_owning_movement() {
        let profile = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)
            .unwrap_or_else(|_| unreachable!("the fixture profile is valid"));
        assert_eq!(
            profile.root_motion(),
            crate::profile::RootMotion::Simulation
        );
        assert!(!profile.root_motion().extracts());
    }

    #[test]
    fn simulation_mode_drops_the_offset_and_leaves_the_body_where_steering_put_it() {
        let (mut world, entity) = world(false);
        report(&mut world, entity, Vec3::new(0.4, 0.0, -0.2));
        run(&mut world);
        let transform = transform_of(&world, entity);
        assert!(
            transform.translation.abs_diff_eq(Vec3::ZERO, f32::EPSILON),
            "the body moved on its own: {:?}",
            transform.translation
        );
        assert_eq!(trace(&world).applied(), 0);
        assert_eq!(trace(&world).discarded(), 1);
        assert_eq!(
            trace(&world).digest(),
            RootMotionTrace::SEED,
            "nothing applied"
        );
    }

    #[test]
    fn clip_mode_applies_the_offset_once_and_tells_the_walk_phase_about_it() {
        let (mut world, entity) = world(true);
        report(&mut world, entity, Vec3::new(0.4, 0.9, -0.2));
        run(&mut world);
        let transform = transform_of(&world, entity);
        assert!((transform.translation.x - 0.4).abs() < QUANTUM_METRES);
        assert!(
            transform.translation.y.abs() < f32::EPSILON,
            "no vertical motion"
        );
        assert!((transform.translation.z + 0.2).abs() < QUANTUM_METRES);
        // A second pass consumes nothing: the offset is gone rather than applied again.
        run(&mut world);
        let again = transform_of(&world, entity);
        assert!(
            (again.translation.x - 0.4).abs() < QUANTUM_METRES,
            "applied twice"
        );
        assert_eq!(trace(&world).applied(), 1);
        let walked = world
            .get::<Locomotion>(entity)
            .map(Locomotion::distance_metres)
            .unwrap_or_default();
        let expected = f64::from(Vec2::new(0.4_f32, -0.2).length());
        assert!(
            (walked - expected).abs() < 1e-5,
            "the walk phase learned {walked} m, expected {expected}"
        );
    }

    #[test]
    fn two_runs_of_the_same_clips_produce_the_same_transform_and_digest() {
        let mut runs = Vec::new();
        for _ in 0..2 {
            let (mut world, entity) = world(true);
            for tick in 0..240 {
                // A root channel that is not a tidy number, which is the case that catches a missing
                // quantisation: the sum of 240 inexact f32 steps would drift between runs.
                let x = 0.017_3 + num::u32_to_f32(tick) * 1e-4;
                report(&mut world, entity, Vec3::new(x, 0.0, -x / 3.0));
                run(&mut world);
            }
            let translation = transform_of(&world, entity).translation;
            runs.push((
                translation.x.to_bits(),
                translation.z.to_bits(),
                trace(&world).digest(),
            ));
        }
        assert_eq!(runs.first(), runs.last(), "two runs disagreed");
        let (mut control, control_body) = world(true);
        report(&mut control, control_body, Vec3::new(0.017_3, 0.0, 0.0));
        run(&mut control);
        assert_ne!(
            trace(&control).digest(),
            RootMotionTrace::SEED,
            "the trace is live"
        );
    }

    #[test]
    fn the_quantum_is_a_micrometre_and_nonsense_is_zero() {
        assert!((quantise(0.123_456_7) - 0.123_457).abs() < 1e-9);
        assert!(quantise(1.0e-9).abs() < f32::EPSILON, "below the quantum");
        assert!(quantise(f32::NAN).abs() < f32::EPSILON);
        assert!(quantise(f32::INFINITY).abs() < f32::EPSILON);
    }

    #[test]
    fn a_report_replaces_rather_than_accumulates_within_a_tick() {
        let mut offset = RootOffset::default();
        offset.report(Vec3::new(0.5, 0.0, 0.0));
        offset.report(Vec3::new(0.25, 0.0, 0.0));
        assert_eq!(offset.pending(), Some(Vec2::new(0.25, 0.0)));
        assert_eq!(offset.take(), Some(Vec2::new(0.25, 0.0)));
        assert_eq!(offset.pending(), None);
    }
}
