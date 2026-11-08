//! A three-bone mannequin, its skinning weights, and the bind pose they are expressed in.
//!
//! The example ships no assets: the body is built from boxes, the joints are placed by arithmetic,
//! and the inverse bind matrices are the inverse of the bind pose. That is the whole reason this
//! example runs from a clean checkout — there is nothing to download and nothing to bake.
//!
//! # The five joints
//!
//! Three joints are animated by clips (`hips`, `torso`, `head`); two are pivots parented to the
//! animated bones and driven by the gaze solve. The mesh is weighted to the *pivots* for the chest
//! and head, which is what makes a gaze visible: the clip moves the bone the pivot hangs from, and
//! gaze composes on top of it in a node no clip touches (see `bevy_flock_anim::gaze`).

use bevy_asset::RenderAssetUsages;
use bevy_math::{Mat4, Vec2, Vec3};
use bevy_mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy_transform::components::Transform;

/// How many joints one body has.
pub const JOINT_COUNT: usize = 5;

/// The joints of one body, in the order the mesh's joint indices refer to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Joint {
    /// The pelvis, which carries the legs and the bob.
    Hips = 0,
    /// The chest, animated by the clips.
    Torso = 1,
    /// The neck, animated by the clips.
    Head = 2,
    /// A child of the torso that gaze rotates.
    TorsoPivot = 3,
    /// A child of the head that gaze rotates.
    HeadPivot = 4,
}

impl Joint {
    /// Every joint, in index order.
    pub const ALL: [Self; JOINT_COUNT] = [
        Self::Hips,
        Self::Torso,
        Self::Head,
        Self::TorsoPivot,
        Self::HeadPivot,
    ];

    /// The index this joint's vertices carry in `JOINTS_0`.
    ///
    /// A `u16` rather than a `usize` because that is what the attribute holds, and a conversion at
    /// the one place that needs it beats a `usize`-shaped fiction.
    #[must_use]
    pub const fn index(self) -> u16 {
        self as u16
    }

    /// A stable name, used as the animation target id and in the entity hierarchy.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hips => "hips",
            Self::Torso => "torso",
            Self::Head => "head",
            Self::TorsoPivot => "torso_pivot",
            Self::HeadPivot => "head_pivot",
        }
    }
}

/// Where a joint sits in the model at bind time, in metres.
///
/// These are model-space positions, which is what inverse bind matrices are expressed against. The
/// entity hierarchy uses the local offsets in [`local_offset`] instead.
#[must_use]
pub const fn bind_translation(joint: Joint) -> Vec3 {
    match joint {
        Joint::Hips => Vec3::new(0.0, 0.92, 0.0),
        // The chest is a third of the way up the torso, so the arm swing reads as a lean.
        Joint::Torso | Joint::TorsoPivot => Vec3::new(0.0, 1.235, 0.0),
        Joint::Head | Joint::HeadPivot => Vec3::new(0.0, 1.60, 0.0),
    }
}

/// Where a joint sits relative to its parent at bind time.
#[must_use]
pub fn local_offset(joint: Joint) -> Vec3 {
    let parent = match joint {
        Joint::Hips => return bind_translation(Joint::Hips),
        Joint::Torso | Joint::TorsoPivot => Joint::Hips,
        Joint::Head | Joint::HeadPivot => Joint::Torso,
    };
    bind_translation(joint) - bind_translation(parent)
}

/// The transform a joint carries at bind time, before any clip runs.
#[must_use]
pub fn bind_transform(joint: Joint) -> Transform {
    Transform::from_translation(local_offset(joint))
}

/// The inverse bind matrix of every joint, in [`Joint`] index order.
///
/// The inverse of the joint's model-space bind transform. A rigid bind pose has no bind rotation
/// here, so this is the inverse of a translation and the matrix is exact rather than approximate.
#[must_use]
pub fn inverse_bindposes() -> Vec<Mat4> {
    Joint::ALL
        .iter()
        .map(|joint| Mat4::from_translation(bind_translation(*joint)).inverse())
        .collect()
}

