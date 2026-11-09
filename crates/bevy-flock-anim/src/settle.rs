//! Starting and stopping over a window of ticks, instead of snapping between poses.
//!
//! The blend follows `Locomotion` exactly, which is frame-rate independent but abrupt: on the tick a
//! body's speed reaches zero its walk weight is gone. Games soften that with an animation window, and
//! the usual implementation lerps against `Time::delta`, which reintroduces the frame-rate
//! dependency the rest of this crate exists to avoid.
//!
//! Here the ease is a fixed step per **simulation tick**: the pose a body is drawn in moves
//! `1 / window` of the remaining distance to the pose the simulation asks for, and the window is in
//! ticks. A replay at 20 fps and one at 200 pass through the same intermediate poses, because the
//! sequence of poses is a function of the tick count and nothing else.
//!
//! A window of zero ticks — the default for both start and stop — snaps, so an app that never
//! authors one gets exactly the blend the simulation describes.
use bevy_ecs::prelude::Component;
use bevy_flock::domain::num;

use crate::binding::BlendFrame;
use crate::profile::ProfileError;

/// How many ticks a body takes to pick up its stride, and to put it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlendWindows {
    start_ticks: u32,
    stop_ticks: u32,
}

impl BlendWindows {
    /// No window: the pose follows the simulation on the tick it changes.
    #[must_use]
    pub const fn immediate() -> Self {
        Self {
            start_ticks: 0,
            stop_ticks: 0,
        }
    }

    /// Author both windows.
    ///
    /// # Errors
    ///
    /// [`ProfileError::SettleWindow`] if either window is longer than four seconds of ticks at 60 Hz.
    /// Zero is always allowed and means "no window".
    pub const fn try_new(start_ticks: u32, stop_ticks: u32) -> Result<Self, ProfileError> {
        if start_ticks > 240 || stop_ticks > 240 {
            return Err(ProfileError::SettleWindow {
                start_ticks,
                stop_ticks,
            });
        }
        Ok(Self {
            start_ticks,
            stop_ticks,
        })
    }

    /// Ticks a starting body takes to reach its stride pose.
    #[must_use]
    pub const fn start_ticks(self) -> u32 {
        self.start_ticks
    }

    /// Ticks a stopping body takes to return to idle.
    #[must_use]
    pub const fn stop_ticks(self) -> u32 {
        self.stop_ticks
    }

    /// Whether both windows are zero.
    #[must_use]
    pub const fn is_immediate(self) -> bool {
        self.start_ticks == 0 && self.stop_ticks == 0
    }

    /// How far through its window a body is, given the ticks it has spent in it.
    ///
    /// The ramp is linear and lands exactly on the window: a six-tick start is half committed after
    /// three ticks and committed after six. An exponential ease never arrives, and "reaches its
    /// weight within the window" has to be a fact a test can assert.
    #[must_use]
    pub fn progress(self, moving: bool, ticks: u32) -> f32 {
        let window = if moving {
            self.start_ticks
        } else {
            self.stop_ticks
        };
        if window == 0 {
            return 1.0;
        }
        num::u32_to_f32(ticks.min(window)) / num::u32_to_f32(window)
    }
}

/// The pose one body is actually drawn in, eased toward the pose the simulation asks for.
///
/// Inserted by the binding on first sight and advanced once per evaluated tick. It is the only
/// stateful part of the binding, and it is stateful in ticks rather than in seconds: two runs that
/// pass the same ticks get the same pose, whatever the frame rate said.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct Settle {
    shown: BlendFrame,
    /// The pose the current window started from, which is what a linear ramp interpolates from.
    entry: BlendFrame,
    ticks: u32,
    moving: bool,
}

impl Settle {
    /// A body already drawn in `pose`, for a caller that spawns mid-walk — and for the tests.
    #[must_use]
    pub const fn at(pose: BlendFrame) -> Self {
        Self {
            shown: pose,
            entry: pose,
            ticks: 0,
            moving: false,
        }
    }

    /// The pose this body is drawn in.
    #[must_use]
    pub const fn shown(&self) -> BlendFrame {
        self.shown
    }

    /// How many ticks this body has spent in its current window.
    #[must_use]
    pub const fn ticks(&self) -> u32 {
        self.ticks
    }

    /// Whether the body was moving when its window last restarted.
    #[must_use]
    pub const fn moving(&self) -> bool {
        self.moving
    }

