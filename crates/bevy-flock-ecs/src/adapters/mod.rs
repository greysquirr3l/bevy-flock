//! The seams between the simulation and the host engine.
//!
//! `primary` is driving: the plugin, the components, the systems — the code a
//! game talks to. `secondary` is driven: indexes and stores the simulation owns
//! and optional integrations it delegates to. Rendering lives in neither; it
//! observes the simulation from outside, which is what keeps a headless server
//! and a rendered client on the same code path.

pub mod primary;
pub mod secondary;