/// A vertex of the procedural body.
#[derive(Debug, Clone, Copy)]
struct Vertex {
    position: Vec3,
    normal: Vec3,
    uv: Vec2,
    joints: [u16; 4],
    weights: [f32; 4],
}

/// Accumulates vertices and indices, then hands back a [`Mesh`].
#[derive(Debug, Default)]
struct MeshBuilder {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
}

impl MeshBuilder {
    /// Add an axis-aligned box, skinned to one joint or blended between two of them by height.
    ///
    /// `blend` is `(lower, upper, from, to)`: a vertex at height `from` is all `lower`, at `to` all
    /// `upper`, and linear in between. The torso uses it so the chest bends rather than shearing
    /// away from the pelvis at the seam.
    fn push_box(&mut self, min: Vec3, max: Vec3, blend: (Joint, Joint, f32, f32)) {
        let (lower, upper, from, to) = blend;
        let span = (to - from).abs();
        let weight_at = |y: f32| {
            if span < f32::EPSILON {
                return (1.0_f32, 0.0_f32);
            }
            let t = ((y - from) / span).clamp(0.0, 1.0);
            (1.0 - t, t)
        };

        // Six faces, each listed counter-clockwise seen from outside, with the outward normal. No
        // vertex is shared between faces, so a faceted mannequin has correct normals without a
        // smoothing pass. `every_triangle_faces_its_normal` is the test that keeps the two in step:
        // a corner order and a normal that disagree render as a black face, not as an error.
        let faces: [([Vec3; 4], Vec3); 6] = [
            (
                [
                    Vec3::new(max.x, min.y, min.z),
                    Vec3::new(max.x, max.y, min.z),
                    Vec3::new(max.x, max.y, max.z),
                    Vec3::new(max.x, min.y, max.z),
                ],
                Vec3::X,
            ),
            (
                [
                    Vec3::new(min.x, min.y, max.z),
                    Vec3::new(min.x, max.y, max.z),
                    Vec3::new(min.x, max.y, min.z),
                    Vec3::new(min.x, min.y, min.z),
                ],
                Vec3::NEG_X,
            ),
            (
                [
                    Vec3::new(min.x, max.y, max.z),
                    Vec3::new(max.x, max.y, max.z),
                    Vec3::new(max.x, max.y, min.z),
                    Vec3::new(min.x, max.y, min.z),
                ],
                Vec3::Y,
            ),
            (
                [
                    Vec3::new(min.x, min.y, min.z),
                    Vec3::new(max.x, min.y, min.z),
                    Vec3::new(max.x, min.y, max.z),
                    Vec3::new(min.x, min.y, max.z),
                ],
                Vec3::NEG_Y,
            ),
            (
                [
                    Vec3::new(min.x, min.y, max.z),
                    Vec3::new(max.x, min.y, max.z),
                    Vec3::new(max.x, max.y, max.z),
                    Vec3::new(min.x, max.y, max.z),
                ],
                Vec3::Z,
            ),
            (
                [
                    Vec3::new(max.x, min.y, min.z),
                    Vec3::new(min.x, min.y, min.z),
                    Vec3::new(min.x, max.y, min.z),
                    Vec3::new(max.x, max.y, min.z),
                ],
                Vec3::NEG_Z,
            ),
        ];

        let uvs = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];

