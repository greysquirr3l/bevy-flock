//! A steady 1,000,000-inhabitant band, for profiling rather than measuring.
//!
//! `benches/band.rs` reports a frame time; this exists so `cargo flamegraph` has something
//! that runs long enough to sample. It builds the ADR 0021 layout — 5,000 near agents at
//! 60 Hz, 15,000 mid agents at 20 Hz, everything else as density — and runs the same frame
//! the bench measures, without criterion's timing overhead in the profile.
//!
//! Usage:
//!
//! ```text
//! cargo flamegraph --example profile_band -- --frames 2000
//! ```

use std::time::Instant;

use bevy_app::prelude::*;
use bevy_transform::prelude::Transform;

use bevy_flock::adapters::primary::band::{
    Band, BandAgent, BandConfig, BandPlugin, MidAgent, seed_field,
};
use bevy_flock::adapters::primary::cognition::PlayerFocus;
use bevy_flock::domain::num;

const NEAR: u32 = 5_000;
const MID: u32 = 15_000;
const TOTAL: u32 = 1_000_000;

fn main() {
    let frames = std::env::args()
        .nth(1)
        .and_then(|flag| {
            flag.strip_prefix("--frames=")
                .map(str::to_owned)
                .or(Some(flag))
        })
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1_000);

    let config = BandConfig::default();
    let mut app = App::new();
    app.add_plugins(BandPlugin::new(config));
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(512.0, 512.0, 0.0)));

    {
        let mut band = app.world_mut().resource_mut::<Band>();
        seed_field(&mut band, TOTAL - NEAR);
    }

    let radius = config.radius();
    let mut state = 0x1234_5678_u64;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        num::u64_to_f32(state >> 33) / num::u32_to_f32(u32::MAX >> 1)
    };
    for _ in 0..NEAR {
        let angle = next() * std::f32::consts::TAU;
        let distance = radius.sqrt() * next() * radius.sqrt();
        app.world_mut().spawn((
            BandAgent::still(),
            Transform::from_xyz(
                angle.cos().mul_add(distance, 512.0),
                angle.sin().mul_add(distance, 512.0),
                0.0,
            ),
        ));
    }
    for index in 0..MID {
        let angle = num::u32_to_f32(index) * 0.000_419;
        let distance = 400.0 + num::u32_to_f32(index % 700);
        app.world_mut().spawn((
            MidAgent {
                velocity: [0.4, 0.1],
            },
            Transform::from_xyz(
                angle.cos().mul_add(distance, 512.0),
                angle.sin().mul_add(distance, 512.0),
                0.0,
            ),
        ));
    }

    for _ in 0..8 {
        app.update();
    }

    let started = Instant::now();
    for _ in 0..frames {
        app.update();
    }
    let elapsed = started.elapsed();
    let per_frame = elapsed.as_secs_f64() * 1_000.0 / f64::from(frames);
    println!(
        "{frames} frames, {TOTAL} inhabitants ({NEAR} near, {MID} mid): {per_frame:.3} ms/frame"
    );

    let band = app.world().resource::<Band>();
    println!(
        "manifested {}, folded {}, density {}",
        band.manifested,
        band.folded,
        band.field.population()
    );
}
