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

use bevy_flock::testkit::OrDie;
use std::hint::black_box;

use bevy_app::App;
use bevy_flock::application::avoidance::AvoidanceParams;
use bevy_flock::application::crowd::{Crowd, CrowdParams, SteeringRoad};
use bevy_flock::boids::BoidsConfig;
use bevy_flock::domain::ids::{NpcId, SimSeed};
use bevy_flock::domain::rng::RngStream;
use bevy_flock::domain::spatial::{GridEntry, Neighbour, SpatialGrid, SpatialGridConfig};
use bevy_flock::domain::steering::{AgentState, SteeringParams, desired_velocity};
use bevy_flock::prelude::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// A running crowd with every NPC already spawned.
fn crowd(size: usize) -> App {
    crowd_on(size, None)
}

/// The same crowd, steered on a chosen road (ADR 0044).
fn crowd_on(size: usize, road: Option<SteeringRoad>) -> App {
    let mut app = App::new();
    let config = FlockConfig {
        population: PopulationSize::clamped(size),
        spawn_budget: 1_000_000,
        mode: PopulationMode::Unique,
        seed: SimSeed::new(0xC0FF_EE00),
        ..FlockConfig::preset(CrowdPreset::CityAmbient)
    };
    app.add_plugins(FlockPlugin::new(config));
    if let Some(road) = road {
        let boids = app.world().resource::<BoidsConfig>().with_road(road);
        app.insert_resource(boids);
    }
    // One update to drain the spawn queue so the measured frames are steady
    // state, not spawn work.
    app.update();
    app
}

