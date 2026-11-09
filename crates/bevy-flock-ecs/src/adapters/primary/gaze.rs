//! Where an NPC is looking, in the two pieces an animation layer can use.
//!
//! Looking at somebody is a decision and a pose, and this crate only owns the first. A cognition
//! system decides *which entity matters*; this module turns that entity into a point in the world
//! and a weight, and hands both to whoever owns the skeleton.
//!
//! The split is deliberate (ADR 0052). Querying the player's transform here would work in a demo and
//! fail the moment a game replaces perception with its own — a supported thing to do (ADR 0008) —
//! because "the player" is a name this crate does not have. So [`GazeTarget`] carries an [`Entity`]:
//! the one a cognition system already picked out of the stimulus set, or the player, or a lamp, or
//! a dog. [`resolve_gaze`] reads *that* entity's transform and writes nothing else.
//!
//! Two rules make the result a value rather than a smoothing filter:
//!
//! * **The clock is the tick.** Acquiring and dropping a target are integer counters compared with
//!   [`Tick::elapsed_at_least`], never a delta-time lerp. A replay gaze at 20 frames a second and at
//!   200 produces the same weights at the same ticks, which is the whole of the determinism claim.
//! * **Release ends at exactly zero.** The release ramp is `1 - ramp`, and the ramp saturates at one,
//!   so the tick the release window closes the weight is `0.0` — bitwise, not approximately. The
//!   pose side is then allowed to do nothing at all, and a joint that was never tracked and one that
//!   was tracked and released are the same bits.
//!
//! [`resolve_gaze`] runs in [`FlockSet::Integration`](crate::adapters::primary::plugin::FlockSet),
//! after [`derive_locomotion`](crate::adapters::primary::locomotion::derive_locomotion), so the aim a
//! game renders is computed from this tick's positions rather than last tick's.

use bevy_ecs::prelude::{Component, Entity, Query, Res, Resource};
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

use crate::application::SimClock;
use crate::domain::num;
use crate::domain::tick::Tick;

/// Ticks from a fresh target to full weight: nine, a sixth of a second at sixty a second.
pub const DEFAULT_RAMP_TICKS: u64 = 9;

/// Ticks from a dropped target back to the animated pose: twelve, a fifth of a second.
pub const DEFAULT_RELEASE_TICKS: u64 = 12;

/// How long a gaze takes to arrive and to leave, in ticks.
///
/// A resource rather than a constant on the component because it is one decision for the crowd, like
/// [`PerceptionSettings`](crate::adapters::primary::stimulus::PerceptionSettings), and not something
/// an individual NPC is authored with.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GazeConfig {
    ramp_ticks: u64,
    release_ticks: u64,
}

impl GazeConfig {
    /// A ramp and a release, both in ticks.
    ///
    /// Each is raised to at least one tick. A zero-length ramp is a division by zero wearing a
    /// policy hat, and a snap is what `ramp_ticks = 1` already buys.
    #[must_use]
    pub const fn new(ramp_ticks: u64, release_ticks: u64) -> Self {
        Self {
            ramp_ticks: if ramp_ticks == 0 { 1 } else { ramp_ticks },
            release_ticks: if release_ticks == 0 { 1 } else { release_ticks },
        }
    }

    /// Ticks from a fresh target to full weight.
    #[must_use]
    pub const fn ramp_ticks(&self) -> u64 {
        self.ramp_ticks
    }

    /// Ticks from a dropped target back to the animated pose.
    #[must_use]
    pub const fn release_ticks(&self) -> u64 {
        self.release_ticks
    }
}

impl Default for GazeConfig {
    fn default() -> Self {
        Self::new(DEFAULT_RAMP_TICKS, DEFAULT_RELEASE_TICKS)
    }
}

/// The entity cognition decided this body should look at, and how much it matters.
///
/// Written by whatever selects targets — a behaviour leaf, a utility scorer, a game's own
/// perception — and read by [`resolve_gaze`]. A body without the component is never resolved and
/// never animated, which is the right default: gaze is something a game opts into per NPC.
///
/// # Example
///
/// ```
/// use bevy_ecs::prelude::Entity;
/// use bevy_flock_core::application::SimClock;
/// use bevy_flock_core::domain::tick::Tick;
/// use bevy_flock_ecs::prelude::{GazeConfig, GazeTarget};
///
/// # let somebody = Entity::PLACEHOLDER;
/// let mut gaze = GazeTarget::new();
/// gaze.look_at(somebody, 1.0, Tick::ZERO);
/// // Weight ramps in over the configured ticks and is full at the end of them.
/// assert!(gaze.weight(Tick::ZERO, &GazeConfig::default()).abs() < f32::EPSILON);
/// assert!((gaze.weight(Tick::new(9), &GazeConfig::default()) - 1.0).abs() < f32::EPSILON);
///
/// // Dropping it decays to exactly zero, which is what lets the joints return untouched.
/// gaze.release(Tick::new(20));
/// assert!(gaze.weight(Tick::new(20 + 12), &GazeConfig::default()).abs() < f32::EPSILON);
/// ```
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct GazeTarget {
    target: Option<Entity>,
    interest: f32,
    acquired: Option<Tick>,
    released: Option<Tick>,
}

