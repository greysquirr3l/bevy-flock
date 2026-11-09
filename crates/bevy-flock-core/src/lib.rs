//! The simulation core of `bevy-flock`, with no engine in its dependency graph.
//!
//! Everything here is a value or a function over values: the spatial index, the steering kernels, the behaviour-tree and
//! state-machine machinery, the cognition tiers, the continuum ambient tier, social memory, navigation, saves. None of it
//! needs a `World`, a scheduler or a window, so a headless server, a test harness or a replay tool can depend on this
//! crate alone and get the same deterministic results a game gets.
//!
//! The Bevy-facing half — plugins, components, systems, the crowd resource — lives in the `bevy-flock` facade, which
//! re-exports these modules under the same paths they always had. A game that compiled against `bevy_flock::domain::…`
//! keeps compiling, because the path is a re-export rather than a copy.
//!
//! The `ecs` feature exists only so the facade can put `#[derive(Resource)]` on three of these types; it is off by
//! default here, which is what keeps `bevy_ecs` out of this crate's graph.
pub mod application;
pub mod behavior;
pub mod domain;
pub mod error;
/// Test-only helpers for the crate's own fixtures. Not part of the simulation API.
#[doc(hidden)]
pub mod testkit;
pub use error::{ConfigError, FlockError, PopulationError, RegistrationError};
