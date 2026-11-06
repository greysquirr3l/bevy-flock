//! Replication: what a client costs to serve.
//!
//! The bandwidth budget is the product of two numbers — how many NPCs a viewer is
//! told about, and how many bytes each one takes. The second is a constant
//! ([`NpcNetState::BYTES`]) and the first is a cap; this benchmark measures the work
//! between them, so a busy market cannot quietly become a latency spike.

use bevy_flock::adapters::secondary::network::{InterestSet, NpcNetState};
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::num;
use bevy_math::Vec3;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

fn positions(count: usize) -> Vec<Vec3> {
    (0..count)
        .map(|index| {
            let x = num::usize_to_f32(index % 512) * 0.5;
            let z = num::usize_to_f32(index / 512) * 0.5;
            Vec3::new(x, 0.0, z)
        })
        .collect()
}

fn bench_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("replication");
    for count in [1_000_usize, 10_000, 100_000] {
        let positions = positions(count);
        let interest = InterestSet {
            centre: Vec3::new(64.0, 0.0, 64.0),
            radius: 40.0,
            limit: 256,
        };
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::new("interest_filter", count),
            &count,
            |bencher, _| {
                bencher.iter(|| {
                    let mut kept = 0_usize;
                    for position in &positions {
                        if interest.contains(*position) {
                            kept += 1;
                        }
                    }
                    std::hint::black_box(kept)
                });
            },
        );
    }
    group.finish();
}

fn bench_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("replication");
    let states: Vec<NpcNetState> = (0..1_000_u32)
        .map(|index| {
            NpcNetState::capture(
                NpcId::unique(index),
                Vec3::new(
                    num::u32_to_f32(index) * 0.1,
                    0.0,
                    num::u32_to_f32(index) * 0.2,
                ),
                num::u32_to_f32(index) * 0.01,
                bevy_flock::adapters::primary::components::LodTier::Near,
                0.25,
            )
        })
        .collect();
    group.throughput(Throughput::Elements(states.len() as u64));
    let mut buffer = Vec::with_capacity(states.len() * NpcNetState::BYTES);
    group.bench_function("encode/1000", |bencher| {
        bencher.iter(|| {
            buffer.clear();
            for state in &states {
                state.encode(&mut buffer);
            }
            std::hint::black_box(buffer.len())
        });
    });
    group.bench_function("decode/1000", |bencher| {
        buffer.clear();
        for state in &states {
            state.encode(&mut buffer);
        }
        bencher.iter(|| {
            let mut offset = 0;
            let mut count = 0;
            while let Some(rest) = buffer.get(offset..) {
                let Some((_, used)) = NpcNetState::decode(rest) else {
                    break;
                };
                offset += used;
                count += 1;
            }
            std::hint::black_box(count)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_filter, bench_encoding);
criterion_main!(benches);
