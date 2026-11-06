//! What each tier of cognition costs, in a frame.
//!
//! ADR 0017 claims that tiering lowers the *average* cost of thinking rather than raising
//! the ceiling, and that a crowd of reflex agents is cheap enough to be uninteresting. Both
//! claims are arithmetic until measured, so this bench measures them: the same world, the
//! same number of NPCs, four different tiers, and the frame time each produces.
//!
//! # How a tier is forced
//!
//! Tiers are assigned from distance and engagement, so the fixture places its population at
//! the distance band the tier corresponds to. Social is the exception: it is only ever
//! reached by engagement, which is exactly the property `domain::cognition` tests, so the
//! fixture marks its population [`NpcEngaged`].
//!
//! The promotion ration is set out of the way here (every NPC promotes on the first tick).
//! In a game it stays at sixteen, because the ration exists to protect the frame this bench
//! is measuring.

use bevy_flock::testkit::OrDie;
use std::sync::Arc;

use bevy_app::prelude::*;
use bevy_transform::prelude::Transform;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use bevy_flock::adapters::primary::cognition::{
    NpcBlackboard, NpcEngaged, NpcThinkWeight, NpcTier, PlayerFocus, ThinkSchedule, TierConfig,
};
use bevy_flock::adapters::primary::components::{Npc, NpcBrain};
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::application::SimClock;
use bevy_flock::behavior::tree::{BehaviorTree, NodeSpec};
use bevy_flock::domain::cognition::CognitionTier;
use bevy_flock::domain::cost::VirtualCost;
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::tick::Tick;

/// The tier-typical declared cost of one think, in virtual microseconds.
///
/// These are the numbers ADR 0017 quotes (50–200 ns, 1–3 µs, 4–12 µs, 20–50 µs), reduced to
/// one representative value each so the bench measures one thing at a time.
fn tier_cost(tier: CognitionTier) -> VirtualCost {
    match tier {
        CognitionTier::Reflex => VirtualCost::new(200), // 200 ns: the top of the reflex range
        CognitionTier::Reactive => VirtualCost::from_micros(2),
        CognitionTier::Deliberative => VirtualCost::from_micros(8),
        CognitionTier::Social => VirtualCost::from_micros(30),
    }
}

/// Where an NPC has to stand to be placed in a tier.
fn tier_distance(tier: CognitionTier) -> f32 {
    match tier {
        // Beyond the reflex boundary (120 units by default).
        CognitionTier::Reflex => 400.0,
        // Inside reactive (40), outside deliberative (12).
        CognitionTier::Reactive => 30.0,
        // Inside deliberative.
        CognitionTier::Deliberative | CognitionTier::Social => 5.0,
    }
}

fn tree() -> Arc<BehaviorTree> {
    Arc::new(BehaviorTree::try_from_spec(NodeSpec::Action(0)).or_die())
}

fn world(count: u32, tier: Option<CognitionTier>) -> App {
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(FlockConfig::preset(
        CrowdPreset::CityAmbient,
    )));
    // Big enough for the fixture, and a ration that promotes everybody on the first tick so
    // the measurement is of a steady state rather than of a 625-tick warm-up.
    app.insert_resource(ThinkSchedule::new(
        count + 16,
        ThinkSchedule::DEFAULT_BUDGET,
    ));
    app.insert_resource(TierConfig {
        promotions_per_tick: count + 16,
        ..TierConfig::default()
    });
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));

    let Some(tier) = tier else {
        return app;
    };
    let compiled = tree();
    let cost = tier_cost(tier);
    for id in 0..count {
        let entity = app
            .world_mut()
            .spawn((
                Npc(NpcId::unique(id)),
                Transform::from_xyz(tier_distance(tier), 0.0, 0.0),
                NpcBrain::new(compiled.clone(), cost),
                NpcBlackboard::default(),
                NpcThinkWeight::default(),
                NpcTier::default(),
            ))
            .id();
        if tier == CognitionTier::Social {
            app.world_mut().entity_mut(entity).insert(NpcEngaged);
        }
    }
    app
}

fn run(app: &mut App) {
    let tick = app.world().resource::<SimClock>().tick.get() + 1;
    app.world_mut().resource_mut::<SimClock>().tick = Tick::new(tick);
    app.update();
}

fn bench_tiers(c: &mut Criterion) {
    let mut group = c.benchmark_group("tier_frame");
    // 64,000 is the published reflex ceiling (ADR 0017, measured at ~217 ns per think);
    // 100,000 is past it, and is here to show what "past it" costs rather than to be
    // extrapolated from.
    for count in [1_000_u32, 10_000, 64_000, 100_000] {
        group.throughput(Throughput::Elements(u64::from(count)));

        // No brains: the crowd the plugin spawns on its own.
        let mut bare = world(count, None);
        group.bench_function(BenchmarkId::new("no_brains", count), |b| {
            b.iter(|| {
                run(&mut bare);
                black_box(bare.world().resource::<ThinkSchedule>().stats())
            });
        });

        for tier in CognitionTier::ALL {
            let mut app = world(count, Some(tier));
            // Warm up: the first update promotes the population and enrols it.
            run(&mut app);
            group.bench_function(BenchmarkId::new(tier.name(), count), |b| {
                b.iter(|| {
                    run(&mut app);
                    black_box(app.world().resource::<ThinkSchedule>().stats())
                });
            });
        }
    }
    group.finish();
}

fn bench_mixed(c: &mut Criterion) {
    // The composition ADR 0017 argues for: almost everything reflex, a thin reactive
    // crust, a few deliberative, and the occasional conversation.
    let mut group = c.benchmark_group("tier_mix");
    for count in [1_000_u32, 10_000, 64_000, 100_000] {
        group.throughput(Throughput::Elements(u64::from(count)));
        let mut app = App::new();
        app.add_plugins(FlockPlugin::new(FlockConfig::preset(
            CrowdPreset::CityAmbient,
        )));
        app.insert_resource(ThinkSchedule::new(
            count + 16,
            ThinkSchedule::DEFAULT_BUDGET,
        ));
        app.insert_resource(TierConfig {
            promotions_per_tick: count + 16,
            ..TierConfig::default()
        });
        app.world_mut()
            .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
        let compiled = tree();
        for id in 0..count {
            // 95% reflex, 4% reactive, 1% deliberative — by identifier so the split is
            // deterministic and reproducible.
            let tier = match id % 100 {
                0..=94 => CognitionTier::Reflex,
                95..=98 => CognitionTier::Reactive,
                _ => CognitionTier::Deliberative,
            };
            app.world_mut().spawn((
                Npc(NpcId::unique(id)),
                Transform::from_xyz(tier_distance(tier), 0.0, 0.0),
                NpcBrain::new(compiled.clone(), tier_cost(tier)),
                NpcBlackboard::default(),
                NpcThinkWeight::default(),
                NpcTier::default(),
            ));
        }
        run(&mut app);
        group.bench_function(BenchmarkId::new("95_4_1", count), |b| {
            b.iter(|| {
                run(&mut app);
                black_box(app.world().resource::<ThinkSchedule>().stats())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_tiers, bench_mixed);
criterion_main!(benches);
