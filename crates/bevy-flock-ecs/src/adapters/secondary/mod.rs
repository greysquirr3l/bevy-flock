//! Driven adapters: indexes and stores the simulation owns.
//!
//! The spatial index lives in [`crate::domain::spatial`] because it holds no
//! engine types and its determinism guarantees are part of the simulation
//! contract; this module is where the optional integrations land as they are
//! built — snapshot storage, replication interest management and the navmesh
//! bridge. Nothing here is required to run a crowd.

pub mod nav_adapter;
pub mod network;
pub mod physics;

pub use nav_adapter::{FlowFields, NavGrid, invalidate_stale_fields};
pub use network::{InterestSet, NpcNetState, ReplicationSet, collect_interest};
pub use physics::{PhysicsBacked, PhysicsBridge, PhysicsPort, export_velocities};
