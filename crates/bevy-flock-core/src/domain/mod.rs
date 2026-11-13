//! Pure simulation state and mathematics.
//!
//! Nothing in this module imports Bevy, reads a clock, touches the file system
//! or allocates in a steady-state hot path. That is not asceticism: it is what
//! makes the simulation testable without an `App`, replayable from a seed, and
//! portable to a server binary that never links a renderer.

pub mod cognition;
pub mod continuum;
pub mod cost;
pub mod flow;
pub mod generator;
pub mod groups;
pub mod ids;
pub mod links;
pub mod lookback;
#[cfg(feature = "memory")]
pub mod memory;
pub mod nav;
#[cfg(feature = "navmesh")]
pub mod navmesh;
pub mod num;
pub mod obstacle_nav;
pub mod population;
pub mod rng;
#[cfg(feature = "memory")]
pub mod saves;
pub mod spatial;
pub mod spawn;
pub mod steering;
pub mod stimulus;
#[cfg(feature = "memory")]
pub mod telemetry;
pub mod temperament;
pub mod tick;
pub mod weight;

pub use cost::VirtualCost;
pub use flow::{
    FlowCache, FlowField, FlowKey, FlowLookup, FlowSolver, FlowTarget, SolveProgress, solve,
};
pub use generator::{
    ArchetypeEntry, ArchetypeTable, Generator, GeneratorError, Name, NameStyle, NpcBlueprint, TagId,
};
pub use groups::{
    FactionId, FactionMatrix, Formation, Group, GroupError, GroupSnapshot, GroupStore,
    MAX_FACTIONS, MAX_NESTING_DEPTH, QueuedDirective, Relation,
};
pub use ids::{GroupId, NpcId, SimSeed, SlotId};
pub use links::{LinkTable, LinkTag, NavLink, UNTAGGED};
#[cfg(feature = "memory")]
pub use memory::{
    DecayPolicy, Encounter, EncounterKind, MemoryArchive, MemoryIndex, MemorySlot, MemoryStore,
    NpcMemories, SubjectId,
};
pub use nav::{GridNavSource, NavBounds, NavCell, NavError, NavSource, NavVersion};
#[cfg(feature = "navmesh")]
pub use navmesh::{
    AreaId, AreaMask, Crossing, NavMesh, NavMeshError, NavMeshSource, NavPolygon, PathPoint,
};
pub use population::{PopulationConfig, PopulationMode, PopulationSize, SlotCount};
pub use rng::RngStream;
#[cfg(feature = "memory")]
pub use saves::{LoadReport, MAGIC, MemorySnapshot, Origin, SaveError, SaveRecord, SavedSlot};
pub use spatial::{GridEntry, Neighbour, SpatialGrid, SpatialGridConfig};
pub use spawn::{DEFAULT_SPACING, Placement};
pub use steering::{SteeringParams, SteeringWeights};
#[cfg(feature = "memory")]
pub use telemetry::{Sample, measure, references_to, retire};
pub use temperament::{Archetype, Mood, Needs, Temperament, Weight};
pub use tick::{Tick, TicksPerSecond};
