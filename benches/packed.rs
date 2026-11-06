//! Does steering over arrays buy the frame back?
//!
//! ADR 0018 gate 8 failed because a 5,000-entity band costs 9.47 ms in the ECS crowd path.
//! This measures the same arithmetic in [`bevy_flock::application::packed`]: contiguous
//! positions, a linked-list bucket grid, no component access. If the packed path is well
//! under a millisecond at 5,000, the gate's failure is the wiring, not the arithmetic, and
//! ADR 0020 specifies the wiring.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use bevy_flock::application::packed::{Pack, SteerParams, scatter};
use bevy_flock::domain::num;

const fn params() -> SteerParams {
    SteerParams {
        radius: 12.0,
        separation: 4.0,
        max_speed: 6.0,
        max_force: 0.4,
        cohesion: 0.4,
        alignment: 0.6,
        separation_weight: 1.2,
        extent: 512.0,
    }
}

fn bench_packed_steer(c: &mut Criterion) {
    let mut group = c.benchmark_group("packed_steer");
    for count in [1_000_usize, 5_000, 50_000] {
        let params = params();
        let mut pack = Pack::with_capacity(count, params);
        scatter(count, params.extent, &mut pack);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(BenchmarkId::new("steer_integrate_advance", count), |b| {
            b.iter(|| {
                let report = pack.steer(&params);
                pack.integrate(params.max_speed);
                pack.advance(1.0 / 60.0, params.extent);
                black_box(report)
            });
        });
    }
    group.finish();
}

fn bench_packed_density(c: &mut Criterion) {
    // Density is the honest stress: the same count in a tenth of the area means ten times
    // the neighbours, and a bucket grid that stops helping is a grid that is wrong.
    let mut group = c.benchmark_group("packed_density");
    for extent in [512.0_f32, 256.0, 128.0] {
        let params = SteerParams { extent, ..params() };
        let mut pack = Pack::with_capacity(5_000, params);
        scatter(5_000, extent, &mut pack);
        group.bench_function(
            BenchmarkId::new("5000_agents", num::f32_to_u32(extent)),
            |b| {
                b.iter(|| black_box(pack.steer(&params)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_packed_steer, bench_packed_density);
criterion_main!(benches);
