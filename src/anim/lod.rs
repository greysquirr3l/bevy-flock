//! How much animation a body is worth, given where it is and what the scheduler thinks it is.
//!
//! A crowd of ten thousand drawn bodies is a crowd of ten thousand skeletons, and the binding cost
//! of writing a blend into each of them every frame is the least of it: the bones are what cost.
//! This module decides, per body, which of three levels it gets:
//!
//! | Level | What the binding does | When |
//! |---|---|---|
//! | [`LodLevel::Full`] | writes the blend every tick | near the focus, or any distance from a body the scheduler is thinking about |
//! | [`LodLevel::Stepped`] | writes the blend every `far_step_ticks` | further out, and only for cheap tiers |
//! | [`LodLevel::Paused`] | pauses the player and writes nothing | beyond `far` |
//!
//! The tier is the one the simulation's scheduler already assigned — [`NpcTier`] — not a second
//! distance test, so a body the scheduler is about to promote is animated before it can be seen
//! moving. The distance is measured from [`AnimationFocus`], which the game sets to whatever it is
//! watching: a camera, a listener, a spectator's target. With no focus set, nothing is culled.
//!
//! [`NpcTier`]: crate::adapters::primary::cognition::NpcTier
use crate::anim::profile::ProfileError;
use crate::domain::cognition::CognitionTier;
use bevy_ecs::prelude::{Component, Resource};
use bevy_math::Vec3;

/// The default distance, in metres, inside which every body animates fully.
pub const DEFAULT_NEAR: f32 = 40.0;

/// The default distance, in metres, beyond which a body stops animating at all.
pub const DEFAULT_FAR: f32 = 120.0;

/// The default number of ticks between blended updates for a stepped body.
pub const DEFAULT_STEP_TICKS: u32 = 6;

/// Where a body sits between "animate every tick" and "do not animate at all".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LodLevel {
    /// Blend every tick.
    #[default]
    Full,
    /// Blend on a cadence: `far_step_ticks` apart, because nobody can see the difference.
    Stepped,
    /// Pause the player and write nothing until the body comes back.
    Paused,
}

impl LodLevel {
    /// Whether the level writes a blend on the tick numbered `tick`.
    ///
    /// A stepped body evaluates when the tick is a multiple of `step`, so the cadence survives a
    /// replay: it is a function of the tick, not of how many frames a machine managed.
    #[must_use]
    pub fn evaluates(self, tick: u64, step: u32) -> bool {
        match self {
            Self::Full => true,
            Self::Stepped => {
                if step <= 1 {
                    return true;
                }
                tick.is_multiple_of(u64::from(step))
            }
            Self::Paused => false,
        }
    }

    /// Whether the player should be paused at this level.
    #[must_use]
    pub const fn pauses_player(self) -> bool {
        matches!(self, Self::Paused)
    }

    /// The level's name, for the telemetry line and for error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Stepped => "stepped",
            Self::Paused => "paused",
        }
    }
}

/// The distance band and the cadence a far body steps at.
///
/// This lives inside [`LocomotionProfile`](crate::anim::LocomotionProfile) rather than in a resource
/// because it is a property of the artwork and the crowd, not of the run: a rig whose walk cycle is
/// four metres long can step further out than a one-metre stride before the freeze is visible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlendLod {
    near: f32,
    far: f32,
    far_step_ticks: u32,
}

impl Default for BlendLod {
    fn default() -> Self {
        Self {
            near: DEFAULT_NEAR,
            far: DEFAULT_FAR,
            far_step_ticks: DEFAULT_STEP_TICKS,
        }
    }
}

