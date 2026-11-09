//! What the game tells the library about its clips, and the pure maths that turns motion into weights.
//!
//! A profile is authored data: how fast a body walks, how fast it runs, how long its stride is, and
//! at what turn rate standing still stops being a pose and becomes a turn. It names the *slots* a
//! game's animation graph fills — idle, walk, run, turn-in-place — and never a clip, an asset or a
//! node index, because those belong to the game's graph and not to a crowd library.
//!
//! [`LocomotionProfile::blend`] is a pure function of speed and turn rate. It reads no clock, holds
//! no state and allocates nothing: the same two numbers always produce the same four weights, which
//! is what lets a replay animate identically at 20 frames a second and at 200.

use crate::anim::lod::BlendLod;
use crate::anim::settle::BlendWindows;
use bevy_asset::Asset;
use bevy_reflect::TypePath;
use thiserror::Error;

/// The four poses a walking body blends between.
///
/// A game maps each slot to a node in its own `AnimationGraph`; this crate only says which slots
/// carry weight at a given speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClipSlot {
    /// Standing still: the pose a body holds when it has nowhere to be.
    Idle,
    /// A walk cycle, played while the body moves at or below the profile's walking speed.
    Walk,
    /// A run cycle, played as the body approaches the profile's running speed.
    Run,
    /// A turn taken on the spot, which applies only when the body is close to stationary.
    TurnInPlace,
}

impl ClipSlot {
    /// Every slot, in a fixed order, so a game can iterate its bindings without a map.
    pub const ALL: [Self; 4] = [Self::Idle, Self::Walk, Self::Run, Self::TurnInPlace];

    /// The slot's position in [`ClipSlot::ALL`], which is also its index in a per-body table.
    ///
    /// The binding keeps four weights and four rates per body; this is how it finds a body's row
    /// without a map. A test asserts it agrees with `ALL` for every slot.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Idle => 0,
            Self::Walk => 1,
            Self::Run => 2,
            Self::TurnInPlace => 3,
        }
    }

    /// The slot's name in an error message or a log line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Walk => "walk",
            Self::Run => "run",
            Self::TurnInPlace => "turn-in-place",
        }
    }
}

/// Why a profile was refused.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum ProfileError {
    /// The walking speed was zero or negative.
    #[error("walking speed must be positive, got {0}")]
    WalkSpeed(f32),
    /// The running speed was not above the walking speed.
    #[error("running speed {run} must be greater than walking speed {walk}")]
    RunSpeed {
        /// The running speed as authored.
        run: f32,
        /// The walking speed it has to exceed.
        walk: f32,
    },
    /// The stride length was zero or negative, so playback rate would divide by zero.
    #[error("stride length must be positive, got {0}")]
    Stride(f32),
    /// The turn-rate band was empty or inverted.
    #[error(
        "turn rate band must satisfy 0 <= threshold < full, got threshold {threshold} and full {full}"
    )]
    TurnBand {
        /// Where turn-in-place begins, radians per second.
        threshold: f32,
        /// Where it reaches full weight, radians per second.
        full: f32,
    },
    /// The LOD band was inverted or empty, so no body would ever be stepped or paused.
    #[error("lod band must satisfy 0 <= near < far, got near {near} and far {far}")]
    LodBand {
        /// The inner distance as authored.
        near: f32,
        /// The outer distance as authored.
        far: f32,
    },
    /// The LOD cadence was zero ticks, which is a pause expressed as a step.
    #[error("lod step interval must be at least one tick, got {0}")]
    LodStep(u32),
    /// A start or stop window was longer than four seconds of ticks.
    #[error(
        "settle windows must be at most 240 ticks, got start {start_ticks} and stop {stop_ticks}"
    )]
    SettleWindow {
        /// The start window as authored, in ticks.
        start_ticks: u32,
        /// The stop window as authored, in ticks.
        stop_ticks: u32,
    },
    /// A field was `NaN` or infinite, which would poison every comparison downstream.
    #[error("{field} must be finite, got {value}")]
    NotFinite {
        /// The field's name in the authored file.
        field: &'static str,
        /// The value as authored.
        value: f32,
    },
}

