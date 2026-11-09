//! What the body is doing, in the terms an animation layer needs.
//!
//! The steering solution knows a velocity and a desire. An animation layer needs something else:
//! how fast the body is travelling, how hard it is turning, how hard it is accelerating, and how far
//! it has come. This module derives those four facts once per tick and publishes them as
//! [`Locomotion`], so that neither the anim crate nor a game has to reinterpret velocity and hope it
//! matches what the renderer drew.
//!
//! Two rules make the numbers usable rather than merely present:
//!
//! * **No wall clock.** The derivation takes the tick's fixed delta from [`FlockConfig`], never
//!   `Time<Virtual>`. A replay at a different frame rate computes the same speed, the same turn rate
//!   and the same distance, which is what lets an animation replay at all (ADR 0052).
//! * **No unit vectors from zero.** A body below [`MIN_MOVING_SPEED`] has no travel direction, so
//!   the turn rate falls back to the direction it has been *asked* to take. That covers the standing
//!   start — the case where a body pivots before it walks — without inventing a heading out of a
//!   zero-length vector.
//!
//! [`derive_locomotion`] runs at the end of
//! [`FlockSet::Integration`](crate::adapters::primary::plugin::FlockSet), after
//! [`integrate_flock`](crate::boids::integrate_flock) has written this tick's velocity, and it takes
//! its delta from the same clock that system uses — so the two can never disagree about how long a
//! tick was. A game ordering its own animation work after `Integration` sees a `Locomotion` that
//! describes the tick it is drawing.

use bevy_ecs::prelude::{Component, Query, Res};
use bevy_math::Vec2;

use crate::adapters::primary::components::{NpcDesiredVelocity, NpcVelocity};
use crate::application::SimClock;
use crate::domain::num;
use crate::domain::tick::TicksPerSecond;

/// Below this planar speed, a body has no travel direction of its own.
///
/// Five centimetres a second: slower than a person shuffles, fast enough that float noise in a
/// resting body does not register as a direction.
pub const MIN_MOVING_SPEED: f32 = 0.05;

/// How a body is moving this tick, in the units an animation layer blends with.
///
/// Attached to every NPC by [`NpcBundle`](crate::adapters::primary::components::NpcBundle); written
/// once per tick by [`derive_locomotion`]; read by the anim crate and by anything else that wants to
/// know how fast the crowd is going without re-deriving it from velocity.
///
/// # Example
///
/// ```
/// use bevy_flock::prelude::Locomotion;
/// use bevy_math::Vec2;
///
/// // A body walking at 1.4 m/s for a tenth of a second.
/// let mut motion = Locomotion::default();
/// motion.advance(Vec2::new(0.0, 1.4), Vec2::new(0.0, 1.4), 0.1);
/// assert!((motion.speed() - 1.4).abs() < 1e-5);
/// assert!((motion.distance_metres() - 0.14).abs() < 1e-4);
///
/// // One 1.5 m stride later, the phase is back where it started.
/// motion.advance(Vec2::new(0.0, 1.4), Vec2::new(0.0, 1.4), 1.0);
/// assert!((motion.phase(1.5) - 0.026_666_7).abs() < 1e-4);
/// ```
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct Locomotion {
    /// Planar speed in metres per second: forward motion, ignoring any vertical component.
    speed: f32,
    /// Signed yaw rate in radians per second, positive to the body's left in a right-handed frame.
    turn_rate: f32,
    /// Change in planar speed, in metres per second squared.
    acceleration: f32,
    /// Ground covered since spawn, in metres.
    ///
    /// `f64` on purpose: an NPC in a long session covers tens of kilometres, and `f32` loses
    /// millimetres by the time it has, which makes a stride phase visibly drift.
    distance: f64,
    /// The last non-degenerate travel direction, as a unit vector in the ground plane.
    heading: Vec2,
    /// The speed the previous tick reported, for the acceleration term.
    previous_speed: f32,
}

impl Locomotion {
    /// Planar speed in metres per second.
    pub const fn speed(&self) -> f32 {
        self.speed
    }

