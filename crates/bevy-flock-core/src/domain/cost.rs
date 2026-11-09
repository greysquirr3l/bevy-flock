//! Declared cognition cost, in virtual nanoseconds.
//!
//! The scheduler described in [ADR 0009] spends a budget of *virtual* nanoseconds
//! against costs declared by logic, never measured wall-clock time: a budget
//! measured against a real clock dispatches different NPCs on a faster machine,
//! and a replay stops being a replay.
//!
//! So a cost is an integer claim, made once, calibrated in CI, and then treated as
//! simulation state. It is deliberately a newtype rather than a `u64` so that a
//! duration, a tick count and a cost cannot be passed to each other.
//!
//! [ADR 0009]: https://github.com/greysquirr3l/bevy-flock/blob/main/docs/dev/adr/0009-virtual-cost-stride-scheduler.md

use core::fmt;
use core::iter::Sum;
use core::ops::Add;

/// A declared cost, in virtual nanoseconds.
///
/// Chosen to be roughly one real nanosecond on the reference machine, which makes
/// the numbers readable (`VirtualCost::new(240)` is "about a quarter of a
/// microsecond") without making them true on any other machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VirtualCost(u64);

impl VirtualCost {
    /// No cost at all. Only appropriate for logic that provably does nothing.
    pub const ZERO: Self = Self(0);

    /// The cost charged to logic that declares nothing.
    ///
    /// Deliberately pessimistic — several times the most expensive built-in leaf,
    /// which measures at nine virtual nanoseconds — so that an undeclared
    /// behaviour cannot hide as cheap and quietly take more than its share of the
    /// budget. The calibration gate lists every unit still sitting on this default.
    pub const DEFAULT: Self = Self(40);

    /// A cost of `nanos` virtual nanoseconds.
    #[must_use]
    pub const fn new(nanos: u64) -> Self {
        Self(nanos)
    }

    /// A cost from microseconds, for declarations that read better that way.
    #[must_use]
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros.saturating_mul(1_000))
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Whether this cost is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Add without overflowing.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Multiply without overflowing, for a leaf that runs a known number of times
    /// per think.
    #[must_use]
    pub const fn saturating_mul(self, factor: u32) -> Self {
        Self(self.0.saturating_mul(factor as u64))
    }
}

impl Add for VirtualCost {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl Sum for VirtualCost {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Self::saturating_add)
    }
}

impl fmt::Display for VirtualCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} vns", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addition_and_saturation_agree_with_sanity() {
        assert_eq!(
            VirtualCost::new(10) + VirtualCost::new(5),
            VirtualCost::new(15)
        );
        assert_eq!(
            VirtualCost::new(u64::MAX).saturating_add(VirtualCost::new(1)),
            VirtualCost::new(u64::MAX)
        );
    }

    #[test]
    fn sum_collects_a_catalogue() {
        let costs = [VirtualCost::new(60), VirtualCost::new(140)];
        let total: VirtualCost = costs.into_iter().sum();
        assert_eq!(total, VirtualCost::new(200));
    }

    #[test]
    fn the_default_is_more_expensive_than_any_honest_declaration() {
        // A smell guard: if DEFAULT ever becomes cheap, undeclared logic quietly
        // gets a free ride in the scheduler's fairness arithmetic.
        assert!(
            VirtualCost::DEFAULT >= VirtualCost::new(9).saturating_mul(4),
            "the default must stay several times the priciest built-in leaf"
        );
    }

    #[test]
    fn display_names_the_unit() {
        assert_eq!(VirtualCost::new(240).to_string(), "240 vns");
    }
}