/// Who moves a body whose clip carries its own translation.
///
/// The flag exists because both answers are defensible and only one of them can be the default. A
/// simulation-driven crowd wants [`RootMotion::Simulation`]: the steering solution is the ground truth
/// and a clip that translates its root is a rigging artifact to be dropped. A game built around
/// mocap wants [`RootMotion::Clip`]: the clip's translation *is* the movement, and the steering target
/// has to follow it. Choosing neither and letting both run is the one wrong answer, and it is what
/// happens by accident when nobody decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RootMotion {
    /// The simulation owns movement: reported clip translation is dropped on arrival.
    #[default]
    Simulation,
    /// The clip owns movement: reported translation moves the body and advances its walk phase.
    Clip,
}

impl RootMotion {
    /// Whether a reported offset moves the body.
    #[must_use]
    pub const fn extracts(self) -> bool {
        matches!(self, Self::Clip)
    }
}

/// How a body's motion maps onto the four clip slots.
///
/// Construct one with [`LocomotionProfile::try_new`]: every field is validated once, at the
/// boundary, so nothing inside the blend has to re-check it.
///
/// It is a Bevy [`Asset`], so a game can load it from a file and hot-reload a designer's tweak
/// without a rebuild. Being an asset changes nothing about the maths: the blend is a function of the
/// numbers it holds, not of where they came from.
#[derive(Asset, TypePath, Debug, Clone, Copy, PartialEq)]
pub struct LocomotionProfile {
    walk_speed: f32,
    run_speed: f32,
    stride_metres: f32,
    turn_threshold: f32,
    turn_full: f32,
    lod: BlendLod,
    windows: BlendWindows,
    root_motion: RootMotion,
}

impl LocomotionProfile {
    /// The speed, in metres per second, at which the walk clip is fully weighted.
    pub const fn walk_speed(&self) -> f32 {
        self.walk_speed
    }

    /// The speed, in metres per second, at which the run clip is fully weighted.
    pub const fn run_speed(&self) -> f32 {
        self.run_speed
    }

    /// How far one full gait cycle carries the body, in metres.
    ///
    /// Playback rate is derived from this rather than from speed alone, so a slow walk does not
    /// look like a skating run.
    pub const fn stride_metres(&self) -> f32 {
        self.stride_metres
    }

    /// How far away this rig stays fully animated.
    ///
    /// Part of the profile rather than a constant because it is a property of the artwork: a body
    /// whose silhouette is three pixels tall can be paused further out than one you can read.
    pub const fn lod(&self) -> BlendLod {
        self.lod
    }

    /// Who moves the body when a clip carries its own translation.
    pub const fn root_motion(&self) -> RootMotion {
        self.root_motion
    }
    /// Choose who owns movement, keeping every other number.
    ///
    /// ```
    /// use bevy_flock::anim::{LocomotionProfile, RootMotion};
    ///
    /// let profile = LocomotionProfile::try_new(1.4, 4.2, 1.5, 0.6, 2.0)?
    ///     .with_root_motion(RootMotion::Clip);
    /// assert!(profile.root_motion().extracts());
    /// # Ok::<(), bevy_flock::anim::ProfileError>(())
    /// ```
    #[must_use]
    pub const fn with_root_motion(mut self, root_motion: RootMotion) -> Self {
        self.root_motion = root_motion;
        self
    }
    /// How long this body takes to pick up its stride and to put it down, in ticks.
    ///
    /// Zero ticks for both — the default — means the pose follows the simulation exactly on the tick
    /// it changes, which is what a replay of the simulation should show. A game that wants a start
    /// and stop animation authors a window here.
    pub const fn windows(&self) -> BlendWindows {
        self.windows
    }
    /// Replace the settle windows, keeping every other number.
    ///
    /// ```
    /// use bevy_flock::anim::{BlendWindows, LocomotionProfile};
    ///
    /// let profile = LocomotionProfile::try_new(1.4, 4.2, 1.5, 0.6, 2.0)?
    ///     .with_windows(BlendWindows::try_new(6, 8)?);
    /// assert_eq!(profile.windows().start_ticks(), 6);
    /// # Ok::<(), bevy_flock::anim::ProfileError>(())
    /// ```
    #[must_use]
    pub const fn with_windows(mut self, windows: BlendWindows) -> Self {
        self.windows = windows;
        self
    }
    /// Replace the LOD band, keeping every other number.
    ///
    /// ```
    /// use bevy_flock::anim::{BlendLod, LocomotionProfile};
    ///
    /// let profile = LocomotionProfile::try_new(1.4, 4.2, 1.5, 0.6, 2.0)?
    ///     .with_lod(BlendLod::disabled());
    /// assert_eq!(profile.lod(), BlendLod::disabled());
    /// # Ok::<(), bevy_flock::anim::ProfileError>(())
    /// ```
    #[must_use]
    pub const fn with_lod(mut self, lod: BlendLod) -> Self {
        self.lod = lod;
        self
    }