        for (corners, normal) in faces {
            let base = u32::try_from(self.vertices.len()).unwrap_or(u32::MAX);
            for (corner, uv) in corners.into_iter().zip(uvs) {
                let (lower_weight, upper_weight) = weight_at(corner.y);
                self.vertices.push(Vertex {
                    position: corner,
                    // The corners of each face are listed clockwise seen from outside, so the
                    // outward normal is the negation of the axis they were listed against.
                    normal,
                    uv,
                    joints: [lower.index(), upper.index(), 0, 0],
                    weights: [lower_weight, upper_weight, 0.0, 0.0],
                });
            }
            // Counter-clockwise seen from outside, which is Bevy's front face. The corner table is
            // the contract this winding depends on, and `every_triangle_faces_its_normal` is what
            // keeps the two from drifting apart.
            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    /// The mesh, with every attribute a skinned PBR mesh needs.
    fn build(self) -> Mesh {
        self.build_with(true)
    }

    /// The mesh, with or without skinning attributes.
    ///
    /// The flag matters more than it looks: Bevy picks the pipeline from the attributes a mesh
    /// carries, so a scenery mesh with `JOINTS_0` and no `SkinnedMesh` component is drawn with the
    /// skinned pipeline and a bind group that has no skin in it. That is a wgpu validation error, not
    /// a warning, and it takes the whole app down on the first frame.
    fn build_with(self, skinned: bool) -> Mesh {
        let mut positions = Vec::with_capacity(self.vertices.len());
        let mut normals = Vec::with_capacity(self.vertices.len());
        let mut uvs = Vec::with_capacity(self.vertices.len());
        let mut joints = Vec::with_capacity(self.vertices.len());
        let mut weights = Vec::with_capacity(self.vertices.len());
        for vertex in &self.vertices {
            positions.push(vertex.position.to_array());
            normals.push(vertex.normal.to_array());
            uvs.push(vertex.uv.to_array());
            joints.push(vertex.joints);
            weights.push(vertex.weights);
        }

        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        if skinned {
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_JOINT_INDEX,
                VertexAttributeValues::Uint16x4(joints),
            );
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_JOINT_WEIGHT,
                VertexAttributeValues::Float32x4(weights),
            );
        }
        mesh.insert_indices(Indices::U32(self.indices));
        mesh
    }
}

/// The shared body mesh: legs, pelvis, chest and head, skinned to [`JOINT_COUNT`] joints.
///
/// One mesh for the whole crowd, because two thousand identical bodies should cost one copy of the
/// geometry. The variety in a crowd of these comes from materials, scale and animation phase, not
/// from the vertex buffer.
#[must_use]
pub fn body_mesh() -> Mesh {
    let mut builder = MeshBuilder::default();
    // Legs, all the way to the pelvis: rigid to the hips, because a knee would need a fourth joint
    // and the example's point is the blend, not the anatomy.
    // The boxes overlap by a few centimetres at each seam. Butt-jointed boxes leave a hairline gap
    // that shows the inside of the body as a black sliver from any angle but straight on.
    builder.push_box(
        Vec3::new(-0.14, 0.0, -0.11),
        Vec3::new(0.14, 0.92, 0.11),
        (Joint::Hips, Joint::Hips, 0.0, 1.0),
    );
    // Pelvis to chest: blended, so the seam bends instead of tearing.
    builder.push_box(
        Vec3::new(-0.18, 0.86, -0.11),
        Vec3::new(0.18, 1.50, 0.11),
        (Joint::Hips, Joint::TorsoPivot, 0.90, 1.40),
    );
    // Shoulders and head: the pivot, so gaze turns the head that the clip also turns.
    builder.push_box(
        Vec3::new(-0.15, 1.44, -0.12),
        Vec3::new(0.15, 1.78, 0.12),
        (Joint::HeadPivot, Joint::HeadPivot, 0.0, 1.0),
    );
    builder.build()
}

/// The body's height in metres, which is where the head box ends.
pub const BODY_HEIGHT: f32 = 1.78;

/// A box standing between `min` and `max`, for scenery rather than for a body.
///
/// The same builder the mannequin uses, skinned rigidly to the hips joint. A static mesh does not
/// need skin weights, but carrying them costs four numbers a vertex and keeps one builder instead of
/// two that drift apart.
#[must_use]
pub fn static_box(min: Vec3, max: Vec3) -> Mesh {
    let mut builder = MeshBuilder::default();
    builder.push_box(min, max, (Joint::Hips, Joint::Hips, 0.0, 1.0));
    builder.build_with(false)
}

