//! Pure simulation state and mathematics.
//!
//! Nothing in this module imports Bevy, reads a clock, touches the file system
//! or allocates in a steady-state hot path. That is not asceticism: it is what
//! makes the simulation testable without an `App`, replayable from a seed, and
//! portable to a server binary that never links a renderer.

pub mod cost;
pub mod ids;
#[cfg(feature = "memory")]
pub mod memory;
pub mod population;
pub mod rng;
#[cfg(feature = "memory")]
pub mod saves;
pub mod spatial;
pub mod steering;
pub mod temperament;
pub mod tick;

pub use cost::VirtualCost;
pub use ids::{GroupId, NpcId, SimSeed, SlotId};
#[cfg(feature = "memory")]
pub use memory::{
    DecayPolicy, Encounter, EncounterKind, MemoryArchive, MemoryIndex, MemorySlot, MemoryStore,
    NpcMemories, SubjectId,
};
pub use population::{PopulationConfig, PopulationMode, PopulationSize, SlotCount};
pub use rng::RngStream;
#[cfg(feature = "memory")]
pub use saves::{MAGIC, MemorySnapshot, Origin, RestoreReport, SaveError, SaveRecord, SavedSlot};
pub use spatial::{GridEntry, Neighbour, SpatialGrid, SpatialGridConfig};
pub use steering::{SteeringParams, SteeringWeights};
pub use temperament::{Archetype, Mood, Needs, Temperament, Weight};
pub use tick::{Tick, TicksPerSecond};
