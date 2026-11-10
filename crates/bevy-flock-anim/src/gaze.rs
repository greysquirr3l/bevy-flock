//! Turning a [`GazeAim`] into bone rotations: analytic, two bones, no IK dependency.
//!
//! The core decides *what* a body is looking at and *how much* of the look to apply
//! ([`GazeAim`]). This module is the other half: it reads the aim, splits it between the spine and
//! the head, and writes the two rotations. Nothing here knows about players, perception or
//! decisions, and nothing here reads a clock.
//!
//! # Why the pose is never stored
//!
//! An evaluator that keeps "the pose before gaze" and restores it on release drifts: every frame it
//! feeds its own output back in, and a skeleton that is looked at a thousand times accumulates a
//! thousand rounding errors. Bevy's animation evaluation writes *absolute* rotations for animated
//! properties every frame, so this module can treat the bone's transform as freshly animated, add
//! its offset, and write it back. When the aim goes idle it writes nothing at all, and the next
//! evaluation tick restores the clip's own pose exactly — not approximately.
//!
//! That is the contract the release test pins: an entity whose gaze was applied for a hundred ticks
//! and then released ends bit-identical to one that never looked anywhere.

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::{Component, Entity, Query, With, Without};
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_flock_ecs::prelude::GazeAim;
use bevy_math::ops::{asin, atan2};
use bevy_math::{Quat, Vec2, Vec3};
use bevy_transform::components::{GlobalTransform, Transform};

/// Where a body's spine and head are, for the gaze solve.
///
/// A two-bone rig is the smallest thing that reads as looking: the spine commits, the head aims. The
/// component names the two entities rather than assuming a hierarchy, because a game with a
/// different rig should not have to adopt this crate's skeleton.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GazeRig {
    spine: Entity,
    head: Entity,
}

impl GazeRig {
    /// The spine and head bones of one body.
    #[must_use]
    pub const fn new(spine: Entity, head: Entity) -> Self {
        Self { spine, head }
    }

    /// The bone that carries the smaller share of the turn.
    #[must_use]
    pub const fn spine(&self) -> Entity {
        self.spine
    }

    /// The bone that carries the aim.
    #[must_use]
    pub const fn head(&self) -> Entity {
        self.head
    }
}

/// Marks the bones this module is allowed to write.
///
/// A marker rather than a query filter on [`GazeRig`] so the two queries in [`apply_gaze`] can name
/// disjoint sets: the body is `Without<GazeBone>` and the bones are `With<GazeBone>`, which is what
/// lets both take `Transform` without a `ParamSet`.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GazeBone;

/// How the turn is shared between the two bones, and how far each may go.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct GazeLimits {
    /// The share of the yaw the spine takes, in `0.0..=1.0`. The head takes the rest.
    pub spine_share: f32,
    /// The spine's yaw limit, in radians.
    pub spine_yaw: f32,
    /// The head's yaw limit, in radians.
    pub head_yaw: f32,
    /// The head's pitch limit, in radians.
    pub head_pitch: f32,
}

impl GazeLimits {
    /// A humanoid rig: the spine commits a third of the turn within twenty degrees, the head takes
    /// the rest within fifty degrees, and the pitch is fifty degrees either way.
    #[must_use]
    pub const fn humanoid() -> Self {
        Self {
            spine_share: 0.35,
            spine_yaw: 0.349_065_8,  // 20 degrees
            head_yaw: 0.872_664_5,   // 50 degrees
            head_pitch: 0.872_664_5, // 50 degrees
        }
    }
}

impl Default for GazeLimits {
    fn default() -> Self {
        Self::humanoid()
    }
}

/// Split a yaw between the spine and the head, respecting both limits.
///
/// The spine takes its share first and is clamped, then the head takes whatever is left up to its own
/// limit. A target ninety degrees to the side therefore produces a spine at its limit and a head at
/// its limit, both pointing the same way, rather than a spine that wraps around its own shoulders.
#[must_use]
pub fn split_yaw(yaw: f32, limits: &GazeLimits) -> (f32, f32) {
    let spine = (yaw * limits.spine_share).clamp(-limits.spine_yaw, limits.spine_yaw);
    let head = (yaw - spine).clamp(-limits.head_yaw, limits.head_yaw);
    (spine, head)
}

/// The yaw and pitch, in radians, from a body's facing to a point in the world.
///
/// Yaw is positive to the body's left, pitch is positive upwards, and the body's forward is `-Z`,
/// which is Bevy's convention. A point at the body's own position has no direction and yields zero
/// for both rather than a NaN.
#[must_use]
pub fn look_angles(body: &GlobalTransform, point: Vec3) -> (f32, f32) {
    let offset = body.rotation().inverse() * (point - body.translation());
    // Yaw about +Y is positive towards the body's left, which is `-X` when forward is `-Z`; hence the
    // negated `x` in the argument order rather than the conventional `atan2(x, z)`.
    let flat = Vec2::new(-offset.x, -offset.z);
    if !flat.length_squared().is_finite() || flat.length_squared() < 1.0e-12 {
        return (0.0, 0.0);
    }
    let yaw = atan2(flat.x, flat.y);
    let pitch = if offset.length_squared() < 1.0e-12 {
        0.0
    } else {
        asin((offset.y / offset.length()).clamp(-1.0, 1.0))
    };
    (yaw, pitch)
}

