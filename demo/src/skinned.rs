//! A rigged character: skin, skeleton, clips, and CPU skinning.
//!
//! The Meshy bodies are static meshes — one pose, baked at generation time — so a crowd of them
//! stands still. The walkers here are a different kind of asset: a `KayKit` mannequin (CC0, vendored
//! with its licence beside it) with a twenty-three joint skeleton and eleven clips. Sampling a clip
//! and skinning the mesh is about a page of linear algebra, and it is the difference between a
//! diorama and a street.
//!
//! What is deliberately *not* here: blending between clips with weights, additive layers, IK, or a
//! state machine. The library owns behaviour; this owns the arithmetic between a clip name, a time
//! in seconds, and vertices. Anything more belongs to the game, which is the point of a
//! presentation seam rather than a renderer.

use crate::DemoError;
use crate::gltf;
use crate::jsonr::{self, Value};
use bevy_math::{Mat4, Quat, Vec3};
use std::collections::BTreeMap;
use std::path::Path;

/// One node of the skeleton: a joint, a mesh holder, or a group.
#[derive(Debug, Clone)]
struct Node {
    name: String,
    parent: Option<usize>,
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}

/// Which part of a node a clip drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelPath {
    Translation,
    Rotation,
    Scale,
}

/// One animated channel: times, values, and a node.
#[derive(Debug, Clone)]
struct Channel {
    node: usize,
    path: ChannelPath,
    times: Vec<f32>,
    values: Vec<[f32; 4]>,
}

/// A named clip.
#[derive(Debug, Clone)]
pub struct Clip {
    name: String,
    duration: f32,
    channels: Vec<Channel>,
}

impl Clip {
    /// How long the clip runs, seconds.
    #[must_use]
    pub const fn duration(&self) -> f32 {
        self.duration
    }

    /// Its name, as the asset spells it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The largest translation a channel applies, which is a rough "does this clip move the feet"
    /// check for the tests.
    #[must_use]
    pub fn travels(&self) -> f32 {
        self.channels
            .iter()
            .filter(|channel| channel.path == ChannelPath::Translation)
            .flat_map(|channel| channel.values.iter().map(|value| value[0].abs()))
            .fold(0.0_f32, f32::max)
    }
}

/// A rigged, skinnable character.
pub struct Rig {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    joints: Vec<[u16; 4]>,
    weights: Vec<[f32; 4]>,
    indices: Vec<u32>,
    nodes: Vec<Node>,
    /// Joint node indices, in skin order.
    skin_joints: Vec<usize>,
    inverse_bind: Vec<Mat4>,
    clips: BTreeMap<String, Clip>,
    height: f32,
}

