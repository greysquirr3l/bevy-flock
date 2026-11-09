//! The costs the binding adds to a frame, measured apart so a regression names itself.
//!
//! Everything runs through the real systems on a real `World` with a real number of bodies. A
//! micro-benchmark of `blend()` in isolation would miss the query iteration, the asset lookups and the
//! state writes, which is where the surprises are. The fixtures match the crate's tests, so a number
//! here and a passing test there describe the same code.
//!
//! `cargo bench -p bevy-flock-anim` runs it. The numbers, the machine, and the ceilings they are
//! compared against live in `docs/dev/tuning/performance.md`; `scripts/bench_gate.py` is the gate that fails a
//! build on a 25% regression against the committed baseline.
use bevy_animation::graph::AnimationNodeIndex;
use bevy_animation::prelude::AnimationPlayer;
use bevy_asset::Assets;
use bevy_ecs::prelude::{Entity, With, World};
use bevy_ecs::system::RunSystemOnce;
use bevy_flock::domain::num;
use bevy_flock::prelude::{GazeAim, Locomotion};
use bevy_math::{Vec2, Vec3};
use bevy_transform::prelude::{GlobalTransform, Transform};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use bevy_flock::anim::{
    AnimationFocus, BlendLod, ClipBindings, GazeBone, GazeLimits, GazeRig, LocomotionProfile,
    RootMotionTrace, RootOffset, apply_gaze, apply_locomotion_blend, apply_root_motion,
};

/// Two thousand is the plaza; ten thousand is the tier where a write starts to matter.
const POPULATIONS: [usize; 2] = [2_000, 10_000];

/// Bodies in the gaze fixture: the plaza population.
const GAZE_BODIES: usize = 2_000;

/// A deterministic scatter over a 60 m plaza, so the focus distance varies like a real crowd's.
fn scatter(index: usize) -> (f32, f32) {
    let angle = f64::from(num::usize_to_u32(index)) * 0.618_034;
    (
        num::f64_to_f32(angle.cos()) * 30.0,
        num::f64_to_f32(angle.sin()) * 30.0,
    )
}

fn profile(lod: BlendLod) -> LocomotionProfile {
    LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0).map_or_else(
        |_| unreachable!("the bench profile is valid"),
        |profile| profile.with_lod(lod),
    )
}

/// A world of `bodies` animated NPCs, each with a motion, a transform, a player and a binding.
fn crowd(bodies: usize, lod: BlendLod) -> World {
    let mut world = World::new();
    world.init_resource::<RootMotionTrace>();
    world.init_resource::<AnimationFocus>();
    let mut profiles = Assets::<LocomotionProfile>::default();
    let handle = profiles.add(profile(lod));
    world.insert_resource(profiles);

    let nodes = [
        AnimationNodeIndex::new(0),
        AnimationNodeIndex::new(1),
        AnimationNodeIndex::new(2),
        AnimationNodeIndex::new(3),
    ];
    let bindings =
        ClipBindings::new(handle, nodes[0], nodes[1], nodes[2]).with_turn_in_place(nodes[3]);
    for index in 0..bodies {
        let (x, z) = scatter(index);
        let mut motion = Locomotion::default();
        // One tick of motion, so the component is not in its default state when the bench starts.
        motion.advance(Vec2::new(1.2, 0.3), Vec2::new(1.2, 0.3), 1.0 / 60.0);
        world.spawn((
            motion,
            Transform::from_translation(Vec3::new(x, 0.0, z)),
            bindings.clone(),
            AnimationPlayer::default(),
            RootOffset::default(),
        ));
    }
    world
}

fn weights(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("binding/weights");
    for bodies in POPULATIONS {
        let mut unculled = crowd(bodies, BlendLod::disabled());
        group.bench_with_input(
            BenchmarkId::new("no_culling", bodies),
            &bodies,
            |bencher, _| {
                bencher.iter(|| {
                    unculled.run_system_once(apply_locomotion_blend).ok();
                });
            },
        );
        let mut culled = crowd(bodies, BlendLod::default());
        // The camera on the plaza edge, which is the case the band exists for.
        culled.insert_resource(AnimationFocus::at(Vec3::new(30.0, 1.7, 0.0)));
        group.bench_with_input(
            BenchmarkId::new("plaza_edge", bodies),
            &bodies,
            |bencher, _| {
                bencher.iter(|| {
                    culled.run_system_once(apply_locomotion_blend).ok();
                });
            },
        );
    }
    group.finish();
}

fn culling(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("binding/culling");
    for bodies in POPULATIONS {
        let mut near = crowd(bodies, BlendLod::default());
        near.insert_resource(AnimationFocus::at(Vec3::ZERO));
        group.bench_with_input(
            BenchmarkId::new("inside_band", bodies),
            &bodies,
            |bencher, _| {
                bencher.iter(|| {
                    near.run_system_once(apply_locomotion_blend).ok();
                });
            },
        );
        let mut far = crowd(bodies, BlendLod::default());
        far.insert_resource(AnimationFocus::at(Vec3::new(400.0, 1.7, 0.0)));
        group.bench_with_input(
            BenchmarkId::new("beyond_far", bodies),
            &bodies,
            |bencher, _| {
                bencher.iter(|| {
                    far.run_system_once(apply_locomotion_blend).ok();
                });
            },
        );
    }
    group.finish();
}

/// Gaze: two bones per body, solved against a camera at the plaza edge.
fn gaze(criterion: &mut Criterion) {
    let mut world = World::new();
    let target = Vec3::new(30.0, 1.7, 0.0);
    for index in 0..GAZE_BODIES {
        let (x, z) = scatter(index);
        let spine = world
            .spawn((Transform::from_translation(Vec3::new(x, 1.0, z)), GazeBone))
            .id();
        let head = world
            .spawn((Transform::from_translation(Vec3::new(x, 1.6, z)), GazeBone))
            .id();
        world.spawn((
            Transform::from_translation(Vec3::new(x, 1.0, z)),
            GlobalTransform::from_translation(Vec3::new(x, 1.0, z)),
            GazeRig::new(spine, head),
            GazeLimits::humanoid(),
            GazeAim::new(target, 1.0),
        ));
    }
    criterion.bench_function("binding/gaze/2_000_bodies", |bencher| {
        bencher.iter(|| {
            world.run_system_once(apply_gaze).ok();
        });
    });
}

/// Root motion: the reconciler's own cost, on the cheap path (no report) and the busy one.
fn root_motion(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("binding/root_motion");
    for bodies in POPULATIONS {
        let mut idle = crowd(bodies, BlendLod::disabled());
        group.bench_with_input(
            BenchmarkId::new("no_report", bodies),
            &bodies,
            |bencher, _| {
                bencher.iter(|| {
                    idle.run_system_once(apply_root_motion).ok();
                });
            },
        );
        let mut busy = crowd(bodies, BlendLod::disabled());
        let reporting: Vec<Entity> = busy
            .query_filtered::<Entity, With<ClipBindings>>()
            .iter(&busy)
            .collect();
        group.bench_with_input(
            BenchmarkId::new("every_body_reports", bodies),
            &bodies,
            |bencher, _| {
                bencher.iter(|| {
                    for entity in &reporting {
                        if let Some(mut offset) = busy.get_mut::<RootOffset>(*entity) {
                            offset.report(Vec3::new(0.023, 0.0, -0.011));
                        }
                    }
                    busy.run_system_once(apply_root_motion).ok();
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, weights, culling, gaze, root_motion);
criterion_main!(benches);
