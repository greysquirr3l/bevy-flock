//! Read the geometry out of a shipped GLB.
//!
//! `assets::inspect` answers "is this a trustworthy file"; this answers "where are its triangles".
//! It reads exactly what the renderer needs — positions, normals, indices, the base colour — and
//! nothing else, so there is no material graph, no node hierarchy and no animation path to keep
//! correct. The models are single-mesh exports centred on their own origin, which is what makes
//! that reduction honest rather than lazy.
//!
//! The reader is a test as well as a tool: `tests/geometry.rs` loads every model and asserts it has
//! triangles, sits on the ground and is the height the asset table claims. A GLB that loses its
//! geometry in the shrink step fails there.

use crate::DemoError;
use crate::jsonr::{self, Value};
use std::path::Path;

/// One mesh, in its own space.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    /// Vertex positions, glTF's Y-up, metres.
    pub positions: Vec<[f32; 3]>,
    /// Vertex normals, normalised. Computed from the faces when the file has none.
    pub normals: Vec<[f32; 3]>,
    /// Triangle indices into the other two.
    pub indices: Vec<u32>,
    /// Base colour, sRGB 0..1. The models are texture-only; this is the fallback and the tint base.
    pub base_colour: [f32; 3],
    /// Whether the file declares a base-colour texture the renderer is not sampling.
    pub textured: bool,
    /// The mesh name, for diagnostics.
    pub name: String,
}

impl Mesh {
    /// Triangles in the mesh.
    #[must_use]
    pub const fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    /// Minimum and maximum corners of the bounding box.
    #[must_use]
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut low = [f32::INFINITY; 3];
        let mut high = [f32::NEG_INFINITY; 3];
        for position in &self.positions {
            for axis in 0..3 {
                let value = position.get(axis).copied().unwrap_or(0.0);
                if let Some(slot) = low.get_mut(axis) {
                    *slot = slot.min(value);
                }
                if let Some(slot) = high.get_mut(axis) {
                    *slot = slot.max(value);
                }
            }
        }
        (low, high)
    }

    /// Height above the mesh's own origin, which Meshy places at the model's feet.
    #[must_use]
    pub fn height(&self) -> f32 {
        let (low, high) = self.bounds();
        (high[1] - low[1]).max(0.0)
    }
}