    /// Validate an authored profile.
    ///
    /// # Errors
    ///
    /// Returns the first field that cannot be used: a non-positive speed or stride, a run speed
    /// that does not exceed the walk speed, a turn band that is inverted or empty, or a value that
    /// is not finite. One error per call, naming the field and the value, because an authored file
    /// with two mistakes should still tell the author about the first one precisely.
    pub fn try_new(
        walk_speed: f32,
        run_speed: f32,
        stride_metres: f32,
        turn_threshold: f32,
        turn_full: f32,
    ) -> Result<Self, ProfileError> {
        for (field, value) in [
            ("walk_speed", walk_speed),
            ("run_speed", run_speed),
            ("stride_metres", stride_metres),
            ("turn_threshold", turn_threshold),
            ("turn_full", turn_full),
        ] {
            if !value.is_finite() {
                return Err(ProfileError::NotFinite { field, value });
            }
        }
        if walk_speed <= 0.0 {
            return Err(ProfileError::WalkSpeed(walk_speed));
        }
        if run_speed <= walk_speed {
            return Err(ProfileError::RunSpeed {
                run: run_speed,
                walk: walk_speed,
            });
        }
        if stride_metres <= 0.0 {
            return Err(ProfileError::Stride(stride_metres));
        }
        if turn_threshold < 0.0 || turn_full <= turn_threshold {
            return Err(ProfileError::TurnBand {
                threshold: turn_threshold,
                full: turn_full,
            });
        }
        Ok(Self {
            walk_speed,
            run_speed,
            stride_metres,
            turn_threshold,
            turn_full,
            lod: BlendLod::default(),
            windows: BlendWindows::immediate(),
            root_motion: RootMotion::Simulation,
        })
    }

    /// The weights this profile assigns to a body moving at `speed` while turning at `turn_rate`.
    ///
    /// `speed` is the planar speed in metres per second and `turn_rate` the yaw rate in radians per
    /// second; both come from the simulation's `Locomotion` component, never from a clock. Negative
    /// speeds are treated as zero and negative turn rates
    /// as their magnitude, because a body turning left and one turning right are the same pose.
    ///
    /// The four weights always sum to one, so a game can apply them straight to
    /// `ActiveAnimation::weight` without normalising. Turn-in-place takes its weight out of idle
    /// only: a body travelling at walking speed turns by steering, not by pivoting.
    pub fn blend(&self, speed: f32, turn_rate: f32) -> BlendWeights {
        let speed = if speed > 0.0 { speed } else { 0.0 };

        // A one-dimensional blend space with three anchors: standing, walking, running.
        let (mut idle, walk, run) = if speed <= self.walk_speed {
            let walked = speed / self.walk_speed;
            (1.0 - walked, walked, 0.0)
        } else if speed < self.run_speed {
            let span = self.run_speed - self.walk_speed;
            let ran = (speed - self.walk_speed) / span;
            (0.0, 1.0 - ran, ran)
        } else {
            (0.0, 0.0, 1.0)
        };

        // Turn-in-place is a share of the standing pose, ramped across the profile's band.
        let magnitude = turn_rate.abs();
        let turned = if magnitude <= self.turn_threshold {
            0.0
        } else if magnitude >= self.turn_full {
            1.0
        } else {
            (magnitude - self.turn_threshold) / (self.turn_full - self.turn_threshold)
        };
        let turn = turned * idle;
        idle -= turn;

        BlendWeights {
            idle,
            walk,
            run,
            turn_in_place: turn,
        }
    }

