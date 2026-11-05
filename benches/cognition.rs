//! What deciding who thinks costs.
//!
//! The scheduler's contract (ADR 0009, ADR 0017) is that a frame spends a fixed
//! virtual budget and nothing else: the crowd may grow by an order of magnitude and
//! the planner's own cost must not follow it. These benchmarks are how that claim
//! stays honest — they measure the planner, not the thinking it admits, because the
//! thinking is the part the budget bounds.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::missing_docs_in_private_items
)]

use bevy_flock::application::schedule::ThinkScheduler;
use bevy_flock::domain::cost::VirtualCost;
use bevy_flock::domain::tick::Tick;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// Weights in the units the planner uses: near agents outrank the ambient crowd.
const NEAR: u32 = 8;
const MID: u32 = 4;
const FAR: u32 = 2;
const AMBIENT: u32 = 1;

fn scheduler(population: u32) -> ThinkScheduler {
    let mut scheduler = ThinkScheduler::new(population);
    for index in 0..population {
        let weight = match index % 8 {
            0..=1 => NEAR,
            2..=3 => MID,
            4..=5 => FAR,
            _ => AMBIENT,
        };
        scheduler.enrol(index, weight).expect("capacity matches");
    }
    scheduler
}

fn bench_planning(c: &mut Criterion) {
    let mut group = c.benchmark_group("cognition");
    for population in [1_000_u32, 10_000, 100_000] {
        group.throughput(Throughput::Elements(u64::from(population)));
        let mut scheduler = scheduler(population);
        let mut chosen = Vec::with_capacity(4_096);
        let mut tick = Tick::ZERO;
        group.bench_with_input(
            BenchmarkId::new("plan", population),
            &population,
            |bencher, _| {
                bencher.iter(|| {
                    tick = tick.next();
                    let plan = scheduler.plan(
                        VirtualCost::new(40_000),
                        |_id| VirtualCost::new(6),
                        &mut chosen,
                    );
                    std::hint::black_box(plan.chosen)
                });
            },
        );
    }
    group.finish();
}

fn bench_starvation(c: &mut Criterion) {
    // The worst case for the queue: one participant whose weight never wins against
    // the many, so every tick re-heaps it. A planner that degrades here degrades in
    // exactly the situation a game notices — a big city.
    let mut group = c.benchmark_group("cognition");
    let mut scheduler = scheduler(100_000);
    scheduler
        .enrol(100_000, 65_535)
        .expect("one more participant fits");
    let mut chosen = Vec::with_capacity(4_096);
    let mut tick = Tick::ZERO;
    group.bench_function("plan/100000_with_a_loud_newcomer", |bencher| {
        bencher.iter(|| {
            tick = tick.next();
            let plan = scheduler.plan(
                VirtualCost::new(40_000),
                |_id| VirtualCost::new(6),
                &mut chosen,
            );
            std::hint::black_box(plan.chosen)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_planning, bench_starvation);
criterion_main!(benches);
