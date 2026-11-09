//! Driving adapters: what a game actually calls.

pub mod band;
pub mod clock;
pub mod cognition;
pub mod components;
pub mod dialogue;
pub mod gaze;
pub mod gesture;
pub mod grouping;
#[cfg(feature = "memory")]
pub mod interaction;
pub mod locomotion;
#[cfg(feature = "memory")]
pub mod memory_bridge;
pub mod observability;
pub mod obstacle_nav;
pub mod perception;
pub mod plugin;
pub mod render;
pub mod world_director;

pub use cognition::{
    NpcBlackboard, NpcEngaged, NpcThinkWeight, NpcTier, PlayerFocus, ThinkSchedule, ThinkStats,
    TierConfig, WeightConfig, assign_tiers, plan_thinks, tick_chosen,
};
pub use components::{
    FlockMember, LodTier, Npc, NpcBundle, NpcDesiredVelocity, NpcIdentity, NpcMood, NpcNeeds,
    NpcTarget, NpcTemperament, NpcVelocity,
};
pub use dialogue::{
    Barks, DialogHub, DialogOutbox, NpcBarks, SilentProvider, SpokenLine, produce_barks,
    tick_dialog,
};
pub use gaze::{GazeAim, GazeConfig, GazeTarget, resolve_gaze};
pub use gesture::{
    GestureChannel, GestureConfig, GestureCue, GestureKind, GestureRequest, NpcGestures,
    advance_gestures, collect_gesture_requests, request_gesture,
};
pub use grouping::{
    GroupLeader, GroupMember, Groups, NpcIndex, index_new_npcs, reconcile_despawns,
};
#[cfg(feature = "memory")]
pub use interaction::{
    InteractionConfig, InteractionKind, InteractionLog, InteractionOutcome, InteractionPlugin,
    InteractionQueue, InteractionRequest, Reaction, register, resolve_interactions, submit,
};
pub use observability::{
    CrowdPulse, Diagnostics, Violation, check_world, count_frames, sample_crowd,
};
pub use obstacle_nav::{
    NavObstacle, ObstacleNavPlugin, ObstacleNavSettings, ObstacleNavState, rebuild_obstacle_nav,
};
pub use perception::{
    KEY_PLAYER_DISTANCE, KEY_PLAYER_SEEN, NoticedPeople, PerceptionConfig, PlayerCharacter,
    forget_the_absent, observe_players,
};
pub use plugin::{
    CrowdPreset, FlockCommands, FlockConfig, FlockPlugin, FlockRequest, FlockSet, FlockStats,
    SpawnQueue, drain_spawn_queue, recycled_mode, spawn_position,
};
pub use render::{CrowdView, RenderPlan, VisibleInstance, build_render_plan, tint_from_seed};
pub use stimulus::{
    PerceivedStimuli, Perception, PerceptionSettings, StimulusBufferResource, StimulusPlugin,
    StimulusSource, emit_stimuli, perceive_stimuli,
};
pub use world_director::{AmbientMood, CrowdAlarms, CrowdDirector, run_world_director};
pub mod stimulus;
