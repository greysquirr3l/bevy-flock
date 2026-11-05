//! Driving adapters: what a game actually calls.

pub mod band;
pub mod cognition;
pub mod components;
pub mod grouping;
pub mod observability;
pub mod perception;
pub mod plugin;
pub mod world_director;

pub use cognition::{
    NpcBlackboard, NpcEngaged, NpcThinkWeight, NpcTier, PlayerFocus, ThinkSchedule, ThinkStats,
    TierConfig, WeightConfig, assign_tiers, plan_thinks, tick_chosen,
};
pub use components::{
    FlockMember, LodTier, Npc, NpcBundle, NpcDesiredVelocity, NpcIdentity, NpcMood, NpcNeeds,
    NpcTarget, NpcTemperament, NpcVelocity,
};
pub use grouping::{GroupLeader, GroupMember, Groups, NpcIndex, index_new_npcs, reconcile_despawns};
pub use observability::{
    CrowdPulse, Diagnostics, Violation, check_world, count_frames, sample_crowd,
};
pub use perception::{
    KEY_PLAYER_DISTANCE, KEY_PLAYER_SEEN, NoticedPeople, PerceptionConfig, PlayerCharacter,
    forget_the_absent, observe_players,
};
pub use world_director::{AmbientMood, CrowdAlarms, CrowdDirector, run_world_director};
pub use plugin::{
    CrowdPreset, FlockCommands, FlockConfig, FlockPlugin, FlockRequest, FlockSet, FlockStats,
    SpawnQueue, drain_spawn_queue, recycled_mode, spawn_position,
};
