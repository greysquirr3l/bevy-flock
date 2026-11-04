//! Driving adapters: what a game actually calls.

pub mod cognition;
pub mod components;
pub mod plugin;

pub use cognition::{
    NpcBlackboard, NpcThinkWeight, PlayerFocus, ThinkSchedule, ThinkStats, WeightConfig,
};
pub use components::{
    FlockMember, LodTier, Npc, NpcBundle, NpcDesiredVelocity, NpcIdentity, NpcMood, NpcNeeds,
    NpcTarget, NpcTemperament, NpcVelocity,
};
pub use plugin::{
    CrowdPreset, FlockCommands, FlockConfig, FlockPlugin, FlockRequest, FlockSet, FlockStats,
    SpawnQueue, drain_spawn_queue, recycled_mode, spawn_position,
};