    /// How many gait cycles per second a body at `speed` is taking.
    ///
    /// A game multiplies this by the frame's elapsed time to get a playback rate; the library stops
    /// at cycles per second because it does not own the clock (ADR 0052).
    pub fn cycles_per_second(&self, speed: f32) -> f32 {
        let speed = if speed > 0.0 { speed } else { 0.0 };
        speed / self.stride_metres
    }
}

/// The four clip weights for one body at one instant.
///
/// Named fields rather than an array: a slot is a thing a game can reason about, and an index is a
/// thing that goes out of bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlendWeights {
    idle: f32,
    walk: f32,
    run: f32,
    turn_in_place: f32,
}

impl BlendWeights {
    /// Scale the stride slots by `factor` and give the rest to idle.
    ///
    /// This is what a settle window applies: a body 40% through its start window is drawn as a body
    /// standing still with 60% of its weight, which is the honest pose for "the legs have not caught
    /// up yet". Nothing is normalised afterwards because the two weights trade exactly.
    #[must_use]
    pub fn with_stride_scaled(self, factor: f32) -> Self {
        let factor = factor.clamp(0.0, 1.0);
        let stride = self.walk + self.run;
        let kept = stride * factor;
        Self {
            idle: self.idle + (stride - kept),
            walk: self.walk * factor,
            run: self.run * factor,
            turn_in_place: self.turn_in_place,
        }
    }
    /// A pose with every weight at zero, which no profile produces and every default needs.
    pub const ZERO: Self = Self {
        idle: 0.0,
        walk: 0.0,
        run: 0.0,
        turn_in_place: 0.0,
    };

    /// The weight for one slot.
    pub const fn weight(&self, slot: ClipSlot) -> f32 {
        match slot {
            ClipSlot::Idle => self.idle,
            ClipSlot::Walk => self.walk,
            ClipSlot::Run => self.run,
            ClipSlot::TurnInPlace => self.turn_in_place,
        }
    }

    /// The weight for one slot, mutably, for a caller assembling a pose by hand.
    pub const fn weight_mut(&mut self, slot: ClipSlot) -> &mut f32 {
        match slot {
            ClipSlot::Idle => &mut self.idle,
            ClipSlot::Walk => &mut self.walk,
            ClipSlot::Run => &mut self.run,
            ClipSlot::TurnInPlace => &mut self.turn_in_place,
        }
    }

    /// Every weight, in [`ClipSlot::ALL`] order.
    pub const fn as_array(&self) -> [f32; 4] {
        [self.idle, self.walk, self.run, self.turn_in_place]
    }

    /// The sum of the weights, which a profile always leaves at one.
    ///
    /// Exposed because a game that edits weights by hand — a gesture that overrides idle, say —
    /// needs to know whether it has left the pose normalised.
    pub fn sum(&self) -> f32 {
        self.idle + self.walk + self.run + self.turn_in_place
    }