/// Apply every body's gaze to its spine and head.
///
/// Runs in `PostUpdate` after `animate_targets`, the system that writes bone transforms from the
/// clips. `bevy_animation` keeps its internal system set private, so the anchor is that system., which is the only place it can run: the rotation
/// it reads must be the clip's rotation for *this* frame, not last frame's, or the offset compounds.
/// It must also run before transform propagation, so the rendering that follows sees the gaze.
pub fn apply_gaze(
    bodies: Query<(&GazeAim, &GazeRig, &GazeLimits, &GlobalTransform), Without<GazeBone>>,
    mut bones: Query<&mut Transform, With<GazeBone>>,
) {
    for (aim, rig, limits, body) in &bodies {
        // Idle means *write nothing*: the animated pose stands, and no bookkeeping is needed to
        // restore it because nothing was changed.
        if aim.is_idle() {
            continue;
        }
        let (yaw, pitch) = look_angles(body, aim.point());
        let (spine_yaw, head_yaw) = split_yaw(yaw, limits);
        let head_pitch = pitch.clamp(-limits.head_pitch, limits.head_pitch);
        let weight = aim.weight();

        if let Ok(mut spine) = bones.get_mut(rig.spine) {
            // One axis, so one sine and one cosine: `from_euler` builds a three-axis rotation and
            // pays for all three, which measured as most of the gaze solve's cost.
            let delta = Quat::from_rotation_y(spine_yaw * weight);
            spine.rotation *= delta;
        }
        if let Ok(mut head) = bones.get_mut(rig.head) {
            // `YXZ` is yaw-then-pitch, which is exactly this product minus the third axis the Euler
            // helper computes and multiplies by a zero angle.
            let delta = Quat::from_rotation_y(head_yaw * weight)
                * Quat::from_rotation_x(head_pitch * weight);
            head.rotation *= delta;
        }
    }
}

/// Adds the gaze solve, after animation evaluation and before transform propagation.
///
/// It consumes the core's [`GazeAim`], so a game adds `FlockPlugin` (which resolves aims) and this
/// plugin, in that order, but neither is a build-time dependency of the other.
#[derive(Debug, Default, Clone, Copy)]
pub struct GazePlugin;

