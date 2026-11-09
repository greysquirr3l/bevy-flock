//! Foot slide, measured instead of eyeballed.
//!
//! Slide is the distance a body travels that its clips do not account for: the legs plant, the body
//! keeps moving, and the result is a crowd of people ice-skating. It cannot be measured from the
//! skeleton here — the bone transforms belong to Bevy's animation system, not to this crate — but it
//! can be measured from the contract that *should* prevent it: every stride slot advances at
//! [`LocomotionProfile::cycles_per_second`], so the ground distance a weighted blend implies is
//!
//! ```text
//! implied = (walk_weight + run_weight) * cycles_per_second(speed) * stride * delta
//! ```
//!
//! and the slide is `|speed * delta - implied|`. With the shipped blend the two agree exactly at
//! full stride weight, which is the point: the measurement exists so that a change which decouples
//! playback rate from stride — a per-slot rate, a `Time`-driven lerp, a hand-tuned multiplier —
//! shows up as a number in a test rather than as a look somebody has to remember.
//!
//! A settled body reports the share of its motion that its legs are not carrying, on purpose: it is
//! the honest answer to "how far did this body move without taking a step".
use bevy_ecs::prelude::Component;

use crate::anim::binding::BlendFrame;
use crate::anim::profile::{ClipSlot, LocomotionProfile};

/// The fastest a body's foot slide has been, and how far it has slid in total.
///
/// Inserted by the binding on first sight and advanced on every tick the body is evaluated, so an
/// example can report the worst case in a still and a test can assert a ceiling over a walk.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct FootSlide {
    peak_metres: f32,
    total_metres: f32,
    ticks: u32,
}

impl FootSlide {
    /// The largest single-tick slide seen, in metres.
    #[must_use]
    pub const fn peak_metres(&self) -> f32 {
        self.peak_metres
    }

    /// Every metre of slide added up, which grows with the length of the walk.
    #[must_use]
    pub const fn total_metres(&self) -> f32 {
        self.total_metres
    }

    /// How many ticks have been measured.
    #[must_use]
    pub const fn ticks(&self) -> u32 {
        self.ticks
    }

    /// Record one tick's slide.
    pub fn observe(&mut self, slide_metres: f32) {
        if !slide_metres.is_finite() {
            return;
        }
        let slide = slide_metres.abs();
        self.peak_metres = self.peak_metres.max(slide);
        self.total_metres += slide;
        self.ticks = self.ticks.saturating_add(1);
    }
}

/// The slide one tick of this blend implies, in metres.
///
/// `delta_seconds` is the simulation tick, never a frame time: the same walk measured at 20 fps and
/// at 240 fps has to come out at the same metres per second of slide, or the ceiling would depend on
/// the machine that ran it.
#[must_use]
pub fn slide_per_tick(
    profile: &LocomotionProfile,
    frame: &BlendFrame,
    speed: f32,
    delta_seconds: f32,
) -> f32 {
    let delta = if delta_seconds.is_finite() && delta_seconds > 0.0 {
        delta_seconds
    } else {
        return 0.0;
    };
    let stride_weight = frame.weight(ClipSlot::Walk) + frame.weight(ClipSlot::Run);
    let stride = profile.stride_metres();
    let implied = stride_weight * profile.cycles_per_second(speed) * stride * delta;
    let travelled = speed.abs() * delta;
    travelled - implied
}

#[cfg(test)]
mod tests {
    use super::{FootSlide, slide_per_tick};
    use crate::anim::binding::BlendFrame;
    use crate::anim::profile::LocomotionProfile;

    fn profile() -> LocomotionProfile {
        LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)
            .unwrap_or_else(|_| unreachable!("the fixture profile is valid"))
    }

    #[test]
    fn a_full_weight_walk_does_not_slide() {
        let profile = profile();
        let mut frame = BlendFrame::default();
        frame.fill(&profile, profile.walk_speed(), 0.0);
        let slide = slide_per_tick(&profile, &frame, profile.walk_speed(), 1.0 / 60.0);
        assert!(slide.abs() < 1e-6, "a walk at walk speed slid {slide} m");
    }

    #[test]
    fn a_full_weight_blend_never_slides_at_any_speed() {
        // The invariant the measurement protects: playback rate is derived from the same speed the
        // body travels at, so a committed stride clip covers exactly the ground the body covers.
        let profile = profile();
        for speed in [0.4_f32, 1.4, 2.0, 3.7, 5.0, 9.0] {
            let mut frame = BlendFrame::default();
            frame.fill(&profile, speed, 0.0);
            if frame.weight(crate::anim::profile::ClipSlot::Walk)
                + frame.weight(crate::anim::profile::ClipSlot::Run)
                < 0.999
            {
                continue;
            }
            let slide = slide_per_tick(&profile, &frame, speed, 1.0 / 60.0);
            assert!(slide.abs() < 1e-6, "{speed} m/s slid {slide} m");
        }
    }

    #[test]
    fn a_half_committed_stride_reports_the_share_the_legs_are_not_carrying() {
        let profile = profile();
        let mut frame = BlendFrame::default();
        frame.fill(&profile, profile.walk_speed(), 0.0);
        let settled = frame.weight(crate::anim::profile::ClipSlot::Walk) * 0.5;
        assert!((settled - 0.5).abs() < 1e-6);
        let mut settled = frame;
        settled.scale_stride(0.5);
        let slide = slide_per_tick(&profile, &settled, profile.walk_speed(), 1.0 / 60.0);
        // Half the stride weight against a full-speed body is half the distance unaccounted for.
        assert!(
            (slide - (profile.walk_speed() * 0.5) / 60.0).abs() < 1e-5,
            "expected half the movement unaccounted, got {slide}"
        );
    }

    #[test]
    fn a_nonsense_delta_measures_nothing_rather_than_poisoning_the_total() {
        let profile = profile();
        let mut frame = BlendFrame::default();
        frame.fill(&profile, 1.0, 0.0);
        assert!(slide_per_tick(&profile, &frame, 1.0, 0.0).abs() < f32::EPSILON);
        assert!(slide_per_tick(&profile, &frame, 1.0, f32::NAN).abs() < f32::EPSILON);
    }

    #[test]
    fn the_counter_keeps_the_peak_and_the_running_total() {
        let mut slide = FootSlide::default();
        slide.observe(0.01);
        slide.observe(-0.03);
        slide.observe(f32::INFINITY);
        assert!((slide.peak_metres() - 0.03).abs() < 1e-6);
        assert!((slide.total_metres() - 0.04).abs() < 1e-6);
        assert_eq!(
            slide.ticks(),
            2,
            "a non-finite measurement is dropped, not counted"
        );
    }
}
