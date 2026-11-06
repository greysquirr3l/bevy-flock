//! The gates in ADR 0018 that can be measured today.
//!
//! The continuum's domain half exists ([`bevy_flock::domain::continuum`]); its ECS
//! integration — spawning on manifest, despawning on fold, and the manifest band as a
//! plugin — does not. So this bench measures the half that is real and reports the halves
//! that are not, rather than dressing a proposal as a result.
//!
//! | Gate | What it needs | Status here |
//! | --- | --- | --- |
//! | 1. 1M ambient, 2k manifested, frame < 6 ms | advection + manifest ids | measured |
//! | 2. Conservation over 10,000 strides | advection | measured (unit test) |
//! | 3. Determinism | manifest iterator | measured (unit test) |
//! | 8. 100k with 5,000 manifested, frame < 8 ms | band plugin, folding | **open** |
//! | 9. Folded NPC matches no query | folding | **open** |

use bevy_flock::testkit::OrDie;
use std::hint::black_box;

use bevy_app::prelude::*;
use bevy_transform::prelude::Transform;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use bevy_ecs::prelude::Resource;
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::domain::continuum::{AmbientField, CellId, Occupation};
use bevy_flock::domain::tick::Tick;
use bevy_flock::prelude::PopulationSize;

/// The domain type stays free of Bevy, so the bench wraps it. The plugin would do the
/// same thing in `adapters`, which is where the ECS belongs.
#[derive(Resource)]
struct FieldRes(AmbientField);

/// A field holding `people` spread evenly, at four-metre spacing.
fn field(side: u32, people_per_cell: u32) -> AmbientField {
    let mut field = AmbientField::new(side, side, 4.0, 0x5EED).or_die();
    for y in 0..side {
        for x in 0..side {
            if let Some(cell) = field.cell_mut(CellId::new(x, y)) {
                cell.absorb(people_per_cell, [0.6, 0.0], 0, Occupation::default());
            }
        }
    }
    field
}

fn bench_advection(c: &mut Criterion) {
    let mut group = c.benchmark_group("continuum");

    // Gate 1, the tick: 40,000 cells holding a million people, one stride of advection.
    // The target is 0.5 ms; the argument behind the whole design is that this cost does
    // not care how many people are in the cells.
    let mut million = field(200, 25);
    assert_eq!(million.population(), 1_000_000);
    group.throughput(Throughput::Elements(40_000));
    group.bench_function("advect_1m_40k_cells", |b| {
        b.iter(|| black_box(million.advect(&[0.6, 0.2], 8, 1.0 / 60.0)));
    });

    // The scaling claim, at three populations over the same grid: cost follows cells, not
    // people. If these three numbers differ materially, the design is wrong.
    for people in [1_u32, 25, 100] {
        let mut scaled = field(200, people);
        group.bench_function(BenchmarkId::new("advect_same_cells", people), |b| {
            b.iter(|| black_box(scaled.advect(&[0.6, 0.2], 8, 1.0 / 60.0)));
        });
    }

    // A sparse city: most cells empty, which is the case real worlds present.
    let mut sparse = AmbientField::new(200, 200, 4.0, 3).or_die();
    for i in 0..2_000_u32 {
        let x = (i * 37) % 200;
        let y = (i * 53) % 200;
        if let Some(cell) = sparse.cell_mut(CellId::new(x, y)) {
            cell.absorb(25, [0.0, 0.0], 0, Occupation::default());
        }
    }
    group.bench_function("advect_50k_sparse", |b| {
        b.iter(|| black_box(sparse.advect(&[0.6, 0.2], 8, 1.0 / 60.0)));
    });

    group.finish();
}

fn bench_manifest(c: &mut Criterion) {
    let mut group = c.benchmark_group("continuum_manifest");
    group.throughput(Throughput::Elements(2_000));
    let sample = field(200, 25);
    let cell = CellId::new(100, 100);
    group.bench_function("manifest_2000", |b| {
        b.iter(|| black_box(sample.manifest(cell, 2_000, Tick::new(1_000)).count()));
    });

    // Folding 2,000 people back into one cell, which is the inverse and must be as cheap.
    let mut folding = field(200, 25);
    group.bench_function("fold_2000", |b| {
        b.iter(|| {
            folding.fold(cell, 2_000, [0.6, 0.0], 0);
            black_box(folding.cell(cell).map(|c| c.population()))
        });
    });
    group.finish();
}

/// Gate 8's entity half: what the crowd costs when only the manifest band is entities.
///
/// This is **not** the gate. The gate needs the band plugin that folds the other 95,000
/// out of the ECS, and until that exists this measures the part that would carry it: five
/// thousand NPCs, steered and partitioned as usual, with the continuum's advection paid
/// on its stride. What it does prove is that five thousand entities are nowhere near the
/// frame budget, so the remaining risk in gate 8 is the plugin, not the crowd.
fn bench_band(c: &mut Criterion) {
    let mut group = c.benchmark_group("continuum_band");

    for manifested in [1_000_u32, 5_000] {
        let mut app = App::new();
        let config = FlockConfig {
            population: PopulationSize::try_new(manifested as usize).or_die(),
            ..FlockConfig::preset(CrowdPreset::CityAmbient)
        };
        app.add_plugins(FlockPlugin::new(config));
        app.insert_resource(FieldRes(field(200, 24))); // 960,000 in the cells
        app.world_mut().spawn(Transform::default());

        // Warm up: spawns are budgeted, so the first frames are not steady state.
        for _ in 0..8 {
            app.update();
        }

        let mut stride = 0_u32;
        group.throughput(Throughput::Elements(u64::from(manifested)));
        group.bench_function(BenchmarkId::new("frame_plus_advection", manifested), |b| {
            b.iter(|| {
                app.update();
                // Advect one frame in eight, as the plugin would.
                if stride.is_multiple_of(8)
                    && let Some(mut field) = app.world_mut().get_resource_mut::<FieldRes>()
                {
                    field.0.advect(&[0.6, 0.2], 8, 1.0 / 60.0);
                }
                stride = stride.wrapping_add(1);
                black_box(manifested)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_advection, bench_manifest, bench_band);
criterion_main!(benches);
