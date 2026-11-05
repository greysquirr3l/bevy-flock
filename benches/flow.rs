//! Flow fields: what a rebuild costs, and what it saves.
//!
//! A flow field is the reason a crowd can path thousands of agents to one door: the
//! search happens once per destination, not once per NPC. The cost is paid in the
//! frame a destination appears, which makes it exactly the kind of work that has to
//! be measurable rather than assumed.
//!
//! Two numbers matter: the full solve (what a game sees when a new goal appears) and
//! the cached lookup (what a game sees every frame after).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::missing_docs_in_private_items
)]

use bevy_flock::domain::flow::{FlowCache, FlowLookup, FlowTarget, solve};
use bevy_flock::domain::nav::{GridNavSource, NavCell};
use bevy_math::Vec3;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

fn grid(size: u16) -> GridNavSource {
    GridNavSource::open(Vec3::ZERO, size, size, 1.0).expect("grid")
}

fn bench_solve(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow");
    for size in [32_u16, 64, 128, 256] {
        let source = grid(size);
        let target = FlowTarget::at_cell(&source, NavCell::new(size - 1, size - 1));
        group.throughput(Throughput::Elements(u64::from(size) * u64::from(size)));
        group.bench_with_input(BenchmarkId::new("solve", size), &size, |bencher, _| {
            bencher.iter(|| {
                let field = solve(&source, target);
                std::hint::black_box(field.reachable_cells())
            });
        });
    }
    group.finish();
}

fn bench_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow");
    let size = 128_u16;
    let source = grid(size);
    let mut cache = FlowCache::new(4).expect("capacity");
    // Warm the cache with the destination every frame asks about.
    let target = FlowTarget::at_cell(&source, NavCell::new(size - 1, size - 1));
    let _ = cache.field_for(&source, target);
    group.bench_function("cached_lookup/128", |bencher| {
        bencher.iter(|| {
            let (lookup, field) = cache.field_for(&source, target);
            std::hint::black_box((lookup == FlowLookup::Hit, field.reachable_cells()))
        });
    });
    group.finish();
}

fn bench_sample(c: &mut Criterion) {
    // A hundred thousand agents asking "which way is the door" is the workload the
    // field exists for; this measures the per-agent cost of that question.
    let mut group = c.benchmark_group("flow");
    let size = 128_u16;
    let source = grid(size);
    let target = FlowTarget::at_cell(&source, NavCell::new(size - 1, size - 1));
    let field = solve(&source, target);
    let positions: Vec<Vec3> = (0..1_000_u32)
        .map(|index| {
            Vec3::new(
                (index % size as u32) as f32 + 0.5,
                0.0,
                (index / size as u32) as f32 + 0.5,
            )
        })
        .collect();
    group.throughput(Throughput::Elements(positions.len() as u64));
    group.bench_function("sample/1000_agents", |bencher| {
        bencher.iter(|| {
            let mut sum = 0.0_f32;
            for position in &positions {
                let direction = field.sample(*position, &source);
                sum += direction.length();
            }
            std::hint::black_box(sum)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_solve, bench_cache, bench_sample);
criterion_main!(benches);
