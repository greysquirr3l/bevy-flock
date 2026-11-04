//! The logical clock.
//!
//! Simulation time is a `u64` counter that the simulation itself advances. It
//! is never derived from wall-clock elapsed time, because a replay that depends
//! on how long a frame took is not a replay.

use core::fmt;
use core::num::NonZeroU32;

use crate::error::ConfigError;

/// A simulation tick.
///
/// Ticks are the only unit of time in the simulation. Systems that need seconds
/// convert at the edge via [`TicksPerSecond::delta_seconds`], and any state that
/// would otherwise be stored as elapsed seconds is stored as a tick count so it
/// survives a replay exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tick(u64);

impl Tick {
    /// The tick a simulation starts on, before anything has run.
    pub const ZERO: Self = Self(0);

    /// Wrap a raw tick number.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw tick number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next tick.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// How many ticks have elapsed since `earlier`, saturating at zero.
    #[must_use]
    pub const fn since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// Whether at least `interval` ticks have passed since `earlier`.
    ///
    /// This is the idiom for cooldowns and staggered thinking: compare ticks,
    /// never accumulate float seconds.
    #[must_use]
    pub const fn elapsed_at_least(self, earlier: Self, interval: u64) -> bool {
        self.since(earlier) >= interval
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

/// How many simulation ticks make up one second of game time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TicksPerSecond(NonZeroU32);

impl TicksPerSecond {
    /// Sixty ticks per second, the default for a real-time game.
    pub const SIXTY: Self = Self(NonZeroU32::MIN.saturating_add(59));

    /// One hundred and twenty ticks per second, for fighting-game precision.
    pub const ONE_TWENTY: Self = Self(NonZeroU32::MIN.saturating_add(119));

    /// Build a tick rate.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ZeroTickRate`] for zero.
    pub const fn try_new(ticks: u32) -> Result<Self, ConfigError> {
        match NonZeroU32::new(ticks) {
            Some(nonzero) => Ok(Self(nonzero)),
            None => Err(ConfigError::ZeroTickRate),
        }
    }

    /// A tick rate that is known to be non-zero.
    #[must_use]
    pub const fn new(nonzero: NonZeroU32) -> Self {
        Self(nonzero)
    }

    /// The raw rate.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    /// Seconds of game time in one tick.
    ///
    /// The `f32` conversion is inherent to the vector math this feeds; it is
    /// computed from an integer divisor so the result is identical on every
    /// platform that rounds correctly.
    #[must_use]
    pub fn delta_seconds(self) -> f32 {
        1.0_f32 / self.get() as f32
    }

    /// How many ticks are needed to cover `ticks` ticks' worth of scheduling at
    /// this rate, used by the think scheduler to express budgets in ticks.
    #[must_use]
    pub const fn interval_for_hz(self, hz: u32) -> u64 {
        if hz == 0 {
            return 1;
        }
        let rate = self.get();
        let interval = rate / hz;
        if interval == 0 { 1 } else { interval as u64 }
    }
}

impl Default for TicksPerSecond {
    fn default() -> Self {
        Self::SIXTY
    }
}

impl fmt::Display for TicksPerSecond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}Hz", self.0.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_tick_rate_is_rejected() {
        assert_eq!(TicksPerSecond::try_new(0), Err(ConfigError::ZeroTickRate));
    }

    #[test]
    fn delta_seconds_matches_the_rate() {
        assert!((TicksPerSecond::SIXTY.delta_seconds() - (1.0 / 60.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn since_saturates_instead_of_underflowing() {
        assert_eq!(Tick::new(5).since(Tick::new(9)), 0);
        assert_eq!(Tick::new(9).since(Tick::new(5)), 4);
    }

    #[test]
    fn elapsed_at_least_is_inclusive() {
        assert!(Tick::new(10).elapsed_at_least(Tick::new(8), 2));
        assert!(!Tick::new(9).elapsed_at_least(Tick::new(8), 2));
    }

    #[test]
    fn interval_for_hz_never_returns_zero() {
        assert_eq!(TicksPerSecond::SIXTY.interval_for_hz(120), 1);
        assert_eq!(TicksPerSecond::SIXTY.interval_for_hz(30), 2);
        assert_eq!(TicksPerSecond::SIXTY.interval_for_hz(0), 1);
    }

    #[test]
    fn next_wraps_rather_than_panicking() {
        assert_eq!(Tick::new(u64::MAX).next(), Tick::ZERO);
    }
}
