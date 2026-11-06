//! What ships with the demo, and what a file has to be to count as shipped.
//!
//! The table below is the demo's asset contract. It exists as Rust rather than as a manifest file
//! because a manifest is a second copy of a list, and a second copy is a list that drifts — the
//! demo writes `manifest.json` from this table at run time instead, so the artefact can only ever
//! agree with the code.
//!
//! Three bodies cover five archetypes. That is the variant budget from
//! `skills/bevy-flock-meshy-assets/SKILL.md`, applied: a crowd is silhouettes and tints, and a
//! sixth body is a generation task that buys less than a wider tint range. The mapping is
//! exhaustive, so a new archetype fails to compile until somebody decides what it looks like.

use crate::DemoError;
use bevy_flock::domain::temperament::Archetype;
use std::path::{Path, PathBuf};

/// What kind of thing a model is, which decides how the scene places it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// A humanoid body for the crowd.
    Body,
    /// A prop that stands where the layout puts it.
    Prop,
}

/// One shipped model.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Stable identifier, used in the scene and the plan.
    pub slug: &'static str,
    /// Path under `demo/assets`.
    pub file: &'static str,
    /// Body or prop.
    pub kind: ModelKind,
    /// Height in metres, measured off the shipped mesh (`tools/shrink_glb.py --report`) rather
    /// than estimated from the concept, so a scale error shows up here and not in the scene.
    pub height_m: f32,
    /// The concept image this was built from.
    pub concept: &'static str,
    /// One line on what it is for.
    pub purpose: &'static str,
}

/// Every model the demo ships.
pub const MODELS: [Model; 6] = [
    Model {
        slug: "gatewarden",
        file: "npcs/gatewarden.glb",
        kind: ModelKind::Body,
        height_m: 1.80,
        concept: "concepts/gatewarden.png",
        purpose: "the city watch: plate over quilted coat, teal chevron tabard",
    },
    Model {
        slug: "aerie",
        file: "npcs/aerie.glb",
        kind: ModelKind::Body,
        height_m: 1.80,
        concept: "concepts/aerie.png",
        purpose: "aerial-settlement trader: gliding membranes and a flight harness",
    },
    Model {
        slug: "scavenger",
        file: "npcs/scavenger.glb",
        kind: ModelKind::Body,
        height_m: 1.75,
        concept: "concepts/scavenger.png",
        purpose: "ruin scavenger: hooded, patched, satchel and dust mask",
    },
    Model {
        slug: "pylon",
        file: "props/pylon.glb",
        kind: ModelKind::Prop,
        height_m: 3.00,
        concept: "concepts/pylon.png",
        purpose: "wayfinding: stacked enamel plates on a cast-iron post",
    },
    Model {
        slug: "shelter-door",
        file: "props/shelter-door.glb",
        kind: ModelKind::Prop,
        height_m: 2.50,
        concept: "concepts/shelter-door.png",
        purpose: "a shelter door in a stone frame: wheel latch, hazard stripes",
    },
    Model {
        slug: "market-stall",
        file: "props/market-stall.glb",
        kind: ModelKind::Prop,
        height_m: 2.50,
        concept: "concepts/market-stall.png",
        purpose: "a collapsible stall: timber frame, striped awning, balance scale",
    },
];

/// Which body an archetype wears.
///
/// Exhaustive on purpose: adding an archetype to the library should stop the demo compiling
/// until someone decides what that person looks like.
#[must_use]
pub const fn body_for(archetype: Archetype) -> &'static str {
    match archetype {
        Archetype::Guard => "gatewarden",
        // The aerial folk are the traders and the money in this city; both archetypes wear the
        // harness, and the tint does the rest.
        Archetype::Merchant | Archetype::Noble => "aerie",
        Archetype::Urchin | Archetype::Wanderer => "scavenger",
    }
}

/// The model with this slug, if the demo ships it.
#[must_use]
pub fn model(slug: &str) -> Option<&'static Model> {
    MODELS.iter().find(|entry| entry.slug == slug)
}

/// Where the shipped assets live.
#[must_use]
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// What a GLB turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlbSummary {
    /// Size on disk.
    pub bytes: u64,
    /// Declared glTF version.
    pub version: u32,
    /// Whether the JSON chunk declares at least one mesh.
    pub has_mesh: bool,
    /// Whether the JSON chunk declares at least one animation.
    pub has_animation: bool,
}

/// Read a GLB header and enough of its JSON chunk to say what it is.
///
/// Deliberately not a glTF loader: this is the check the ported skill asks for before an asset is
/// trusted — right container, right version, geometry present — not a parser the demo would then
/// have to keep correct.
///
/// # Errors
///
/// Returns [`DemoError::Asset`] when the file is missing, is not a GLB, is not glTF 2, or declares
/// no mesh.
pub fn inspect(path: &Path) -> Result<GlbSummary, DemoError> {
    let bytes = std::fs::read(path).map_err(|error| DemoError::Asset {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    if bytes.len() < 20 {
        return Err(DemoError::Asset {
            path: path.display().to_string(),
            reason: "shorter than a GLB header".to_owned(),
        });
    }
    let magic = bytes.get(0..4).unwrap_or_default();
    if magic != b"glTF" {
        return Err(DemoError::Asset {
            path: path.display().to_string(),
            reason: "not a GLB: the magic is not `glTF`".to_owned(),
        });
    }
    let version = u32::from_le_bytes(
        bytes
            .get(4..8)
            .unwrap_or(&[0, 0, 0, 0])
            .try_into()
            .unwrap_or([0; 4]),
    );
    if version != 2 {
        return Err(DemoError::Asset {
            path: path.display().to_string(),
            reason: format!("glTF version {version}, expected 2"),
        });
    }
    let chunk_length = u32::from_le_bytes(
        bytes
            .get(12..16)
            .unwrap_or(&[0, 0, 0, 0])
            .try_into()
            .unwrap_or([0; 4]),
    ) as usize;
    let chunk_type = bytes.get(16..20).unwrap_or_default();
    if chunk_type != b"JSON" {
        return Err(DemoError::Asset {
            path: path.display().to_string(),
            reason: "the first chunk is not JSON".to_owned(),
        });
    }
    let end = 20 + chunk_length;
    let json = bytes.get(20..end).ok_or_else(|| DemoError::Asset {
        path: path.display().to_string(),
        reason: "the JSON chunk runs past the end of the file".to_owned(),
    })?;
    let text = String::from_utf8_lossy(json);
    let has_mesh = text.contains("\"meshes\"");
    if !has_mesh {
        return Err(DemoError::Asset {
            path: path.display().to_string(),
            reason: "the file declares no meshes".to_owned(),
        });
    }
    Ok(GlbSummary {
        bytes: bytes.len() as u64,
        version,
        has_mesh,
        has_animation: text.contains("\"animations\""),
    })
}

/// A stable 64-bit fingerprint of a file's bytes, for the run report.
#[must_use]
pub fn fingerprint(path: &Path) -> u64 {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
