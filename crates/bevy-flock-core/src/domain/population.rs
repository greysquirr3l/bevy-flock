//! Population size, mode and the bookkeeping that keeps them consistent.

use core::fmt;
use core::num::NonZeroU32;

use crate::domain::num;
use crate::error::{MAX_POPULATION, PopulationError};

/// A validated population size.
///
/// The only ways to obtain one are [`PopulationSize::try_new`], which fails, and
/// [`PopulationSize::clamped`], which does not. There is deliberately no
/// `From<usize>`: silently accepting zero from a config file is how a game ends
/// up shipping a scene with no crowd and no error to explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PopulationSize(usize);

impl PopulationSize {
    /// Validate a population size.
    ///
    /// # Errors
    ///
    /// Returns [`PopulationError::Empty`] for `0` and
    /// [`PopulationError::TooLarge`] above [`MAX_POPULATION`].
    pub const fn try_new(requested: usize) -> Result<Self, PopulationError> {
        if requested == 0 {
            return Err(PopulationError::Empty {
                max: MAX_POPULATION,
            });
        }
        if requested > MAX_POPULATION {
            return Err(PopulationError::TooLarge {
                requested,
                max: MAX_POPULATION,
            });
        }
        Ok(Self(requested))
    }

    /// Clamp an arbitrary value into the valid range.
    ///
    /// Exists for literals in examples and presets where a hard-coded constant
    /// is provably in range and the alternative is an awkward `match` on a
    /// `Result` that can never be `Err`. Do not use it on user input.
    #[must_use]
    pub const fn clamped(requested: usize) -> Self {
        if requested == 0 {
            Self(1)
        } else if requested > MAX_POPULATION {
            Self(MAX_POPULATION)
        } else {
            Self(requested)
        }
    }

    /// The validated count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    /// The count as a `u32`, which every population here fits inside.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        num::usize_to_u32(self.0)
    }
}

impl fmt::Display for PopulationSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Default for PopulationSize {
    fn default() -> Self {
        Self::clamped(1_000)
    }
}

/// The number of slots in a recycled pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotCount(NonZeroU32);

impl SlotCount {
    /// Build a slot count.
    ///
    /// # Errors
    ///
    /// Returns [`PopulationError::NoSlots`] when `slots` is zero. A
    /// `NonZeroU32` cannot be zero by construction, so this constructor exists
    /// for the `u32`-in API surface.
    pub const fn try_new(slots: u32) -> Result<Self, PopulationError> {
        match NonZeroU32::new(slots) {
            Some(nonzero) => Ok(Self(nonzero)),
            None => Err(PopulationError::NoSlots),
        }
    }

    /// A slot count that is known to be non-zero.
    #[must_use]
    pub const fn new(nonzero: NonZeroU32) -> Self {
        Self(nonzero)
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for SlotCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.get())
    }
}

/// How NPCs relate to the identities they were given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PopulationMode {
    /// Every NPC is generated once and keeps its identity forever. Memory grows
    /// with the population, and no two NPCs are ever the same individual.
    #[default]
    Unique,
    /// A fixed pool of slot identities that NPCs occupy while alive. A
    /// despawned NPC's slot returns to the free list and is reused by the next
    /// spawn, lowest slot first.
    Recycled(SlotCount),
}

impl PopulationMode {
    /// The number of distinct identities this mode can produce.
    #[must_use]
    pub const fn capacity(self) -> u32 {
        match self {
            Self::Unique => u32::MAX,
            Self::Recycled(slots) => slots.get(),
        }
    }

    /// Whether this mode reuses identities.
    #[must_use]
    pub const fn is_recycled(self) -> bool {
        matches!(self, Self::Recycled(_))
    }
}

/// A validated population configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PopulationConfig {
    /// How many NPCs exist.
    pub size: PopulationSize,
    /// How identities are allocated.
    pub mode: PopulationMode,
}

impl PopulationConfig {
    /// Build a configuration.
    ///
    /// # Errors
    ///
    /// Returns [`PopulationError::Empty`] when `size` is zero, which is only
    /// reachable because this constructor takes a raw `usize` for convenience.
    pub fn try_new(size: usize, mode: PopulationMode) -> Result<Self, PopulationError> {
        Ok(Self {
            size: PopulationSize::try_new(size)?,
            mode,
        })
    }

    /// A unique population of `size`.
    #[must_use]
    pub const fn unique(size: PopulationSize) -> Self {
        Self {
            size,
            mode: PopulationMode::Unique,
        }
    }

    /// A recycled population of `size` living in `slots` identities.
    #[must_use]
    pub const fn recycled(size: PopulationSize, slots: SlotCount) -> Self {
        Self {
            size,
            mode: PopulationMode::Recycled(slots),
        }
    }

    /// Whether the configuration is internally consistent.
    ///
    /// A recycled pool must be at least as large as the live population,
    /// otherwise it cannot hold everyone who is alive at once.
    #[must_use]
    pub const fn is_coherent(&self) -> bool {
        match self.mode {
            PopulationMode::Unique => true,
            PopulationMode::Recycled(slots) => slots.get() as usize >= self.size.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{OrDie, OrDieErr};

    #[test]
    fn zero_is_rejected_with_a_usable_message() {
        let err = PopulationSize::try_new(0).or_die_err();
        assert_eq!(
            err,
            PopulationError::Empty {
                max: MAX_POPULATION
            }
        );
        let message = err.to_string();
        assert!(message.contains("at least 1"), "{message}");
        assert!(message.contains("1..="), "{message}");
    }

    #[test]
    fn boundaries_are_inclusive_at_one_and_at_the_maximum() {
        assert!(PopulationSize::try_new(1).is_ok());
        assert!(PopulationSize::try_new(MAX_POPULATION).is_ok());
        assert!(PopulationSize::try_new(MAX_POPULATION + 1).is_err());
    }

    #[test]
    fn clamped_never_produces_an_invalid_value() {
        assert_eq!(PopulationSize::clamped(0).get(), 1);
        assert_eq!(PopulationSize::clamped(42).get(), 42);
        assert_eq!(PopulationSize::clamped(usize::MAX).get(), MAX_POPULATION);
    }

    #[test]
    fn recycled_pool_smaller_than_the_population_is_incoherent() {
        let config = PopulationConfig::recycled(
            PopulationSize::clamped(500),
            SlotCount::try_new(100).or_die(),
        );
        assert!(!config.is_coherent());
    }

    #[test]
    fn recycled_pool_exactly_the_population_size_is_coherent() {
        let config = PopulationConfig::recycled(
            PopulationSize::clamped(500),
            SlotCount::try_new(500).or_die(),
        );
        assert!(config.is_coherent());
    }

    #[test]
    fn slot_count_rejects_zero() {
        assert!(SlotCount::try_new(0).is_err());
    }
}
