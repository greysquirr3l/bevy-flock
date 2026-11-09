//! Test-only ways to unwrap a fixture without tripping the crate's panic-family lints.
//!
//! The crate denies `clippy::unwrap_used`, `clippy::expect_used` and `clippy::panic` everywhere,
//! tests included, so `fixture.expect("grid")` is not available to a test. Scattering
//! `#[allow(clippy::expect_used)]` over the test modules would defeat the point of denying the
//! lint: the reason those lints are denied is that a panic in a *test* is a one-line failure
//! message and a panic in *production* is an outage, and a file-level opt-out cannot tell them
//! apart.
//!
//! So a test that needs to fail loudly says so with [`OrDie`], which reports the failure with
//! `unreachable!` and a message that names the fixture:
//!
//! ```rust
//! use bevy_flock::testkit::OrDie;
//!
//! let parsed: u32 = "42".parse::<u32>().or_die();
//! assert_eq!(parsed, 42);
//! ```
//!
//! This module is `#[doc(hidden)]` and exists for the crate's own tests, benchmarks and examples.
//! It is not part of the simulation API and carries no stability promise.

use crate::domain::spatial::SpatialGrid;
use crate::domain::{num, spatial::Neighbour};
use bevy_math::Vec3;
use core::fmt::Debug;

/// FNV-1a over 32-bit words, the digest the save format already uses.
///
/// `SubjectRegistry::fingerprint` hashes subject keys with exactly these constants, for exactly this
/// reason: a microsecond-scale structural comparison that is not protecting a file against corruption.
/// A parity harness needs the same thing over simulation state, and two digests in one crate that
/// disagree about what "the same bytes" means would be a worse problem than sharing the constants.
#[derive(Debug, Clone, Copy)]
pub struct Digest(u32);

impl Digest {
    /// The FNV-1a offset basis.
    pub const SEED: u32 = 0x811C_9DC5;

    /// A digest with nothing in it.
    #[must_use]
    pub const fn new() -> Self {
        Self(Self::SEED)
    }

    /// Fold one word in.
    pub fn write_u32(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.0 ^= u32::from(byte);
            self.0 = self.0.wrapping_mul(0x0100_0193);
        }
    }

    /// Fold a float in, by its bits: `1.0` and `1.0 + 1e-7` must not collide, and `-0.0` must not
    /// equal `0.0`.
    pub fn write_f32(&mut self, value: f32) {
        self.write_u32(value.to_bits());
    }

    /// Fold a vector in.
    pub fn write_vec3(&mut self, value: Vec3) {
        self.write_f32(value.x);
        self.write_f32(value.y);
        self.write_f32(value.z);
    }

    /// The digest so far.
    #[must_use]
    pub const fn finish(self) -> u32 {
        self.0
    }
}

impl Default for Digest {
    fn default() -> Self {
        Self::new()
    }
}

/// A digest of everything a neighbour query can observe about a grid.
///
/// One radius query per probe, folded in the order the query returns: identity, then squared distance,
/// then position. Order matters, because the parity claim is about the *ordered* neighbour list — a set
/// comparison would pass while two agents swapped places in the answer.
///
/// The probes are the caller's, so a harness can choose cells that straddle a band boundary or sit in
/// the middle of the densest cell it has.
#[must_use]
pub fn spatial_digest(grid: &SpatialGrid, probes: &[Vec3], radius: f32) -> u32 {
    let mut digest = Digest::new();
    let mut out: Vec<Neighbour> = Vec::new();
    digest.write_u32(num::usize_to_u32(grid.len()));
    for probe in probes {
        grid.radius(*probe, radius, &mut out);
        digest.write_u32(num::usize_to_u32(out.len()));
        for neighbour in &out {
            digest.write_u32(neighbour.id.to_bits());
            digest.write_f32(neighbour.distance_squared);
            digest.write_vec3(neighbour.position);
        }
    }
    digest.finish()
}

/// Unwrap a fixture, reporting the failure as a bug in the harness.
///
/// `unreachable!` rather than `panic!` because the branch is genuinely unreachable in a passing
/// run: the value came from a constructor the test just called with valid arguments, or from a
/// literal. If it does fire, the message is the one the test wrote, not a bare `called
/// Option::unwrap() on a None value`.
pub trait OrDie {
    /// The value inside.
    type Value;

    /// Return the value, or fail the test with a message naming the fixture.
    ///
    /// # Panics
    ///
    /// Panics (via `unreachable!`) when the value is absent. That is the point: a test that cannot
    /// build its fixture has nothing to assert, and reporting it as a test failure is better than
    /// skipping.
    #[track_caller]
    fn or_die(self) -> Self::Value;
}

impl<T, E: Debug> OrDie for Result<T, E> {
    type Value = T;

