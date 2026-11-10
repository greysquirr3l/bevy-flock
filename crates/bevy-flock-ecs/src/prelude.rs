//! The Bevy half's public imports.
//!
//! A game rarely names these paths: it depends on `bevy-flock`, whose prelude re-exports this one. They are here
//! so the ECS crate is usable on its own, which is exactly how the facade uses it.

// Engine types that appear in this crate's public API. Re-exported so a game stays on one `Vec3` and one
// `Transform` rather than accidentally depending on a second copy of either.
pub use bevy_app::App;
pub use bevy_math::Vec3;
pub use bevy_transform::prelude::Transform;

pub use crate::adapters::primary::components::{
    FlockMember, LodTier, Npc, NpcAvoidancePriority, NpcBrain, NpcBundle, NpcDesiredVelocity,
    NpcIdentity, NpcMood, NpcNeeds, NpcTarget, NpcTemperament, NpcVelocity,
};
pub use crate::adapters::primary::gaze::{GazeAim, GazeConfig, GazeTarget, resolve_gaze};
pub use crate::adapters::primary::gesture::{
    GestureChannel, GestureConfig, GestureCue, GestureKind, GestureRequest, KEY_GESTURE,
    NpcGestures, advance_gestures, collect_gesture_requests, request_gesture,
};
pub use crate::adapters::primary::locomotion::{Locomotion, MIN_MOVING_SPEED};
pub use crate::adapters::primary::observability::{
    DebugLabel, DebugOverlay, DebugOverlayConfig, ExplainError, collect_debug_overlay, explain_npc,
};
pub use crate::adapters::primary::obstacle_nav::{
    NavObstacle, ObstacleNavPlugin, ObstacleNavSettings, ObstacleNavState,
};
pub use crate::adapters::primary::plugin::{
    CrowdPreset, FlockCommands, FlockConfig, FlockPlugin, FlockRequest, FlockSet, FlockStats,
    spawn_position,
};
pub use crate::adapters::primary::stimulus::{
    PerceivedStimuli, Perception, PerceptionSettings, StimulusPlugin, StimulusSource,
};
pub use crate::boids::{BoidsConfig, BoidsPlugin, FlockSpatialGrid};

#[cfg(feature = "memory")]
pub use crate::adapters::primary::memory_bridge::{MemoryBridgeStats, MemoryConfig, NpcMemoryRow};
#[cfg(feature = "memory")]
pub use crate::adapters::primary::plugin::MemoryBridgePlugin;
