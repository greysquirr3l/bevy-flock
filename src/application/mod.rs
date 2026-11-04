//! Orchestration state that is not domain logic and not Bevy-facing glue.
//!
//! Today this is the simulation clock. It lives between the two because it owns
//! the one thing that must never leak: the tick counter. Domain functions receive
//! a [`Tick`] as an argument; they cannot read the clock, so they cannot depend
//! on when they were called.

use bevy_ecs::prelude::*;

use crate::domain::tick::{Tick, TicksPerSecond};

/// The logical clock the simulation advances.
///
/// A Bevy resource because systems need shared access, but the value it holds is
/// a plain domain type. It is advanced by the simulation, never by wall-clock
/// elapsed time — a frame that takes 40 ms must not skip a tick, because a replay
/// would take a different path through the world.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimClock {
    /// The tick currently being simulated.
    pub tick: Tick,
    /// How many ticks make a second.
    pub rate: TicksPerSecond,
}

impl SimClock {
    /// A clock at tick zero.
    #[must_use]
    pub const fn new(rate: TicksPerSecond) -> Self {
        Self {
            tick: Tick::ZERO,
            rate,
        }
    }

    /// Seconds of game time in one tick.
    #[must_use]
    pub fn delta_seconds(&self) -> f32 {
        self.rate.delta_seconds()
    }

    /// Advance exactly one tick.
    pub const fn advance(&mut self) {
        self.tick = self.tick.next();
    }
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new(TicksPerSecond::SIXTY)
    }
}

/// Advance the simulation clock by one tick.
///
/// Runs last in the frame so every system that reads `clock.tick` sees the tick
/// they are actually simulating.
pub fn advance_clock(mut clock: ResMut<SimClock>) {
    clock.advance();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_starts_at_zero_and_advances_by_one() {
        let mut clock = SimClock::default();
        assert_eq!(clock.tick, Tick::ZERO);
        clock.advance();
        assert_eq!(clock.tick, Tick::new(1));
    }

    #[test]
    fn delta_seconds_comes_from_the_rate() {
        let clock = SimClock::new(TicksPerSecond::ONE_TWENTY);
        assert!((clock.delta_seconds() - (1.0 / 120.0)).abs() < f32::EPSILON);
    }
}