    /// Advance one tick toward `target` and return the pose to write.
    ///
    /// The pose interpolates from the one the body held when the window opened, so a six-tick start
    /// is exactly half committed after three ticks and exactly arrived after six. A window of zero
    /// ticks closes the distance in one tick, which is the answer the blend itself gives — the ease
    /// is a no-op unless the game authored one.
    pub fn advance(
        &mut self,
        target: BlendFrame,
        moving: bool,
        windows: BlendWindows,
    ) -> BlendFrame {
        if moving != self.moving {
            self.moving = moving;
            self.ticks = 0;
            self.entry = self.shown;
        }
        self.ticks = self.ticks.saturating_add(1);
        self.shown = self
            .entry
            .eased_toward(target, windows.progress(moving, self.ticks));
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::{BlendWindows, Settle};
    use crate::binding::BlendFrame;
    use crate::profile::{ClipSlot, LocomotionProfile};
    use bevy_flock::domain::num;

    fn profile(windows: BlendWindows) -> LocomotionProfile {
        LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0).map_or_else(
            |_| unreachable!("the fixture profile is valid"),
            |profile| profile.with_windows(windows),
        )
    }

    fn window(windows: BlendWindows) -> BlendWindows {
        BlendWindows::try_new(windows.start_ticks(), windows.stop_ticks())
            .unwrap_or_else(|_| unreachable!("the fixture windows are valid"))
    }

    fn blend_at(profile: &LocomotionProfile, speed: f32) -> BlendFrame {
        let mut frame = BlendFrame::default();
        frame.fill(profile, speed, 0.0);
        frame
    }

    #[test]
    fn an_immediate_profile_snaps_to_the_blend() {
        let profile = profile(BlendWindows::immediate());
        let mut settle = Settle::default();
        let target = blend_at(&profile, 1.4);
        let shown = settle.advance(target, true, profile.windows());
        assert!((shown.weight(ClipSlot::Walk) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_start_commits_over_the_configured_ticks() {
        let windows = window(BlendWindows::try_new(6, 8).unwrap_or_default());
        let profile = profile(windows);
        let target = blend_at(&profile, 1.4);
        let mut settle = Settle::default();
        // Sixths of the way there: the body is idling, and each tick moves a sixth of what is left.
        let mut shown = settle.shown();
        for tick in 1..=6 {
            shown = settle.advance(target, true, windows);
            let expected = num::u32_to_f32(tick) / 6.0;
            assert!(
                (shown.weight(ClipSlot::Walk) - expected).abs() < 1e-5,
                "tick {tick} should be {expected}, got {}",
                shown.weight(ClipSlot::Walk)
            );
        }
        assert!(
            (shown.weight(ClipSlot::Walk) - 1.0).abs() < 1e-5,
            "six ticks commits the walk exactly"
        );
    }

    #[test]
    fn a_stop_lets_go_over_the_configured_ticks() {
        let windows = window(BlendWindows::try_new(0, 8).unwrap_or_default());
        let profile = profile(windows);
        let walking = blend_at(&profile, 1.4);
        let mut settle = Settle::at(walking);
        let stopped = blend_at(&profile, 0.0);
        let mut shown;
        for tick in 1..=7 {
            shown = settle.advance(stopped, false, windows);
            let expected = 1.0 - num::u32_to_f32(tick) / 8.0;
            assert!(
                (shown.weight(ClipSlot::Walk) - expected).abs() < 1e-5,
                "tick {tick} of the stop should leave {expected}, got {}",
                shown.weight(ClipSlot::Walk)
            );
        }
        shown = settle.advance(stopped, false, windows);
        assert!(
            shown.weight(ClipSlot::Walk).abs() < 1e-6,
            "eight ticks lets the walk go"
        );
        assert!(
            (shown.weight(ClipSlot::Idle) - 1.0).abs() < 1e-6,
            "and idle carries it"
        );
    }

    #[test]
    fn an_absurd_window_is_refused() {
        assert!(BlendWindows::try_new(0, 0).is_ok());
        assert!(BlendWindows::try_new(241, 4).is_err());
        assert!(BlendWindows::try_new(4, 241).is_err());
    }

    #[test]
    fn the_progress_is_per_tick_and_lands_on_the_window() {
        let windows = window(BlendWindows::try_new(4, 2).unwrap_or_default());
        assert!((windows.progress(true, 1) - 0.25).abs() < 1e-6);
        assert!((windows.progress(false, 1) - 0.5).abs() < 1e-6);
        assert!((windows.progress(true, 99) - 1.0).abs() < 1e-6, "clamped");
        assert!((BlendWindows::immediate().progress(true, 0) - 1.0).abs() < 1e-6);
    }
}
