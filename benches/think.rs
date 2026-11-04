//! What the think scheduler costs, at the scale the plan claims.
//!
//! Two different numbers matter and they are easy to confuse. The **virtual budget** is
//! simulated cost: it decides how much thinking fits in a tick, and it is the thing the
//! scheduler optimises. The **wall time** is how long the scheduler itself takes to
//! decide, which is overhead the budget does not pay for and must therefore stay small.
//!
//! A scheduler that spent the whole frame deciding who thinks would be worse than no
//! scheduler at all, so the planner is measured on its own.

#![expect(
    clippy::expect_used,
    reason = "the fixture asserts the tree it just built"
)]

use bevy_flock::adapters::primary::cognition::ThinkSchedule;
use bevy_flock::application::schedule::ThinkScheduler;
use bevy_flock::domain::cost::VirtualCost;
use bevy_flock::domain::tick::Tick;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

/// A tick at 60 Hz, expressed as the virtual budget it corresponds to.
const BUDGET: VirtualCost = VirtualCost::from_micros(16_600);

/// A plausible mix: most NPCs cheap, a minority expensive, as measured in
/// `benches/brain.rs`. The scheduler's job is to notice the difference.
fn cost_of(id: u32) -> VirtualCost {
    match id % 16 {
        0 => VirtualCost::from_micros(12),
        1..=3 => VirtualCost::from_micros(4),
        _ => VirtualCost::from_micros(1),
    }
}

fn scheduler(count: u32) -> ThinkScheduler {
    let mut scheduler = ThinkScheduler::new(count);
    for id in 0..count {
        // Every fourth NPC is a "named" NPC weighted higher: the mechanics the
        // extension API exposes, exercised at scale.
        // The identifier comes from the loop bound the capacity was built from, so the
        // error arm is unreachable; discarding it keeps the bench free of unwraps.
        let _ = scheduler.enrol(id, if id % 4 == 0 { 4 } else { 1 });
    }
    scheduler
}

fn bench_plan(c: &mut Criterion) {
    let mut group = c.benchmark_group("think_plan");
    for count in [1_000_u32, 10_000, 100_000] {
        group.throughput(Throughput::Elements(u64::from(count)));
        let mut scheduler = scheduler(count);
        let mut out = Vec::with_capacity(count as usize);
        let mut tick = 0_u64;
        group.bench_function(BenchmarkId::from_parameter(count), |b| {
            b.iter(|| {
                scheduler.advance(Tick::new(tick));
                tick = tick.wrapping_add(1);
                black_box(scheduler.plan(BUDGET, cost_of, &mut out))
            });
        });
    }
    group.finish();
}

fn bench_steady_state(c: &mut Criterion) {
    // The interesting property: as the population grows past what the budget can serve,
    // the plan itself does not grow. Whether that is true is the difference between a
    // scheduler that scales and one that merely sorts.
    let mut group = c.benchmark_group("think_plan_steady_state");
    for count in [10_000_u32, 100_000] {
        let mut scheduler = scheduler(count);
        let mut out = Vec::with_capacity(count as usize);
        let mut tick = 0_u64;
        group.bench_function(BenchmarkId::new("chosen_per_tick", count), |b| {
            b.iter(|| {
                scheduler.advance(Tick::new(tick));
                tick = tick.wrapping_add(1);
                black_box(scheduler.plan(BUDGET, cost_of, &mut out).chosen)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_plan, bench_steady_state, bench_world);
criterion_main!(benches);

// ---------------------------------------------------------------------------
// End to end: a real Bevy world, the real systems, the real budget.
//
// The planner numbers above are the cost of deciding. This is the cost of deciding *and*
// ticking the trees, through the ECS, which is what a frame actually spends.
// ---------------------------------------------------------------------------

fn world(count: u32, brains: bool) -> bevy_app::App {
    use bevy_transform::prelude::Transform;

    use bevy_flock::adapters::primary::cognition::{NpcBlackboard, NpcThinkWeight};
    use bevy_flock::adapters::primary::components::{Npc, NpcBrain};
    use bevy_flock::adapters::primary::plugin::{FlockConfig, FlockPlugin};
    use bevy_flock::domain::ids::NpcId;

    let mut app = bevy_app::App::new();
    app.add_plugins(FlockPlugin::new(FlockConfig::preset(
        bevy_flock::adapters::primary::plugin::CrowdPreset::CityAmbient,
    )));
    // The preset's population is the schedule's capacity; grow it to fit the benchmark.
    app.insert_resource(ThinkSchedule::new(count, ThinkSchedule::DEFAULT_BUDGET));
    if brains {
        let tree = std::sync::Arc::new(
            bevy_flock::behavior::tree::BehaviorTree::try_from_spec(
                bevy_flock::behavior::tree::NodeSpec::Action(0),
            )
            .expect("a leaf is a tree"),
        );
        for id in 0..count {
            app.world_mut().spawn((
                Npc(NpcId::unique(id)),
                Transform::from_xyz(400.0, 0.0, 0.0),
                NpcBrain::new(tree.clone(), VirtualCost::from_micros(2)),
                NpcBlackboard::default(),
                NpcThinkWeight::default(),
            ));
        }
    }
    app
}

fn bench_world(c: &mut Criterion) {
    let mut group = c.benchmark_group("ecs_think");
    for count in [1_000_u32, 10_000] {
        group.throughput(Throughput::Elements(u64::from(count)));
        let mut app = world(count, true);
        let mut tick = 0_u64;
        group.bench_function(BenchmarkId::new("brains", count), |b| {
            b.iter(|| {
                tick = tick.wrapping_add(1);
                app.world_mut()
                    .resource_mut::<bevy_flock::application::SimClock>()
                    .tick = bevy_flock::domain::tick::Tick::new(tick);
                app.update();
                black_box(app.world().resource::<ThinkSchedule>().stats())
            });
        });

        // The same world with no brains: the cost of the plugin itself, subtracted by eye.
        let mut bare = world(count, false);
        group.bench_function(BenchmarkId::new("no_brains", count), |b| {
            b.iter(|| {
                bare.update();
                black_box(bare.world().resource::<ThinkSchedule>().stats())
            });
        });
    }
    group.finish();
}