impl BlendLod {
    /// A band that culls nothing: everything animates fully, whatever the tier or the distance.
    ///
    /// Expressed as an infinite `near`, because that is what "every body is close enough" means to
    /// [`BlendLod::level`]; [`BlendLod::try_new`] refuses to author one, so a game turns culling off
    /// with this constructor or by not setting an [`AnimationFocus`] at all.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            near: f32::INFINITY,
            far: f32::INFINITY,
            far_step_ticks: 1,
        }
    }

    /// Author a band.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::NotFinite`] for a non-finite distance, [`ProfileError::LodBand`] if
    /// `near` is negative or not inside `far`, and [`ProfileError::LodStep`] for a cadence below one
    /// tick — a body that steps every zero ticks never updates at all, which is what
    /// [`LodLevel::Paused`] is for.
    pub fn try_new(near: f32, far: f32, far_step_ticks: u32) -> Result<Self, ProfileError> {
        for (field, value) in [("lod_near", near), ("lod_far", far)] {
            if !value.is_finite() {
                return Err(ProfileError::NotFinite { field, value });
            }
        }
        if near < 0.0 || far <= near {
            return Err(ProfileError::LodBand { near, far });
        }
        if far_step_ticks == 0 {
            return Err(ProfileError::LodStep(far_step_ticks));
        }
        Ok(Self {
            near,
            far,
            far_step_ticks,
        })
    }

    /// The distance, in metres, inside which every body animates fully.
    #[must_use]
    pub const fn near(&self) -> f32 {
        self.near
    }

    /// The distance, in metres, beyond which a body stops animating.
    #[must_use]
    pub const fn far(&self) -> f32 {
        self.far
    }

    /// How many ticks apart a stepped body blends.
    #[must_use]
    pub const fn far_step_ticks(&self) -> u32 {
        self.far_step_ticks
    }

    /// The level for a body, given its distance from the focus and the tier the scheduler gave it.
    ///
    /// `distance` is `None` when the game has set no focus, which means "animate everything": a
    /// binding that culls by default would silently stop animating a scene that never told it where
    /// the camera was.
    ///
    /// # Examples
    ///
    /// ```
    /// use bevy_flock::anim::{BlendLod, LodLevel};
    ///
    /// let lod = BlendLod::try_new(10.0, 50.0, 4)?;
    /// assert_eq!(lod.level(Some(5.0), None), LodLevel::Full);
    /// assert_eq!(lod.level(Some(500.0), None), LodLevel::Paused);
    /// assert_eq!(lod.level(None, None), LodLevel::Full);
    /// # Ok::<(), bevy_flock::anim::ProfileError>(())
    /// ```
    #[must_use]
    pub fn level(&self, distance: Option<f32>, tier: Option<CognitionTier>) -> LodLevel {
        let Some(distance) = distance else {
            return LodLevel::Full;
        };
        if distance <= self.near {
            return LodLevel::Full;
        }
        if distance > self.far {
            return LodLevel::Paused;
        }
        // Inside the band the tier decides: a body the scheduler is thinking about can change what
        // it is doing between two samples, so it keeps every tick; a reflex body cannot, so it steps.
        match tier {
            Some(CognitionTier::Deliberative | CognitionTier::Social) | None => LodLevel::Full,
            Some(CognitionTier::Reflex | CognitionTier::Reactive) => LodLevel::Stepped,
        }
    }
}

/// Where the game is looking, for the purposes of animation.
///
/// Set it once per frame to the camera's position, or to the listener's, or to whatever the player
/// is watching. Leave it at [`AnimationFocus::everywhere`] and every body animates fully.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct AnimationFocus(Option<Vec3>);

impl AnimationFocus {
    /// Watch from one point.
    #[must_use]
    pub const fn at(position: Vec3) -> Self {
        Self(Some(position))
    }

    /// Watch nothing: no distance culling. The default, so an app that never sets a focus keeps
    /// every body animating.
    #[must_use]
    pub const fn everywhere() -> Self {
        Self(None)
    }

    /// The watched point, if there is one.
    #[must_use]
    pub const fn point(&self) -> Option<Vec3> {
        self.0
    }

    /// How far a body at `position` is from the focus, if there is one.
    #[must_use]
    pub fn distance_to(&self, position: Vec3) -> Option<f32> {
        self.0.map(|focus| focus.distance(position))
    }
}

/// What the binding decided about one body last time it ran.
///
/// Inserted by the binding itself on first sight, so a game does not have to. It carries the level
/// and two counters: evaluations, which is what the "an ambient body is not animated" test reads,
/// and resumes, which counts the times a body came back from [`LodLevel::Paused`] and had its pose
/// restored.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnimationLod {
    level: LodLevel,
    evaluations: u32,
    resumes: u32,
}

impl AnimationLod {
    /// The level last applied.
    #[must_use]
    pub const fn level(&self) -> LodLevel {
        self.level
    }

    /// How many times this body's blend has been written.
    #[must_use]
    pub const fn evaluations(&self) -> u32 {
        self.evaluations
    }

    /// How many times this body has resumed from a pause.
    #[must_use]
    pub const fn resumes(&self) -> u32 {
        self.resumes
    }

    /// Record a level and whether this run evaluated the blend.
    ///
    /// Returns whether the body was paused and is not any more, which is the one transition the
    /// binding has to undo rather than overwrite: a paused player has to be resumed, and its clips
    /// moved to the phase the distance walked implies.
    pub(crate) fn record(&mut self, level: LodLevel, evaluated: bool) -> bool {
        let resumed = self.level == LodLevel::Paused && level != LodLevel::Paused;
        self.level = level;
        if evaluated {
            self.evaluations = self.evaluations.saturating_add(1);
        }
        if resumed {
            self.resumes = self.resumes.saturating_add(1);
        }
        resumed
    }
}

#[cfg(test)]
mod tests {
    use super::{AnimationFocus, AnimationLod, BlendLod, LodLevel};
    use crate::domain::cognition::CognitionTier;
    use bevy_math::Vec3;

