//! Spawning the crowd: one body, nine joints, and a walking direction that keeps them moving.
//!
//! Every body is the same mesh, the same graph and the same profile, and differs only in where it
//! stands, how fast it walks and which phase of its cycle it is in. That is the honest version of a
//! crowd this size: variety from parameters, not from assets.

use bevy_animation::graph::AnimationGraph;
use bevy_animation::graph::AnimationGraphHandle;
use bevy_animation::{AnimatedBy, AnimationPlayer, AnimationTargetId};
use bevy_asset::{Assets, Handle};
use bevy_camera::visibility::Visibility;
use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::{Entity, Query, Res, Resource, World};
use bevy_flock::adapters::primary::cognition::{NpcBlackboard, NpcTier};
use bevy_flock::domain::cognition::CognitionTier;
use bevy_flock::domain::num;
use bevy_flock::prelude::{
    GazeAim, GazeTarget, GestureKind, Npc, NpcBundle, NpcDesiredVelocity, NpcGestures, NpcId,
    NpcVelocity, SimClock,
};
use bevy_flock::anim::{
    BlendWindows, ClipBindings, GazeBone, GazeLimits, GazeRig, GestureClips, GesturePlayback,
    LocomotionProfile, ProfileError,
};
use bevy_math::{Quat, Vec2, Vec3};
use bevy_mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy_mesh::{Mesh, Mesh3d};
use bevy_pbr::{MeshMaterial3d, StandardMaterial};
use bevy_transform::components::{GlobalTransform, Transform};

use super::clips::{ClipError, RigClips};
use super::rig::{Joint, body_mesh, inverse_bindposes, local_offset};

/// The assets one crowd shares.
#[derive(Debug, Clone, Resource)]
pub struct RigHandles {
    /// The body mesh, skinned to [`super::rig::JOINT_COUNT`] joints.
    pub mesh: Handle<Mesh>,
    /// The inverse bind matrices for those joints.
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    /// The palette the crowd is drawn from.
    pub materials: Vec<Handle<StandardMaterial>>,
    /// The animation graph every body plays.
    pub graph: Handle<AnimationGraph>,
    /// The profile that decides how speed maps onto idle, walk and run.
    pub profile: Handle<LocomotionProfile>,
    /// The node indices inside the graph.
    pub clips: RigClips,
}

impl RigHandles {
    /// The material body `index` wears.
    ///
    /// Six tints rather than one: a thousand bodies of a single colour reads as a texture, and six
    /// reads as a crowd. The choice is by index so a given body wears the same colour every run.
    #[must_use]
    pub fn material_for(&self, index: usize) -> Handle<StandardMaterial> {
        self.materials
            .get(index % self.materials.len().max(1))
            .cloned()
            .unwrap_or_default()
    }
}

/// How tall body `index` stands, as a multiplier on the rig.
///
/// Between 0.94 and 1.06 in seven steps, which is the difference between "a crowd" and "a hundred
/// copies of one person" at the distance this example is viewed from.
#[must_use]
pub fn body_scale(index: usize) -> f32 {
    0.94 + num::usize_to_f32(index % 7) * 0.02
}

/// What one spawn produced, for the report the example prints and the tests assert on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrowdReport {
    /// Bodies spawned.
    pub bodies: usize,
    /// Joint entities spawned, [`super::rig::JOINT_COUNT`] per body.
    pub joints: usize,
}

/// Why the crowd's assets could not be built.
#[derive(Debug, Clone, PartialEq)]
pub enum RigError {
    /// A clip's keyframes were malformed.
    Clips(ClipError),
    /// The locomotion profile was rejected.
    Profile(ProfileError),
}

impl From<ClipError> for RigError {
    fn from(error: ClipError) -> Self {
        Self::Clips(error)
    }
}

impl From<ProfileError> for RigError {
    fn from(error: ProfileError) -> Self {
        Self::Profile(error)
    }
}

/// The profile this crowd walks with: 1.4 m/s walk, 5 m/s run, a 1.5 m stride.
///
/// The stride is what ties the clip's tempo to the ground: a body covers 1.5 m per cycle at the walk
/// speed, which is what makes the feet stop sliding once the mesh has legs worth watching.
///
/// # Errors
///
/// Returns [`ProfileError`] if a value is not positive and finite, which the constants below are.
pub fn rig_profile() -> Result<LocomotionProfile, ProfileError> {
    // A sixth of a second to pick the stride up and an eighth to put it down: long enough that a
    // body leaving the plaza does not snap from standing to walking between two ticks.
    let profile = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)?;
    Ok(profile.with_windows(BlendWindows::try_new(6, 8)?))
}