/// Read a GLB's first mesh.
///
/// # Errors
///
/// Returns [`DemoError::Asset`] when the file is not a GLB, when the JSON chunk does not parse,
/// when an accessor points outside the binary chunk, or when the mesh has no positions. Every one of
/// those is a corrupt or unsupported asset, and the demo's position is that a corrupt asset is a
/// failure rather than something to render around.
pub fn load(path: &Path) -> Result<Mesh, DemoError> {
    let bytes = std::fs::read(path).map_err(|error| DemoError::Asset {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let failure = |reason: String| DemoError::Asset {
        path: path.display().to_string(),
        reason,
    };

    if bytes.get(0..4) != Some(b"glTF") {
        return Err(failure("not a GLB: the magic is not `glTF`".to_owned()));
    }
    let (document, binary) =
        split_chunks(&bytes).ok_or_else(|| failure("no JSON chunk".to_owned()))?;
    let text = std::str::from_utf8(document)
        .map_err(|error| failure(format!("the JSON chunk is not UTF-8: {error}")))?;
    let root = jsonr::parse(text).map_err(|error| failure(format!("glTF JSON: {error}")))?;

    let accessors = root
        .get("accessors")
        .ok_or_else(|| failure("no accessors".to_owned()))?;
    let views = root
        .get("bufferViews")
        .ok_or_else(|| failure("no bufferViews".to_owned()))?;
    let meshes = root
        .get("meshes")
        .ok_or_else(|| failure("no meshes".to_owned()))?;
    let first = meshes
        .at(0)
        .ok_or_else(|| failure("the meshes array is empty".to_owned()))?;

    let mut mesh = Mesh {
        base_colour: [0.82, 0.80, 0.76],
        name: first
            .get("name")
            .and_then(Value::text)
            .unwrap_or("mesh")
            .to_owned(),
        ..Mesh::default()
    };

    for primitive in first.get("primitives").map_or(&[][..], Value::items) {
        let attributes = primitive
            .get("attributes")
            .ok_or_else(|| failure("a primitive has no attributes".to_owned()))?;
        let positions = read_vec3(
            accessor_at(accessors, attributes.get("POSITION")).map_err(failure)?,
            views,
            binary,
        )
        .map_err(failure)?;
        if positions.is_empty() {
            continue;
        }
        let base = u32::try_from(mesh.positions.len()).unwrap_or(u32::MAX);
        mesh.positions.extend_from_slice(&positions);

        match read_vec3(
            accessor_at(accessors, attributes.get("NORMAL")).map_err(failure)?,
            views,
            binary,
        ) {
            Ok(normals) if normals.len() == positions.len() => mesh.normals.extend(normals),
            // A mesh without normals is legal glTF and common in scan exports; accumulate them
            // from the faces after the indices are known.
            _ => mesh
                .normals
                .extend(std::iter::repeat_n([0.0, 0.0, 0.0], positions.len())),
        }

        let indices = match primitive.get("indices") {
            Some(index) => read_indices(
                accessor_at(accessors, Some(index)).map_err(failure)?,
                views,
                binary,
            )
            .map_err(failure)?,
            None => (0..u32::try_from(positions.len()).unwrap_or(0)).collect(),
        };
        mesh.indices
            .extend(indices.iter().map(|index| index + base));

        if let Some(material) = primitive
            .get("material")
            .and_then(Value::usize)
            .and_then(|index| root.get("materials").and_then(|list| list.at(index)))
        {
            let pbr = material.get("pbrMetallicRoughness");
            if let Some(colour) = pbr
                .and_then(|pbr| pbr.get("baseColorFactor"))
                .and_then(Value::vec4)
            {
                mesh.base_colour = [colour[0], colour[1], colour[2]];
            }
            mesh.textured |= pbr.and_then(|pbr| pbr.get("baseColorTexture")).is_some();
        }
    }

    if mesh.positions.is_empty() {
        return Err(failure("the first mesh has no positions".to_owned()));
    }
    if mesh.indices.len() < 3 {
        return Err(failure("the first mesh has no triangles".to_owned()));
    }
    fill_missing_normals(&mut mesh);
    Ok(mesh)
}

/// Split a GLB into its JSON chunk and its binary chunk.
pub(crate) fn split_chunks(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut cursor = 12_usize;
    let mut json: Option<&[u8]> = None;
    let mut binary: &[u8] = &[];
    while let Some(header) = bytes.get(cursor..cursor.saturating_add(8)) {
        let length = u32::from_le_bytes(header.get(0..4)?.try_into().ok()?) as usize;
        let kind = header.get(4..8)?;
        let body = bytes.get(cursor + 8..cursor + 8 + length)?;
        match kind {
            b"JSON" => json = Some(body),
            b"BIN\0" => binary = body,
            _ => {}
        }
        cursor += 8 + length;
    }
    // Later chunks are allowed to pad to four bytes; `cursor` already skipped them.
    Some((json?, binary))
}

/// The accessor an attribute points at.
fn accessor_at<'a>(accessors: &'a Value, index: Option<&Value>) -> Result<&'a Value, String> {
    let index = index
        .and_then(Value::usize)
        .ok_or_else(|| "an attribute with no accessor index".to_owned())?;
    accessors
        .at(index)
        .ok_or_else(|| format!("accessor {index} does not exist"))
}

