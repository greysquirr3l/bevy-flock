//! Crowd benchmarks.
//!
//! Two kinds of measurement, because they answer different questions:
//!
//! - `flock_frame` measures a whole `App::update()` — spawn, grid rebuild,
//!   steering, integration — at increasing population. This is the number that
//!   decides whether a game hits 60 Hz.
//! - `spatial_query` and `steering_step` measure the two hot inner loops on
//!   their own, so a regression can be attributed to one of them instead of
//!   "the frame got slower".
//!
//! Everything runs headless (`App::new()` with no plugins beyond ours), so the
//! numbers describe the simulation and not the GPU.

#![expect(
    clippy::expect_used,
    reason = "benchmark setup asserts its own fixtures"
)]

use std::hint::black_box;

use bevy_app::App;
use bevy_flock::application::crowd::{Crowd, CrowdParams};
use bevy_flock::boids::BoidsConfig;
use bevy_flock::domain::ids::{NpcId, SimSeed};
use bevy_flock::domain::rng::RngStream;
use bevy_flock::domain::spatial::{GridEntry, Neighbour, SpatialGrid, SpatialGridConfig};
use bevy_flock::domain::steering::{AgentState, SteeringParams, desired_velocity};
use bevy_flock::prelude::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// A running crowd with every NPC already spawned.
fn crowd(size: usize) -> App {
    let mut app = App::new();
    let config = FlockConfig {
        population: PopulationSize::clamped(size),
        spawn_budget: 1_000_000,
        mode: PopulationMode::Unique,
        seed: SimSeed::new(0xC0FF_EE00),
        ..FlockConfig::preset(CrowdPreset::CityAmbient)
    };
    app.add_plugins(FlockPlugin::new(config));
    // One update to drain the spawn queue so the measured frames are steady
    // state, not spawn work.
    app.update();
    app
}

fn random_grid(size: usize) -> SpatialGrid {
    let mut rng = RngStream::new(SimSeed::new(7), 7);
    let mut grid = SpatialGrid::new(SpatialGridConfig::default()).expect("default config is valid");
    for index in 0..size {
        grid.insert(GridEntry {
            id: NpcId::unique(index as u32),
            position: bevy_math::Vec3::new(
                rng.next_range_f32(-200.0, 200.0),
                rng.next_range_f32(-200.0, 200.0),
                rng.next_range_f32(-200.0, 200.0),
            ),
            velocity: bevy_math::Vec3::ZERO,
        });
    }
    grid.build();
    grid
}

fn bench_frames(c: &mut Criterion) {
    let mut group = c.benchmark_group("flock_frame");
    group.sample_size(20);
    for size in [1_usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let mut app = crowd(size);
            b.iter(|| {
                app.update();
                black_box(app.world().resource::<FlockStats>().population());
            });
        });
    }
    group.finish();

    // 100,000 is the scale this crate claims to support headless. It gets its own
    // group with fewer samples so the suite stays runnable on a laptop.
    let mut group = c.benchmark_group("flock_frame_large");
    group.sample_size(10);
    group.throughput(Throughput::Elements(100_000));
    group.bench_function("100000", |b| {
        let mut app = crowd(100_000);
        b.iter(|| {
            app.update();
            black_box(app.world().resource::<FlockStats>().population());
        });
    });
    group.finish();
}

fn bench_spatial_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("spatial_query");
    group.sample_size(20);
    for size in [1_000_usize, 10_000, 100_000] {
        let grid = random_grid(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("radius_8m", size), &size, |b, _| {
            let mut out: Vec<Neighbour> = Vec::with_capacity(64);
            b.iter(|| {
                grid.radius(bevy_math::Vec3::ZERO, 8.0, &mut out);
                black_box(out.len());
            });
        });
    }
    group.finish();
}

