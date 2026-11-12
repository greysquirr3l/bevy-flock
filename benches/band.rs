//! The multi-band frame (ADR 0021): a near band of manifested agents, a mid band on a
//! cadence, and everybody else as a density field.
//!
//! The three populations share a 5,000-agent near band and a 15,000-agent mid band, so the
//! only thing that changes between them is how many inhabitants exist as density. That is
//! the property the published numbers rest on: the frame tracks what is manifested, not
//! what exists.

use std::hint::black_box;

use bevy_app::prelude::*;
use bevy_transform::prelude::Transform;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use bevy_flock::adapters::primary::band::{
    Band, BandAgent, BandConfig, BandPlugin, MidAgent, seed_field,
};
use bevy_flock::adapters::primary::cognition::PlayerFocus;
use bevy_flock::domain::num;

fn world(total: u32, manifested: u32) -> App {
    let config = BandConfig::default();
    let mut app = App::new();
    app.add_plugins(BandPlugin::new(config));
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(256.0, 256.0, 0.0)));

    // Everything not manifested lives as density.
    {
        let mut band = app.world_mut().resource_mut::<Band>();
        seed_field(&mut band, total - manifested);
        let _ = &mut band.field;
    }

    // The band itself: spread over the disc the config derives.
    let radius = config.radius();
    let mut state = 0x1234_5678_u64;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        num::u64_to_f32(state >> 33) / num::u32_to_f32(u32::MAX >> 1)
    };
    for _ in 0..manifested {
        let angle = next() * std::f32::consts::TAU;
        let distance = radius.sqrt() * next() * radius.sqrt();
        app.world_mut().spawn((
            BandAgent::still(),
            Transform::from_xyz(
                angle.cos().mul_add(distance, 256.0),
                angle.sin().mul_add(distance, 256.0),
                0.0,
            ),
        ));
    }
    app
}

fn bench_band(c: &mut Criterion) {
    let mut group = c.benchmark_group("band_frame");
    for (total, manifested) in [
        (100_000_u32, 5_000_u32),
        (250_000, 5_000),
        (1_000_000, 5_000),
    ] {
        let mut app = world(total, manifested);
        for _ in 0..4 {
            app.update();
        }
        group.throughput(Throughput::Elements(u64::from(total)));
        // Mid band: 15,000 agents at 20 Hz, seeded once outside the timed loop.
        // A ring from 400 m to 1,100 m: 15,000 agents over ~3.5 km², which is the density
        // the mid band is specified at. The previous ring was 300-800 m and the pack's grid
        // only covered 512 m, so every mid agent fell outside it.
        for index in 0..15_000_u32 {
            let angle = num::u32_to_f32(index) * 0.000_419;
            let distance = 400.0 + num::u32_to_f32(index % 700);
            app.world_mut().spawn((
                MidAgent {
                    velocity: [0.4, 0.1],
                },
                Transform::from_xyz(
                    angle.cos().mul_add(distance, 256.0),
                    angle.sin().mul_add(distance, 256.0),
                    0.0,
                ),
            ));
        }
        group.bench_function(format!("{total}_total_{manifested}_manifested"), |b| {
            b.iter(|| {
                app.update();
                black_box(app.world().resource::<Band>().manifested)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_band);
criterion_main!(benches);
