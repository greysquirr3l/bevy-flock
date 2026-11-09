#![expect(
    clippy::print_stdout,
    reason = "a profiling harness reports to stdout: the numbers are the deliverable"
)]

//! How long a band tick costs, and where the camera's motion shows up in it.
//!
//! `tests/band_continuity.rs` drives 10,000 ticks and the `gate_*` tests in it were taking tens of minutes,
//! which is either the price of the fixture or a per-tick cost nobody has looked at. This example is the
//! instrument: the same app, the same seeded field and the same sine path, run for a few hundred ticks with
//! every tick timed.
//!
//! ```text
//! cargo run --release --example band_ticks -- --ticks 400
//! cargo run --release --example band_ticks -- --ticks 400 --still
//! ```
//!
//! `--still` holds the camera, which separates the two halves of the cost: a still camera never crosses the band
//! edge, so nothing folds or manifests and the figure is the steering kernel plus the systems' own scans. A moving
//! camera pays the ration's spawns and despawns on top. If the two figures are the same, the cost is per-tick work
//! rather than entity churn, and the fixture can be made cheaper by shrinking what is constant per tick.
//!
//! One `meta` record, then one `tick` summary and one `bench` record, all JSON lines, in the shape
//! `examples/build_passes.rs` established so the same scripts can read either.
use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_flock::adapters::primary::band::{
    Band, BandAgent, BandAnchor, BandConfig, BandPlugin, seed_field,
};
use bevy_flock::adapters::primary::cognition::PlayerFocus;
use bevy_flock::domain::num;
use bevy_transform::prelude::Transform;
use std::time::{Duration, Instant};

/// The fixture `tests/band_continuity.rs` uses.
const SEEDED: u32 = 40_000;
/// Ticks to run unless `--ticks` says otherwise: enough for the band to reach steady state.
const DEFAULT_TICKS: u32 = 400;

/// `--ticks N`, or the default.
fn ticks() -> u32 {
    let mut args = std::env::args().skip_while(|arg| arg != "--ticks");
    args.next();
    args.next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_TICKS)
}

fn main() {
    let still = std::env::args().any(|arg| arg == "--still");
    let ticks = ticks();
    let config = BandConfig::default();

    let mut app = App::new();
    app.add_plugins(BandPlugin::new(config));
    // The camera entity is found again by `PlayerFocus` each tick, so its id is not kept.
    let _camera = app
        .world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(256.0, 256.0, 0.0)))
        .id();
    app.world_mut().spawn((
        BandAnchor,
        BandAgent::still(),
        Transform::from_xyz(10_256.0, 256.0, 0.0),
    ));
    {
        let mut band = app.world_mut().resource_mut::<Band>();
        seed_field(&mut band, SEEDED);
    }
    app.update();

    let amplitude = app.world().resource::<Band>().fold_radius;
    let (width, height) = {
        let band = app.world().resource::<Band>();
        (band.field.width(), band.field.height())
    };
    println!(
        "{{\"kind\":\"meta\",\"ticks\":{ticks},\"still\":{still},\"seeded\":{SEEDED},\
         \"field\":[{width},{height}],\"target\":{},\"ration\":{},\"radius\":{},\
         \"fold_radius\":{amplitude}}}",
        config.target,
        config.ration,
        config.radius()
    );

    let mut samples: Vec<Duration> = Vec::with_capacity(ticks as usize);
    let mut spent = Duration::ZERO;
    for tick in 0..ticks {
        if !still {
            let x = amplitude.mul_add(num::f64_to_f32((f64::from(tick) * 0.010_472).sin()), 256.0);
            let world = app.world_mut();
            let mut query = world.query_filtered::<&mut Transform, With<PlayerFocus>>();
            for mut transform in query.iter_mut(world) {
                transform.translation.x = x;
            }
        }
        let started = Instant::now();
        app.update();
        let elapsed = started.elapsed();
        spent += elapsed;
        samples.push(elapsed);
    }

    let live = {
        let band = app.world().resource::<Band>();
        band.manifested.saturating_sub(band.folded)
    };
    let population = app.world().resource::<Band>().field.population();
    samples.sort_unstable();
    let median = samples.get(samples.len() / 2).copied().unwrap_or_default();
    let p95 = samples
        .get(samples.len() * 95 / 100)
        .copied()
        .unwrap_or_default();
    let worst = samples.last().copied().unwrap_or_default();
    let mean = spent.div_f64(f64::from(ticks));
    println!(
        "{{\"kind\":\"tick\",\"median_ms\":{},\"p95_ms\":{},\"max_ms\":{},\"mean_ms\":{},\
         \"live\":{live},\"population\":{population},\"entities\":{}}}",
        median.as_secs_f64() * 1_000.0,
        p95.as_secs_f64() * 1_000.0,
        worst.as_secs_f64() * 1_000.0,
        mean.as_secs_f64() * 1_000.0,
        app.world().entities().len()
    );
    // The number the fixture actually pays: 10,000 ticks at this rate.
    println!(
        "{{\"kind\":\"bench\",\"projected_10k_s\":{}}}",
        mean.as_secs_f64() * 10_000.0
    );
}