fn bench_steering(c: &mut Criterion) {
    let params = SteeringParams::human_scale();
    let mut rng = RngStream::new(SimSeed::new(11), 11);
    let neighbours: Vec<Neighbour> = (0..32)
        .map(|index| {
            let position = bevy_math::Vec3::new(
                rng.next_range_f32(-8.0, 8.0),
                0.0,
                rng.next_range_f32(-8.0, 8.0),
            );
            Neighbour {
                id: NpcId::unique(index),
                position,
                velocity: bevy_math::Vec3::new(0.0, 0.0, 1.0),
                distance_squared: position.length_squared(),
            }
        })
        .collect();
    let agent = AgentState {
        position: bevy_math::Vec3::ZERO,
        velocity: bevy_math::Vec3::ZERO,
        target: Some(bevy_math::Vec3::new(40.0, 0.0, 4.0)),
    };

    let mut group = c.benchmark_group("steering_step");
    group.throughput(Throughput::Elements(1));
    group.bench_function("32_neighbours", |b| {
        b.iter(|| {
            black_box(desired_velocity(agent, &neighbours, &params));
        });
    });
    group.finish();
}

/// The kernel on its own, planar and volumetric, so ADR 0029's scalar/SIMD comparison is measured
/// on the same crowd rather than inferred from two different harnesses.
fn bench_kernel(c: &mut Criterion) {
    let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
    let params = CrowdParams::from_steering(&SteeringParams::human_scale());
    let mut group = c.benchmark_group("crowd_kernel");
    group.throughput(Throughput::Elements(10_000));

    for (label, spread, pack) in [
        ("planar", 0.0_f32, 1.6_f32),
        ("volumetric", 20.0, 1.6),
        // A market square: 10,000 people inside 30 m, which is where buckets get crowded enough
        // for four lanes to have four neighbours to work on.
        ("dense", 0.0, 0.3),
    ] {
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        crowd.begin();
        for index in 0..10_000_u32 {
            let step = index as f32;
            let ring = pack * libm::sqrtf(step);
            let angle = step * 2.399_963_2;
            crowd.push(
                [
                    libm::cosf(angle) * ring,
                    libm::sinf(step * 0.7) * spread,
                    libm::sinf(angle) * ring,
                ],
                [0.1, 0.0, -0.1],
                if index % 5 == 0 {
                    Some([25.0, 8.0, -25.0])
                } else {
                    None
                },
                true,
            );
        }
        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            b.iter(|| {
                let mut frame = crowd.clone();
                black_box(frame.steer(&params));
            });
        });
    }
    group.finish();
}

/// The ECS query path against cell size (ADR 0031's open question).
///
/// The steering kernel wants 6-10 agents a bucket, which in a one-kilometre volume means 7-9 m
/// cells — 1.4M to 2.9M cells, against the 405k the default budget allows. The kernel has been
/// measured on its own; this measures the thing that pays for the finer grid, the per-tick rebuild
/// and the neighbour queries that walk it.
fn bench_cell_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("flock_frame_cell");
    group.sample_size(10);
    for population in [10_000_usize, 100_000] {
        for (label, cell, budget) in [
            ("13.5m_2^20", 13.5_f32, 1_u32 << 20),
            ("9m_2^21", 9.0, 1 << 21),
            ("7m_2^22", 7.0, 1 << 22),
        ] {
            group.throughput(Throughput::Elements(population as u64));
            group.bench_with_input(
                BenchmarkId::new(format!("{population}"), label),
                &(population, cell, budget),
                |b, &(population, cell, budget)| {
                    let mut app = crowd_with_cell(population, cell, budget);
                    b.iter(|| {
                        app.update();
                        black_box(app.world().resource::<FlockStats>().population());
                    });
                },
            );
        }
    }
    group.finish();
}

/// A crowd whose grid uses a particular cell size and budget.
fn crowd_with_cell(size: usize, cell: f32, budget: u32) -> App {
    let mut app = App::new();
    let preset = FlockConfig::preset(CrowdPreset::CityAmbient);
    let grid = SpatialGridConfig::try_new(
        bevy_math::Vec3::splat(-500.0),
        bevy_math::Vec3::splat(500.0),
        cell,
    )
    .expect("the benchmark's volume is valid");
    let config = FlockConfig {
        population: PopulationSize::clamped(size),
        spawn_budget: 1_000_000,
        mode: PopulationMode::Unique,
        seed: SimSeed::new(0xC0FF_EE00),
        boids: BoidsConfig::default()
            .with_max_acceleration(5.0)
            .with_grid(grid.with_cell_budget(budget)),
        ..preset
    };
    app.add_plugins(FlockPlugin::new(config));
    app.update();
    app
}

criterion_group!(
    benches,
    bench_frames,
    bench_cell_size,
    bench_spatial_queries,
    bench_steering,
    bench_kernel
);
criterion_main!(benches);
