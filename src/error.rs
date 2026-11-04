//! Typed errors for every fallible public entry point.
//!
//! The rule this module exists to enforce: a caller never has to guess why
//! something was rejected. Each variant carries the offending value and the
//! range or rule that rejected it, so the message a user sees is the fix.

use thiserror::Error;

/// The largest population this crate will construct.
///
/// Ten million is the ceiling because the ambient tier is the intended answer
/// above it: past this point a density field is cheaper than entities by three
/// orders of magnitude, and pretending otherwise would produce a library that
/// silently allocates gigabytes.
pub const MAX_POPULATION: usize = 10_000_000;

/// A population size was outside `1..=MAX_POPULATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PopulationError {
    /// A population of zero was requested.
    #[error("population must be at least 1, got 0 (valid range 1..={max})")]
    Empty {
        /// The largest accepted value, repeated here so the message is usable
        /// without consulting the docs.
        max: usize,
    },
    /// More NPCs were requested than the crate will construct.
    #[error(
        "population {requested} exceeds the maximum of {max}; use the ambient tier for larger crowds"
    )]
    TooLarge {
        /// What the caller asked for.
        requested: usize,
        /// The largest accepted value.
        max: usize,
    },
    /// A recycled population was configured without any slots.
    #[error("a recycled population needs at least one slot, got 0")]
    NoSlots,
}

/// Configuration was internally inconsistent.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum ConfigError {
    /// A radius or distance was zero, negative, NaN or infinite.
    #[error("{field} must be finite and greater than zero, got {value}")]
    InvalidDistance {
        /// Which field was wrong.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A weight fell outside `0.0..=1.0`.
    #[error("{field} must be within 0.0..=1.0, got {value}")]
    WeightOutOfRange {
        /// Which field was wrong.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A tick rate of zero was requested.
    #[error("ticks_per_second must be greater than zero")]
    ZeroTickRate,
    /// A per-frame budget was zero, which would stall the simulation.
    #[error("{field} must be greater than zero, otherwise nothing can ever run")]
    ZeroBudget {
        /// Which budget was wrong.
        field: &'static str,
    },
}

/// A registry rejected a registration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistrationError {
    /// Two extensions were registered under the same identifier.
    #[error("`{id}` is already registered as a {kind}; identifiers must be unique within a scope")]
    Duplicate {
        /// The duplicated identifier.
        id: String,
        /// What kind of extension already held it.
        kind: &'static str,
    },
}

/// Any error this crate can return from a public entry point.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum FlockError {
    /// A population size was rejected.
    #[error(transparent)]
    Population(#[from] PopulationError),
    /// A configuration value was rejected.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A registration was rejected.
    #[error(transparent)]
    Registration(#[from] RegistrationError),
}