impl GazeTarget {
    /// A body that is looking at nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            target: None,
            interest: 0.0,
            acquired: None,
            released: None,
        }
    }

    /// Look at `target`, with `interest` in `0.0..=1.0`, starting now.
    ///
    /// Calling this again with the same entity keeps the current ramp — a cognition system that
    /// re-picks the same target every tick must not restart the fade, or the head never arrives.
    /// Calling it with a different entity restarts the ramp, because a body swinging from one
    /// person to another has genuinely changed its mind.
    ///
    /// An interest that is `NaN`, infinite or outside the range is clamped: it is a weight, and a
    /// weight outside `0.0..=1.0` would either scale a pose past its limits or invert it.
    pub fn look_at(&mut self, target: Entity, interest: f32, now: Tick) -> &mut Self {
        if self.target != Some(target) {
            self.acquired = Some(now);
        }
        self.target = Some(target);
        self.interest = clamp_interest(interest);
        self.released = None;
        self
    }

    /// Stop looking, starting the release ramp from `now`.
    ///
    /// The target is forgotten immediately so nothing depends on a despawned entity; the weight
    /// decays over [`GazeConfig::release_ticks`] because a head that snaps back to centre reads as
    /// a flinch.
    pub const fn release(&mut self, now: Tick) -> &mut Self {
        if self.target.is_some() && self.released.is_none() {
            self.released = Some(now);
        }
        self.target = None;
        self
    }

    /// The entity being looked at, if any.
    #[must_use]
    pub const fn target(&self) -> Option<Entity> {
        self.target
    }

    /// How much this target matters, in `0.0..=1.0`.
    #[must_use]
    pub const fn interest(&self) -> f32 {
        self.interest
    }

    /// Whether a target is currently held.
    #[must_use]
    pub const fn is_looking(&self) -> bool {
        self.target.is_some()
    }

    /// How much of the gaze to apply at `now`, in `0.0..=1.0`.
    ///
    /// Pure: the tick and the configuration go in, a number comes out, and nothing is stored. That
    /// is what lets a test assert the weight at tick 21 without having simulated ticks 0 to 20.
    #[must_use]
    pub fn weight(&self, now: Tick, config: &GazeConfig) -> f32 {
        let Some(acquired) = self.acquired else {
            return 0.0;
        };
        let arriving = ramp(now.since(acquired), config.ramp_ticks());
        let leaving = self
            .released
            .map_or(1.0, |at| 1.0 - ramp(now.since(at), config.release_ticks()));
        self.interest * arriving * leaving
    }
}

/// The point a body is looking at, and how much of the look to apply.
///
/// Written once per tick by [`resolve_gaze`], read by the animation layer. Kept separate from
/// [`GazeTarget`] because they answer different questions: that one is *what cognition decided*, this
/// one is *where that is in the world this tick*. A target that despawns leaves the last point here,
/// so a head eases off along the direction it was already facing rather than towards the origin.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct GazeAim {
    point: Vec3,
    weight: f32,
}

impl GazeAim {
    /// An aim at a point, at a weight.
    #[must_use]
    pub const fn new(point: Vec3, weight: f32) -> Self {
        Self { point, weight }
    }

    /// The world position being looked at.
    #[must_use]
    pub const fn point(&self) -> Vec3 {
        self.point
    }

    /// How much of the look to apply, in `0.0..=1.0`.
    #[must_use]
    pub const fn weight(&self) -> f32 {
        self.weight
    }

    /// Whether there is nothing to apply, which is the pose side's cue to write nothing at all.
    ///
    /// Written as an explicit `NaN` test rather than a negated comparison, because a `NaN` weight
    /// must read as idle (there is no look to apply) while still being visibly not-a-number to
    /// anyone debugging the pose.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.weight.is_nan() || self.weight <= 0.0
    }
}

/// Resolve every body's [`GazeTarget`] into a [`GazeAim`].
///
/// Reads the transform of the entity cognition named — never "the player", because this crate has
/// no such name — and writes only the aim. A target that has despawned, or an entity with no
/// transform at all, leaves the last known point in place and lets the weight do the work: the head
/// finishes its release towards where the thing used to be, which is what a person does.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn resolve_gaze(
    clock: Res<SimClock>,
    config: Res<GazeConfig>,
    mut bodies: Query<(&GazeTarget, &mut GazeAim)>,
    anchors: Query<&Transform>,
) {
    let now = clock.tick;
    for (target, mut aim) in &mut bodies {
        aim.weight = target.weight(now, &config);
        if let Some(entity) = target.target()
            && let Ok(anchor) = anchors.get(entity)
        {
            aim.point = anchor.translation;
        }
    }
}

