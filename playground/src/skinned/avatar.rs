//! The district's avatars: the three Meshy characters, rigged and walking.
//!
//! The mannequin rig next door is built in code: nine joints, eight boxes, clips authored as
//! keyframes. This module is the other half of the picture — a character modelled as a mesh with a
//! texture, rigged by Meshy into 24 bones, with idle, walk and run clips recorded onto it. Every
//! asset comes from `demo/assets/npcs/rigged/`, one file per avatar per clip, and Bevy's glTF loader
//! builds the bones, the skin and the clip assets.
//!
//! The one piece of glue is the [`AnimatedBy`] rewrite: the loader points every bone at the scene's
//! own animation root, and this crowd wants the bone to point at the body so that the shipped
//! locomotion blend, the gesture channel and the motion gate all see one entity per NPC rather than
//! two.

use bevy_animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
use bevy_animation::{AnimatedBy, AnimationPlayer};
use bevy_asset::{AssetServer, Assets, Handle};
use bevy_camera::visibility::Visibility;
use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::*;
use bevy_flock::domain::num;
use bevy_flock::prelude::{GazeAim, NpcBundle, NpcGestures, NpcId};
use bevy_flock_anim::{ClipBindings, LocomotionProfile};
use bevy_gltf::Gltf;
use bevy_math::Vec3;
use bevy_transform::components::Transform;
use bevy_world_serialization::WorldAssetRoot;
use std::collections::HashMap;
use std::path::Path;

use super::SpawnPositions;
use super::crowd::{body_scale, rig_profile, spawn_position};

/// The repository's asset directory, resolved at compile time so the example runs from any directory.
pub const ASSET_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../demo/assets");

/// Where the rigged avatars live, relative to the asset root.
const RIGGED: &str = "npcs/rigged";

/// Whether the rigged avatar files are where [`ASSET_ROOT`] says they are.
///
/// The example falls back to the mannequin when they are not: a checkout without the generated
/// assets should still run, and a crowd of nothing is a worse answer than a crowd of boxes.
#[must_use]
pub fn assets_present() -> bool {
    Path::new(ASSET_ROOT)
        .join(RIGGED)
        .join("aerie.glb")
        .is_file()
}

/// The three characters, in the order they cycle through the crowd.
pub const NAMES: [&str; 3] = ["aerie", "gatewarden", "scavenger"];

/// Bodies to spawn when the caller did not say: a rigged avatar is nine thousand triangles and
/// twenty-four bones, so a plaza holds fewer of them than it holds mannequins.
pub const DEFAULT_POPULATION: usize = 300;

/// The clips one avatar's graph carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvatarClips {
    /// Standing still.
    pub idle: AnimationNodeIndex,
    /// The walk cycle the blend calls at low speed.
    pub walk: AnimationNodeIndex,
    /// The run cycle, at high speed.
    pub run: AnimationNodeIndex,
}

/// One avatar's scene, graph and clip nodes.
#[derive(Debug, Clone)]
pub struct AvatarKind {
    /// The file name stem, for the report.
    pub name: &'static str,
    /// The glTF scene: mesh, texture, skeleton and skin.
    pub scene: Handle<bevy_world_serialization::WorldAsset>,
    /// The graph holding idle, walk and run as sibling nodes.
    pub graph: Handle<AnimationGraph>,
    /// Where those three sit in the graph.
    pub clips: AvatarClips,
}

/// Every avatar the crowd can wear.
#[derive(Resource, Debug, Default, Clone)]
pub struct Avatars(Vec<AvatarKind>);

impl Avatars {
    /// The avatar body `index` wears.
    ///
    /// Cycled by index, so a crowd is a mix of three characters and the same body wears the same
    /// character in every run.
    #[must_use]
    pub fn kind(&self, index: usize) -> Option<&AvatarKind> {
        if self.0.is_empty() {
            return None;
        }
        self.0.get(index % self.0.len())
    }