    /// Signed yaw rate in radians per second: positive is a turn to the body's left.
    pub const fn turn_rate(&self) -> f32 {
        self.turn_rate
    }

    /// Change in planar speed over the tick, in metres per second squared.
    pub const fn acceleration(&self) -> f32 {
        self.acceleration
    }

    /// Ground covered since the body was spawned, in metres.
    pub const fn distance_metres(&self) -> f64 {
        self.distance
    }

    /// How far into its gait cycle the body is, as a fraction in `0.0..1.0`.
    ///
    /// The core does not know a stride length — the game's artwork does — so it reports distance and
    /// lets the animation layer divide. A non-positive stride has no cycle, and reports `0.0` rather
    /// than a division by zero.
    pub fn phase(&self, stride_metres: f32) -> f32 {
        if stride_metres <= 0.0 {
            return 0.0;
        }
        let cycles = self.distance / f64::from(stride_metres);
        num::f64_to_f32(cycles - cycles.floor())
    }

    /// Whether the body is travelling fast enough to have a direction of its own.
    pub fn is_moving(&self) -> bool {
        self.speed > MIN_MOVING_SPEED
    }

    /// Fold one tick of motion into the component.
    ///
    /// `velocity` is the body's velocity after integration and `desired` the velocity steering asked
    /// for; `delta_seconds` is the fixed tick length. Called by [`derive_locomotion`] once per tick,
    /// and public because a game that integrates motion itself — a kinematic controller, a physics
    /// engine — has to be able to feed the same numbers in.
    ///
    /// The turn rate is the signed angle between the previous travel direction and this tick's,
    /// divided by the tick length, so a one-hundred-and-eighty-degree reversal reports a half-turn
    /// rather than an ambiguity, and a body standing still reports the turn it is being asked to
    /// take rather than zero.
    pub fn advance(&mut self, velocity: Vec2, desired: Vec2, delta_seconds: f32) -> &mut Self {
        let speed = velocity.length();
        let searched = if speed > MIN_MOVING_SPEED {
            velocity
        } else {
            desired
        };

        if searched.length() > MIN_MOVING_SPEED {
            let direction = searched.normalize();
            // The first direction a body ever takes is not a turn: there is nothing to have turned
            // from, and reporting one would make every spawn look like a flinch.
            self.turn_rate = if self.heading.length_squared() > 0.0 {
                signed_angle(self.heading, direction) / delta_seconds
            } else {
                0.0
            };
            self.heading = direction;
        } else {
            self.turn_rate = 0.0;
        }

        self.acceleration = (speed - self.previous_speed) / delta_seconds;
        self.previous_speed = speed;
        // Distance is accumulated from the motion the body actually has, not from what it wanted.
        // An NPC pushed into a wall travels nothing, and its stride phase should agree.
        self.distance += f64::from(speed) * f64::from(delta_seconds);
        self.speed = speed;
        self
    }
}

/// The signed angle from `from` to `to`, in `-pi..=pi`.
///
/// `libm::atan2f` rather than `f32::atan2`: the latter delegates to the platform's libm and differs
/// in the last bits between operating systems, which is the difference between a replay and a
/// near-replay (ADR 0004).
fn signed_angle(from: Vec2, to: Vec2) -> f32 {
    let cross = from.x * to.y - from.y * to.x;
    let dot = from.dot(to);
    libm::atan2f(cross, dot)
}

/// Derive [`Locomotion`] for every NPC from this tick's velocity and desire.
///
/// Writes only `Locomotion`. Reads `NpcVelocity`, `NpcDesiredVelocity` and the clock, using the
/// same expression as [`integrate_flock`](crate::boids::integrate_flock): a tick that integration
/// treated as one sixtieth of a second is one sixtieth of a second here too, whether the rate came
/// from a [`SimClock`] or from the default.
pub fn derive_locomotion(
    clock: Option<Res<SimClock>>,
    mut bodies: Query<(&NpcVelocity, &NpcDesiredVelocity, &mut Locomotion)>,
) {
    let delta_seconds = clock.map_or_else(
        || TicksPerSecond::SIXTY.delta_seconds(),
        |clock| clock.delta_seconds(),
    );
    for (velocity, desired, mut motion) in &mut bodies {
        motion.advance(velocity.planar(), desired.planar(), delta_seconds);
    }
}