/// The ramp from nothing to everything: linear in ticks, and exactly one at the end of them.
///
/// Integer in, float out, so a tick count cannot drift: ten of nineteen is the same number on every
/// machine, and fifty of nineteen saturates rather than overshooting.
fn ramp(elapsed: u64, ticks: u64) -> f32 {
    if elapsed >= ticks {
        return 1.0;
    }
    num::u64_to_f32(elapsed) / num::u64_to_f32(ticks)
}

/// An interest clamped into `0.0..=1.0`; anything unusable reads as no interest at all.
const fn clamp_interest(interest: f32) -> f32 {
    if interest.is_finite() {
        interest.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::{GazeAim, GazeConfig, GazeTarget, resolve_gaze};
    use crate::application::SimClock;
    use crate::domain::tick::Tick;
    use crate::testkit::OrDie;
    use bevy_ecs::prelude::{Entity, World};
    use bevy_ecs::system::RunSystemOnce;
    use bevy_math::Vec3;
    use bevy_transform::prelude::Transform;

    fn config() -> GazeConfig {
        GazeConfig::default()
    }

    #[test]
    fn a_body_looking_at_nothing_applies_nothing() {
        let gaze = GazeTarget::default();
        assert!(!gaze.is_looking());
        assert!(gaze.target().is_none());
        assert!(gaze.weight(Tick::ZERO, &config()).abs() < f32::EPSILON);
    }

    #[test]
    fn the_ramp_is_linear_and_arrives_on_the_configured_tick() {
        let mut gaze = GazeTarget::new();
        let somebody = Entity::PLACEHOLDER;
        gaze.look_at(somebody, 1.0, Tick::ZERO);
        let config = GazeConfig::new(10, 4);
        // Halfway through the ramp is halfway up it, to the float's own precision.
        let halfway = gaze.weight(Tick::new(5), &config);
        assert!((halfway - 0.5).abs() < 1e-6, "halfway was {halfway}");
        // And the tenth tick is full, not 0.9.
        assert!((gaze.weight(Tick::new(10), &config) - 1.0).abs() < f32::EPSILON);
        assert!((gaze.weight(Tick::new(10_000), &config) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn re_picking_the_same_target_does_not_restart_the_ramp() {
        let mut world = World::new();
        let somebody = world.spawn_empty().id();
        let somebody_else = world.spawn_empty().id();
        let mut gaze = GazeTarget::new();
        gaze.look_at(somebody, 1.0, Tick::ZERO);
        let config = config();
        // A cognition system that re-decides every tick must not hold the head at zero forever.
        for tick in 1..20_u64 {
            gaze.look_at(somebody, 1.0, Tick::new(tick));
        }
        assert!((gaze.weight(Tick::new(19), &config) - 1.0).abs() < f32::EPSILON);
        // A different target is a different decision: the ramp starts again, from zero.
        gaze.look_at(somebody_else, 1.0, Tick::new(19));
        assert!(gaze.weight(Tick::new(19), &config).abs() < f32::EPSILON);
        assert!((gaze.weight(Tick::new(39), &config) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn releasing_reaches_exactly_zero_and_stays_there() {
        let mut gaze = GazeTarget::new();
        gaze.look_at(Entity::PLACEHOLDER, 1.0, Tick::ZERO);
        let config = GazeConfig::new(1, 12);
        gaze.release(Tick::new(5));
        assert!(!gaze.is_looking());
        // One tick before the window closes is still something; the tick it closes is nothing.
        assert!(gaze.weight(Tick::new(16), &config) > 0.0);
        let closed = gaze.weight(Tick::new(17), &config);
        // Bit equality, not tolerance: the pose side is allowed to skip writing on this value.
        assert_eq!(closed.to_bits(), 0.0_f32.to_bits());
        assert_eq!(
            gaze.weight(Tick::new(97), &config).to_bits(),
            0.0_f32.to_bits()
        );
    }

    #[test]
    fn releasing_twice_does_not_restart_the_window() {
        let mut gaze = GazeTarget::new();
        gaze.look_at(Entity::PLACEHOLDER, 1.0, Tick::ZERO);
        gaze.release(Tick::new(5));
        gaze.release(Tick::new(9));
        // The window is measured from the first release, so the head does not stall at nine tenths.
        let weight = gaze.weight(Tick::new(17), &GazeConfig::new(1, 12));
        assert_eq!(weight.to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn interest_is_clamped_because_it_is_a_weight() {
        let mut gaze = GazeTarget::new();
        let somebody = Entity::PLACEHOLDER;
        gaze.look_at(somebody, 4.0, Tick::ZERO);
        assert!((gaze.interest() - 1.0).abs() < f32::EPSILON);
        gaze.look_at(somebody, -2.0, Tick::ZERO);
        assert!(gaze.interest().abs() < f32::EPSILON);
        gaze.look_at(somebody, f32::NAN, Tick::ZERO);
        assert!(gaze.interest().abs() < f32::EPSILON);
        assert!(gaze.weight(Tick::new(100), &config()).abs() < f32::EPSILON);
    }

    #[test]
    fn a_zero_length_ramp_is_raised_to_one_tick() {
        let config = GazeConfig::new(0, 0);
        assert_eq!(config.ramp_ticks(), 1);
        assert_eq!(config.release_ticks(), 1);
        // And it means a snap rather than a division by zero.
        let mut gaze = GazeTarget::new();
        gaze.look_at(Entity::PLACEHOLDER, 1.0, Tick::ZERO);
        assert!(gaze.weight(Tick::ZERO, &config).abs() < f32::EPSILON);
        assert!((gaze.weight(Tick::new(1), &config) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn an_idle_aim_applies_nothing() {
        assert!(GazeAim::default().is_idle());
        let aim = GazeAim::new(Vec3::ZERO, 0.25);
        assert!(!aim.is_idle());
        assert!((aim.weight() - 0.25).abs() < f32::EPSILON);
        // A weight nobody can satisfy reads as idle rather than as a look.
        assert!(GazeAim::new(Vec3::ZERO, f32::NAN).is_idle());
    }

    #[test]
    fn resolving_writes_this_ticks_point_and_keeps_the_last_one_after_a_despawn() {
        let mut world = World::new();
        world.insert_resource(SimClock::default());
        world.insert_resource(GazeConfig::new(1, 2));

        let anchor = world
            .spawn(Transform::from_translation(Vec3::new(0.0, 0.0, 8.0)))
            .id();
        let body = world.spawn((GazeAim::default(), GazeTarget::new())).id();

        // Looking at something eight metres away.
        if let Some(mut gaze) = world.get_mut::<GazeTarget>(body) {
            gaze.look_at(anchor, 1.0, Tick::ZERO);
        }
        world.run_system_once(resolve_gaze).or_die();
        let aim = *world.get::<GazeAim>(body).or_die();
        assert!((aim.point() - Vec3::new(0.0, 0.0, 8.0)).length() < 1e-6);
        // The acquisition tick is the bottom of a one-tick ramp, not the top of it.
        assert!(aim.weight().abs() < f32::EPSILON);
        // A tick later the ramp is complete, and the weight is unity.
        if let Some(mut clock) = world.get_resource_mut::<SimClock>() {
            clock.tick = Tick::new(1);
        }
        world.run_system_once(resolve_gaze).or_die();
        let aim = *world.get::<GazeAim>(body).or_die();
        assert!((aim.weight() - 1.0).abs() < f32::EPSILON);

        // The anchor moves; the aim follows it the same tick.
        if let Some(mut transform) = world.get_mut::<Transform>(anchor) {
            transform.translation = Vec3::new(4.0, 0.0, 8.0);
        }
        world.run_system_once(resolve_gaze).or_die();
        let aim = *world.get::<GazeAim>(body).or_die();
        assert!((aim.point() - Vec3::new(4.0, 0.0, 8.0)).length() < 1e-6);

        // The anchor despawns: the point stays where it was and the weight decays to zero, so the
        // head eases off instead of snapping to the origin.
        world.despawn(anchor);
        if let Some(mut gaze) = world.get_mut::<GazeTarget>(body) {
            gaze.release(Tick::ZERO);
        }
        world.run_system_once(resolve_gaze).or_die();
        let aim = *world.get::<GazeAim>(body).or_die();
        assert!((aim.point() - Vec3::new(4.0, 0.0, 8.0)).length() < 1e-6);
        assert!(aim.weight() > 0.0);

        world.resource_mut::<SimClock>().advance();
        world.resource_mut::<SimClock>().advance();
        world.run_system_once(resolve_gaze).or_die();
        assert!(world.get::<GazeAim>(body).or_die().is_idle());
    }

    #[test]
    fn the_weight_does_not_depend_on_how_many_times_it_was_asked() {
        // The pure function is the point: asking twice changes nothing, so a system that resolves
        // before and after something else cannot leave a different answer behind.
        let mut gaze = GazeTarget::new();
        gaze.look_at(Entity::PLACEHOLDER, 0.4, Tick::ZERO);
        let config = config();
        let first = gaze.weight(Tick::new(4), &config);
        for _ in 0..10 {
            assert_eq!(
                gaze.weight(Tick::new(4), &config).to_bits(),
                first.to_bits()
            );
        }
    }
}