    /// How many characters were loaded.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no character loaded, which is the only state that stops the crowd spawning.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One avatar's four files: the rig, and one file per clip.
#[derive(Debug, Clone)]
struct AvatarSource {
    name: &'static str,
    rig: Handle<Gltf>,
    idle: Handle<Gltf>,
    walk: Handle<Gltf>,
    run: Handle<Gltf>,
}

/// The load in flight, removed once every file has arrived and the graphs are built.
#[derive(Resource, Debug)]
pub struct Loading(Vec<AvatarSource>);

/// Which avatar a body wears, and the marker the bone rewrite looks for.
#[derive(Component, Debug, Clone, Copy)]
pub struct AvatarBody {
    /// Index into [`Avatars`].
    pub kind: usize,
}

/// Start loading the twelve files. Called once, from `Startup`.
pub(super) fn load(server: Res<AssetServer>, mut commands: Commands) {
    let sources = NAMES
        .into_iter()
        .map(|name| AvatarSource {
            name,
            rig: server.load(format!("{RIGGED}/{name}.glb")),
            idle: server.load(format!("{RIGGED}/{name}-idle.glb")),
            walk: server.load(format!("{RIGGED}/{name}-walk.glb")),
            run: server.load(format!("{RIGGED}/{name}-run.glb")),
        })
        .collect();
    commands.insert_resource(Loading(sources));
}

/// Build a graph per avatar once its four files have loaded, then spawn the crowd.
///
/// A glTF file's clip is one animation; the graph is what makes three of them a blend, so the graph
/// is built here rather than authored as an asset. Idle starts at full weight and the other two at
/// zero, which is the pose a body holds on the frame before the blend runs.
pub(super) fn build_and_spawn(
    mut commands: Commands,
    loading: Option<Res<Loading>>,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut profiles: ResMut<Assets<LocomotionProfile>>,
    run: Option<Res<super::Run>>,
    mut spawn_positions: ResMut<SpawnPositions>,
) {
    let Some(loading) = loading else {
        return;
    };
    if loading.0.is_empty() {
        commands.remove_resource::<Loading>();
        return;
    }
    // All or nothing: a half-loaded set would spawn a crowd where two thirds of the bodies stand in
    // a T-pose, and the frame that proves it would be the one the user looks at.
    let Some(loaded) = loading
        .0
        .iter()
        .map(|source| loaded(source, &gltfs))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let Some(profile) = rig_profile().ok().map(|profile| profiles.add(profile)) else {
        eprintln!("skinned: the avatar profile was rejected");
        return;
    };

    let mut kinds = Vec::with_capacity(loaded.len());
    for one in loaded {
        let mut graph = AnimationGraph::new();
        let idle = graph.add_clip(one.idle, 1.0, graph.root);
        let walk = graph.add_clip(one.walk, 0.0, graph.root);
        let run = graph.add_clip(one.run, 0.0, graph.root);
        kinds.push(AvatarKind {
            name: one.name,
            scene: one.scene,
            graph: graphs.add(graph),
            clips: AvatarClips { idle, walk, run },
        });
    }
    let avatars = Avatars(kinds);

    let population = run
        .as_ref()
        .map_or(DEFAULT_POPULATION, |run| run.options.population);
    let radius = run.as_ref().map_or(28.0, |run| run.options.radius);
    let mut placements = HashMap::with_capacity(population);
    for index in 0..population {
        let Some(kind) = avatars.kind(index) else {
            break;
        };
        let position = spawn_position(index, radius, population);
        let id = NpcId::unique(num::usize_to_u32(index));
        let mut bundle = NpcBundle::default_for(id);
        bundle.transform =
            Transform::from_translation(position).with_scale(Vec3::splat(body_scale(index)));
        let body = commands
            .spawn((
                bundle,
                // The scene's nodes inherit visibility from the body, so the body needs one: a
                // spawn point with no `Visibility` is a subtree that is never drawn.
                Visibility::default(),
                AvatarBody { kind: index },
                WorldAssetRoot(kind.scene.clone()),
                AnimationPlayer::default(),
                AnimationGraphHandle(kind.graph.clone()),
                ClipBindings::new(
                    profile.clone(),
                    kind.clips.idle,
                    kind.clips.walk,
                    kind.clips.run,
                ),
                // The motion gate reads these on every body, and a body with no gesture layer is a
                // body the gate cannot see. An avatar has no Meshy gesture clips, so the cue never
                // fires; the components still have to be there for the sample to count it.
                NpcGestures::new(),
                GazeAim::default(),
            ))
            .id();
        placements.insert(body, position);
    }
    for (entity, position) in placements {
        spawn_positions.0.insert(entity, position);
    }
    println!(
        "skinned: spawned {population} avatars over {} characters",
        avatars.len()
    );
    commands.insert_resource(avatars);
    commands.remove_resource::<Loading>();
}

/// The four files one avatar needs, once the loader has finished with them.
struct Loaded {
    name: &'static str,
    scene: Handle<bevy_world_serialization::WorldAsset>,
    idle: Handle<bevy_animation::AnimationClip>,
    walk: Handle<bevy_animation::AnimationClip>,
    run: Handle<bevy_animation::AnimationClip>,
}

/// The four files one avatar needs, or `None` while any of them is still loading.
///
/// Each clip file carries exactly one animation, so "the clip" is the file's first animation rather
/// than something looked up by name: Meshy names them `Armature|walking_man|baselayer`, which is a
/// name to break on, and the file is the contract.
fn loaded(source: &AvatarSource, gltfs: &Assets<Gltf>) -> Option<Loaded> {
    let rig = gltfs.get(&source.rig)?;
    let clip = |handle: &Handle<Gltf>| gltfs.get(handle)?.animations.first().cloned();
    Some(Loaded {
        name: source.name,
        scene: rig
            .default_scene
            .clone()
            .or_else(|| rig.scenes.first().cloned())?,
        idle: clip(&source.idle)?,
        walk: clip(&source.walk)?,
        run: clip(&source.run)?,
    })
}

/// Point every bone at the body that owns it, so one entity per NPC carries the animation.
pub(super) fn rebind_bones(
    avatars: Option<Res<Avatars>>,
    bodies: Query<(Entity, &AvatarBody)>,
    parents: Query<&ChildOf>,
    mut bones: Query<(&ChildOf, &mut AnimatedBy), Added<AnimatedBy>>,
) {
    if avatars.is_none() {
        return;
    }
    for (parent, mut animated_by) in &mut bones {
        let mut cursor = parent.parent();
        let mut owner = None;
        for _ in 0..8 {
            if bodies.get(cursor).is_ok() {
                owner = Some(cursor);
                break;
            }
            let Ok(next) = parents.get(cursor) else {
                break;
            };
            cursor = next.parent();
        }
        if let Some(body) = owner {
            animated_by.0 = body;
        }
    }
}