#[cfg(test)]
mod tests {
    use super::{Locomotion, MIN_MOVING_SPEED, signed_angle};
    use bevy_math::Vec2;

    const TICK: f32 = 1.0 / 60.0;

    #[test]
    fn a_resting_body_reports_nothing() {
        let motion = Locomotion::default();
        assert!(motion.speed().abs() < f32::EPSILON);
        assert!(motion.turn_rate().abs() < f32::EPSILON);
        assert!(motion.acceleration().abs() < f32::EPSILON);
        assert!(motion.distance_metres().abs() < f64::EPSILON);
        assert!(!motion.is_moving());
    }

    #[test]
    fn a_straight_walk_reports_its_speed_exactly() {
        let mut motion = Locomotion::default();
        let walk = Vec2::new(0.0, 1.4);
        for _ in 0..60 {
            motion.advance(walk, walk, TICK);
        }
        assert!(
            (motion.speed() - 1.4).abs() < 1e-4,
            "speed {}",
            motion.speed()
        );
        assert!(
            motion.turn_rate().abs() < 1e-4,
            "turn {}",
            motion.turn_rate()
        );
        // Settled speed has settled acceleration.
        assert!(motion.acceleration().abs() < 1e-3);
        assert!((motion.distance_metres() - 1.4_f64 * f64::from(60.0 * TICK)).abs() < 1e-3);
    }

