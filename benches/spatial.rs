//! The serial spatial path, in three separable pieces, so a threaded replacement can be compared
//! against it piece by piece rather than against a frame time that hides which part moved.
//!
//! Phase 3 (multi-threaded spatial partitioning) needs this baseline before it needs a thread. If a
//! parallel build halves the index build and the query is untouched, that is a different result from
//! the reverse, and a single "frame time" number cannot tell the two apart.
//!
//! Three groups, one fixture each:
//!
//! * `spatial_build` — filling and `build()`ing a grid over the million-inhabitant volume the bands
//!   use. The entries are the ones the grid will index; the fixture is fixed so successive runs
//!   measure the build and not the scatter.
//! * `spatial_query` — a fixed set of radius queries over one frozen grid, with the output vector
//!   reused. This is the cost the steering kernel pays once per body per tick.
//! * `spatial_kernel` — the packed steering kernel over the same population, which is what consumes
//!   the query results. Including it here is what makes the three numbers a budget rather than three
//!   unrelated timings.
//!
//! `cargo bench --bench spatial` runs it; `python3 scripts/bench_gate.py --bench spatial --crate .`
//! gates it against `benches/spatial-baseline.json`.
use std::hint::black_box;

use bevy_flock::application::packed::{Pack, SteerParams, scatter};
use bevy_flock::domain::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{Neighbour, SpatialGrid, SpatialGridConfig};
use bevy_math::Vec3;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// The volume the banded continuum uses: 1 km on a side, which is the 1M-inhabitant case.
const EXTENT: f32 = 1_000.0;
/// Cell size the volumetric sweeps settled on (ADR 0031).
const CELL: f32 = 8.0;
/// Radius a body queries at, matching the packed kernel's separation radius.
const QUERY_RADIUS: f32 = 12.0;
/// Population for the scalar reference kernel. The million-body kernel is the SIMD road in
/// `benches/crowd.rs`; a scalar run at 1M takes six seconds per iteration, which is a measurement nobody
/// will wait for or act on.
const KERNEL_POPULATION: usize = 100_000;
/// Queries per timed pass: a thousand bodies asking, not a million, because a query is per body per
/// tick and the benchmark's job is the per-query cost.
const QUERIES: usize = 1_000;

/// A deterministic scatter in the unit volume scaled to `extent`, spread over the whole volume rather
/// than a plane, because the banded continuum is volumetric.
fn position(index: usize, extent: f32) -> Vec3 {
    let seed = num::i64_to_u64(num::usize_to_i64(index)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let hash = |shift: u32| {
        let bits = seed.rotate_left(shift).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        num::u64_to_f32(bits >> 40) / num::u32_to_f32(1 << 24)
    };
    Vec3::new(
        hash(0) * extent,
        hash(11) * extent * 0.25,
        hash(23) * extent,
    )
}

fn entry(index: usize) -> (NpcId, Vec3, Vec3) {
    (
        NpcId::unique(num::usize_to_u32(index)),
        position(index, EXTENT),
        Vec3::new(1.0, 0.0, 0.5),
    )
}

fn config() -> SpatialGridConfig {
    SpatialGridConfig::try_new(Vec3::ZERO, Vec3::splat(EXTENT), CELL).unwrap_or_else(|_| {
        // The constants above are valid; a failure here means the constants changed under the bench.
        unreachable!("the benchmark fixture's cell size and volume are valid")
    })
}

fn grid(population: usize) -> SpatialGrid {
    let mut grid = SpatialGrid::new(config())
        .unwrap_or_else(|_| unreachable!("the benchmark fixture's cell size and volume are valid"));
    grid.extend_from((0..population).map(entry));
    grid.build();
    grid
}

fn bench_build(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("spatial_build");
    for population in [100_000_usize, 1_000_000] {
        // Filled once, outside the loop: `build()` is what is being measured, not `extend_from`.
        let mut grid = grid(population);
        group.throughput(Throughput::Elements(num::i64_to_u64(num::usize_to_i64(
            population,
        ))));
        group.bench_with_input(
            BenchmarkId::new("build", population),
            &population,
            |bencher, _| {
                bencher.iter(|| {
                    grid.build();
                    black_box(&grid);
                });
            },
        );
    }
    group.finish();
}

fn bench_query(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("spatial_query");
    for population in [100_000_usize, 1_000_000] {
        let grid = grid(population);
        let centres: Vec<Vec3> = (0..QUERIES)
            .map(|index| position(index * 977, EXTENT))
            .collect();
        let mut out: Vec<Neighbour> = Vec::with_capacity(256);
        group.bench_with_input(
            BenchmarkId::new("radius_1k_queries", population),
            &population,
            |bencher, _| {
                bencher.iter(|| {
                    let mut found = 0_usize;
                    for centre in &centres {
                        out.clear();
                        grid.radius(*centre, QUERY_RADIUS, &mut out);
                        found += out.len();
                    }
                    black_box(found)
                });
            },
        );
    }
    group.finish();
}

fn bench_kernel(criterion: &mut Criterion) {
    let params = SteerParams {
        radius: QUERY_RADIUS,
        separation: 4.0,
        max_speed: 6.0,
        max_force: 0.4,
        cohesion: 0.4,
        alignment: 0.6,
        separation_weight: 1.2,
        extent: EXTENT,
    };
    // The scalar reference path only: see `KERNEL_POPULATION`.
    let mut pack = Pack::with_capacity(KERNEL_POPULATION, params);
    scatter(KERNEL_POPULATION, params.extent, &mut pack);
    let mut group = criterion.benchmark_group("spatial_kernel");
    group.throughput(Throughput::Elements(num::i64_to_u64(num::usize_to_i64(
        KERNEL_POPULATION,
    ))));
    group.bench_function("steer_integrate_advance/100000", |bencher| {
        bencher.iter(|| {
            let report = pack.steer(&params);
            pack.integrate(params.max_speed);
            pack.advance(1.0 / 60.0, params.extent);
            black_box(report)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_build, bench_query, bench_kernel);
criterion_main!(benches);