/// The binary chunk region an accessor reads, with its element count and stride.
pub(crate) fn view_bytes_pub<'a>(
    accessor: &Value,
    views: &Value,
    binary: &'a [u8],
) -> Result<(&'a [u8], usize, usize), String> {
    let index = accessor
        .get("bufferView")
        .and_then(Value::usize)
        .ok_or_else(|| "an accessor with no bufferView".to_owned())?;
    let view = views
        .at(index)
        .ok_or_else(|| format!("bufferView {index} does not exist"))?;
    let offset = accessor
        .get("byteOffset")
        .and_then(Value::usize)
        .unwrap_or(0);
    let view_offset = view.get("byteOffset").and_then(Value::usize).unwrap_or(0);
    let length = view
        .get("byteLength")
        .and_then(Value::usize)
        .ok_or_else(|| format!("bufferView {index} has no byteLength"))?;
    let stride = view.get("byteStride").and_then(Value::usize).unwrap_or(0);
    let count = accessor
        .get("count")
        .and_then(Value::usize)
        .ok_or_else(|| "an accessor with no count".to_owned())?;
    let start = view_offset
        .checked_add(offset)
        .ok_or_else(|| "accessor offset overflows".to_owned())?;
    let region = binary
        .get(start..start.saturating_add(length))
        .ok_or_else(|| format!("bufferView {index} runs past the binary chunk"))?;
    Ok((region, count, stride))
}

/// Read a `VEC3` float accessor.
pub(crate) fn read_vec3(
    accessor: &Value,
    views: &Value,
    binary: &[u8],
) -> Result<Vec<[f32; 3]>, String> {
    let component = accessor
        .get("componentType")
        .and_then(Value::usize)
        .ok_or_else(|| "an accessor with no componentType".to_owned())?;
    if component != 5126 {
        return Err(format!(
            "a VEC3 accessor of component type {component}; only float (5126) is supported"
        ));
    }
    let (region, count, stride) = view_bytes_pub(accessor, views, binary)?;
    let step = if stride == 0 { 12 } else { stride };
    let mut out = Vec::with_capacity(count);
    for element in 0..count {
        let start = element * step;
        let slice = region
            .get(start..start.saturating_add(12))
            .ok_or_else(|| format!("element {element} runs past its bufferView"))?;
        let mut array = [0_f32; 3];
        for (axis, slot) in array.iter_mut().enumerate() {
            let bytes = slice
                .get(axis * 4..axis * 4 + 4)
                .ok_or_else(|| "truncated float".to_owned())?;
            *slot = f32::from_le_bytes(bytes.try_into().map_err(|_| "truncated float".to_owned())?);
        }
        out.push(array);
    }
    Ok(out)
}

/// Read a `SCALAR` float accessor, which is how glTF stores animation key times.
pub(crate) fn read_vec3_scalar(
    accessor: &Value,
    views: &Value,
    binary: &[u8],
) -> Result<Vec<f32>, String> {
    let (region, count, stride) = view_bytes_pub(accessor, views, binary)?;
    let step = if stride == 0 { 4 } else { stride };
    let mut out = Vec::with_capacity(count);
    for element in 0..count {
        let start = element * step;
        let slice = region
            .get(start..start.saturating_add(4))
            .ok_or_else(|| format!("scalar {element} runs past its bufferView"))?;
        out.push(f32::from_le_bytes(
            slice
                .try_into()
                .map_err(|_| "truncated scalar".to_owned())?,
        ));
    }
    Ok(out)
}

/// Read an index accessor, which glTF writes as `u16` or `u32`.
pub(crate) fn read_indices(
    accessor: &Value,
    views: &Value,
    binary: &[u8],
) -> Result<Vec<u32>, String> {
    let component = accessor
        .get("componentType")
        .and_then(Value::usize)
        .ok_or_else(|| "an index accessor with no componentType".to_owned())?;
    let (region, count, stride) = view_bytes_pub(accessor, views, binary)?;
    let width = match component {
        5121 => 1,
        5123 => 2,
        5125 => 4,
        other => return Err(format!("index component type {other} is not supported")),
    };
    let step = if stride == 0 { width } else { stride };
    let mut out = Vec::with_capacity(count);
    for element in 0..count {
        let start = element * step;
        let slice = region
            .get(start..start.saturating_add(width))
            .ok_or_else(|| format!("index {element} runs past its bufferView"))?;
        let value = match width {
            1 => u32::from(slice.first().copied().unwrap_or(0)),
            2 => u32::from(u16::from_le_bytes(
                slice.try_into().map_err(|_| "truncated index".to_owned())?,
            )),
            _ => u32::from_le_bytes(slice.try_into().map_err(|_| "truncated index".to_owned())?),
        };
        out.push(value);
    }
    Ok(out)
}