    /// Whether every weight is finite and non-negative, which is what a hand-edited pose must be.
    pub fn is_sane(&self) -> bool {
        self.as_array()
            .into_iter()
            .all(|weight| weight.is_finite() && weight >= 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{BlendWeights, ClipSlot, LocomotionProfile, ProfileError};

    /// A profile with the numbers the book uses: a 1.4 m/s walk, a 5 m/s run, a 1.5 m stride.
    ///
    /// `unreachable!` rather than `expect`: the lints allow it as a claim about the code (the
    /// numbers below are valid by inspection and by the validation tests), and deny `expect_used`
    /// because an `expect` in a fixture hides the day its premise stops being true.
    fn profile() -> LocomotionProfile {
        LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)
            .unwrap_or_else(|_| unreachable!("the fixture profile is valid"))
    }

    #[test]
    fn standing_still_is_idle_and_never_turns() {
        let blend = profile().blend(0.0, 0.0);
        assert!((blend.weight(ClipSlot::Idle) - 1.0).abs() < 1e-6);
        assert!(blend.weight(ClipSlot::Walk).abs() < 1e-6);
        assert!(blend.weight(ClipSlot::Run).abs() < 1e-6);
        assert!(blend.weight(ClipSlot::TurnInPlace).abs() < 1e-6);
    }

    #[test]
    fn the_anchors_are_pure() {
        let profile = profile();
        // At the walking anchor, walk is everything.
        let walk = profile.blend(profile.walk_speed(), 0.0);
        assert!((walk.weight(ClipSlot::Walk) - 1.0).abs() < 1e-6);
        assert!(walk.weight(ClipSlot::Idle).abs() < 1e-6);
        // At the running anchor, run is everything.
        let run = profile.blend(profile.run_speed(), 0.0);
        assert!((run.weight(ClipSlot::Run) - 1.0).abs() < 1e-6);
        // Past the running anchor it stays at run rather than overshooting.
        let fast = profile.blend(profile.run_speed() * 4.0, 0.0);
        assert!((fast.weight(ClipSlot::Run) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn every_blend_sums_to_one() {
        let profile = profile();
        // Integer steps, converted once: a float loop counter is how a sweep silently skips its
        // last sample or never terminates.
        for step in 0..=160_u16 {
            let speed = f32::from(step) * 0.05;
            for turn in [-4.0_f32, -0.7, 0.0, 0.7, 4.0] {
                let blend = profile.blend(speed, turn);
                assert!(
                    (blend.sum() - 1.0).abs() < 1e-5,
                    "speed {speed} turn {turn} summed to {}",
                    blend.sum()
                );
                assert!(blend.is_sane(), "speed {speed} turn {turn} was not sane");
            }
        }
    }

    #[test]
    fn walking_is_monotonic_and_continuous_in_speed() {
        let profile = profile();
        let mut previous = profile.blend(0.0, 0.0).weight(ClipSlot::Walk);
        // 140 steps of 0.01 m/s covers the 1.4 m/s anchor exactly.
        for step in 1..=140_u16 {
            let speed = f32::from(step) * 0.01;
            let walk = profile.blend(speed, 0.0).weight(ClipSlot::Walk);
            assert!(walk >= previous, "walk weight fell at {speed}");
            assert!(walk - previous < 0.05, "walk weight jumped at {speed}");
            previous = walk;
        }
        assert!((previous - 1.0).abs() < 1e-5);
    }

    #[test]
    fn turn_in_place_comes_out_of_idle_only() {
        let profile = profile();
        // A body at walking speed steering hard does not pivot: it walks the turn.
        let walking = profile.blend(profile.walk_speed(), 9.0);
        assert!(walking.weight(ClipSlot::TurnInPlace).abs() < 1e-6);
        // A body standing still and turning is pivoting, and it takes the weight from idle.
        let pivoting = profile.blend(0.0, 9.0);
        assert!((pivoting.weight(ClipSlot::TurnInPlace) - 1.0).abs() < 1e-6);
        assert!(pivoting.weight(ClipSlot::Idle).abs() < 1e-6);
        // Halfway through the band, the two share the standing pose.
        let band = profile.blend(0.0, 1.25);
        assert!((band.weight(ClipSlot::TurnInPlace) - 0.5).abs() < 1e-5);
        assert!((band.weight(ClipSlot::Idle) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn turning_left_and_right_are_the_same_pose() {
        let profile = profile();
        let left = profile.blend(0.0, -2.5);
        let right = profile.blend(0.0, 2.5);
        assert_eq!(left, right);
    }

    #[test]
    fn negative_speed_is_standing_still() {
        let profile = profile();
        assert_eq!(profile.blend(-3.0, 0.0), profile.blend(0.0, 0.0));
    }

    #[test]
    fn playback_rate_is_stride_length_not_speed() {
        let profile = profile();
        // One stride a second at the stride length.
        assert!((profile.cycles_per_second(1.5) - 1.0).abs() < 1e-6);
        assert!((profile.cycles_per_second(3.0) - 2.0).abs() < 1e-6);
        assert!(profile.cycles_per_second(-1.0).abs() < 1e-6);
    }

    /// The error from a profile the validator must refuse, as a value rather than a comparison of
    /// error structs: one of them carries `NaN`, and `NaN` is not equal to itself.
    fn refusal_of(walk: f32, run: f32, stride: f32, threshold: f32, full: f32) -> ProfileError {
        LocomotionProfile::try_new(walk, run, stride, threshold, full)
            .err()
            .unwrap_or_else(|| unreachable!("the fixture values are invalid"))
    }

    #[test]
    fn invalid_profiles_are_refused_with_the_field() {
        // Each message names the problem and the number, because that is what the author of a RON
        // file sees and all they have to go on.
        let walk = refusal_of(0.0, 5.0, 1.5, 0.5, 2.0);
        assert!(matches!(walk, ProfileError::WalkSpeed(_)));
        assert!(walk.to_string().contains("walking speed must be positive"));

        let run = refusal_of(5.0, 5.0, 1.5, 0.5, 2.0);
        assert!(matches!(run, ProfileError::RunSpeed { .. }));
        assert!(
            run.to_string()
                .contains("must be greater than walking speed")
        );

        let stride = refusal_of(1.4, 5.0, 0.0, 0.5, 2.0);
        assert!(matches!(stride, ProfileError::Stride(_)));
        assert!(
            stride
                .to_string()
                .contains("stride length must be positive")
        );

        let band = refusal_of(1.4, 5.0, 1.5, 2.0, 2.0);
        assert!(matches!(band, ProfileError::TurnBand { .. }));
        assert!(band.to_string().contains("turn rate band"));

        let infinite = refusal_of(f32::INFINITY, 5.0, 1.5, 0.5, 2.0);
        assert!(matches!(
            infinite,
            ProfileError::NotFinite {
                field: "walk_speed",
                ..
            }
        ));

        // `NaN` is refused by the same check, which is the reason the fixture compares variants
        // rather than values: `NaN` never equals itself, so a derived `PartialEq` on the error
        // would report a false failure.
        assert!(matches!(
            refusal_of(f32::NAN, 5.0, 1.5, 0.5, 2.0),
            ProfileError::NotFinite {
                field: "walk_speed",
                ..
            }
        ));
    }

    #[test]
    fn hand_edited_weights_report_whether_they_are_sane() {
        let mut weights = BlendWeights::ZERO;
        assert!(weights.is_sane());
        assert!(weights.sum().abs() < 1e-6);
        *weights.weight_mut(ClipSlot::Idle) = 0.75;
        *weights.weight_mut(ClipSlot::Run) = 0.25;
        assert!(weights.is_sane());
        assert!((weights.sum() - 1.0).abs() < 1e-6);
        *weights.weight_mut(ClipSlot::Walk) = -0.5;
        assert!(!weights.is_sane());
        let slots = ClipSlot::ALL;
        let values = weights.as_array();
        for (slot, value) in slots.iter().zip(values) {
            assert!((weights.weight(*slot) - value).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn every_slot_index_matches_its_position_in_all() {
        for (position, slot) in ClipSlot::ALL.iter().enumerate() {
            assert_eq!(slot.index(), position, "{} is at {position}", slot.as_str());
        }
    }

    #[test]
    fn turn_in_place_is_unreachable_at_walking_speed() {
        let profile = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)
            .unwrap_or_else(|_| unreachable!("the fixture profile is valid"));
        // A body travelling at walking speed steers; it does not pivot, however hard it turns.
        let walking = profile.blend(profile.walk_speed(), 40.0);
        assert!(walking.weight(ClipSlot::TurnInPlace).abs() < 1e-6);
        assert!((walking.weight(ClipSlot::Walk) - 1.0).abs() < 1e-6);
        // Standing still and turning is the pivot's whole job, and it comes out of idle.
        let pivot = profile.blend(0.0, 40.0);
        assert!((pivot.weight(ClipSlot::TurnInPlace) - 1.0).abs() < 1e-6);
        assert!(pivot.weight(ClipSlot::Idle).abs() < 1e-6);
        // Below the profile's threshold there is no pivot at all: a body standing still and turning
        // slowly is a body standing still.
        assert!(profile.blend(0.0, 0.2).weight(ClipSlot::TurnInPlace).abs() < 1e-6);
    }
}
