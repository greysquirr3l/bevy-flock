//! Deterministic crowd and NPC simulation for [Bevy](https://bevy.org).
//!
//! `bevy-flock` simulates one NPC or a million. It gives every NPC, every
//! group and the whole population pluggable behaviour, and it keeps the result
//! deterministic enough to replay.
//!
//! # What you get
//!
//! - **Boids and steering** — separation, alignment, cohesion, seek and arrive
//!   with priority-weighted truncation, so avoidance is never outvoted by
//!   cohesion no matter how the weights are tuned.
//! - **A spatial index** — a deterministic uniform grid that answers radius
//!   and nearest-neighbour queries in time proportional to the neighbours
//!   found, not to the population size.
//! - **Budgeted spawning** — a spawn queue that spends an explicit per-frame
//!   allowance, so asking for 100,000 NPCs costs a few frames of work instead of
//!   a 400 ms hitch.
//! - **Pluggable logic** — behaviour trees with resume semantics, guarded state
//!   machines, group and world directors, and full takeover, all registered
//!   through one surface.
//! - **A deterministic core** — fixed logical ticks, per-entity RNG streams and
//!   `libm` transcendentals, so the same seed produces the same world on every
//!   platform.
//!
//! # Status
//!
//! This is `0.1.0`: the simulation foundation. Steering, the spatial index,
//! spawning, behaviour trees, state machines and the extension registry are
//! implemented and tested. The budgeted think scheduler, the continuum ambient
//! tier, dialogue, snapshots and the render adapters are specified in
//! `plan.toml` and tracked in the README's status table. The docs in this crate
//! describe what exists; they do not promise more.
//!
//! # Five lines
//!
//! ```
//! use bevy_app::App;
//! use bevy_flock::prelude::*;
//!
//! let mut app = App::new();
//! app.add_plugins(FlockPlugin::new(FlockConfig::preset(CrowdPreset::MarketplaceBustle)));
//! app.update();
//!
//! assert_eq!(app.world().resource::<FlockStats>().population(), CrowdPreset::MarketplaceBustle.population());
//! ```
//!
//! # Architecture
//!
//! The crate is a collapsed hexagonal layout with a hard rule at each seam:
//!
//! - [`domain`] is pure simulation state and mathematics. It does not import
//!   Bevy, allocate in steady state, or read a clock.
//! - `application` schedules that state against a budget and a logical tick.
//! - `adapters::primary` is the Bevy-facing surface: components, systems,
//!   the plugin and the command extension trait. Rendering is never one of
//!   these adapters; a renderer observes the simulation and cannot drive it.
//!
//! See `ARCHITECTURE.md` and `docs/adr/` in the repository for the reasoning
//! behind each of those boundaries.
//!
//! # Feature flags
//!
//! | feature | default | what it does |
//! | --- | --- | --- |
//! | `serde` | off | `Serialize`/`Deserialize` for blueprints, authored trees and snapshots |
//!
//! Optional integrations (avian physics, instanced rendering, snapshotting,
//! replication, inspectors) are tracked in `plan.toml` and land behind their
//! own features; none of them will ever be required to run a crowd.

#![deny(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
// The steering kernels are compared bit for bit across the scalar, SIMD and threaded roads, so the
// arithmetic is deliberately left unfused: `a * b + c` rounds twice where `a.mul_add(b, c)` rounds
// once, and fusing one road but not another is a one-ulp divergence the parity tests catch. The
// manifest allows the lint crate-wide; this attribute states the same decision in the source, where
// it outranks the lint flags an editor appends to the command line.
#![expect(
    clippy::suboptimal_flops,
    reason = "the steering roads are compared bit for bit, and fusion rounds once where the unfused form rounds twice"
)]

// The engine-facing half. Everything in here knows about Bevy; nothing outside it does. Gated on `ecs` so the
// simulation half of the crate can be built with `--no-default-features` and no `bevy_ecs` in the graph at all --
// which is the boundary ADR 0058 describes and `bevy-flock-core` will depend on.
#[cfg(feature = "ecs")]
pub mod adapters;
// The ECS systems that drive the simulation. Like `adapters`, this is engine code: the core exposes
// `boids::BoidsParams`-shaped data and the systems that read it live on this side of the boundary.
#[cfg(feature = "ecs")]
pub mod boids;
pub mod prelude;

#[cfg(feature = "ecs")]
pub use adapters::primary::{
    CrowdPreset, FlockCommands, FlockConfig, FlockPlugin, FlockRequest, FlockStats,
};
// The simulation, re-exported rather than reimplemented: every path a reader has in their code keeps working, and
// `bevy_flock_core` is where the code actually is. `crate::domain::…` inside this crate resolves through these lines.
pub use bevy_flock_core::{application, behavior, domain, error};
#[doc(hidden)]
pub use bevy_flock_core::testkit;
pub use bevy_flock_core::error::{ConfigError, FlockError, PopulationError, RegistrationError};
