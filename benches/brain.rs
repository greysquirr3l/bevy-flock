//! What cognition costs, leaf by leaf and tree by tree.
//!
//! These numbers are not vanity metrics: they are the declared costs in
//! `src/behavior/leaves.rs` and the fairness weights the think scheduler will use
//! (ADR 0009). A leaf whose measured cost drifts more than 20% from its
//! declaration fails the calibration gate, so the table in the source and the
//! table this bench prints have to agree.
//!
//! Three groups:
//!
//! - `leaf` — one tick of one built-in leaf, which is the unit everything else is
//!   quoted in.
//! - `archetype` — one think of each archetype's tree, which is what a game
//!   multiplies by its population to size the budget.
//! - `shape` — the same work in a small tree and a thousand-node tree, to show
//!   that evaluation cost tracks leaves visited rather than nodes stored.
use bevy_flock::testkit::OrDie;
use std::hint::black_box;
use std::sync::Arc;

use bevy_flock::behavior::leaves::{
    Browse, Chatter, Eat, Flee, Idle, IsAlarmed, IsHungry, IsLonely, IsTired, Patrol, Rest, Serve,
    StandardLeaves, Trade, Travel, Wander,
};
use bevy_flock::behavior::tree::{
    BehaviorTree, LeafContext, NodeSpec, TreeCursor, action, sequence,
};
use bevy_flock::prelude::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

const TICKS_PER_MEASURE: u32 = 1_000;

fn registry_and_leaves() -> (FlockRegistry, StandardLeaves) {
    let mut registry = FlockRegistry::new();
    let leaves = registry.register_standard_leaves().or_die();
    (registry, leaves)
}

fn bench_leaves(c: &mut Criterion) {
    let mut group = c.benchmark_group("leaf");
    group.throughput(Throughput::Elements(1));

    let leaves: Vec<(&str, &dyn LeafBehavior)> = vec![
        ("idle", &Idle),
        ("wander", &Wander),
        ("travel", &Travel),
        ("patrol", &Patrol),
        ("serve", &Serve),
        ("trade", &Trade),
        ("browse", &Browse),
        ("chatter", &Chatter),
        ("flee", &Flee),
        ("rest", &Rest),
        ("eat", &Eat),
        ("is-alarmed", &IsAlarmed),
        ("is-tired", &IsTired),
        ("is-hungry", &IsHungry),
        ("is-lonely", &IsLonely),
    ];

    for (label, leaf) in leaves {
        let mut board = Blackboard::new();
        let mut ctx = LeafContext {
            blackboard: &mut board,
            tick: Tick::ZERO,
            npc: NpcId::unique(0),
        };
        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            b.iter(|| {
                black_box(leaf.tick(black_box(&mut ctx)));
            });
        });
    }
    group.finish();
}

fn bench_archetypes(c: &mut Criterion) {
    let (mut registry, leaves) = registry_and_leaves();
    let mut group = c.benchmark_group("archetype");
    group.throughput(Throughput::Elements(1));

    for archetype in Archetype::ALL {
        let logic = archetype
            .logic(&leaves, |id| registry.leaf_cost(id))
            .or_die();
        let tree = Arc::new(logic.into_tree());
        let mut cursor = TreeCursor::new();
        let mut board = Blackboard::new();
        let mut dispatch = registry.dispatcher();
        let mut tick = Tick::ZERO;
        group.bench_with_input(
            BenchmarkId::from_parameter(archetype.as_str()),
            &archetype,
            |b, _| {
                b.iter(|| {
                    let mut ctx = LeafContext {
                        blackboard: &mut board,
                        tick,
                        npc: NpcId::unique(0),
                    };
                    tick = tick.next();
                    black_box(tree.tick(black_box(&mut cursor), &mut dispatch, &mut ctx));
                });
            },
        );
    }
    group.finish();
}

/// A wide tree: `width` sequences hanging off one selector.
fn wide_tree(width: usize, depth: usize) -> NodeSpec {
    let branches = (0..width)
        .map(|branch| {
            let actions = (0..depth)
                .map(|step| action(u32::try_from((branch + step) % 12).unwrap_or(0)))
                .collect();
            sequence(actions)
        })
        .collect();
    bevy_flock::behavior::tree::selector(branches)
}

fn bench_shapes(c: &mut Criterion) {
    let (mut registry, _leaves) = registry_and_leaves();
    let mut group = c.benchmark_group("shape");
    group.throughput(Throughput::Elements(1));

    for (label, spec) in [
        ("20_nodes", wide_tree(4, 4)),
        ("1000_nodes", wide_tree(32, 31)),
    ] {
        let tree = BehaviorTree::try_from_spec(&spec).or_die();
        let mut cursor = TreeCursor::new();
        let mut board = Blackboard::new();
        let mut dispatch = registry.dispatcher();
        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            b.iter(|| {
                let mut ctx = LeafContext {
                    blackboard: &mut board,
                    tick: Tick::ZERO,
                    npc: NpcId::unique(0),
                };
                black_box(tree.tick(black_box(&mut cursor), &mut dispatch, &mut ctx));
            });
        });
    }
    group.finish();
}

fn bench_crowd_think(c: &mut Criterion) {
    // Ten thousand NPCs thinking once, which is the unit the scheduler budgets in.
    let (mut registry, leaves) = registry_and_leaves();
    let guard = Archetype::Guard
        .logic(&leaves, |id| registry.leaf_cost(id))
        .or_die();
    let tree = Arc::new(guard.into_tree());
    let mut brains: Vec<(TreeCursor, Blackboard)> = (0..10_000)
        .map(|_| (TreeCursor::new(), Blackboard::new()))
        .collect();
    let mut dispatch = registry.dispatcher();

    let mut group = c.benchmark_group("crowd");
    group.throughput(Throughput::Elements(
        u64::try_from(brains.len()).unwrap_or(1),
    ));
    group.bench_function("10k_guards_one_think", |b| {
        let mut tick = Tick::ZERO;
        b.iter(|| {
            tick = tick.next();
            for (cursor, board) in &mut brains {
                let mut ctx = LeafContext {
                    blackboard: board,
                    tick,
                    npc: NpcId::unique(0),
                };
                black_box(tree.tick(cursor, &mut dispatch, &mut ctx));
            }
        });
    });
    group.finish();
    let _ = TICKS_PER_MEASURE;
}

criterion_group!(
    benches,
    bench_leaves,
    bench_archetypes,
    bench_shapes,
    bench_crowd_think
);
criterion_main!(benches);
