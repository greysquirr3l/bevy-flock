//! Which part of the gaze solve costs the 181 ns per body the envelope blames.
//!
//! Four variants of the same work on the same fixture, so the difference between two rows is the cost
//! of the thing that differs:
//!
//! * `math_only` — the angles and the two quaternions per body, accumulated into a black box and
//!   written nowhere. The floor no optimisation can go below.
//! * `lookup_spine` — the same math, then one `bones.get_mut(spine)` per body.
//! * `lookup_both` — the shipped `apply_gaze`: two `get_mut` calls, one per bone.
//! * `table_order` — bones iterated in table order with the body read through `ChildOf`, so the bone
//!   writes are sequential. The candidate replacement if the lookups are the cost.
//!
//! `cargo run --release -p bevy-flock-anim --example profile_gaze` prints microseconds per pass over
//! 2,000 bodies, runs about three seconds, and is the measurement named in `docs/dev/tuning/performance.md`.
use std::hint::black_box;
use std::time::Instant;

use bevy_ecs::prelude::{ChildOf, Entity, Query, With, World};
use bevy_ecs::system::RunSystemOnce;
use bevy_flock::domain::num;
use bevy_flock::prelude::GazeAim;
use bevy_math::{EulerRot, Quat, Vec3};
use bevy_transform::prelude::{GlobalTransform, Transform};

use bevy_flock::anim::{GazeBone, GazeLimits, GazeRig, apply_gaze, look_angles, split_yaw};

/// Bodies in the fixture: the plaza, and the number the 0.3 ms ceiling names.
const BODIES: usize = 2_000;
/// Timed passes per variant, after a warm-up of `WARMUP`.
const PASSES: u32 = 200;
/// Warm-up passes, so a first-touch page fault is not charged to the variant.
const WARMUP: u32 = 8;

fn scatter(index: usize) -> (f32, f32) {
    let angle = f64::from(num::usize_to_u32(index)) * 0.618_034;
    (
        num::f64_to_f32(angle.cos()) * 30.0,
        num::f64_to_f32(angle.sin()) * 30.0,
    )
}

/// Bodies with two bone children each, spawned the way a game spawns them: body, spine, head.
fn fixture() -> World {
    let mut world = World::new();
    let target = Vec3::new(30.0, 1.7, 0.0);
    for index in 0..BODIES {
        let (x, z) = scatter(index);
        let body = world
            .spawn((
                Transform::from_translation(Vec3::new(x, 1.0, z)),
                GlobalTransform::from_translation(Vec3::new(x, 1.0, z)),
                GazeLimits::humanoid(),
                GazeAim::new(target, 1.0),
            ))
            .id();
        let spine = world
            .spawn((
                Transform::from_translation(Vec3::new(x, 1.0, z)),
                GazeBone,
                ChildOf(body),
            ))
            .id();
        let head = world
            .spawn((
                Transform::from_translation(Vec3::new(x, 1.6, z)),
                GazeBone,
                ChildOf(body),
            ))
            .id();
        world.entity_mut(body).insert(GazeRig::new(spine, head));
    }
    world
}

fn time(label: &str, mut pass: impl FnMut()) {
    for _ in 0..WARMUP {
        pass();
    }
    let start = Instant::now();
    for _ in 0..PASSES {
        pass();
    }
    let micros = start.elapsed().as_secs_f64() * 1e6 / f64::from(PASSES);
    let per_body = micros * 1_000.0 / num::usize_to_f64(BODIES);
    println!("{label:<16}{micros:>9.1} µs/pass{per_body:>9.1} ns/body");
}

/// The arithmetic alone: no bone writes.
fn math_only(world: &mut World) {
    world
        .run_system_once(|bodies: Query<(&GazeAim, &GazeLimits, &GlobalTransform)>| {
            let mut sink = 0.0_f32;
            for (aim, limits, body) in &bodies {
                let (yaw, pitch) = look_angles(body, aim.point());
                let (spine_yaw, head_yaw) = split_yaw(yaw, limits);
                let spine = Quat::from_euler(EulerRot::YXZ, spine_yaw, 0.0, 0.0);
                let head = Quat::from_euler(EulerRot::YXZ, head_yaw, pitch, 0.0);
                sink += spine.x + head.y;
            }
            black_box(sink);
        })
        .ok();
}

/// One random bone lookup per body.
fn lookup_spine(world: &mut World) {
    world
        .run_system_once(
            |bodies: Query<(&GazeAim, &GazeRig, &GazeLimits, &GlobalTransform)>,
             mut bones: Query<&mut Transform, With<GazeBone>>| {
                let mut sink = 0.0_f32;
                for (aim, rig, limits, body) in &bodies {
                    let (yaw, _) = look_angles(body, aim.point());
                    let (spine_yaw, _) = split_yaw(yaw, limits);
                    let delta = Quat::from_euler(EulerRot::YXZ, spine_yaw, 0.0, 0.0);
                    if let Ok(mut spine) = bones.get_mut(rig.spine()) {
                        spine.rotation *= delta;
                        sink += spine.rotation.x;
                    }
                }
                black_box(sink);
            },
        )
        .ok();
}

/// The shipped system: two random lookups per body.
fn lookup_both(world: &mut World) {
    world.run_system_once(apply_gaze).ok();
}

/// Bones in table order; the body read through its `ChildOf` link. Same writes, sequential bone
/// access, one hierarchical lookup per bone instead of a sparse-set `get_mut` into the bone table.
fn table_order(world: &mut World) {
    world
        .run_system_once(
            |bones: Query<(Entity, &mut Transform, &ChildOf), With<GazeBone>>,
             bodies: Query<(&GazeAim, &GazeLimits, &GlobalTransform, &GazeRig)>| {
                let mut sink = 0.0_f32;
                for (entity, mut transform, parent) in bones {
                    let Ok((aim, limits, body, rig)) = bodies.get(parent.parent()) else {
                        continue;
                    };
                    let (yaw, pitch) = look_angles(body, aim.point());
                    let (spine_yaw, head_yaw) = split_yaw(yaw, limits);
                    let delta = if rig.head() == entity {
                        let head_pitch = pitch.clamp(-limits.head_pitch, limits.head_pitch);
                        Quat::from_euler(EulerRot::YXZ, head_yaw, head_pitch, 0.0)
                    } else {
                        Quat::from_euler(EulerRot::YXZ, spine_yaw, 0.0, 0.0)
                    };
                    transform.rotation *= delta;
                    sink += transform.rotation.x;
                }
                black_box(sink);
            },
        )
        .ok();
}

fn main() {
    let mut world = fixture();
    println!("gaze solve, {BODIES} bodies, {PASSES} passes, release build");
    println!("{:<16}{:>16}{:>16}", "variant", "µs/pass", "ns/body");
    time("math_only", || math_only(&mut world));
    time("lookup_spine", || lookup_spine(&mut world));
    time("lookup_both", || lookup_both(&mut world));
    time("table_order", || table_order(&mut world));
}
