//! What deciding who thinks costs.
//!
//! The scheduler's contract (ADR 0009, ADR 0017) is that a frame spends a fixed
//! virtual budget and nothing else: the crowd may grow by an order of magnitude and
//! the planner's own cost must not follow it. These benchmarks are how that claim
//! stays honest — they measure the planner, not the thinking it admits, because the
//! thinking is the part the budget bounds.

use bevy_flock::application::schedule::ThinkScheduler;
use bevy_flock::domain::cost::VirtualCost;
use bevy_flock::domain::tick::Tick;
use bevy_flock::testkit::OrDie;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// Weights in the units the planner uses: near agents outrank the ambient crowd.
const NEAR: u32 = 8;
const MID: u32 = 4;
const FAR: u32 = 2;
const AMBIENT: u32 = 1;

/// A planner holding `population` participants, with `slack` free slots for a benchmark that wants
/// to enrol one more afterwards.
fn scheduler_with_room(population: u32, slack: u32) -> ThinkScheduler {
    let mut scheduler = ThinkScheduler::new(population.saturating_add(slack));
    for index in 0..population {
        let weight = match index % 8 {
            0..=1 => NEAR,
            2..=3 => MID,
            4..=5 => FAR,
            _ => AMBIENT,
        };
        scheduler.enrol(index, weight).or_die();
    }
    scheduler
}

fn scheduler(population: u32) -> ThinkScheduler {
    scheduler_with_room(population, 0)
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
    //
    // The newcomer needs a free slot: `scheduler(100_000)` already enrols ids 0..100_000, so
    // enrolling id 100_000 into a 100_000-capacity planner is an out-of-range enrol, not the
    // benchmark. Asking for 100_001 costs one more slot and makes the newcomer real.
    let mut group = c.benchmark_group("cognition");
    let mut scheduler = scheduler_with_room(100_000, 1);
    scheduler.enrol(100_000, 65_535).or_die();
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
