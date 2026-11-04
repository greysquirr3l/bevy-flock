//! Who deserves the thinking budget, by distance to what the player can see.
//!
//! The scheduler in [`crate::application::schedule`] is fair by weight and says nothing
//! about what a weight should be. This is the policy that fills it in, and it is
//! deliberately a pure function of two numbers: the distance to the nearest focus — a
//! player, a camera, a thing that must pretend to be alive — and the weight the
//! participant held last tick.
//!
//! # Why hysteresis, and why it is not a detail
//!
//! Bands invite a bug that is invisible in a still frame and obvious in motion. An NPC
//! standing exactly on a boundary, jittering by a centimetre of steering noise, flips
//! between a weight of 8 and a weight of 1 forty times a second. Its think cadence then
//! swings with it, which looks like a stutter in the crowd and, worse, makes the replay
//! depend on float dust. The upgrade threshold is therefore further out than the
//! downgrade threshold: you are promoted when you get close, and you keep the promotion
//! until you are unambiguously far away.
//!
//! # Why distance bands rather than a curve
//!
//! A continuous curve looks principled and behaves badly: every NPC gets a slightly
//! different weight, so nothing is ever exactly equal and the queue's tie-break by
//! identifier stops producing stable groups. Bands give a handful of populations with
//! identical weights, which is what makes a share predictable — "the eight nearest get
//! eight times the thinking of the background" is a sentence a designer can use.

/// The weight an NPC is enrolled with, in the scheduler's units.
///
/// One is the background population. Weights are small integers on purpose: the
/// scheduler divides by them, and a weight of seven against a weight of one is a
/// relationship a person can reason about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThinkWeight(u32);

impl ThinkWeight {
    /// The weight of an NPC nobody is looking at.
    pub const BACKGROUND: Self = Self(1);
    /// The weight of an NPC at the edge of relevance.
    pub const DISTANT: Self = Self(2);
    /// The weight of an NPC in the middle distance.
    pub const NEARBY: Self = Self(4);
    /// The weight of an NPC the player could reach out and touch.
    pub const CLOSE: Self = Self(8);

    /// A weight from a raw value, clamped to the range the scheduler accepts.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        // The scheduler refuses zero: a weightless participant would never run again,
        // which is indistinguishable from a removal.
        Self(if raw == 0 { 1 } else { raw })
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// How far away counts as what.
///
/// Distances are in world units and squared, because that is the form a spatial query
/// hands back and taking a square root per NPC per tick to compare against a constant is
/// work for nobody's benefit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightPolicy {
    /// Inside this distance, an NPC is [`ThinkWeight::CLOSE`].
    pub close: f32,
    /// Inside this distance, an NPC is [`ThinkWeight::NEARBY`].
    pub nearby: f32,
    /// Inside this distance, an NPC is [`ThinkWeight::DISTANT`].
    pub distant: f32,
    /// How far past a boundary an NPC must go before it is demoted, as a fraction.
    ///
    /// `0.25` means an NPC promoted at 20 units keeps its weight until it is 25 units
    /// away. This is the anti-jitter term and the reason the policy is stateful.
    pub hysteresis: f32,
}

impl Default for WeightPolicy {
    fn default() -> Self {
        // Roughly a room, a street, a neighbourhood: units are metres by convention.
        Self {
            close: 8.0,
            nearby: 30.0,
            distant: 80.0,
            hysteresis: 0.25,
        }
    }
}

impl WeightPolicy {
    /// A policy with the given boundaries, hysteresis included.
    ///
    /// Falls back to the default when the boundaries are not strictly increasing, because
    /// a band layout that cannot be ordered would silently give every NPC the same weight
    /// and nobody would notice until the crowd looked asleep.
    #[must_use]
    pub fn new(close: f32, nearby: f32, distant: f32, hysteresis: f32) -> Self {
        let sane = close > 0.0 && close < nearby && nearby < distant && hysteresis >= 0.0;
        if sane {
            Self {
                close,
                nearby,
                distant,
                hysteresis,
            }
        } else {
            Self::default()
        }
    }

    /// The weight this NPC should carry, given where it is and what it carried before.
    ///
    /// Promotion is immediate: walk into the close band and you are promoted that tick.
    /// Demotion requires leaving the band you hold, margin included, so an NPC standing on
    /// a boundary keeps the weight it earned until it is unambiguously gone.
    #[must_use]
    pub fn weight_for(self, distance_squared: f32, previous: ThinkWeight) -> ThinkWeight {
        let promoted = self.promoted_band(distance_squared);
        if promoted > previous {
            return promoted;
        }
        if distance_squared.max(0.0) <= self.hold_edge_squared(previous) {
            return previous;
        }
        promoted
    }