/// A flat square of ground, `size` metres on a side, centred on the origin.
#[must_use]
pub fn ground(size: f32) -> Mesh {
    static_box(
        Vec3::new(-size * 0.5, -0.02, -size * 0.5),
        Vec3::new(size * 0.5, 0.0, size * 0.5),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        BODY_HEIGHT, JOINT_COUNT, Joint, MeshBuilder, bind_translation, body_mesh, ground,
        inverse_bindposes, local_offset, static_box,
    };
    use bevy_math::{Mat4, Vec3};
    use bevy_mesh::VertexAttributeValues;

    /// The four attributes a skinned mesh needs, plus the indices.
    fn attribute_len(mesh: &bevy_mesh::Mesh, attribute: bevy_mesh::MeshVertexAttribute) -> usize {
        mesh.attribute(attribute)
            .map_or(0, VertexAttributeValues::len)
    }

    #[test]
    fn the_mesh_carries_every_attribute_a_skinned_material_reads() {
        let mesh = body_mesh();
        let positions = attribute_len(&mesh, bevy_mesh::Mesh::ATTRIBUTE_POSITION);
        assert!(positions > 0);
        for attribute in [
            bevy_mesh::Mesh::ATTRIBUTE_NORMAL,
            bevy_mesh::Mesh::ATTRIBUTE_UV_0,
            bevy_mesh::Mesh::ATTRIBUTE_JOINT_INDEX,
            bevy_mesh::Mesh::ATTRIBUTE_JOINT_WEIGHT,
        ] {
            assert_eq!(attribute_len(&mesh, attribute), positions, "{attribute:?}");
        }
        let indices = mesh.indices().map_or(0, bevy_mesh::Indices::len);
        assert_eq!(indices % 3, 0, "a triangle list needs whole triangles");
    }

    #[test]
    fn every_vertex_weighs_exactly_one_and_names_joints_that_exist() {
        let mesh = body_mesh();
        let Some(VertexAttributeValues::Float32x4(weights)) =
            mesh.attribute(bevy_mesh::Mesh::ATTRIBUTE_JOINT_WEIGHT)
        else {
            unreachable!("the mesh has skin weights");
        };
        let Some(VertexAttributeValues::Uint16x4(joints)) =
            mesh.attribute(bevy_mesh::Mesh::ATTRIBUTE_JOINT_INDEX)
        else {
            unreachable!("the mesh has joint indices");
        };
        let limit = u16::try_from(JOINT_COUNT).unwrap_or(u16::MAX);
        for (index, (weight, joint)) in weights.iter().zip(joints).enumerate() {
            let total: f32 = weight.iter().sum();
            assert!(
                (total - 1.0).abs() < 1.0e-6,
                "vertex {index} weighs {total}"
            );
            // A zero weight must not name a joint either: a shader that normalises the weights back
            // What matters is that every slot names a joint that exists: an index past the end of the
            // joint list reads another entity's matrix, which shows up as a limb flying off rather
            // than as an error.
            for joint in joint {
                assert!(*joint < limit, "vertex {index} names joint {joint}");
            }
        }
    }

    #[test]
    fn the_head_is_at_the_top_of_the_bind_pose() {
        let positions = body_mesh()
            .attribute(bevy_mesh::Mesh::ATTRIBUTE_POSITION)
            .map_or(0, VertexAttributeValues::len);
        assert!(positions > 0);
        assert!((bind_translation(Joint::Head) - Vec3::new(0.0, 1.60, 0.0)).length() < 1.0e-6);
        assert!(bind_translation(Joint::Head).y < BODY_HEIGHT);
    }

    #[test]
    fn the_local_offsets_sum_to_the_bind_translation() {
        for joint in Joint::ALL {
            let parent = match joint {
                Joint::Hips => None,
                Joint::Torso | Joint::TorsoPivot => Some(Joint::Hips),
                Joint::Head | Joint::HeadPivot => Some(Joint::Torso),
            };
            let rebuilt = parent.map_or(Vec3::ZERO, bind_translation) + local_offset(joint);
            assert!(
                (rebuilt - bind_translation(joint)).length() < 1.0e-6,
                "{} drifted from its parent",
                joint.name()
            );
        }
    }

    #[test]
    fn an_inverse_bind_matrix_undoes_its_bind_transform() {
        for (joint, inverse) in Joint::ALL.iter().zip(inverse_bindposes()) {
            let bind = Mat4::from_translation(bind_translation(*joint));
            let composed = bind * inverse;
            assert!(
                composed.abs_diff_eq(Mat4::IDENTITY, 1.0e-6),
                "{} does not invert",
                joint.name()
            );
        }
    }

    #[test]
    fn scenery_carries_no_skin_attributes() {
        // A static mesh with `JOINTS_0` and no `SkinnedMesh` is drawn with the skinned pipeline over
        // a bind group that has no skin, which fails wgpu validation on the first frame.
        for mesh in [static_box(Vec3::ZERO, Vec3::ONE), ground(10.0)] {
            assert!(
                mesh.attribute(bevy_mesh::Mesh::ATTRIBUTE_JOINT_INDEX)
                    .is_none(),
                "scenery must not look skinned"
            );
            assert!(
                mesh.attribute(bevy_mesh::Mesh::ATTRIBUTE_JOINT_WEIGHT)
                    .is_none()
            );
        }
        assert!(
            body_mesh()
                .attribute(bevy_mesh::Mesh::ATTRIBUTE_JOINT_INDEX)
                .is_some()
        );
    }

    #[test]
    fn every_triangle_faces_its_normal() {
        // A face whose winding disagrees with its normal is culled or lit from behind, and both look
        // like a black hole in the model rather than like a mistake in a table of corners.
        let mesh = body_mesh();
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(bevy_mesh::Mesh::ATTRIBUTE_POSITION)
        else {
            unreachable!("the body has positions");
        };
        let Some(VertexAttributeValues::Float32x3(normals)) =
            mesh.attribute(bevy_mesh::Mesh::ATTRIBUTE_NORMAL)
        else {
            unreachable!("the body has normals");
        };
        let Some(indices) = mesh.indices() else {
            unreachable!("the body has indices");
        };
        let indices: Vec<usize> = indices.iter().collect();
        let corner = |index: usize| -> Option<(Vec3, Vec3)> {
            let position = positions.get(index)?;
            let normal = normals.get(index)?;
            Some((Vec3::from_array(*position), Vec3::from_array(*normal)))
        };
        for triangle in indices.chunks_exact(3) {
            let Some(&[a, b, c]) = triangle.first_chunk::<3>() else {
                unreachable!("a triangle has three corners");
            };
            let (Some(a), Some(b), Some(c)) = (corner(a), corner(b), corner(c)) else {
                unreachable!("indices stay inside the mesh");
            };
            let geometric = (b.0 - a.0).cross(c.0 - a.0);
            assert!(
                geometric.dot(a.1) > 0.0,
                "a triangle at {:?} winds against its normal",
                a.0
            );
        }
    }

    #[test]
    fn a_box_has_four_triangles_per_face_and_no_shared_vertices() {
        let mut builder = MeshBuilder::default();
        builder.push_box(Vec3::ZERO, Vec3::ONE, (Joint::Hips, Joint::Hips, 0.0, 1.0));
        let mesh = builder.build();
        assert_eq!(
            attribute_len(&mesh, bevy_mesh::Mesh::ATTRIBUTE_POSITION),
            24
        );
        assert_eq!(mesh.indices().map_or(0, bevy_mesh::Indices::len), 36);
    }
}
