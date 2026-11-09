//! Animation binding for bevy-flock: what turns a simulated crowd into a crowd that looks alive.
//!
//! The simulation knows where every NPC is walking, how fast, and which way it is turning. It has
//! never known what that should look like. This crate is the layer in between, and it is deliberately
//! narrow: it decides *which slot* carries weight and *how much*, and the game owns the skeleton,
//! the clips and the animation graph.
//!
//! The split is the point (ADR 0052). A crowd library that ships a rig ships a rig nobody can use
//! with their own artwork; a crowd library that names four slots and a blend space can drive an
//! orc, a city crowd or a flock of birds without knowing what any of them look like.
//!
//! # The shape of it
//!
//! * [`LocomotionProfile`] is authored data — walking and running speeds, stride length, the turn
//!   band. It is validated once, at the boundary, and cannot hold an invalid value afterwards.
//! * [`LocomotionProfile::blend`] is a pure function from `(speed, turn rate)` to [`BlendWeights`]:
//!   four numbers that always sum to one, computed without a clock, an allocation or a random draw.
//! * [`ClipSlot`] names the four poses: idle, walk, run, turn-in-place. The game maps each to a node
//!   in its own `AnimationGraph`; this crate never sees a clip.
//! * [`BlendLod`] decides how much animation a body is worth: fully animated near the focus, stepped
//!   further out for the cheap tiers, paused beyond `far`. It reads the tier the simulation's
//!   scheduler assigned rather than measuring distance twice, and it resumes a paused body at the
//!   stride phase its own distance walked implies, so nothing pops when it comes back.
//!
//! Because the blend is a pure function of simulation state, a replay animates identically whether
//! it is rendered at 20 frames a second or 200. That property is a test, not a hope: the animation
//! bindings in this crate read `Locomotion` and `Tick`, never `Time::delta`.
//!
//! # Example
//!
//! ```
//! use bevy_flock_anim::{ClipSlot, LocomotionProfile};
//!
//! # fn run() -> Result<(), bevy_flock_anim::ProfileError> {
//! // A city crowd: a 1.4 m/s walk, a 5 m/s run, and a stride of 1.5 m.
//! let profile = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)?;
//!
//! // Somebody walking at 0.7 m/s is half between standing and walking.
//! let blend = profile.blend(0.7, 0.0);
//! assert!((blend.weight(ClipSlot::Idle) - 0.5).abs() < 1e-5);
//! assert!((blend.weight(ClipSlot::Walk) - 0.5).abs() < 1e-5);
//!
//! // Somebody standing still and turning on the spot pivots instead of walking.
//! let pivot = profile.blend(0.0, 2.0);
//! assert!((pivot.weight(ClipSlot::TurnInPlace) - 1.0).abs() < 1e-5);
//! # Ok(())
//! # }
//! # run().expect("the shipped profile and its blends are valid");
//! ```
//!
//! # What this crate does not do
//!
//! It does not load a glTF, own a skeleton, evaluate a curve, or draw anything. It does not smooth
//! its output over time: smoothing a weight against a frame delta is how a crowd ends up animating
//! differently on somebody else's machine. When a transition needs to be gradual, the profile's
//! numbers make it gradual in *space* — a longer blend band — and the tick counts in the binding
//! make it gradual in *time*.

#![deny(unsafe_code)]
#![deny(missing_docs)]

pub mod binding;
pub mod gaze;
pub mod gesture;
pub mod lod;
pub mod profile;

pub use binding::{AnimPlugin, BlendFrame, ClipBindings, apply_locomotion_blend};
pub use gaze::{GazeBone, GazeLimits, GazePlugin, GazeRig, apply_gaze, look_angles, split_yaw};
pub use gesture::{GestureClips, GesturePlayback, GesturePlugin, play_gestures};
pub use lod::{AnimationFocus, AnimationLod, BlendLod, LodLevel};
pub use profile::{BlendWeights, ClipSlot, LocomotionProfile, ProfileError};