    #[test]
    fn the_stride_phase_advances_with_distance_and_wraps() {
        let mut motion = Locomotion::default();
        let mut previous = motion.phase(1.5);
        // Two full strides, in 3 m/s steps of a tenth of a second.
        for _ in 0..10 {
            motion.advance(Vec2::new(0.0, 3.0), Vec2::new(0.0, 3.0), 0.1);
            let phase = motion.phase(1.5);
            assert!((0.0..1.0).contains(&phase), "phase {phase} left its range");
            // Every step is 0.3 m, a fifth of a stride, so the phase moves by a fifth modulo one.
            let expected = (previous + 0.2) % 1.0;
            assert!(
                (phase - expected).abs() < 1e-4,
                "phase {phase} != {expected}"
            );
            previous = phase;
        }
        // A body that never moved has no phase at all.
        assert!(Locomotion::default().phase(1.5).abs() < f32::EPSILON);
        // A nonsensical stride reports zero rather than dividing by it.
        assert!(motion.phase(0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_standing_start_accelerates_then_settles() {
        let mut motion = Locomotion::default();
        let walk = Vec2::new(0.0, 1.4);
        motion.advance(walk, walk, TICK);
        assert!((motion.acceleration() - 1.4 / TICK).abs() < 1.0);
        motion.advance(walk, walk, TICK);
        assert!(motion.acceleration().abs() < 1e-3);
    }

    #[test]
    fn a_ninety_degree_turn_peaks_on_the_tick_it_happens() {
        let mut motion = Locomotion::default();
        let north = Vec2::new(0.0, 1.4);
        let east = Vec2::new(1.4, 0.0);
        // Walk north for ten ticks.
        for _ in 0..10 {
            motion.advance(north, north, TICK);
        }
        assert!(motion.turn_rate().abs() < 1e-4);
        // Turn east: a quarter turn clockwise in this frame, so the rate is negative.
        motion.advance(east, east, TICK);
        let expected = -std::f32::consts::FRAC_PI_2 / TICK;
        assert!(
            (motion.turn_rate() - expected).abs() < 1e-2,
            "turn {} != {expected}",
            motion.turn_rate()
        );
        // And the tick after, the new heading is the reference: no turn at all.
        motion.advance(east, east, TICK);
        assert!(motion.turn_rate().abs() < 1e-4);
    }

    #[test]
    fn a_reversal_is_a_half_turn_and_not_a_full_one() {
        // The signed angle picks the short way round, so 180 degrees never reads as -360.
        let north = Vec2::new(0.0, 1.0);
        let south = Vec2::new(0.0, -1.0);
        let angle = signed_angle(north, south);
        assert!((angle.abs() - std::f32::consts::PI).abs() < 1e-5);
    }

    #[test]
    fn a_standing_body_turns_towards_what_it_is_asked_for() {
        let mut motion = Locomotion::default();
        // Standing still, asked to move east: the body is turning on the spot.
        motion.advance(Vec2::ZERO, Vec2::new(1.0, 0.0), TICK);
        assert!(!motion.is_moving());
        assert!(
            motion.turn_rate().abs() < f32::EPSILON,
            "first direction is not a turn"
        );
        // Now asked to move north instead: that is a quarter turn to the left.
        motion.advance(Vec2::ZERO, Vec2::new(0.0, 1.0), TICK);
        let expected = std::f32::consts::FRAC_PI_2 / TICK;
        assert!(
            (motion.turn_rate() - expected).abs() < 1e-2,
            "turn {} != {expected}",
            motion.turn_rate()
        );
        // Asked for nothing, the body stops turning rather than spinning at the last rate.
        motion.advance(Vec2::ZERO, Vec2::ZERO, TICK);
        assert!(motion.turn_rate().abs() < f32::EPSILON);
    }

    #[test]
    fn a_body_below_the_moving_threshold_has_no_speed() {
        let mut motion = Locomotion::default();
        let crawl = Vec2::new(MIN_MOVING_SPEED * 0.5, 0.0);
        motion.advance(crawl, crawl, TICK);
        assert!(!motion.is_moving());
        // It still moved, and the distance says so.
        assert!(motion.distance_metres() > 0.0);
    }

    #[test]
    fn the_system_takes_its_delta_from_the_clock_not_from_a_constant() {
        use bevy_ecs::system::RunSystemOnce;
        use bevy_ecs::world::World;
        use bevy_math::Vec3;

        use crate::adapters::primary::components::{NpcDesiredVelocity, NpcVelocity};
        use crate::application::SimClock;
        use crate::domain::tick::TicksPerSecond;
        use crate::testkit::OrDie;

        let mut world = World::new();
        // Thirty ticks a second, so a quarter turn in one tick is a turn rate of pi/2 * 30.
        let thirty = TicksPerSecond::try_new(30).or_die();
        world.insert_resource(SimClock::new(thirty));
        let entity = world
            .spawn((
                NpcVelocity(Vec3::new(0.0, 0.0, -1.4)),
                NpcDesiredVelocity(Vec3::new(0.0, 0.0, -1.4)),
                Locomotion::default(),
            ))
            .id();

        world.run_system_once(super::derive_locomotion).or_die();
        let resting = *world.get::<Locomotion>(entity).or_die();
        assert!((resting.speed() - 1.4).abs() < 1e-4);
        assert!(resting.turn_rate().abs() < 1e-4);

        // Turn the body a quarter turn: the velocity is what a system downstream would have
        // written, and the rate has to be divided by the clock's delta rather than by 1/60.
        *world.get_mut::<NpcVelocity>(entity).or_die() = NpcVelocity(Vec3::new(1.4, 0.0, 0.0));
        world.run_system_once(super::derive_locomotion).or_die();
        let turned = *world.get::<Locomotion>(entity).or_die();
        // From heading -Z to heading +X is a quarter turn counter-clockwise in the xz plane.
        let expected = std::f32::consts::FRAC_PI_2 * 30.0;
        assert!(
            (turned.turn_rate() - expected).abs() < 0.5,
            "turn {} != {expected}",
            turned.turn_rate()
        );
    }

    #[test]
    fn turning_is_measured_not_assumed() {
        // Two directions one degree apart: the rate is small and finite, not a sign error.
        let base = Vec2::new(0.0, 1.0);
        let one_degree = 1.0_f32.to_radians();
        let nudged = Vec2::new(libm::sinf(one_degree), libm::cosf(one_degree));
        let rate = signed_angle(base, nudged) / TICK;
        assert!(rate.abs() < 2.0, "rate {rate} for a one-degree turn");
    }
}