/// Create the shared assets: one mesh, one bind pose, one material, one graph, one profile.
///
/// # Errors
///
/// Returns [`RigError`] if a clip's keyframes or the profile are rejected. For the constants in this
/// crate that is a programming error rather than bad input, but it is reported rather than panicked
/// on: a library that decides to kill a game's process because a keyframe list is short is a library
/// a game cannot ship.
pub fn build_handles(world: &mut World) -> Result<RigHandles, RigError> {
    // One borrow at a time: the stores are separate resources, and holding four of them at once is
    // four mutable borrows of the same world.
    let mesh = world.resource_mut::<Assets<Mesh>>().add(body_mesh());
    let inverse_bindposes = world
        .resource_mut::<Assets<SkinnedMeshInverseBindposes>>()
        .add(SkinnedMeshInverseBindposes::from(inverse_bindposes()));
    let clips = {
        let mut store = world.resource_mut::<Assets<bevy_animation::AnimationClip>>();
        super::clips::build_clip_assets(&mut store)?
    };
    let library = {
        let mut graphs = world.resource_mut::<Assets<AnimationGraph>>();
        super::clips::build_graph(&mut graphs, &clips)
    };
    let profile = world
        .resource_mut::<Assets<LocomotionProfile>>()
        .add(rig_profile()?);
    // Weathered concrete and its neighbours: six tints that read against the fog and against each
    // other. One material per tint means six bind groups for the whole crowd, not one per body.
    let materials = [
        (0.58, 0.53, 0.45),
        (0.39, 0.40, 0.44),
        (0.70, 0.66, 0.58),
        (0.55, 0.44, 0.35),
        (0.47, 0.50, 0.55),
        (0.63, 0.58, 0.47),
    ]
    .into_iter()
    .map(|(red, green, blue)| {
        world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: bevy_color::Color::srgb(red, green, blue),
                perceptual_roughness: 0.85,
                ..StandardMaterial::default()
            })
    })
    .collect();
    Ok(RigHandles {
        mesh,
        inverse_bindposes,
        materials,
        graph: library.graph,
        profile,
        clips: library.clips,
    })
}

/// Where a body stands: a phyllotaxis spiral over a disc.
///
/// A spiral rather than a grid because a grid of two thousand mannequins reads as a parade, and
/// because the golden angle scatters the near and far bodies evenly instead of ringing the plaza.
#[must_use]
pub fn spawn_position(index: usize, radius: f32, population: usize) -> Vec3 {
    let i = num::usize_to_f32(index);
    let total = num::usize_to_f32(population.max(1));
    let angle = i * 2.399_963_2_f32;
    // `sqrt` of the normalised index spreads the points evenly over the area rather than the radius.
    let r = radius * bevy_math::ops::sqrt((i + 0.5) / total);
    Vec3::new(
        r * bevy_math::ops::cos(angle),
        0.0,
        r * bevy_math::ops::sin(angle),
    )
}

/// How fast one body walks, in metres per second, from its index.
///
/// Between 1.05 and 1.75 m/s in seven steps. A crowd that all walks at exactly the walk speed looks
/// like a conveyor belt, and the blend is only legible if the weights differ between bodies.
#[must_use]
pub fn walk_speed(index: usize) -> f32 {
    1.05 + num::usize_to_f32(index % 7) * 0.1
}

/// The yaw that makes a body face `direction`, whose forward is `-Z`.
#[must_use]
pub fn yaw_for(direction: Vec3) -> f32 {
    bevy_math::ops::atan2(-direction.x, -direction.z)
}

/// Spawn one joint as a child of `parent`, with the components the animation and gaze systems need.
fn spawn_joint(
    world: &mut World,
    player: Entity,
    joint: Joint,
    parent: Entity,
    gaze: bool,
) -> Entity {
    let mut entity = world.spawn((
        Transform::from_translation(local_offset(joint)),
        GlobalTransform::default(),
        ChildOf(parent),
        AnimationTargetId::from_iter([joint.name()]),
        AnimatedBy(player),
    ));
    if gaze {
        entity.insert(GazeBone);
    }
    entity.id()
}

