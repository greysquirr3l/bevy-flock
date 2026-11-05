//! A steady 10,000-agent crowd, for profiling rather than measuring.
//!
//! `benches/crowd.rs` reports `flock_frame/10000`; this runs the identical world for as many
//! frames as you ask and prints the per-frame breakdown, so a profiler has something to sample
//! and a reader has something to compare against the bench.
//!
//! ```text
//! cargo flamegraph --profile profiling --example profile_crowd -- 2000
//! ```
use bevy_app::prelude::*;
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::domain::ids::SimSeed;
use bevy_flock::domain::population::{PopulationMode, PopulationSize};
use std::time::Instant;

fn main() {
    let frames: u32 = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(500);
    let size: usize = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(10_000);

    let config = FlockConfig {
        population: PopulationSize::clamped(size),
        spawn_budget: 1_000_000,
        mode: PopulationMode::Unique,
        seed: SimSeed::new(0xC0FF_EE00),
        ..FlockConfig::preset(CrowdPreset::CityAmbient)
    };
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(config));
    // One update drains the spawn queue, so the timed frames are steady state.
    app.update();

    let start = Instant::now();
    for _ in 0..frames {
        app.update();
    }
    let elapsed = start.elapsed();
    let per_frame = elapsed.as_secs_f64() * 1_000.0 / f64::from(frames);
    println!("{size} agents, {frames} frames: {per_frame:.3} ms/frame");
}
