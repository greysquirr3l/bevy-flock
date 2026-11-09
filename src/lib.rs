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
//! `0.1.0`. Steering, the spatial index, budgeted spawning, behaviour trees,
//! state machines, the think scheduler, tiered cognition, the ambient continuum,
//! dialogue, gesture and gaze playback, social memory and save compatibility are
//! implemented and tested. The tests that enforce each claim are listed in
//! `site/src/content/docs/reference/guarantees.md`, one row per promise.
//!
//! What remains before `1.0` is API surface rather than features: the freeze, the
//! MSRV run and the empty-crate install, tracked in `docs/dev/meta/1.0-readiness.md`.
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
//! - [`application`] schedules that state against a budget and a logical tick.
//! - [`adapters::primary`] is the Bevy-facing surface: components, systems,
//!   the plugin and the command extension trait. Rendering is never one of
//!   these adapters; a renderer observes the simulation and cannot drive it.
//!
//! [`domain`], [`application`] and [`behavior`] are re-exported from
//! `bevy-flock-core`, a crate with no Bevy dependency at all; [`adapters`] and
//! [`boids`] are the half that lives here. The seam is enforced by
//! `tests/core_boundary.rs`, which fails if a core file names an engine crate.
//!
//! See `ARCHITECTURE.md` and `docs/dev/adr/` in the repository for the reasoning
//! behind each of those boundaries.
//!
//! # Feature flags
//!
//! | feature | default | what it does |
//! | --- | --- | --- |
//! | `ecs` | on | the Bevy half: plugin, components, systems and the adapter surface |
//! | `simd` | on | the wide steering kernel, four lanes at a time |
//! | `threads` | on | parallel spatial build and scan under `std::thread::scope` |
//! | `serde` | off | `Serialize`/`Deserialize` for blueprints, authored trees, saves and snapshots |
//! | `memory` | off | social memory: bounded subject rows, ageing and persistence |
//! | `navmesh` | off | pathfinding over a baked navmesh, behind the navigation adapter |
//! | `anim` | off | blended clip weights, gaze and gesture playback; the only feature that needs `bevy_animation` |
//! | `full` | off | all of the above, for a game that wants the whole engine |
//!
//! `--no-default-features` builds the engine-free simulation alone, which is exactly
//! what `bevy-flock-core` publishes. `ecs` is the only feature that puts a Bevy crate
//! in the dependency graph.
//!
//! So the sentence a new dependency needs is `bevy-flock = { version = "0.1", features = ["full"] }`:
//! the crowd engine, memory, saves, navigation and the animation layer in one line. `anim` is a
//! feature rather than a second package because a layer you have to discover in another crate is a
//! layer half the games never add. It is off by default because it is the only feature that pulls
//! `bevy_animation` and `bevy_asset`, which a dedicated server should never compile. The old
//! `bevy-flock-anim` package still exists as a re-export shim for 0.x compatibility.
//!
//! Optional integrations (avian physics, instanced rendering, replication,
//! inspectors) land behind their own features; none is required to run a crowd.

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
// simulation half can be built with `--no-default-features` and no `bevy_ecs` in the graph at all, which is the
// boundary ADR 0058 describes and the reason `bevy-flock-core` exists as a crate of its own.
#[cfg(feature = "ecs")]
pub mod adapters;
/// Animation binding: turns simulated locomotion into blended clip weights.
///
/// Behind the `anim` feature, because it is the only layer in this crate that needs
/// `bevy_animation` and `bevy_asset`.
#[cfg(feature = "anim")]
pub mod anim;
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
pub use bevy_flock_core::error::{ConfigError, FlockError, PopulationError, RegistrationError};
#[doc(hidden)]
pub use bevy_flock_core::testkit;
pub use bevy_flock_core::{application, behavior, domain, error};
