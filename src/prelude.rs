//! One import for the common case.
//!
//! The prelude is deliberately small. If it needs a comment to explain what an
//! item is for, it belongs in a module path instead — a prelude that imports
//! everything is a prelude nobody reads.

// Engine types that appear in this crate's public API. Re-exported so a game
// stays on one `Vec3` and one `Transform` rather than accidentally depending on
// a second copy of either.
pub use bevy_app::App;
pub use bevy_math::Vec3;
pub use bevy_transform::prelude::Transform;

pub use crate::adapters::primary::components::{
    FlockMember, LodTier, Npc, NpcBrain, NpcBundle, NpcDesiredVelocity, NpcIdentity, NpcMood,
    NpcNeeds, NpcTarget, NpcTemperament, NpcVelocity,
};
pub use crate::adapters::primary::plugin::{
    CrowdPreset, FlockCommands, FlockConfig, FlockPlugin, FlockRequest, FlockSet, FlockStats,
};

pub use crate::behavior::{
    ArchetypeLogic, BehaviorTree, Blackboard, Directive, DirectiveKind, FlockRegistry,
    LeafBehavior, LeafContext, LeafId, NodeSpec, ParallelPolicy, StandardLeaves, StateMachine,
    Status, TreeCursor, action, condition, cooldown, invert, parallel, repeat_until_fail, selector,
    sequence, timeout,
};

pub use crate::boids::{BoidsConfig, BoidsPlugin, FlockSpatialGrid};
pub use crate::domain::cost::VirtualCost;
pub use crate::domain::ids::{GroupId, NpcId, SimSeed};
#[cfg(feature = "memory")]
pub use crate::domain::memory::{
    DecayPolicy, Encounter, EncounterKind, MemoryArchive, MemoryIndex, MemorySlot, MemoryStore,
    NpcMemories, SubjectId,
};
pub use crate::domain::population::{PopulationMode, PopulationSize, SlotCount};
pub use crate::domain::spatial::SpatialGridConfig;
pub use crate::domain::steering::SteeringParams;
pub use crate::domain::temperament::{Archetype, Temperament};
pub use crate::domain::tick::{Tick, TicksPerSecond};