/// Spawn `count` bodies on a disc of `radius` metres, each with its joints and clips bound.
pub fn spawn_crowd(
    world: &mut World,
    handles: &RigHandles,
    count: usize,
    radius: f32,
) -> CrowdReport {
    for index in 0..count {
        let position = spawn_position(index, radius, count);
        let id = NpcId::unique(num::usize_to_u32(index));
        // The bundle owns the transform, so the spawn position goes in through it: adding a second
        // `Transform` to the same entity is a duplicate component, not an override.
        let mut bundle = NpcBundle::default_for(id);
        bundle.transform =
            Transform::from_translation(position).with_scale(Vec3::splat(body_scale(index)));
        let body = world
            .spawn((
                bundle,
                // The tier the scheduler would assign a plaza body: the animation LOD reads it, so
                // without it every body inside the band would blend every tick.
                NpcTier(CognitionTier::Reflex),
                Visibility::default(),
                Mesh3d(handles.mesh.clone()),
                MeshMaterial3d(handles.material_for(index)),
                AnimationPlayer::default(),
                AnimationGraphHandle(handles.graph.clone()),
                ClipBindings::new(
                    handles.profile.clone(),
                    handles.clips.idle,
                    handles.clips.walk,
                    handles.clips.run,
                ),
                GestureClips::new()
                    .with(GestureKind::Wave, handles.clips.wave)
                    .with(GestureKind::Nod, handles.clips.nod)
                    .with(GestureKind::Flinch, handles.clips.flinch)
                    .with(GestureKind::Bark, handles.clips.bark),
                GesturePlayback::new(),
                NpcGestures::new(),
                GazeTarget::new(),
                GazeAim::default(),
                GazeLimits::humanoid(),
                // The blackboard is where a behaviour leaf asks for a gesture, so the crowd has one
                // from the first tick rather than gaining it when a game's AI system happens to run.
                NpcBlackboard::default(),
            ))
            .id();

        // One entity per joint, parented exactly as `Joint::parent` declares and pushed in the
        // joint's own index order, so the hierarchy, the `SkinnedMesh` list and the mesh's `JOINTS_0`
        // values are three views of one table rather than three tables that have to agree.
        //
        // A parent is always an earlier entry in `Joint::ALL`, which is what makes a single forward
        // pass enough; `the_parent_of_a_joint_is_always_before_it` is the test that keeps that true.
        let mut joints: Vec<Entity> = Vec::with_capacity(super::rig::JOINT_COUNT);
        for joint in Joint::ALL {
            let parent = joint
                .parent()
                .and_then(|parent| joints.get(usize::from(parent.index())).copied())
                .unwrap_or(body);
            let gaze = matches!(joint, Joint::TorsoPivot | Joint::HeadPivot);
            joints.push(spawn_joint(world, body, joint, parent, gaze));
        }
        let pivot = |joint: Joint| {
            joints
                .get(usize::from(joint.index()))
                .copied()
                .unwrap_or(body)
        };
        let gaze_rig = GazeRig::new(pivot(Joint::TorsoPivot), pivot(Joint::HeadPivot));

        world.entity_mut(body).insert((
            SkinnedMesh {
                inverse_bindposes: handles.inverse_bindposes.clone(),
                joints,
            },
            gaze_rig,
        ));
    }

    CrowdReport {
        bodies: count,
        joints: count * super::rig::JOINT_COUNT,
    }
}

/// Keep every body walking around the plaza at its own radius and pace.
///
/// The desired velocity is a tangent to the circle the body is standing on, recomputed from where it
/// is now, so a body pushed towards the middle walks back out on its own instead of leaving the
/// district. Pace comes from the identifier, so the same crowd walks the same way in every run.
pub fn mill_around(
    clock: Res<SimClock>,
    mut bodies: Query<(&Npc, &Transform, &mut NpcDesiredVelocity, &mut NpcVelocity)>,
) {
    let tick = num::u64_to_f32(clock.tick.get());
    for (npc, transform, mut desired, mut velocity) in &mut bodies {
        let index = num::u32_to_usize(npc.0.to_bits());
        let position = transform.translation;
        let flat = Vec2::new(position.x, position.z);
        let radius = flat.length().max(0.5);
        // Counter-clockwise tangent, plus an outward bias that grows as a body approaches the middle.
        let tangent = Vec2::new(-flat.y, flat.x) / radius;
        let outward = flat / radius * (1.0 - (radius / 24.0).min(1.0)) * 0.35;
        let wobble = 0.85 + 0.15 * bevy_math::ops::cos(tick * 0.002 + num::usize_to_f32(index));
        let pace = walk_speed(index) * wobble;
        let direction = (tangent + outward).normalize_or_zero();
        desired.0 = Vec3::new(direction.x * pace, 0.0, direction.y * pace);
        // A body that has not been integrated yet still has to face where it is going, or the first
        // frame is two thousand mannequins staring at the camera.
        if velocity.0.length_squared() < 1.0e-6 {
            velocity.0 = desired.0;
        }
    }
}