    /// The band the distance falls into, ignoring what the NPC held before.
    fn promoted_band(self, distance_squared: f32) -> ThinkWeight {
        let distance = distance_squared.max(0.0);
        if distance <= self.close * self.close {
            ThinkWeight::CLOSE
        } else if distance <= self.nearby * self.nearby {
            ThinkWeight::NEARBY
        } else if distance <= self.distant * self.distant {
            ThinkWeight::DISTANT
        } else {
            ThinkWeight::BACKGROUND
        }
    }

    /// The squared distance at which a held weight is finally surrendered.
    ///
    /// The band's own boundary, pushed out by the hysteresis margin. BACKGROUND never
    /// surrenders anything: it is the floor.
    fn hold_edge_squared(self, held: ThinkWeight) -> f32 {
        let margin = 1.0 + self.hysteresis;
        match held {
            ThinkWeight::CLOSE => (self.close * margin) * (self.close * margin),
            ThinkWeight::NEARBY => (self.nearby * margin) * (self.nearby * margin),
            ThinkWeight::DISTANT => (self.distant * margin) * (self.distant * margin),
            _ => f32::INFINITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> WeightPolicy {
        WeightPolicy::default()
    }

    #[test]
    fn distance_picks_the_band() {
        let policy = policy();
        let background = ThinkWeight::BACKGROUND;
        assert_eq!(policy.weight_for(1.0, background), ThinkWeight::CLOSE);
        assert_eq!(policy.weight_for(100.0, background), ThinkWeight::NEARBY);
        assert_eq!(policy.weight_for(1_600.0, background), ThinkWeight::DISTANT);
        assert_eq!(
            policy.weight_for(10_000.0, background),
            ThinkWeight::BACKGROUND
        );
    }

    #[test]
    fn a_boundary_does_not_flip_a_weight_every_tick() {
        let policy = policy();
        // Promoted on approach, to the close band.
        let promoted = policy.weight_for(1.0, ThinkWeight::BACKGROUND);
        assert_eq!(promoted, ThinkWeight::CLOSE);

        // Now jitter across the close boundary: just outside it, the weight is held.
        let outside = (policy.close + 0.05) * (policy.close + 0.05);
        assert_eq!(policy.weight_for(outside, promoted), ThinkWeight::CLOSE);
        let inside_again = (policy.close - 0.05) * (policy.close - 0.05);
        assert_eq!(
            policy.weight_for(inside_again, promoted),
            ThinkWeight::CLOSE
        );

        // Past the margin, it is surrendered — and to the band the distance implies.
        let past_margin = (policy.close * 1.30) * (policy.close * 1.30);
        assert_eq!(
            policy.weight_for(past_margin, promoted),
            ThinkWeight::NEARBY
        );
    }

    #[test]
    fn promotion_is_immediate_and_demotion_is_not() {
        let policy = policy();
        // Walking in promotes at once: nobody waits a tick to be noticed.
        assert_eq!(
            policy.weight_for(1.0, ThinkWeight::BACKGROUND),
            ThinkWeight::CLOSE
        );
        // Walking out does not: the margin has to be crossed first.
        let held = policy.weight_for(
            (policy.close + 0.1) * (policy.close + 0.1),
            ThinkWeight::CLOSE,
        );
        assert_eq!(held, ThinkWeight::CLOSE);
        // The near band behaves the same way, which is what makes this a rule rather
        // than a special case for the innermost tier.
        let nearby_held = policy.weight_for(
            (policy.nearby + 1.0) * (policy.nearby + 1.0),
            ThinkWeight::NEARBY,
        );
        assert_eq!(nearby_held, ThinkWeight::NEARBY);
        let gone = policy.weight_for(
            (policy.nearby * 1.4) * (policy.nearby * 1.4),
            ThinkWeight::NEARBY,
        );
        assert_eq!(gone, ThinkWeight::DISTANT);
    }

    #[test]
    fn an_absurd_policy_falls_back_instead_of_silently_flattening() {
        let silly = WeightPolicy::new(30.0, 8.0, 80.0, 0.25);
        assert_eq!(silly, WeightPolicy::default());
        let negative = WeightPolicy::new(-1.0, 8.0, 80.0, -4.0);
        assert_eq!(negative, WeightPolicy::default());
    }

    #[test]
    fn weights_are_never_zero() {
        assert_eq!(ThinkWeight::new(0), ThinkWeight::BACKGROUND);
        assert_eq!(ThinkWeight::new(3).get(), 3);
    }

    #[test]
    fn the_nearest_band_gets_the_thinking() {
        // The claim a designer cares about, as arithmetic: an NPC at arm's length is
        // scheduled eight times as often as one in the background, because the scheduler
        // divides its stride by the weight.
        let policy = policy();
        let close = policy.weight_for(1.0, ThinkWeight::BACKGROUND);
        let background = policy.weight_for(10_000.0, ThinkWeight::BACKGROUND);
        assert_eq!(close.get(), 8 * background.get());
    }
}
