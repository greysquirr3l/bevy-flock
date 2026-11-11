//! One import for the common case.
//!
//! The prelude is deliberately small. If it needs a comment to explain what an
//! item is for, it belongs in a module path instead — a prelude that imports
//! everything is a prelude nobody reads.

// Engine types that appear in this crate's public API. Re-exported so a game
// stays on one `Vec3` and one `Transform` rather than accidentally depending on
// a second copy of either.
#[cfg(feature = "anim")]
pub use bevy_flock_anim::{
    AnimPlugin, AnimationFocus, AnimationLod, AnimationState, BlendFrame, BlendLod, BlendWeights,
    ClipBindings, ClipSlot, FootSlide, GazeBone, GazeLimits, GazePlugin, GazeRig, GestureClips,
    GesturePlayback, GesturePlugin, LocomotionProfile, LodLevel, ProfileError, RootMotion,
    RootMotionTrace, RootOffset,
};
#[cfg(feature = "ecs")]
#[cfg(feature = "ecs")]
pub use bevy_flock_ecs::prelude::*;

pub use crate::behavior::{
    ArchetypeLogic, BehaviorTree, Blackboard, Directive, DirectiveKind, FlockRegistry,
    LeafBehavior, LeafContext, LeafId, NodeSpec, ParallelPolicy, StandardLeaves, StateMachine,
    Status, TreeCursor, action, condition, cooldown, invert, parallel, repeat_until_fail, selector,
    sequence, timeout,
};

pub use crate::application::schedule::{Plan, SchedulerError, ThinkScheduler};
pub use crate::application::{AvoidReport, AvoidanceParams, SimClock, SteeringRoad};
pub use crate::behavior::memory_leaves::{
    GRUDGE_VALENCE, GreetFriend, KEY_MEMORY_FAMILIARITY, KEY_MEMORY_FLAGS, KEY_MEMORY_VALENCE,
    MemoryLeaves, RECOGNITION_THRESHOLD, RecogniseKnown, Remembered, ShunGrudge, WARM_VALENCE,
};
pub use crate::domain::continuum::{AmbientField, CellId, CellState, Occupation};
pub use crate::domain::cost::VirtualCost;
pub use crate::domain::ids::{GroupId, NpcId, SimSeed};
#[cfg(feature = "memory")]
pub use crate::domain::memory::{
    DecayPolicy, Encounter, EncounterKind, MemoryArchive, MemoryIndex, MemorySlot, MemoryStore,
    NpcMemories, SubjectId,
};
pub use crate::domain::population::{PopulationMode, PopulationSize, SlotCount};
#[cfg(feature = "memory")]
pub use crate::domain::saves::{LoadReport, MemorySnapshot, Origin, SaveError, SaveRecord};
pub use crate::domain::spatial::{RebuildCause, RebuildOutcome, SpatialGridConfig};
pub use crate::domain::spawn::{DEFAULT_SPACING, Placement};
pub use crate::domain::steering::SteeringParams;
pub use crate::domain::stimulus::{Perceived, Stimulus, StimulusKind};
pub use crate::domain::temperament::{Archetype, Temperament};
pub use crate::domain::tick::{Tick, TicksPerSecond};
