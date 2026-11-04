//! Driven adapters: indexes and stores the simulation owns.
//!
//! The spatial index lives in [`crate::domain::spatial`] because it holds no
//! engine types and its determinism guarantees are part of the simulation
//! contract; this module is where the optional integrations land as they are
//! built — snapshot storage, replication interest management and the navmesh
//! bridge. Nothing here is required to run a crowd.
