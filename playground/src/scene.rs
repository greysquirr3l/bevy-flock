//! The Ashfall scene as Bevy entities.
//!
//! The demo crate already owns the geometry, the layout and the casting: [`Scene::build`] turns a
//! city and a crowd into instances, and [`Meshes`] loads the models those instances name. This
//! module is the one place that translation happens, and it is deliberately a pure function of the
//! demo's data: nothing here reads a clock, a device or a global, so every mapping below can be
//! tested without a GPU.

use bevy_asset::{Assets, Handle, RenderAssetUsages};
use bevy_color::Color;
use bevy_ecs::prelude::*;
use bevy_flock_demo::gltf::Mesh as GltfMesh;
use bevy_flock_demo::render3d::Meshes;
use bevy_flock_demo::scene::{BlockInstance, Instance, Scene};
use bevy_math::{Quat, Vec3};
use bevy_mesh::{Indices, Mesh as BevyMesh, Mesh3d, PrimitiveTopology};
use bevy_pbr::{MeshMaterial3d, StandardMaterial};
use bevy_transform::components::Transform;
use std::collections::BTreeMap;

/// Tints are quantised to this many levels per channel before a material is made.
///
/// A crowd of two thousand has two thousand appearances, and one material each is one bind group
/// each: the draw calls then outnumber the triangles. Four levels a channel is 64 buckets at most,
/// which is enough to keep a crowd from looking stamped and few enough that Bevy batches it.
pub const TINT_LEVELS: u8 = 4;

/// A demo mesh as the Bevy mesh asset the renderer wants.
///
/// glTF's `Y`-up, right-handed, counter-clockwise winding is what Bevy's PBR pipeline expects, so
/// the only work is splitting the interleaved index and vertex arrays into attributes.
pub fn bevy_mesh(model: &GltfMesh) -> BevyMesh {
    let mut mesh = BevyMesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(BevyMesh::ATTRIBUTE_POSITION, model.positions.clone());
    mesh.insert_attribute(BevyMesh::ATTRIBUTE_NORMAL, model.normals.clone());
    mesh.insert_indices(Indices::U32(model.indices.clone()));
    mesh
}

/// The ground: one quad, upward-facing, reaching past the last block and into the fog.
///
/// `Plane3d` would be a dependency for four vertices, and a plane this large is the cheapest thing
/// in the scene; ten lines is the better trade.
pub fn ground_mesh(extent: f32) -> BevyMesh {
    let half = extent.max(1.0) * 1.6;
    let mut mesh = BevyMesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(
        BevyMesh::ATTRIBUTE_POSITION,
        vec![
            [-half, 0.0, -half],
            [half, 0.0, -half],
            [half, 0.0, half],
            [-half, 0.0, half],
        ],
    );
    mesh.insert_attribute(BevyMesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4]);
    // Counter-clockwise from above, so the triangles face the sky rather than the underside.
    mesh.insert_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]));
    mesh
}