    #[track_caller]
    fn or_die(self) -> T {
        let Ok(value) = self else {
            unreachable!(
                "the fixture should build, and it reported: {:?}",
                self.err()
            );
        };
        value
    }
}

/// Unwrap the *error* of a fixture, for tests that assert a constructor refuses its input.
pub trait OrDieErr {
    /// The error inside.
    type Error;

    /// Return the error, or fail the test with a message naming the fixture.
    ///
    /// # Panics
    ///
    /// Panics (via `unreachable!`) when the value was `Ok`, which means the test's premise — that
    /// this input is rejected — is wrong.
    #[track_caller]
    fn or_die_err(self) -> Self::Error;
}

impl<T: Debug, E> OrDieErr for Result<T, E> {
    type Error = E;

    #[track_caller]
    fn or_die_err(self) -> E {
        let Err(error) = self else {
            unreachable!("the fixture should be refused, and it was accepted");
        };
        error
    }
}

/// Index a slice in a test without the `indexing_slicing` lint.
///
/// `frames[0]` in a test asserts two things at once — that the frame exists and that it is the
/// first — and the panic message names neither. `frames.at(0)` says which comma nd returns the
/// value, because a fixture in a test is nearly always wanted by value and `*frames.get(0)` at
/// forty call sites is noise.
pub trait GetOrDie<T> {
    /// The element at `index`, by value.
    ///
    /// # Panics
    ///
    /// Panics (via `unreachable!`) when the index is out of bounds.
    #[track_caller]
    fn at(&self, index: usize) -> T;
}

impl<T: Copy> GetOrDie<T> for [T] {
    #[track_caller]
    fn at(&self, index: usize) -> T {
        let Some(value) = self.get(index) else {
            unreachable!(
                "the fixture should have an element at {index}, and it has {}",
                self.len()
            );
        };
        *value
    }
}

// Arrays, because method resolution does not unsize `[T; N]` to `[T]` for a trait method: a test
// tallying into `[u32; 2]` has to find the impl on the array itself.
impl<T: Copy, const N: usize> GetOrDie<T> for [T; N] {
    #[track_caller]
    fn at(&self, index: usize) -> T {
        let Some(value) = self.get(index) else {
            unreachable!("the fixture should have {N} elements, and {index} is past the end");
        };
        *value
    }
}

/// The same, for a fixture that is not `Copy` and has to be borrowed.
pub trait GetOrDieRef<T> {
    /// The element at `index`, by reference.
    ///
    /// # Panics
    ///
    /// Panics (via `unreachable!`) when the index is out of bounds.
    #[track_caller]
    fn at_ref(&self, index: usize) -> &T;
}

impl<T> GetOrDieRef<T> for [T] {
    #[track_caller]
    fn at_ref(&self, index: usize) -> &T {
        let Some(value) = self.get(index) else {
            unreachable!(
                "the fixture should have an element at {index}, and it has {}",
                self.len()
            );
        };
        value
    }
}

impl<T, const N: usize> GetOrDieRef<T> for [T; N] {
    #[track_caller]
    fn at_ref(&self, index: usize) -> &T {
        let Some(value) = self.get(index) else {
            unreachable!("the fixture should have {N} elements, and {index} is past the end");
        };
        value
    }
}

/// The same, for a fixture a test writes into.
pub trait GetOrDieMut<T> {
    /// The element at `index`, mutably.
    ///
    /// # Panics
    ///
    /// Panics (via `unreachable!`) when the index is out of bounds.
    #[track_caller]
    fn at_mut(&mut self, index: usize) -> &mut T;
}

impl<T> GetOrDieMut<T> for [T] {
    #[track_caller]
    fn at_mut(&mut self, index: usize) -> &mut T {
        let len = self.len();
        let Some(value) = self.get_mut(index) else {
            unreachable!("the fixture should have an element at {index}, and it has {len}");
        };
        value
    }
}

impl<T, const N: usize> GetOrDieMut<T> for [T; N] {
    #[track_caller]
    fn at_mut(&mut self, index: usize) -> &mut T {
        let Some(value) = self.get_mut(index) else {
            unreachable!("the fixture should have {N} elements, and {index} is past the end");
        };
        value
    }
}

impl<T> OrDie for Option<T> {
    type Value = T;

    #[track_caller]
    fn or_die(self) -> T {
        let Some(value) = self else {
            unreachable!("the fixture should be present, and it was `None`");
        };
        value
    }
}

#[cfg(test)]
mod tests {
    use super::OrDie;

    #[test]
    fn or_die_returns_the_value() {
        assert_eq!(Ok::<u32, String>(7).or_die(), 7);
        assert_eq!(Some(9_u8).or_die(), 9);
    }

    #[test]
    #[should_panic(expected = "the fixture should build")]
    fn or_die_reports_the_error_it_was_given() {
        let _: u32 = Err::<u32, &str>("no grid").or_die();
    }
}