    #[test]
    fn the_default_band_is_the_documented_one() {
        let lod = BlendLod::default();
        assert!(lod.near() < lod.far());
        assert!(lod.far_step_ticks() > 1);
        assert_eq!(
            lod,
            BlendLod::try_new(lod.near(), lod.far(), lod.far_step_ticks()).unwrap_or(lod)
        );
    }

    #[test]
    fn distance_decides_the_level_at_the_edges() {
        let lod = BlendLod::try_new(10.0, 50.0, 4).unwrap_or_default();
        assert_eq!(lod.level(Some(0.0), None), LodLevel::Full);
        assert_eq!(
            lod.level(Some(10.0), None),
            LodLevel::Full,
            "near is inclusive"
        );
        assert_eq!(
            lod.level(Some(50.0), None),
            LodLevel::Full,
            "tier decides inside the band"
        );
        assert_eq!(
            lod.level(Some(50.1), None),
            LodLevel::Paused,
            "far is exclusive"
        );
    }

    #[test]
    fn the_tier_splits_the_band_at_one_distance() {
        let lod = BlendLod::try_new(10.0, 50.0, 4).unwrap_or_default();
        let distance = Some(30.0);
        assert_eq!(
            lod.level(distance, Some(CognitionTier::Reflex)),
            LodLevel::Stepped
        );
        assert_eq!(
            lod.level(distance, Some(CognitionTier::Deliberative)),
            LodLevel::Full
        );
        assert_eq!(
            lod.level(distance, Some(CognitionTier::Social)),
            LodLevel::Full
        );
    }

    #[test]
    fn a_disabled_band_animates_everything() {
        let lod = BlendLod::disabled();
        for distance in [0.0, 5.0, 300.0, 10_000.0] {
            for tier in [
                None,
                Some(CognitionTier::Reflex),
                Some(CognitionTier::Social),
            ] {
                assert_eq!(lod.level(Some(distance), tier), LodLevel::Full);
            }
        }
    }

    #[test]
    fn no_focus_means_no_culling() {
        let focus = AnimationFocus::everywhere();
        assert_eq!(focus.point(), None);
        assert_eq!(focus.distance_to(Vec3::new(1_000.0, 0.0, 0.0)), None);
        assert_eq!(
            BlendLod::default().level(focus.distance_to(Vec3::ZERO), None),
            LodLevel::Full
        );
    }

    #[test]
    fn the_focus_measures_distance() {
        let focus = AnimationFocus::at(Vec3::new(3.0, 0.0, 4.0));
        assert_eq!(focus.distance_to(Vec3::ZERO), Some(5.0));
        assert_eq!(focus.distance_to(Vec3::new(3.0, 0.0, 4.0)), Some(0.0));
    }

    #[test]
    fn a_stepped_body_evaluates_on_its_cadence_only() {
        let level = LodLevel::Stepped;
        let ticks: Vec<u64> = (0..12).filter(|tick| level.evaluates(*tick, 4)).collect();
        assert_eq!(ticks, vec![0, 4, 8]);
        assert!(!level.evaluates(3, 4));
        assert!(LodLevel::Full.evaluates(3, 4));
        assert!(!LodLevel::Paused.evaluates(4, 4));
    }

    #[test]
    fn a_cadence_of_one_or_zero_still_evaluates() {
        assert!(LodLevel::Stepped.evaluates(7, 1));
        assert!(LodLevel::Stepped.evaluates(7, 0));
    }

    #[test]
    fn pausing_is_the_only_level_that_pauses_the_player() {
        assert!(LodLevel::Paused.pauses_player());
        assert!(!LodLevel::Stepped.pauses_player());
        assert!(!LodLevel::Full.pauses_player());
    }

    #[test]
    fn the_counters_record_what_happened() {
        let mut lod = AnimationLod::default();
        assert!(!lod.record(LodLevel::Full, true));
        assert_eq!(lod.evaluations(), 1);
        assert!(!lod.record(LodLevel::Paused, false));
        assert_eq!(lod.evaluations(), 1);
        assert!(
            lod.record(LodLevel::Full, true),
            "leaving a pause is a resume"
        );
        assert_eq!(lod.resumes(), 1);
        assert_eq!(lod.evaluations(), 2);
        assert!(
            !lod.record(LodLevel::Stepped, false),
            "stepping in place is not a resume"
        );
        assert_eq!(lod.level(), LodLevel::Stepped);
    }

    #[test]
    fn an_invalid_band_is_refused() {
        assert!(BlendLod::try_new(-1.0, 10.0, 2).is_err());
        assert!(BlendLod::try_new(10.0, 10.0, 2).is_err());
        assert!(BlendLod::try_new(20.0, 10.0, 2).is_err());
        assert!(BlendLod::try_new(1.0, 10.0, 0).is_err());
        assert!(BlendLod::try_new(f32::NAN, 10.0, 1).is_err());
        assert!(BlendLod::try_new(1.0, f32::INFINITY, 1).is_err());
    }
}