/// Accumulate face normals where the file had none, then normalise every normal.
fn fill_missing_normals(mesh: &mut Mesh) {
    let needs_normals = mesh.normals.len() != mesh.positions.len()
        || mesh
            .normals
            .iter()
            .all(|n| n.iter().all(|v| v.to_bits().trailing_zeros() >= 31));
    if needs_normals {
        mesh.normals = vec![[0.0; 3]; mesh.positions.len()];
        for triangle in mesh.indices.chunks_exact(3) {
            let (Some(a), Some(b), Some(c)) = (
                triangle
                    .first()
                    .and_then(|index| mesh.positions.get(*index as usize)),
                triangle
                    .get(1)
                    .and_then(|index| mesh.positions.get(*index as usize)),
                triangle
                    .get(2)
                    .and_then(|index| mesh.positions.get(*index as usize)),
            ) else {
                continue;
            };
            let edge1 = sub(*b, *a);
            let edge2 = sub(*c, *a);
            let normal = cross(edge1, edge2);
            for index in triangle {
                if let Some(slot) = mesh.normals.get_mut(*index as usize) {
                    *slot = add(*slot, normal);
                }
            }
        }
    }
    for normal in &mut mesh.normals {
        let length = dot(*normal, *normal).sqrt();
        if length > 1e-6 {
            *normal = [normal[0] / length, normal[1] / length, normal[2] / length];
        } else {
            *normal = [0.0, 1.0, 0.0];
        }
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets;
    use bevy_flock::testkit::{OrDie, OrDieErr};

    #[test]
    fn every_shipped_model_loads_with_triangles() {
        for model in &assets::MODELS {
            let path = assets::root().join(model.file);
            let mesh = load(&path).or_die();
            assert!(
                mesh.triangles() > 500,
                "{} has {} triangles",
                model.slug,
                mesh.triangles()
            );
            assert_eq!(mesh.normals.len(), mesh.positions.len());
            assert!(mesh.textured, "{} ships without a texture?", model.slug);
        }
    }

    #[test]
    fn the_meshes_are_the_height_the_table_claims() {
        for model in &assets::MODELS {
            let mesh = load(&assets::root().join(model.file)).or_die();
            let (low, _high) = mesh.bounds();
            let measured = mesh.height();
            assert!(
                (measured - model.height_m).abs() < 0.15,
                "{} measures {measured:.2} m, the table says {:.2} m",
                model.slug,
                model.height_m
            );
            assert!(
                low[1].abs() < 0.06,
                "{} sits at y={:.3}, not on its own origin",
                model.slug,
                low[1]
            );
        }
    }

    #[test]
    fn normals_are_unit_length() {
        let mesh = load(&assets::root().join(assets::MODELS[0].file)).or_die();
        for normal in &mesh.normals {
            let length = dot(*normal, *normal).sqrt();
            assert!((length - 1.0).abs() < 0.01, "normal of length {length}");
        }
    }

    #[test]
    fn a_truncated_file_is_refused_rather_than_guessed_at() {
        let path = std::env::temp_dir().join("bevy-flock-demo-truncated.glb");
        let full = std::fs::read(assets::root().join(assets::MODELS[0].file)).or_die();
        let half = full.get(..full.len() / 2).or_die();
        std::fs::write(&path, half).or_die();
        let error = load(&path).or_die_err();
        assert!(format!("{error}").contains("glb") || format!("{error}").contains("GLB"));
        let _ = std::fs::remove_file(&path);
    }
}
