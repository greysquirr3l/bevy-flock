//! Gate 8: 100,000 inhabitants with 5,000 manifested, under an 8 ms frame.
#![expect(clippy::expect_used, reason = "fixtures fail loudly")]

use std::hint::black_box;

use bevy_app::prelude::*;
use bevy_transform::prelude::Transform;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use bevy_flock::adapters::primary::band::{Band, BandAgent, BandConfig, BandPlugin, seed_field};
use bevy_flock::adapters::primary::cognition::PlayerFocus;

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
        ((state >> 33) as f32) / (u32::MAX >> 1) as f32
    };
    for _ in 0..manifested {
        let angle = next() * std::f32::consts::TAU;
        let distance = radius.sqrt() * next() * radius.sqrt();
        app.world_mut().spawn((
            BandAgent::still(),
            Transform::from_xyz(
                256.0 + angle.cos() * distance,
                256.0 + angle.sin() * distance,
                0.0,
            ),
        ));
    }
    app
}

fn bench_band(c: &mut Criterion) {
    let mut group = c.benchmark_group("band_frame");
    for (total, manifested) in [(100_000_u32, 5_000_u32), (250_000, 5_000)] {
        let mut app = world(total, manifested);
        for _ in 0..4 {
            app.update();
        }
        group.throughput(Throughput::Elements(u64::from(total)));
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