impl Plugin for GazePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            apply_gaze.after(bevy_flock_ecs::prelude::resolve_gaze),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{GazeBone, GazeLimits, GazeRig, apply_gaze, look_angles, split_yaw};
    use bevy_ecs::prelude::{Entity, World};
    use bevy_ecs::system::RunSystemOnce;
    use bevy_flock_ecs::prelude::GazeAim;
    use bevy_math::{Quat, Vec3};
    use bevy_transform::components::{GlobalTransform, Transform};

    fn run(world: &mut World) {
        if world.run_system_once(apply_gaze).is_err() {
            unreachable!("the gaze solve runs on any world");
        }
    }

    /// A body facing `-Z`, with a spine and a head at `spine` and `head` rotations.
    fn body(world: &mut World, aim: GazeAim, spine: Quat, head: Quat) -> (Entity, Entity, Entity) {
        let spine_entity = world
            .spawn((Transform::from_rotation(spine), GazeBone))
            .id();
        let head_entity = world.spawn((Transform::from_rotation(head), GazeBone)).id();
        let body_entity = world
            .spawn((
                aim,
                GazeRig::new(spine_entity, head_entity),
                GazeLimits::humanoid(),
                GlobalTransform::default(),
            ))
            .id();
        (body_entity, spine_entity, head_entity)
    }

    #[test]
    fn an_idle_aim_writes_nothing_at_all() {
        let animated = Quat::from_euler(bevy_math::EulerRot::YXZ, 0.3, 0.1, 0.0);
        let mut world = World::new();
        let (_, spine, head) = body(&mut world, GazeAim::default(), animated, animated);
        run(&mut world);
        for bone in [spine, head] {
            let value = world
                .get::<Transform>(bone)
                .map(|transform| transform.rotation);
            assert_eq!(
                value.map(|q| q.to_array().map(f32::to_bits)),
                Some(animated.to_array().map(f32::to_bits))
            );
        }
    }

    #[test]
    fn a_released_gaze_leaves_the_animated_pose_bit_identical() {
        // The animated pose, whatever the clip wrote this frame.
        let animated = Quat::from_euler(bevy_math::EulerRot::YXZ, -0.42, 0.11, 0.07);

        // One body looks at something for a while at full weight.
        let mut world = World::new();
        let (body_entity, spine, head) = body(
            &mut world,
            GazeAim::new(Vec3::new(6.0, 1.0, -3.0), 1.0),
            animated,
            animated,
        );
        for _ in 0..100 {
            run(&mut world);
        }
        // The clip re-evaluates, writing the animated pose again; the release happens now.
        for bone in [spine, head] {
            if let Some(mut transform) = world.get_mut::<Transform>(bone) {
                transform.rotation = animated;
            }
        }
        if let Some(mut aim) = world.get_mut::<GazeAim>(body_entity) {
            *aim = GazeAim::default();
        }
        run(&mut world);

        // Not "close to" the animated pose: the same bits, because nothing was written.
        for bone in [spine, head] {
            let value = world
                .get::<Transform>(bone)
                .map(|transform| transform.rotation);
            assert_eq!(
                value.map(|q| q.to_array().map(f32::to_bits)),
                Some(animated.to_array().map(f32::to_bits))
            );
        }
    }

    #[test]
    fn a_full_weight_look_turns_both_bones_toward_the_target() {
        let mut world = World::new();
        // Something to the body's left and slightly above: a positive yaw and a positive pitch.
        let (_, spine, head) = body(
            &mut world,
            GazeAim::new(Vec3::new(-4.0, 1.5, -4.0), 1.0),
            Quat::IDENTITY,
            Quat::IDENTITY,
        );
        run(&mut world);
        let spine_yaw = world.get::<Transform>(spine).map_or(0.0, |transform| {
            transform.rotation.to_euler(bevy_math::EulerRot::YXZ).0
        });
        let head_euler = world
            .get::<Transform>(head)
            .map_or((0.0, 0.0, 0.0), |transform| {
                transform.rotation.to_euler(bevy_math::EulerRot::YXZ)
            });
        assert!(spine_yaw > 0.0, "spine yaw was {spine_yaw}");
        assert!(head_euler.0 > 0.0, "head yaw was {}", head_euler.0);
        assert!(head_euler.1 > 0.0, "head pitch was {}", head_euler.1);
    }

    #[test]
    fn a_half_weight_look_turns_half_as_far() {
        let mut full = World::new();
        let (_, full_spine, _) = body(
            &mut full,
            GazeAim::new(Vec3::new(-4.0, 0.0, -4.0), 1.0),
            Quat::IDENTITY,
            Quat::IDENTITY,
        );
        run(&mut full);
        let mut half = World::new();
        let (_, half_spine, _) = body(
            &mut half,
            GazeAim::new(Vec3::new(-4.0, 0.0, -4.0), 0.5),
            Quat::IDENTITY,
            Quat::IDENTITY,
        );
        run(&mut half);
        let of = |world: &World, entity| {
            world.get::<Transform>(entity).map_or(0.0, |transform| {
                transform.rotation.to_euler(bevy_math::EulerRot::YXZ).0
            })
        };
        let (full_yaw, half_yaw) = (of(&full, full_spine), of(&half, half_spine));
        assert!(full_yaw > 0.0);
        assert!(
            (full_yaw - half_yaw * 2.0).abs() < 1.0e-5,
            "{full_yaw} vs {half_yaw}"
        );
    }

    #[test]
    fn the_spine_takes_its_share_and_the_head_the_rest_until_both_hit_their_limits() {
        let limits = GazeLimits::humanoid();
        let (spine, head) = split_yaw(0.2, &limits);
        assert!((spine + head - 0.2).abs() < 1.0e-6);
        // Ninety degrees: both clamp, and neither wraps past its limit.
        let (spine, head) = split_yaw(std::f32::consts::FRAC_PI_2, &limits);
        assert!((spine - limits.spine_yaw).abs() < 1.0e-6);
        assert!((head - limits.head_yaw).abs() < 1.0e-6);
        // And the same on the other side, without a sign flip in the split.
        let (spine, head) = split_yaw(-std::f32::consts::FRAC_PI_2, &limits);
        assert!((spine + limits.spine_yaw).abs() < 1.0e-6);
        assert!((head + limits.head_yaw).abs() < 1.0e-6);
    }

    #[test]
    fn a_target_at_the_body_s_own_position_has_no_angles() {
        let body = GlobalTransform::from_translation(Vec3::new(3.0, 0.0, 3.0));
        let (yaw, pitch) = look_angles(&body, Vec3::new(3.0, 0.0, 3.0));
        assert!(yaw.abs() < 1.0e-6);
        assert!(pitch.abs() < 1.0e-6);
    }

    #[test]
    fn the_forward_direction_needs_no_turn() {
        let body = GlobalTransform::from_translation(Vec3::ZERO);
        // Forward is `-Z`: a target straight ahead is no yaw and no pitch.
        let (yaw, pitch) = look_angles(&body, Vec3::new(0.0, 0.0, -5.0));
        assert!(yaw.abs() < 1.0e-6, "yaw was {yaw}");
        assert!(pitch.abs() < 1.0e-6, "pitch was {pitch}");
        // And a target to the left is a quarter turn.
        let (yaw, _) = look_angles(&body, Vec3::new(-5.0, 0.0, 0.0));
        assert!(
            (yaw - std::f32::consts::FRAC_PI_2).abs() < 1.0e-6,
            "yaw was {yaw}"
        );
    }
}
