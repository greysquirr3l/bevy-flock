//! The simulation clock as a system.
//!
//! `SimClock` is a core value: a tick counter and a rate, with no opinion about when it is advanced. Advancing it is
//! engine plumbing, so the system lives here rather than beside the type — which also keeps `application` free of any
//! `bevy_ecs` type in its signatures, and lets the core crate build with the `ecs` feature off.
use bevy_ecs::prelude::ResMut;

use crate::application::SimClock;

/// Advance the simulation clock by one tick.
///
/// Runs last in the frame so every system that reads `clock.tick` sees the tick it is actually simulating.
pub fn advance_clock(mut clock: ResMut<SimClock>) {
    clock.advance();
}