impl Rig {
    /// Load a rigged GLB.
    ///
    /// # Errors
    ///
    /// [`DemoError::Asset`] when the file has no skin, no joint weights, or no clips — each of which
    /// means the asset is not the walker the demo expects and the crowd would be a row of statues.
    pub fn load(path: &Path) -> Result<Self, DemoError> {
        let bytes = std::fs::read(path).map_err(|error| DemoError::Asset {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        let failure = |reason: String| DemoError::Asset {
            path: path.display().to_string(),
            reason,
        };
        let (document, binary) =
            gltf::split_chunks(&bytes).ok_or_else(|| failure("no JSON chunk".to_owned()))?;
        let text = std::str::from_utf8(document)
            .map_err(|error| failure(format!("the JSON chunk is not UTF-8: {error}")))?;
        let root = jsonr::parse(text).map_err(|error| failure(format!("`glTF` JSON: {error}")))?;
        let accessors = root
            .get("accessors")
            .ok_or_else(|| failure("no accessors".to_owned()))?;
        let views = root
            .get("bufferViews")
            .ok_or_else(|| failure("no bufferViews".to_owned()))?;

        let nodes = read_nodes(&root, &failure)?;
        let skin = root
            .get("skins")
            .and_then(|skins| skins.at(0))
            .ok_or_else(|| failure("the file has no skin".to_owned()))?;
        let skin_joints: Vec<usize> = skin
            .get("joints")
            .map_or(&[][..], Value::items)
            .iter()
            .filter_map(Value::usize)
            .collect();
        if skin_joints.is_empty() {
            return Err(failure("the skin has no joints".to_owned()));
        }
        let inverse_bind = match skin
            .get("inverseBindMatrices")
            .and_then(Value::usize)
            .and_then(|index| accessors.at(index))
        {
            Some(accessor) => read_mat4(accessor, views, binary).map_err(failure)?,
            None => vec![Mat4::IDENTITY; skin_joints.len()],
        };
        if inverse_bind.len() != skin_joints.len() {
            return Err(failure(format!(
                "{} inverse bind matrices for {} joints",
                inverse_bind.len(),
                skin_joints.len()
            )));
        }

        let mut rig = Self {
            positions: Vec::new(),
            normals: Vec::new(),
            joints: Vec::new(),
            weights: Vec::new(),
            indices: Vec::new(),
            nodes,
            skin_joints,
            inverse_bind,
            clips: BTreeMap::new(),
            height: 0.0,
        };
        rig.read_meshes(&root, accessors, views, binary, &failure)?;
        rig.read_clips(&root, accessors, views, binary, &failure)?;
        if rig.indices.is_empty() {
            return Err(failure("the skin has no skinned primitives".to_owned()));
        }
        rig.height = rig
            .positions
            .iter()
            .fold(0.0_f32, |tallest, position| tallest.max(position[1]));
        Ok(rig)
    }

    /// How tall the mannequin is in bind space, metres.
    ///
    /// This is the untouched vertex extent, which is what a placement needs to know: the demo
    /// scales an instance to `target_height / height()` so a short scavenger and a tall warden can
    /// share one rig.
    #[must_use]
    pub const fn height(&self) -> f32 {
        self.height
    }

    /// The node name behind a skin slot, for diagnostics.
    #[must_use]
    pub fn joint_name(&self, slot: usize) -> Option<&str> {
        self.skin_joints
            .get(slot)
            .and_then(|node| self.nodes.get(*node))
            .map(|node| node.name.as_str())
    }

    /// The clips by name.
    #[must_use]
    pub const fn clips(&self) -> &BTreeMap<String, Clip> {
        &self.clips
    }

    /// Joint count.
    #[must_use]
    pub const fn joint_count(&self) -> usize {
        self.skin_joints.len()
    }

    /// Triangle count across every skinned primitive.
    #[must_use]
    pub const fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    fn read_meshes(
        &mut self,
        root: &Value,
        accessors: &Value,
        views: &Value,
        binary: &[u8],
        failure: &impl Fn(String) -> DemoError,
    ) -> Result<(), DemoError> {
        let meshes = root
            .get("meshes")
            .ok_or_else(|| failure("no meshes".to_owned()))?;
        for mesh in meshes.items() {
            for primitive in mesh.get("primitives").map_or(&[][..], Value::items) {
                let Some(attributes) = primitive.get("attributes") else {
                    continue;
                };
                let Some(positions) =
                    read_float_vec3(accessors, views, binary, attributes.get("POSITION"))
                else {
                    continue;
                };
                let normals = read_float_vec3(accessors, views, binary, attributes.get("NORMAL"))
                    .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; positions.len()]);
                let joints = read_joints(accessors, views, binary, attributes.get("JOINTS_0"))
                    .unwrap_or_else(|| vec![[0, 0, 0, 0]; positions.len()]);
                let weights = read_weights(accessors, views, binary, attributes.get("WEIGHTS_0"))
                    .unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 0.0]; positions.len()]);
                if joints.len() != positions.len() || weights.len() != positions.len() {
                    continue;
                }
                let base = u32::try_from(self.positions.len()).unwrap_or(u32::MAX);
                self.positions.extend_from_slice(&positions);
                self.normals.extend(normals);
                self.joints.extend(joints);
                self.weights.extend(weights);
                let indices = primitive.get("indices").and_then(Value::usize).map_or_else(
                    || (0..u32::try_from(positions.len()).unwrap_or(0)).collect(),
                    |index| {
                        accessors
                            .at(index)
                            .and_then(|accessor| gltf::read_indices(accessor, views, binary).ok())
                            .unwrap_or_default()
                    },
                );
                self.indices
                    .extend(indices.into_iter().map(|index| index + base));
            }
        }
        Ok(())
    }

    fn read_clips(
        &mut self,
        root: &Value,
        accessors: &Value,
        views: &Value,
        binary: &[u8],
        failure: &impl Fn(String) -> DemoError,
    ) -> Result<(), DemoError> {
        let animations = root
            .get("animations")
            .ok_or_else(|| failure("the file has no clips".to_owned()))?;
        for animation in animations.items() {
            let Some(samplers) = animation.get("samplers") else {
                continue;
            };
            let name = animation
                .get("name")
                .and_then(Value::text)
                .unwrap_or("clip")
                .to_owned();
            let mut channels = Vec::new();
            let mut duration = 0.0_f32;
            for channel in animation.get("channels").map_or(&[][..], Value::items) {
                let target = channel.get("target");
                let node = target
                    .and_then(|target| target.get("node"))
                    .and_then(Value::usize);
                let path = match target
                    .and_then(|target| target.get("path"))
                    .and_then(Value::text)
                {
                    Some("translation") => ChannelPath::Translation,
                    Some("rotation") => ChannelPath::Rotation,
                    Some("scale") => ChannelPath::Scale,
                    _ => continue,
                };
                let (Some(node), Some(sampler)) = (
                    node,
                    channel
                        .get("sampler")
                        .and_then(Value::usize)
                        .and_then(|index| samplers.at(index)),
                ) else {
                    continue;
                };
                let (Some(input), Some(output)) = (
                    sampler
                        .get("input")
                        .and_then(Value::usize)
                        .and_then(|index| accessors.at(index)),
                    sampler
                        .get("output")
                        .and_then(Value::usize)
                        .and_then(|index| accessors.at(index)),
                ) else {
                    continue;
                };
                let Ok(times) = gltf::read_vec3_scalar(input, views, binary) else {
                    continue;
                };
                let values = match path {
                    ChannelPath::Rotation => read_rotation_values(output, views, binary),
                    _ => gltf::read_vec3(output, views, binary)
                        .map(|values| {
                            values
                                .into_iter()
                                .map(|value| [value[0], value[1], value[2], 0.0])
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                if times.is_empty() || values.is_empty() {
                    continue;
                }
                duration = duration.max(times.last().copied().unwrap_or(0.0));
                channels.push(Channel {
                    node,
                    path,
                    times,
                    values,
                });
            }
            if channels.is_empty() {
                continue;
            }
            self.clips.insert(
                name.clone(),
                Clip {
                    name,
                    duration,
                    channels,
                },
            );
        }
        Ok(())
    }

    /// The world matrix of every joint for a clip at a time, seconds.
    ///
    /// Times outside the clip wrap, which is what a looping walk wants; the caller decides the
    /// phase, so a crowd of a thousand people is a thousand phases and one sampled pose per clip
    /// per phase bucket if the caller wants to be clever about it.
    #[must_use]
    pub fn pose(&self, clip: &Clip, time: f32) -> Vec<Mat4> {
        let mut trs: Vec<(Vec3, Quat, Vec3)> = self
            .nodes
            .iter()
            .map(|node| (node.translation, node.rotation, node.scale))
            .collect();
        for channel in &clip.channels {
            let Some(value) = sample(channel, time) else {
                continue;
            };
            let Some(slot) = trs.get_mut(channel.node) else {
                continue;
            };
            match channel.path {
                ChannelPath::Translation => slot.0 = Vec3::new(value[0], value[1], value[2]),
                ChannelPath::Scale => slot.2 = Vec3::new(value[0], value[1], value[2]),
                ChannelPath::Rotation => {
                    slot.1 = Quat::from_xyzw(value[0], value[1], value[2], value[3]).normalize();
                }
            }
        }
        let local: Vec<Mat4> = trs
            .into_iter()
            .map(|(translation, rotation, scale)| {
                Mat4::from_scale_rotation_translation(scale, rotation, translation)
            })
            .collect();
        let mut world = vec![Mat4::IDENTITY; self.nodes.len()];
        for index in self.topo_order() {
            let parent = self.nodes.get(index).and_then(|node| node.parent);
            let local_matrix = local.get(index).copied().unwrap_or(Mat4::IDENTITY);
            let combined = parent.map_or(local_matrix, |parent| {
                world.get(parent).copied().unwrap_or(Mat4::IDENTITY) * local_matrix
            });
            if let Some(slot) = world.get_mut(index) {
                *slot = combined;
            }
        }
        self.skin_joints
            .iter()
            .enumerate()
            .map(|(order, node)| {
                let joint = world.get(*node).copied().unwrap_or(Mat4::IDENTITY);
                let inverse = self
                    .inverse_bind
                    .get(order)
                    .copied()
                    .unwrap_or(Mat4::IDENTITY);
                joint * inverse
            })
            .collect()
    }

    /// Skin the mesh for a pose and an instance transform, writing world-space triangles.
    ///
    /// `emit` is called once per triangle with three vertices, so the caller can shade and raster
    /// them without this module owning a renderer.
    pub fn skin(&self, pose: &[Mat4], transform: Mat4, mut emit: impl FnMut([Vec3; 3], [Vec3; 3])) {
        let mut posed = Vec::with_capacity(self.positions.len());
        let mut normals = Vec::with_capacity(self.positions.len());
        for (index, rest) in self.positions.iter().enumerate() {
            let joints = self.joints.get(index).copied().unwrap_or([0; 4]);
            let weights = self
                .weights
                .get(index)
                .copied()
                .unwrap_or([1.0, 0.0, 0.0, 0.0]);
            let position = Vec3::from_array(*rest);
            let normal =
                Vec3::from_array(self.normals.get(index).copied().unwrap_or([0.0, 1.0, 0.0]));
            let mut sum = Vec3::ZERO;
            let mut sum_normal = Vec3::ZERO;
            let mut total = 0.0_f32;
            for lane in 0..4 {
                let weight = weights.get(lane).copied().unwrap_or(0.0);
                if weight <= 0.0 {
                    continue;
                }
                let joint = joints.get(lane).copied().unwrap_or(0) as usize;
                let matrix = pose.get(joint).copied().unwrap_or(Mat4::IDENTITY);
                sum += matrix.transform_point3(position) * weight;
                sum_normal += matrix.transform_vector3(normal) * weight;
                total += weight;
            }
            if total > 0.0 {
                sum /= total;
                sum_normal /= total;
            }
            posed.push(sum);
            normals.push(sum_normal.normalize_or_zero());
        }
        for triangle in self.indices.chunks_exact(3) {
            let (Some(a), Some(b), Some(c)) = (
                triangle.first().copied().map(|index| index as usize),
                triangle.get(1).copied().map(|index| index as usize),
                triangle.get(2).copied().map(|index| index as usize),
            ) else {
                continue;
            };
            let (Some(pa), Some(pb), Some(pc)) = (posed.get(a), posed.get(b), posed.get(c)) else {
                continue;
            };
            let (Some(na), Some(nb), Some(nc)) = (normals.get(a), normals.get(b), normals.get(c))
            else {
                continue;
            };
            emit(
                [
                    transform.transform_point3(*pa),
                    transform.transform_point3(*pb),
                    transform.transform_point3(*pc),
                ],
                [
                    transform.transform_vector3(*na).normalize_or_zero(),
                    transform.transform_vector3(*nb).normalize_or_zero(),
                    transform.transform_vector3(*nc).normalize_or_zero(),
                ],
            );
        }
    }

    fn topo_order(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut placed = vec![false; self.nodes.len()];
        // Depth first from every root, which is fine for a skeleton of twenty-three nodes and does
        // not need a dependency graph.
        for index in 0..self.nodes.len() {
            self.visit(index, &mut placed, &mut order);
        }
        order
    }

    fn visit(&self, index: usize, placed: &mut [bool], order: &mut Vec<usize>) {
        if placed.get(index).copied().unwrap_or(true) {
            return;
        }
        if let Some(parent) = self.nodes.get(index).and_then(|node| node.parent) {
            self.visit(parent, placed, order);
        }
        if let Some(slot) = placed.get_mut(index) {
            *slot = true;
        }
        order.push(index);
    }
}

/// Interpolate one channel at a time, returning the raw value.
///
/// The value is returned rather than a matrix because a channel drives *one* of a node's three
/// components. Building a matrix here and assigning it to the node is the bug that made the first
/// attempt at this module a row of frozen mannequins: the scale channel, applied last, erased the
/// rotation and the translation of the same joint.
fn sample(channel: &Channel, time: f32) -> Option<[f32; 4]> {
    let first = channel.times.first().copied()?;
    let last = channel.times.last().copied()?;
    let span = last - first;
    let time = if span > 1.0e-6 {
        first + (time - first).rem_euclid(span)
    } else {
        first
    };
    let index = channel
        .times
        .partition_point(|key| *key <= time)
        .saturating_sub(1)
        .min(channel.times.len().saturating_sub(1));
    let next = (index + 1).min(channel.times.len().saturating_sub(1));
    let start = channel.times.get(index).copied().unwrap_or(first);
    let end = channel.times.get(next).copied().unwrap_or(start);
    let fraction = if (end - start).abs() > 1.0e-6 {
        ((time - start) / (end - start)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let a = channel.values.get(index).copied().unwrap_or([0.0; 4]);
    let b = channel.values.get(next).copied().unwrap_or(a);
    Some(match channel.path {
        ChannelPath::Rotation => {
            let from = Quat::from_xyzw(a[0], a[1], a[2], a[3]).normalize();
            let to = Quat::from_xyzw(b[0], b[1], b[2], b[3]).normalize();
            let value = from.slerp(to, fraction);
            [value.x, value.y, value.z, value.w]
        }
        ChannelPath::Translation | ChannelPath::Scale => [
            a[0] + (b[0] - a[0]) * fraction,
            a[1] + (b[1] - a[1]) * fraction,
            a[2] + (b[2] - a[2]) * fraction,
            0.0,
        ],
    })
}

fn read_nodes(
    root: &Value,
    failure: &impl Fn(String) -> DemoError,
) -> Result<Vec<Node>, DemoError> {
    let nodes = root
        .get("nodes")
        .ok_or_else(|| failure("no nodes".to_owned()))?;
    let mut parents = vec![None; nodes.items().len()];
    for (index, node) in nodes.items().iter().enumerate() {
        for child in node.get("children").map_or(&[][..], Value::items) {
            if let Some(child) = child.usize()
                && let Some(slot) = parents.get_mut(child)
            {
                *slot = Some(index);
            }
        }
    }
    Ok(nodes
        .items()
        .iter()
        .enumerate()
        .map(|(index, node)| Node {
            name: node
                .get("name")
                .and_then(Value::text)
                .unwrap_or("node")
                .to_owned(),
            parent: parents.get(index).copied().flatten(),
            translation: node
                .get("translation")
                .and_then(Value::vec3)
                .map_or(Vec3::ZERO, Vec3::from_array),
            rotation: node
                .get("rotation")
                .and_then(Value::vec4)
                .map_or(Quat::IDENTITY, |value| {
                    Quat::from_xyzw(value[0], value[1], value[2], value[3]).normalize()
                }),
            scale: node
                .get("scale")
                .and_then(Value::vec3)
                .map_or(Vec3::ONE, Vec3::from_array),
        })
        .collect())
}

fn read_float_vec3(
    accessors: &Value,
    views: &Value,
    binary: &[u8],
    attribute: Option<&Value>,
) -> Option<Vec<[f32; 3]>> {
    let accessor = accessors.at(attribute?.usize()?)?;
    gltf::read_vec3(accessor, views, binary).ok()
}

fn read_joints(
    accessors: &Value,
    views: &Value,
    binary: &[u8],
    attribute: Option<&Value>,
) -> Option<Vec<[u16; 4]>> {
    let accessor = accessors.at(attribute?.usize()?)?;
    let component = accessor.get("componentType").and_then(Value::usize)?;
    let (region, count, stride) = gltf::view_bytes_pub(accessor, views, binary).ok()?;
    let width = match component {
        5121 => 1,
        5123 => 2,
        _ => return None,
    };
    let step = if stride == 0 { width * 4 } else { stride };
    let mut out = Vec::with_capacity(count);
    for element in 0..count {
        let base = element * step;
        let mut lanes = [0_u16; 4];
        for (lane, slot) in lanes.iter_mut().enumerate() {
            let start = base + lane * width;
            let slice = region.get(start..start + width)?;
            *slot = if width == 1 {
                u16::from(*slice.first()?)
            } else {
                u16::from_le_bytes(slice.try_into().ok()?)
            };
        }
        out.push(lanes);
    }
    Some(out)
}

fn read_weights(
    accessors: &Value,
    views: &Value,
    binary: &[u8],
    attribute: Option<&Value>,
) -> Option<Vec<[f32; 4]>> {
    let accessor = accessors.at(attribute?.usize()?)?;
    let (region, count, stride) = gltf::view_bytes_pub(accessor, views, binary).ok()?;
    let step = if stride == 0 { 16 } else { stride };
    let mut out = Vec::with_capacity(count);
    for element in 0..count {
        let base = element * step;
        let mut lanes = [0.0_f32; 4];
        for (lane, slot) in lanes.iter_mut().enumerate() {
            let start = base + lane * 4;
            let slice = region.get(start..start + 4)?;
            *slot = f32::from_le_bytes(slice.try_into().ok()?);
        }
        out.push(lanes);
    }
    Some(out)
}

fn read_rotation_values(accessor: &Value, views: &Value, binary: &[u8]) -> Vec<[f32; 4]> {
    let Ok((region, count, stride)) = gltf::view_bytes_pub(accessor, views, binary) else {
        return Vec::new();
    };
    let step = if stride == 0 { 16 } else { stride };
    let mut out = Vec::with_capacity(count);
    for element in 0..count {
        let base = element * step;
        let mut lanes = [0.0_f32; 4];
        for (lane, slot) in lanes.iter_mut().enumerate() {
            let Some(slice) = region.get(base + lane * 4..base + lane * 4 + 4) else {
                return out;
            };
            let Ok(bytes) = slice.try_into() else {
                return out;
            };
            *slot = f32::from_le_bytes(bytes);
        }
        out.push(lanes);
    }
    out
}

fn read_mat4(accessor: &Value, views: &Value, binary: &[u8]) -> Result<Vec<Mat4>, String> {
    let (region, count, stride) = gltf::view_bytes_pub(accessor, views, binary)?;
    let step = if stride == 0 { 64 } else { stride };
    let mut out = Vec::with_capacity(count);
    for element in 0..count {
        let base = element * step;
        let mut columns = [[0.0_f32; 4]; 4];
        for (axis, column) in columns.iter_mut().enumerate() {
            for (row, slot) in column.iter_mut().enumerate() {
                let start = base + axis * 16 + row * 4;
                let slice = region
                    .get(start..start + 4)
                    .ok_or_else(|| format!("matrix {element} runs past its bufferView"))?;
                *slot = f32::from_le_bytes(
                    slice
                        .try_into()
                        .map_err(|_| "truncated matrix component".to_owned())?,
                );
            }
        }
        out.push(Mat4::from_cols_array_2d(&columns));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets;
    use bevy_flock::testkit::OrDie;

    fn walker() -> Rig {
        Rig::load(&assets::root().join("anim/kaykit-walker.glb")).or_die()
    }

    #[test]
    fn the_rig_loads_with_clips_and_joints() {
        let rig = walker();
        assert!(rig.joint_count() >= 20, "{} joints", rig.joint_count());
        assert!(rig.triangles() > 500, "{} triangles", rig.triangles());
        // The bind-space extent, before the scene transform: a mannequin, not a monument.
        assert!(
            (1.4..=2.6).contains(&rig.height()),
            "the mannequin is {} m tall in bind space",
            rig.height()
        );
        for name in ["Walking_A", "Walking_B", "Running_A", "T-Pose"] {
            assert!(rig.clips().contains_key(name), "{name} is missing");
        }
    }

    #[test]
    fn weights_are_normalised_and_joints_are_in_range() {
        let rig = walker();
        let mut checked = 0;
        for (index, weights) in rig.weights.iter().enumerate() {
            let total: f32 = weights.iter().sum();
            assert!(
                (total - 1.0).abs() < 0.02,
                "vertex {index} has weights summing to {total}"
            );
            for joint in rig.joints.get(index).copied().unwrap_or([0; 4]) {
                assert!(
                    usize::from(joint) < rig.joint_count(),
                    "vertex {index} names joint {joint}"
                );
            }
            checked += 1;
        }
        assert!(checked > 500, "only {checked} vertices were skinned");
    }

    #[test]
    fn a_walk_clip_moves_the_legs_and_the_bind_pose_does_not() {
        let rig = walker();
        let walk = rig.clips().get("Walking_A").or_die();
        let bind = rig.clips().get("T-Pose").or_die();
        assert!(walk.duration() > 0.3, "the walk lasts {}", walk.duration());

        // The furthest any joint moves between two poses. A stride swings a knee through tens of
        // centimetres; a bind-pose clip is a still frame.
        let reach = |left: &[Mat4], right: &[Mat4]| -> f32 {
            left.iter()
                .zip(right.iter())
                .map(|(a, b)| {
                    let a = a.to_cols_array();
                    let b = b.to_cols_array();
                    ((a[12] - b[12]).powi(2) + (a[13] - b[13]).powi(2) + (a[14] - b[14]).powi(2))
                        .sqrt()
                })
                .fold(0.0_f32, f32::max)
        };

        let rest = rig.pose(bind, 0.0);
        let quarter = rig.pose(walk, walk.duration() * 0.25);
        let half = rig.pose(walk, walk.duration() * 0.5);
        assert!(
            reach(&rest, &quarter) > 0.15,
            "the walk barely moves: {:.4} m",
            reach(&rest, &quarter)
        );
        assert!(
            reach(&quarter, &half) > 0.02,
            "the pose does not change over the clip: {:.4} m",
            reach(&quarter, &half)
        );
        assert!(
            reach(&rest, &rig.pose(bind, bind.duration() * 0.5)) < 1.0e-3,
            "the bind pose is not still"
        );
    }

    #[test]
    fn skinning_is_deterministic_and_finite() {
        let rig = walker();
        let walk = rig.clips().get("Walking_A").or_die();
        let transform = Mat4::from_scale_rotation_translation(
            Vec3::ONE,
            Quat::IDENTITY,
            Vec3::new(3.0, 0.0, -2.0),
        );
        let capture = |time: f32| -> Vec<[f32; 3]> {
            let pose = rig.pose(walk, time);
            let mut out: Vec<[f32; 3]> = Vec::new();
            rig.skin(&pose, transform, |triangle, _| {
                out.extend(triangle.into_iter().map(|vertex| vertex.to_array()));
            });
            out
        };
        let first = capture(0.31);
        let second = capture(0.31);
        assert_eq!(first.len(), second.len());
        assert_eq!(
            first.len(),
            rig.triangles() * 3,
            "not every triangle was skinned"
        );
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(
                a.map(f32::to_bits),
                b.map(f32::to_bits),
                "two poses of the same time differ"
            );
            assert!(
                a.iter().all(|value| value.is_finite()),
                "a skinned vertex is not finite"
            );
        }
        let moved = first
            .iter()
            .map(|vertex| (vertex[0] - 3.0).abs() + (vertex[2] + 2.0).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            moved > 0.1,
            "the instance transform did not reach the vertices"
        );
    }

    #[test]
    fn times_wrap_so_a_walk_loops() {
        let rig = walker();
        let walk = rig.clips().get("Walking_A").or_die();
        let start = rig.pose(walk, 0.0);
        let wrapped = rig.pose(walk, walk.duration());
        for (a, b) in start.iter().zip(wrapped.iter()) {
            let difference = (a.to_cols_array()[12] - b.to_cols_array()[12]).abs();
            assert!(difference < 1.0e-3, "the loop does not close: {difference}");
        }
    }
}
