//! Compatibility shim for `bevy-flock-anim`.
//!
//! The animation layer used to be a crate that depended on `bevy-flock`, which meant `bevy-flock`
//! could not offer it: a feature may not depend on a crate that depends back on the feature's own
//! package. The code now lives inside `bevy-flock` behind the `anim` feature, and this package
//! re-exports it so an existing `use bevy_flock_anim::AnimPlugin;` keeps compiling.
//!
//! New code should depend on `bevy-flock` with `features = ["anim"]` and import from
//! `bevy_flock::anim` (or from the prelude, which re-exports the same items when the feature is on).
//! The heading below is why this package is four lines of Rust rather than an implementation.
//!
//! # Why the split was inverted
//!
//! `bevy-flock` is the crate a game depends on. Anything a game needs should be reachable from it in
//! one dependency, or the answer to "how do I animate a crowd" becomes a second crate name nobody
//! told you about. Inverting the dependency is what makes `features = ["full"]` mean the animation
//! layer too.
//!
//! Deprecated in the sense that matters: it still works, it is still published, and it will be
//! removed at 1.0 rather than at 0.2, because removing a path a game already imports is a semver
//! break and a bad one to take for tidiness.

/// Everything the animation layer exports, re-exported unchanged.
pub use bevy_flock::anim::*;
