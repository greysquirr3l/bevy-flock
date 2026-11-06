//! Ashfall Verge: a technical playground for `bevy-flock`.
//!
//! The demo exists to answer three questions with artefacts rather than prose:
//!
//! * **Does it scale?** `--population` and `--ticks` are the only knobs; the report records the
//!   digest, the tier histogram and the wall time for the run that produced it.
//! * **Does it stay deterministic?** every draw in the city and every NPC position is keyed on the
//!   seed, and `ashfall.run.json` carries the digest the library reports. Run it twice and diff.
//! * **What does a renderer get?** `ashfall.scene.json` is the `build_render_plan` contract made
//!   concrete: model, position, facing, scale, tint, grouped by model for batching.
//!
//! The crate draws nothing itself. It has no renderer, no asset loader and no window, which is a
//! deliberate boundary rather than an accident of where it was written: a demo that needs a GPU
//! cannot run in CI, and a demo that cannot run in CI stops being evidence. The shipped assets are
//! real GLBs with a real consumer (`ashfall.scene.json`); see `README.md`.

pub mod app;
pub mod assets;
pub mod camera_rig;
pub mod city;
pub mod gltf;
pub mod input;
pub mod json;
pub mod jsonr;
pub mod plan;
pub mod playback;
pub mod raster;
pub mod render3d;
pub mod report;
pub mod scene;
pub mod session;
pub mod skinned;
pub mod terminal;
#[cfg(test)]
pub mod test_support;

use std::fmt;

/// Everything that can go wrong before the artefacts exist.
#[derive(Debug)]
pub enum DemoError {
    /// A shipped asset is missing or is not what it claims to be.
    Asset {
        /// Which file.
        path: String,
        /// Why it was rejected.
        reason: String,
    },
    /// An artefact could not be written.
    Io {
        /// Which file.
        path: String,
        /// The underlying failure.
        reason: String,
    },
    /// A command-line argument was not understood.
    Usage(String),
    /// The library refused a configuration.
    Config(String),
}

impl fmt::Display for DemoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asset { path, reason } => write!(formatter, "{path}: {reason}"),
            Self::Io { path, reason } => write!(formatter, "writing {path}: {reason}"),
            Self::Usage(reason) => write!(formatter, "{reason}"),
            Self::Config(reason) => write!(formatter, "configuration rejected: {reason}"),
        }
    }
}

impl std::error::Error for DemoError {}
