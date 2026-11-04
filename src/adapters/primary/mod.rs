//! Driving adapters: what a game actually calls.

pub mod band;
pub mod cognition;
pub mod components;
pub mod plugin;

pub use cognition::{
    NpcBlackboard, NpcEngaged, NpcThinkWeight, NpcTier, PlayerFocus, ThinkSchedule, ThinkStats,
    TierConfig, WeightConfig, assign_tiers, plan_thinks, tick_chosen,
};
pub use components::{
    FlockMember, LodTier, Npc, NpcBundle, NpcDesiredVelocity, NpcIdentity, NpcMood, NpcNeeds,
    NpcTarget, NpcTemperament, NpcVelocity,
};
pub use plugin::{
    CrowdPreset, FlockCommands, FlockConfig, FlockPlugin, FlockRequest, FlockSet, FlockStats,
    SpawnQueue, drain_spawn_queue, recycled_mode, spawn_position,
};