/// A unit cube, centred on the origin, for the ruined blocks the layout draws rather than models.
pub fn cube_mesh() -> BevyMesh {
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(24);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(24);
    let mut indices: Vec<u32> = Vec::with_capacity(36);
    for face in 0..6_u8 {
        // +X, -X, +Y, -Y, +Z, -Z: named rather than built by indexing, so the normal is a value the
        // compiler can check instead of an array something writes into.
        let normal = match face {
            0 => [1.0, 0.0, 0.0],
            1 => [-1.0, 0.0, 0.0],
            2 => [0.0, 1.0, 0.0],
            3 => [0.0, -1.0, 0.0],
            4 => [0.0, 0.0, 1.0],
            _ => [0.0, 0.0, -1.0],
        };
        // Two triangles per face, counter-clockwise from outside.
        let (u, v) = match face / 2 {
            0 => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
            1 => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            _ => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        };
        let base = u32::try_from(positions.len()).unwrap_or(0);
        let centre = Vec3::new(normal[0], normal[1], normal[2]) * 0.5;
        for (du, dv) in [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)] {
            let corner =
                centre + Vec3::new(u[0], u[1], u[2]) * du + Vec3::new(v[0], v[1], v[2]) * dv;
            positions.push([corner.x, corner.y, corner.z]);
            normals.push(normal);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let mut mesh = BevyMesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(BevyMesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(BevyMesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Where an instance stands: translated, turned about up, and scaled uniformly.
pub fn transform_for(instance: &Instance) -> Transform {
    Transform::from_translation(instance.position)
        .with_rotation(Quat::from_rotation_y(instance.yaw))
        .with_scale(Vec3::splat(instance.scale))
}

/// Where a ruined block stands: the layout gives a centre and half-extents.
pub fn block_transform(block: &BlockInstance) -> Transform {
    Transform::from_translation(block.centre).with_scale(block.size * 2.0)
}

/// A block's colour: standing concrete, weathered towards rubble grey as `ruin` rises.
pub fn block_material(block: &BlockInstance) -> StandardMaterial {
    let ruin = block.ruin.clamp(0.0, 1.0);
    let standing = [0.42, 0.40, 0.37];
    let rubble = [0.24, 0.22, 0.20];
    let mix = |index: usize| {
        standing.get(index).copied().unwrap_or(0.0) * (1.0 - ruin)
            + rubble.get(index).copied().unwrap_or(0.0) * ruin
    };
    StandardMaterial {
        base_color: Color::srgb(mix(0), mix(1), mix(2)),
        perceptual_roughness: 0.95,
        ..StandardMaterial::default()
    }
}

/// The bucket an instance's tint falls in, one of [`TINT_LEVELS`] cubed.
///
/// Quantised in sRGB space rather than linear: the eye is matching paint, and the buckets are
/// chosen to be evenly spaced as seen.
pub fn tint_bucket(tint: [f32; 4]) -> [u8; 3] {
    // Bands rather than a scaled cast: four comparisons say exactly what the buckets are, and the
    // top band takes everything from three up, which is where a tint of 1.0 lands after the clamp.
    let quantise = |channel: f32| match channel.clamp(0.0, 1.0) * f32::from(TINT_LEVELS) {
        scaled if scaled < 1.0 => 0,
        scaled if scaled < 2.0 => 1,
        scaled if scaled < 3.0 => 2,
        _ => TINT_LEVELS - 1,
    };
    [quantise(tint[0]), quantise(tint[1]), quantise(tint[2])]
}

/// A material for a tint bucket, at the centre of the bucket so it is not biased towards one edge.
pub fn material_for(bucket: [u8; 3]) -> StandardMaterial {
    let centre = |level: u8| {
        let levels = f32::from(TINT_LEVELS);
        (f32::from(level) + 0.5) / levels
    };
    StandardMaterial {
        base_color: Color::srgb(centre(bucket[0]), centre(bucket[1]), centre(bucket[2])),
        perceptual_roughness: 0.85,
        metallic: 0.0,
        ..StandardMaterial::default()
    }
}

/// The scene, loaded and ready to spawn.
///
/// Built before the app runs, so a missing asset or a bad seed is an error on the command line
/// rather than a panic inside a startup system with a window already open.
#[derive(Resource)]
pub struct AshfallScene {
    /// The layout, the crowd per view, the ruins.
    pub scene: Scene,
    /// The models the scene names.
    pub meshes: Meshes,
    /// Every view the demo ships, in order.
    pub views: Vec<&'static str>,
    /// Half-extent of the layout in metres, so the ground reaches past the last block.
    pub extent: f32,
}

impl AshfallScene {
    /// Assemble the scene the software renderer would have drawn.
    pub const fn new(scene: Scene, meshes: Meshes, views: Vec<&'static str>, extent: f32) -> Self {
        Self {
            scene,
            meshes,
            views,
            extent,
        }
    }
}

/// The crowd entity's slot in the body list, so a frame can find the pose that belongs to it.
///
/// The scene is spawned from a snapshot sorted by identity, and `Playground::bodies` sorts the same
/// way, so slot `n` of the snapshot is slot `n` of every later frame. That holds until the
/// population changes, which is why the simulation checks the count before it writes a pose.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyLink(pub usize);

/// Mesh and material handles, resolved once per model and once per tint bucket.
#[derive(Default)]
struct Handles {
    meshes: BTreeMap<&'static str, Handle<BevyMesh>>,
    materials: BTreeMap<[u8; 3], Handle<StandardMaterial>>,
}

impl Handles {
    /// The pair one instance draws with, or `None` if the scene names a model it does not carry.
    fn of(
        &mut self,
        instance: &Instance,
        scene: &AshfallScene,
        meshes: &mut Assets<BevyMesh>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Option<(Handle<BevyMesh>, Handle<StandardMaterial>)> {
        let model = scene.meshes.get(instance.model)?;
        let mesh = self
            .meshes
            .entry(instance.model)
            .or_insert_with(|| meshes.add(bevy_mesh(model)))
            .clone();
        let bucket = tint_bucket(instance.tint);
        let material = self
            .materials
            .entry(bucket)
            .or_insert_with(|| materials.add(material_for(bucket)))
            .clone();
        Some((mesh, material))
    }
}

/// Spawn every instance in the scene, one entity per body, merged materials per tint bucket.
///
/// Bodies share a mesh per model and a material per bucket, which is what lets Bevy draw a
/// two-thousand-strong crowd in a handful of batches.
pub fn spawn_scene(
    mut commands: Commands,
    scene: Res<AshfallScene>,
    mut meshes_assets: ResMut<Assets<BevyMesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut handles = Handles::default();
    // The crowd, linked to its slot so the simulation can move it afterwards.
    for (index, instance) in scene.scene.bodies.iter().enumerate() {
        let Some((mesh, material)) =
            handles.of(instance, &scene, &mut meshes_assets, &mut materials)
        else {
            continue;
        };
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            transform_for(instance),
            BodyLink(index),
        ));
    }
    // The scenery: pylons, shelters and stalls never move, so they carry no link.
    for instance in scene
        .scene
        .pylons
        .iter()
        .chain(scene.scene.shelters.iter())
        .chain(scene.scene.stalls.iter())
    {
        let Some((mesh, material)) =
            handles.of(instance, &scene, &mut meshes_assets, &mut materials)
        else {
            continue;
        };
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            transform_for(instance),
        ));
    }
    // The city sits on something. Without this the crowd stands in the clear colour and the blocks
    // float, which reads as a broken renderer rather than as an empty plain.
    let ground = meshes_assets.add(ground_mesh(scene.extent));
    commands.spawn((
        Mesh3d(ground),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.36, 0.34, 0.30),
            perceptual_roughness: 0.95,
            ..StandardMaterial::default()
        })),
        Transform::default(),
    ));
    let cube = meshes_assets.add(cube_mesh());
    for block in &scene.scene.blocks {
        let material = handles
            .materials
            .entry(tint_bucket([
                block.ruin.clamp(0.0, 1.0) * -0.18 + 0.42,
                0.40,
                0.37,
                1.0,
            ]))
            .or_insert_with(|| materials.add(block_material(block)))
            .clone();
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(material),
            block_transform(block),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::testkit::OrDie;
    use bevy_flock_demo::render3d::Meshes;

    /// One instance, at a place and a facing that makes a wrong axis obvious.
    fn instance() -> Instance {
        Instance {
            model: "walker",
            position: Vec3::new(3.0, 0.0, -4.0),
            yaw: core::f32::consts::FRAC_PI_2,
            scale: 2.0,
            tint: [0.5, 0.25, 1.0, 1.0],
        }
    }

    /// Every shipped model survives the trip with its arrays intact and its indices in range.
    ///
    /// Run over all of them rather than the one the district happens to use: the scene names models
    /// by slug, so a model that loses its normals is a whole class of prop drawn flat.
    #[test]
    fn every_model_becomes_a_triangle_mesh() {
        let meshes = Meshes::load().or_die();
        let mut with_normals = 0;
        for slug in bevy_flock_demo::assets::MODELS
            .iter()
            .map(|model| model.slug)
        {
            let model = meshes.get(slug).or_die();
            let mesh = bevy_mesh(model);
            let positions = mesh.attribute(BevyMesh::ATTRIBUTE_POSITION).or_die().len();
            let indices = mesh.indices().or_die().len();
            assert_eq!(positions, model.positions.len(), "positions for {slug}");
            assert_eq!(indices, model.indices.len(), "indices for {slug}");
            assert_eq!(
                mesh.primitive_topology(),
                PrimitiveTopology::TriangleList,
                "{slug} is not triangles"
            );
            if let Some(normals) = mesh.attribute(BevyMesh::ATTRIBUTE_NORMAL) {
                assert_eq!(normals.len(), model.normals.len(), "normals for {slug}");
                with_normals += 1;
            } else {
                assert!(
                    model.normals.is_empty(),
                    "{slug} has normals that did not cross"
                );
            }
            // Every index addresses a vertex: a model that survives this can be drawn.
            for index in mesh.indices().or_die().iter() {
                assert!(
                    index < positions,
                    "{slug}: index {index} is past {positions}"
                );
            }
        }
        assert!(with_normals > 0, "no shipped model carries normals");
    }

    /// The cube is six quads, closed, with each face's four corners sharing its normal.
    #[test]
    fn the_cube_is_a_closed_box() {
        let mesh = cube_mesh();
        let positions = mesh.attribute(BevyMesh::ATTRIBUTE_POSITION).or_die();
        let normals = mesh.attribute(BevyMesh::ATTRIBUTE_NORMAL).or_die();
        assert_eq!(positions.len(), 24);
        assert_eq!(normals.len(), 24);
        assert_eq!(mesh.indices().or_die().len(), 36);
        // Each of the six normals appears exactly four times, and the bits are the key so that
        // `-0.0` and `0.0` are not counted as one face.
        let mut seen: BTreeMap<[u32; 3], usize> = BTreeMap::new();
        if let Some(bevy_mesh::VertexAttributeValues::Float32x3(normals)) =
            mesh.attribute(BevyMesh::ATTRIBUTE_NORMAL)
        {
            for [x, y, z] in normals.iter().copied() {
                *seen
                    .entry([x.to_bits(), y.to_bits(), z.to_bits()])
                    .or_insert(0) += 1;
            }
        }
        assert_eq!(seen.len(), 6, "six faces, six normals");
        assert!(
            seen.values().all(|count| *count == 4),
            "four corners a face"
        );
    }

    /// Translation, facing and scale all land on the transform, and the facing is about up.
    #[test]
    fn an_instance_becomes_a_transform() {
        let transform = transform_for(&instance());
        assert_eq!(transform.translation, Vec3::new(3.0, 0.0, -4.0));
        assert_eq!(transform.scale, Vec3::splat(2.0));
        // A quarter turn about Y sends +X to -Z.
        let forward = transform.rotation * Vec3::X;
        assert!(forward.distance(Vec3::NEG_Z) < 1e-5, "got {forward}");
        // And it leaves the height alone: the models stand on the ground plane.
        assert!(forward.y.abs() < 1e-6);
    }

    /// A block is scaled by its half-extents doubled, so a 2 m half-extent is a 4 m block.
    #[test]
    fn a_block_becomes_a_scaled_cube() {
        let block = BlockInstance {
            centre: Vec3::new(10.0, 2.0, -6.0),
            size: Vec3::new(2.0, 2.0, 3.0),
            ruin: 0.0,
        };
        let transform = block_transform(&block);
        assert_eq!(transform.translation, block.centre);
        assert_eq!(transform.scale, Vec3::new(4.0, 4.0, 6.0));
    }

    /// Tints collapse to at most sixty-four buckets, and two tints in one bucket stay in one.
    #[test]
    fn tints_quantise_to_a_bounded_number_of_materials() {
        let mut buckets = std::collections::BTreeSet::new();
        for red in 0..64_u8 {
            for green in 0..64_u8 {
                let tint = [f32::from(red) / 63.0, f32::from(green) / 63.0, 0.5, 1.0];
                buckets.insert(tint_bucket(tint));
            }
        }
        let limit = usize::from(TINT_LEVELS).pow(3);
        assert!(
            buckets.len() <= limit,
            "{} buckets is more than {limit}",
            buckets.len()
        );
        assert_eq!(
            tint_bucket([0.5, 0.25, 1.0, 1.0]),
            tint_bucket([0.51, 0.26, 0.99, 1.0])
        );
        // The extremes land in the corner buckets rather than wrapping or panicking.
        assert_eq!(tint_bucket([0.0, 0.0, 0.0, 1.0]), [0, 0, 0]);
        assert_eq!(tint_bucket([1.0, 1.0, 1.0, 1.0]), [3, 3, 3]);
        assert_eq!(tint_bucket([-2.0, 5.0, 0.5, 1.0]), [0, 3, 2]);
    }

    /// A standing block is lighter than a rubble one, and neither is black.
    #[test]
    fn ruin_weathers_a_block_without_extinguishing_it() {
        let standing = block_material(&BlockInstance {
            centre: Vec3::ZERO,
            size: Vec3::ONE,
            ruin: 0.0,
        });
        let rubble = block_material(&BlockInstance {
            centre: Vec3::ZERO,
            size: Vec3::ONE,
            ruin: 1.0,
        });
        let standing = standing.base_color.to_srgba();
        let rubble = rubble.base_color.to_srgba();
        assert!(standing.red > rubble.red);
        assert!(rubble.red > 0.1);
    }

    /// Every model the scene can name resolves to a mesh, which is the assumption `spawn_scene`
    /// is written against: a slug with no model would be a silent hole in the city.
    #[test]
    fn every_shipped_model_resolves() {
        let meshes = Meshes::load().or_die();
        for slug in bevy_flock_demo::assets::MODELS
            .iter()
            .map(|model| model.slug)
        {
            assert!(meshes.get(slug).is_some(), "no mesh for `{slug}`");
        }
    }
}