fn random_grid(size: usize) -> SpatialGrid {
    let mut rng = RngStream::new(SimSeed::new(7), 7);
    let mut grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
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

/// The same frame on each road, so the choice in ADR 0044 rests on a measurement.
fn bench_roads(c: &mut Criterion) {
    let mut group = c.benchmark_group("flock_frame_road");
    group.sample_size(10);
    for size in [10_000_usize, 100_000] {
        for (label, road) in [
            ("auto", None),
            ("wide", Some(SteeringRoad::Wide)),
            ("threads", Some(SteeringRoad::Threads)),
            ("scalar", Some(SteeringRoad::Scalar)),
        ] {
            group.throughput(Throughput::Elements(size as u64));
            group.bench_with_input(
                BenchmarkId::new(label, size),
                &(size, road),
                |b, &(size, road)| {
                    let mut app = crowd_on(size, road);
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
    let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
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

/// What a personality costs a frame (ADR 0051).
///
/// The delta between the two rows is the whole of it: one array lookup, five multiplies and a
/// wider `desired_for` argument, per agent. The biased row carries a bias on *every* agent, which
/// is the expensive shape a crowd of individuals produces; a crowd where only the near ring has a
/// personality pays a fraction of it.
fn bench_temperament(c: &mut Criterion) {
    let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
    let params = CrowdParams::from_steering(&SteeringParams::human_scale());
    let mut group = c.benchmark_group("crowd_temperament");
    group.throughput(Throughput::Elements(10_000));
    let build = |biased: bool| {
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        crowd.begin();
        for index in 0..10_000_u32 {
            let step = index as f32;
            let ring = 1.6 * libm::sqrtf(step);
            let angle = step * 2.399_963_2;
            crowd.push(
                [
                    libm::cosf(angle) * ring,
                    libm::sinf(step * 0.7) * 20.0,
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
            if biased {
                // A spread wide enough to matter: the band a real temperament produces.
                let swing = (index % 7) as f32 / 7.0;
                crowd.set_bias(
                    index as usize,
                    [0.6 + swing, 0.7 + swing * 0.6, 1.4 - swing, 0.9, 1.1],
                );
            }
        }
        crowd
    };
    for (label, biased) in [("uniform", false), ("every_agent_biased", true)] {
        let crowd = build(biased);
        group.bench_function(label, |b| {
            b.iter(|| {
                let mut frame = crowd.clone();
                black_box(frame.steer(&params));
            });
        });
    }
    group.finish();
}

/// Reciprocal avoidance on and off, at the two populations the plan names (ADR 0045).
///
/// The number that matters is the delta between the two rows: the pass reuses the cell order
/// steering already built, so it is one neighbour sweep and a double-buffered write, not a
/// second spatial query. Ten samples at both sizes, because criterion refuses fewer.
fn bench_avoidance(c: &mut Criterion) {
    let mut group = c.benchmark_group("crowd_avoidance");
    let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
    let params = CrowdParams::from_steering(&SteeringParams::human_scale());
    let avoidance = AvoidanceParams::new();
    for size in [10_000_usize, 100_000] {
        group.sample_size(10);
        group.throughput(Throughput::Elements(size as u64));
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        crowd.begin();
        for index in 0..u32::try_from(size).or_die() {
            let step = index as f32;
            let ring = 1.6 * libm::sqrtf(step);
            let angle = step * 2.399_963_2;
            crowd.push(
                [
                    libm::cosf(angle) * ring,
                    libm::sinf(step * 0.7) * 4.0,
                    libm::sinf(angle) * ring,
                ],
                [0.1, 0.0, -0.1],
                if index % 3 == 0 {
                    Some([30.0, 2.0, -30.0])
                } else {
                    None
                },
                true,
            );
        }
        group.bench_with_input(BenchmarkId::new("steer_only", size), &crowd, |b, crowd| {
            b.iter(|| {
                let mut frame = crowd.clone();
                black_box(frame.steer(&params));
            });
        });
        group.bench_with_input(
            BenchmarkId::new("steer_and_avoid", size),
            &crowd,
            |b, crowd| {
                b.iter(|| {
                    let mut frame = crowd.clone();
                    black_box(frame.steer(&params));
                    black_box(frame.avoid(&avoidance));
                });
            },
        );
    }
    group.finish();
}

/// Perception, its index, and obstacle rebuilds: the costs ADRs 0046 and 0047 promise to record.
///
/// Audiences are read-only, so this measures the pass a game would pay at a mid-band cadence:
/// 512 observers against a full 4,096-record buffer in two densities, the same through the bucket
/// index, the index rebuild itself, and a 64x64 obstacle rebuild with 32 obstacles.
/// The fixtures every perception case shares: two densities of the same 4,096 records, an index
/// over each, and the query.
struct PerceptionFixtures {
    /// Records clustered inside 40 m.
    buffer: bevy_flock::domain::stimulus::StimulusBuffer,
    /// The same records spread over 400 m.
    spread: bevy_flock::domain::stimulus::StimulusBuffer,
    /// The index over `buffer`.
    index: bevy_flock::domain::stimulus::StimulusIndex,
    /// The index over `spread`.
    spread_index: bevy_flock::domain::stimulus::StimulusIndex,
    /// The query both paths run.
    query: bevy_flock::domain::stimulus::PerceptionQuery,
}

fn perception_fixtures() -> PerceptionFixtures {
    use bevy_flock::domain::stimulus::{
        PerceptionQuery, Stimulus, StimulusBuffer, StimulusIndex, StimulusKind,
    };
    use bevy_flock::domain::tick::Tick;
    use bevy_math::Vec3;

    let mut buffer = StimulusBuffer::new(4_096).or_die();
    for index in 0..4_096_u32 {
        let angle = index as f32 * 0.37;
        let radius = 2.0 + (index % 40) as f32;
        buffer.push(Stimulus::new(
            if index % 3 == 0 {
                StimulusKind::Sound
            } else {
                StimulusKind::Sight
            },
            Vec3::new(libm::cosf(angle) * radius, 0.0, libm::sinf(angle) * radius),
            30.0,
            0.8,
            Tick::ZERO,
        ));
    }
    // The same records over 400 m instead of 40: almost nothing is in range, and a linear scan
    // cannot tell. This is the measurement that says the cost is spatial-blind.
    let mut spread = StimulusBuffer::new(4_096).or_die();
    for record in buffer.iter() {
        spread.push(Stimulus {
            position: record.position * 10.0,
            ..*record
        });
    }
    let mut index = StimulusIndex::new();
    index.rebuild(&buffer, Tick::ZERO, 22.5);
    let mut spread_index = StimulusIndex::new();
    spread_index.rebuild(&spread, Tick::ZERO, 22.5);
    PerceptionFixtures {
        buffer,
        spread,
        index,
        spread_index,
        query: PerceptionQuery::new(),
    }
}

fn bench_perception(c: &mut Criterion) {
    use bevy_flock::domain::obstacle_nav::{ObstacleFootprint, ObstacleNav, ObstacleNavConfig};
    use bevy_flock::domain::stimulus::{
        OpenAir, StimulusBuffer, StimulusIndex, perceive, perceive_indexed,
    };
    use bevy_flock::domain::tick::Tick;
    use bevy_math::Vec3;

    let PerceptionFixtures {
        buffer,
        spread,
        index,
        spread_index,
        query,
    } = perception_fixtures();
    let mut group = c.benchmark_group("perception");
    group.sample_size(20);
    let observers = |b: &mut criterion::Bencher, buffer: &StimulusBuffer| {
        b.iter(|| {
            let mut seen = 0_usize;
            for observer_index in 0..512_u32 {
                let angle = observer_index as f32 * 0.11;
                let observer = Vec3::new(libm::cosf(angle) * 8.0, 0.0, libm::sinf(angle) * 8.0);
                seen += perceive(buffer, observer, Vec3::X, Tick::ZERO, &query, &OpenAir).len();
            }
            black_box(seen);
        });
    };
    group.bench_function("512_observers_4096_records", |b| observers(b, &buffer));
    group.bench_function("512_observers_4096_records_spread_400m", |b| {
        observers(b, &spread);
    });
    group.bench_function("512_observers_4096_records_indexed", |b| {
        b.iter(|| {
            let mut seen = 0_usize;
            for observer_index in 0..512_u32 {
                let angle = observer_index as f32 * 0.11;
                let observer = Vec3::new(libm::cosf(angle) * 8.0, 0.0, libm::sinf(angle) * 8.0);
                seen += perceive_indexed(
                    &buffer,
                    &index,
                    observer,
                    Vec3::X,
                    Tick::ZERO,
                    &query,
                    &OpenAir,
                )
                .len();
            }
            black_box(seen);
        });
    });
    group.bench_function("512_observers_4096_records_spread_400m_indexed", |b| {
        b.iter(|| {
            let mut seen = 0_usize;
            for observer_index in 0..512_u32 {
                let angle = observer_index as f32 * 0.11;
                let observer = Vec3::new(libm::cosf(angle) * 8.0, 0.0, libm::sinf(angle) * 8.0);
                seen += perceive_indexed(
                    &spread,
                    &spread_index,
                    observer,
                    Vec3::X,
                    Tick::ZERO,
                    &query,
                    &OpenAir,
                )
                .len();
            }
            black_box(seen);
        });
    });
    group.bench_function("rebuild_4096_records", |b| {
        let mut index = StimulusIndex::new();
        b.iter(|| {
            index.rebuild(&buffer, Tick::ZERO, 22.5);
            black_box(index.live());
        });
    });

    let config = ObstacleNavConfig::try_new(Vec3::ZERO, 64, 64, 1.0)
        .or_die()
        .with_budget(1_000_000);
    let obstacles: Vec<ObstacleFootprint> = (0..32)
        .map(|index| {
            let x = 2.0 + (index % 8) as f32 * 7.5;
            let z = 2.0 + (index / 8) as f32 * 15.0;
            ObstacleFootprint::new(Vec3::new(x, 0.0, z), Vec3::new(x + 3.0, 2.0, z + 6.0)).or_die()
        })
        .collect();
    group.bench_function("obstacle_rebuild_64x64_32_obstacles", |b| {
        b.iter(|| {
            let mut nav = ObstacleNav::new(config);
            nav.request(&obstacles);
            while nav.is_staging() {
                nav.step();
            }
            black_box(nav.version());
        });
    });
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
    .or_die();
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
    bench_roads,
    bench_frames,
    bench_cell_size,
    bench_spatial_queries,
    bench_steering,
    bench_kernel,
    bench_avoidance,
    bench_temperament,
    bench_perception
);
criterion_main!(benches);
