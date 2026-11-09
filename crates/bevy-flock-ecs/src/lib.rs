//! The Bevy-facing half of `bevy-flock`: the plugin, the components and the systems.
//!
//! The simulation lives in [`bevy_flock_core`], which knows nothing about an engine. This crate is
//! what makes that simulation a Bevy plugin: components with `Component` derives, resources with
//! `Resource`, systems scheduled in sets, and the commands a game calls from its own systems.
//!
//! The split is a dependency direction, not a preference. A headless server depends on the core and
//! never compiles `bevy_ecs`; a game depends on the facade, which re-exports this crate at the paths
//! it has always used (`bevy_flock::adapters::primary::…`, `bevy_flock_ecs::boids::…`). Nothing a game
//! imports moves because the code moved.
//!
//! # What is here
//!
//! * [`adapters::primary`] — the plugin, components, cognition, perception, dialogue, gesture, gaze,
//!   locomotion, memory bridge, observability and the world director.
//! * [`adapters::secondary`] — driven adapters: the navigation bridge, the replication interest set
//!   and the physics port.
//! * [`boids`] — the steering plugin and its spatial grid resource.
//!
//! Simulation types are re-exported at the root of this crate (`domain`, `behavior`, `application`),
//! so a file inside this crate that says `crate::domain::spatial` compiles exactly as it did when
//! both halves lived in one package.

pub use bevy_flock_core::{application, behavior, domain, error};
#[doc(hidden)]
pub use bevy_flock_core::testkit;

pub mod adapters;
pub mod boids;
pub mod prelude;