/// Turn every body towards its own velocity, once per tick.
///
/// Facing is a rendering concern here: the simulation decides where a body is, and this is the line
/// that makes a walk read in the direction it is going rather than sideways.
pub fn face_travel(mut bodies: Query<(&NpcVelocity, &mut Transform)>) {
    for (velocity, mut transform) in &mut bodies {
        let flat = Vec2::new(velocity.0.x, velocity.0.z);
        if flat.length_squared() < 1.0e-4 {
            continue;
        }
        transform.rotation =
            Quat::from_rotation_y(yaw_for(Vec3::new(velocity.0.x, 0.0, velocity.0.z)));
    }
}

#[cfg(test)]
mod tests {
    use super::{rig_profile, spawn_position, walk_speed, yaw_for};
    use bevy_math::Vec3;

    #[test]
    fn every_body_stands_inside_the_disc_and_none_at_the_origin() {
        let radius = 22.0;
        for index in [0, 1, 500, 1_999] {
            let position = spawn_position(index, radius, 2_000);
            let distance = bevy_math::Vec2::new(position.x, position.z).length();
            assert!(
                distance <= radius + 1.0e-3,
                "body {index} stands {distance} out"
            );
            assert!(distance > 0.01, "body {index} stands on the plaza's centre");
            assert!(position.y.abs() < 1.0e-6, "body {index} floats");
        }
    }

    #[test]
    fn the_spiral_does_not_put_two_bodies_in_the_same_place() {
        let first = spawn_position(0, 22.0, 2_000);
        let second = spawn_position(1, 22.0, 2_000);
        assert!((first - second).length() > 0.01);
        let last = spawn_position(1_999, 22.0, 2_000);
        assert!((first - last).length() > 0.01);
    }

    #[test]
    fn the_crowd_walks_at_seven_speeds_inside_the_profile() {
        let profile = rig_profile().unwrap_or_else(|error| unreachable!("{error:?}"));
        let mut seen = std::collections::BTreeSet::new();
        for index in 0..14 {
            let speed = walk_speed(index);
            assert!(speed >= 1.0, "{speed} is slower than a shuffle");
            assert!(speed < profile.run_speed(), "{speed} is running");
            let _ = seen.insert(num_speed(speed));
        }
        assert_eq!(seen.len(), 7, "the speeds collapsed");
    }

    /// Speeds to the nearest centimetre per second, so the set holds numbers rather than float bits.
    fn num_speed(speed: f32) -> u32 {
        bevy_flock::domain::num::f32_to_u32((speed * 100.0).round())
    }

    #[test]
    fn the_palette_is_cycled_and_the_stature_varies() {
        // Six tints and seven heights: enough that a thousand bodies are not one body, and rational
        // enough that the same crowd looks the same in every run.
        let shortest = super::body_scale(0);
        let tallest = super::body_scale(6);
        assert!((shortest - 0.94).abs() < 1.0e-6);
        assert!((tallest - 1.06).abs() < 1.0e-6);
        let scales: Vec<f32> = (0..7).map(super::body_scale).collect();
        assert!(
            scales
                .windows(2)
                .all(|pair| matches!(pair, [low, high] if high > low)),
            "the stature steps must ascend"
        );
        assert!((super::body_scale(7) - super::body_scale(0)).abs() < 1.0e-6);
    }

    #[test]
    fn a_body_faces_the_way_it_is_going() {
        // Forward is `-Z`, so walking towards `-Z` is a yaw of zero.
        assert!(yaw_for(Vec3::new(0.0, 0.0, -1.0)).abs() < 1.0e-6);
        // Walking towards `-X` is a quarter turn to the left.
        let left = yaw_for(Vec3::new(-1.0, 0.0, 0.0));
        assert!((left - std::f32::consts::FRAC_PI_2).abs() < 1.0e-6);
        // And the two halves of a straight line differ by half a turn.
        let forward = yaw_for(Vec3::new(0.3, 0.0, -0.9));
        let backward = yaw_for(Vec3::new(-0.3, 0.0, 0.9));
        let difference = (forward - backward).abs();
        assert!((difference - std::f32::consts::PI).abs() < 1.0e-5);
    }
}
