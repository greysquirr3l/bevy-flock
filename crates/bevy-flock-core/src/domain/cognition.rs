//! Tiers of cognition, and the budget shares that keep them apart (ADR 0017).
//!
//! ADR 0009 gave every participant a weight, and weight controls *cadence*: how often an
//! NPC thinks. It cannot control *content*, because a weight does not change which logic
//! runs. A crowd of ten thousand therefore has one lever — think less — and pulling it
//! makes the NPC in front of the player as stupid as the ones behind a wall.
//!
//! A tier is the other lever. It names what an NPC is thinking *with*, from a fixed rule
//! set to full social reasoning, and it moves as the NPC moves: walking toward the player
//! promotes, walking away demotes. Weight still says how often; the tier says how much.
//!
//! # The ladder
//!
//! | Tier | What it is | Typical cost | Where it lives |
//! | --- | --- | --- | --- |
//! | [`CognitionTier::Reflex`] | Fixed rules, no deliberation. | 50–200 ns | Everywhere, most of the time |
//! | [`CognitionTier::Reactive`] | A small tree: flee, follow, idle. | 1–3 µs | The middle distance |
//! | [`CognitionTier::Deliberative`] | The full compiled tree, needs, mood, perception. | 4–12 µs | Within sight |
//! | [`CognitionTier::Social`] | Deliberation plus memory, dialogue intent, faction reasoning. | 20–50 µs | In a conversation |
//!
//! Social is not "closer than deliberative": it is *engaged*. An NPC the player is talking
//! to deserves the expensive path even if it is standing still, and one that walks away
//! mid-sentence keeps it until the conversation ends, not until it crosses a distance.

use crate::domain::cost::VirtualCost;

use super::weight::ThinkWeight;

/// How much thinking an NPC is doing, from reflex to social.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum CognitionTier {
    /// Fixed rules. No tree, no deliberation, no allocation.
    #[default]
    Reflex = 0,
    /// A small tree over a handful of leaves.
    Reactive = 1,
    /// The full compiled tree.
    Deliberative = 2,
    /// Deliberation plus memory, dialogue and social reasoning.
    Social = 3,
}

impl CognitionTier {
    /// Every tier, cheapest first.
    pub const ALL: [Self; 4] = [
        Self::Reflex,
        Self::Reactive,
        Self::Deliberative,
        Self::Social,
    ];

    /// The tier's index, for arrays.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The tier at an index, or [`CognitionTier::Reflex`] if the index is out of range.
    ///
    /// Out of range is impossible for an index derived from [`CognitionTier::index`], and
    /// the fallback exists so a corrupt snapshot degrades to the cheapest tier rather than
    /// panicking in a crowd.
    #[must_use]
    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Reactive,
            2 => Self::Deliberative,
            3 => Self::Social,
            // Zero and out-of-range both land on the cheapest tier: a corrupt index should
            // cost frames, not panic in a crowd.
            _ => Self::Reflex,
        }
    }

    /// A human-readable name, for logs and diagnostics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reflex => "reflex",
            Self::Reactive => "reactive",
            Self::Deliberative => "deliberative",
            Self::Social => "social",
        }
    }

    /// Whether this tier runs a behaviour tree.
    ///
    /// Reflex does not: its logic is the fixed rule set, which is what makes it cheap
    /// enough for a crowd. The adapter still ticks a tree for the other three, with
    /// whatever logic the game gave them.
    #[must_use]
    pub const fn uses_tree(self) -> bool {
        !matches!(self, Self::Reflex)
    }

    /// The weight this tier implies for cadence, once an NPC is in it.
    ///
    /// Tier and weight are separate levers, but they are not independent: a social NPC
    /// that thinks once every four seconds is a bug the player will notice. The tier sets
    /// a floor on the weight so the two cannot fight.
    #[must_use]
    pub const fn weight_floor(self) -> ThinkWeight {
        match self {
            Self::Reflex => ThinkWeight::BACKGROUND,
            Self::Reactive => ThinkWeight::NEARBY,
            // Deliberation and social reasoning both demand the closest cadence the weight
            // policy offers: a tier that thinks once every four seconds is a bug the player
            // will notice, whatever it is thinking about.
            Self::Deliberative | Self::Social => ThinkWeight::CLOSE,
        }
    }
}

/// The share of the frame each tier is guaranteed, in permille.
///
/// Reserved rather than shared, and that is the whole point: a parade of ten thousand
/// reflex agents must not be able to spend the social tier's budget, because the social
/// tier is the one the player is looking at. Shares are permille so the arithmetic stays
/// integer and the split is exact to the unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetShares {
    /// Permille reserved for reflex.
    pub reflex: u16,
    /// Permille reserved for reactive.
    pub reactive: u16,
    /// Permille reserved for deliberative.
    pub deliberative: u16,
    /// Permille reserved for social.
    pub social: u16,
}

impl Default for BudgetShares {
    /// The defaults from ADR 0017: 55 / 25 / 15 / 5.
    fn default() -> Self {
        Self {
            reflex: 550,
            reactive: 250,
            deliberative: 150,
            social: 50,
        }
    }
}

impl BudgetShares {
    /// Shares that add up to nothing useful fall back to the default.
    ///
    /// A sum other than 1000 is a configuration bug, and the alternative to a fallback is
    /// a frame that silently spends 60% of its budget or 140% of it.
    #[must_use]
    pub fn normalised(self) -> Self {
        let total = u32::from(self.reflex)
            + u32::from(self.reactive)
            + u32::from(self.deliberative)
            + u32::from(self.social);
        if total == 0 {
            return Self::default();
        }
        // Scale to exactly 1000, giving the remainder to reflex: the tier with the most
        // participants and the least individual cost is the least harmful place to put a
        // rounding error.
        let scale = |permille: u16| -> u16 {
            let scaled = (u32::from(permille) * 1000) / total;
            u16::try_from(scaled).unwrap_or(1000)
        };
        let mut out = Self {
            reflex: scale(self.reflex),
            reactive: scale(self.reactive),
            deliberative: scale(self.deliberative),
            social: scale(self.social),
        };
        let assigned = u32::from(out.reflex)
            + u32::from(out.reactive)
            + u32::from(out.deliberative)
            + u32::from(out.social);
        let shortfall = 1000_u32.saturating_sub(assigned);
        out.reflex = out
            .reflex
            .saturating_add(u16::try_from(shortfall).unwrap_or(0));
        out
    }

    /// Split a budget across the four tiers.
    ///
    /// Every tier gets at least one virtual unit when the budget can afford it, because a
    /// tier with a zero budget never runs and the symptom — "nobody in the room reacts" —
    /// is much harder to diagnose than a slightly unequal split.
    #[must_use]
    pub fn split(self, total: VirtualCost) -> [VirtualCost; 4] {
        let shares = self.normalised();
        let permille = [
            shares.reflex,
            shares.reactive,
            shares.deliberative,
            shares.social,
        ];
        let total_units = total.get();
        let mut out = [0_u64; 4];
        let mut assigned = 0_u64;
        for (slot, share) in out.iter_mut().zip(permille) {
            let amount = total_units.saturating_mul(u64::from(share)) / 1000;
            *slot = amount;
            assigned = assigned.saturating_add(amount);
        }
        // Rounding leaves a remainder; it belongs to reflex for the same reason as above.
        if let Some(first) = out.first_mut() {
            let remainder = total_units.saturating_sub(assigned);
            *first = first.saturating_add(remainder);
        }
        if total_units >= 4 {
            // A tier with a zero budget never runs, and "nobody in the room reacts" is a
            // harder symptom to trace than a slightly unequal split. Units are moved
            // rather than added, so the sum stays exactly the budget.
            for index in 1..4 {
                if out.get(index).copied() == Some(0)
                    && let Some(first) = out.first_mut()
                    && *first > 1
                {
                    *first -= 1;
                    if let Some(slot) = out.get_mut(index) {
                        *slot = 1;
                    }
                }
            }
        }
        [
            VirtualCost::new(out[0]),
            VirtualCost::new(out[1]),
            VirtualCost::new(out[2]),
            VirtualCost::new(out[3]),
        ]
    }
}

/// How distance and engagement decide an NPC's tier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TierPolicy {
    /// Distance bands, shared with the weight policy so the two cannot disagree about
    /// what "near" means.
    pub close: f32,
    /// The outer edge of the reactive band.
    pub nearby: f32,
    /// How far past a boundary an NPC must go before it is demoted, as a fraction.
    pub hysteresis: f32,
}

impl Default for TierPolicy {
    fn default() -> Self {
        // Wider margins than the weight policy: changing tier changes which logic runs,
        // which is a much more expensive mistake to make on a boundary than changing
        // cadence.
        Self {
            close: 12.0,
            nearby: 40.0,
            hysteresis: 0.5,
        }
    }
}

impl TierPolicy {
    /// A policy with the given bands, falling back to the default when they are not
    /// strictly increasing.
    #[must_use]
    pub fn new(close: f32, nearby: f32, hysteresis: f32) -> Self {
        if close > 0.0 && close < nearby && hysteresis >= 0.0 {
            Self {
                close,
                nearby,
                hysteresis,
            }
        } else {
            Self::default()
        }
    }

    /// The tier an NPC should hold, given where it is, what it held and whether the
    /// player is talking to it.
    ///
    /// Engagement outranks distance in both directions: being spoken to promotes to
    /// [`CognitionTier::Social`] regardless of how far away the speaker is, and it holds
    /// the NPC there while the conversation lasts. Everything else follows distance, with
    /// promotion immediate and demotion deferred by the hysteresis margin.
    #[must_use]
    pub fn tier_for(
        self,
        distance_squared: f32,
        previous: CognitionTier,
        engaged: bool,
    ) -> CognitionTier {
        if engaged {
            return CognitionTier::Social;
        }
        if previous == CognitionTier::Social {
            // A conversation that just ended drops the NPC to its distance band, but not
            // through hysteresis: the distance never changed, so there is no boundary
            // being crossed.
            return self.band_for(distance_squared);
        }
        let promoted = self.band_for(distance_squared);
        if promoted > previous {
            return promoted;
        }
        if distance_squared.max(0.0) <= self.hold_edge_squared(previous) {
            return previous;
        }
        promoted
    }

    /// The band the distance implies, ignoring the previous tier.
    #[must_use]
    pub fn band_for(self, distance_squared: f32) -> CognitionTier {
        let distance = distance_squared.max(0.0);
        if distance <= self.close * self.close {
            CognitionTier::Deliberative
        } else if distance <= self.nearby * self.nearby {
            CognitionTier::Reactive
        } else {
            // Note what is absent: nothing is promoted to Social by distance. Only
            // engagement is, because social reasoning is about who, not where. There is
            // also no distance beyond which an NPC stops thinking: the reflex tier is
            // cheap enough that "far away" means "cheap rules", not "asleep".
            CognitionTier::Reflex
        }
    }

    /// The squared distance at which a held tier is surrendered.
    fn hold_edge_squared(self, held: CognitionTier) -> f32 {
        let margin = 1.0 + self.hysteresis;
        match held {
            CognitionTier::Deliberative => (self.close * margin) * (self.close * margin),
            CognitionTier::Reactive => (self.nearby * margin) * (self.nearby * margin),
            // Reflex is the floor and Social is held by engagement; neither has an edge to
            // cross by distance.
            _ => f32::INFINITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_walks_up_the_ladder() {
        let policy = TierPolicy::default();
        let reflex = CognitionTier::Reflex;
        // Boundaries: close 12, nearby 40, in world units, squared here.
        assert_eq!(
            policy.tier_for(900.0, reflex, false),
            CognitionTier::Reactive
        );
        assert_eq!(
            policy.tier_for(100.0, reflex, false),
            CognitionTier::Deliberative
        );
        assert_eq!(
            policy.tier_for(1.0, reflex, false),
            CognitionTier::Deliberative
        );
        assert_eq!(
            policy.tier_for(10_000.0, reflex, false),
            CognitionTier::Reflex
        );
        assert_eq!(
            policy.tier_for(1_000_000.0, reflex, false),
            CognitionTier::Reflex
        );
    }

    #[test]
    fn nothing_is_promoted_to_social_by_distance_alone() {
        // The expensive tier has to be earned by engagement, or every NPC standing next to
        // the player costs 30 us and the budget is gone in three hundred of them.
        let policy = TierPolicy::default();
        assert_eq!(
            policy.tier_for(0.0, CognitionTier::Reflex, false),
            CognitionTier::Deliberative
        );
        assert_eq!(
            policy.tier_for(0.0, CognitionTier::Reflex, true),
            CognitionTier::Social
        );
    }

    #[test]
    fn a_conversation_holds_a_distant_npc_at_social() {
        let policy = TierPolicy::default();
        let talking = policy.tier_for(500.0 * 500.0, CognitionTier::Reflex, true);
        assert_eq!(talking, CognitionTier::Social);
        // Still talking on a later tick, still social.
        assert_eq!(
            policy.tier_for(500.0 * 500.0, talking, true),
            CognitionTier::Social
        );
        // Conversation ends: back to the band the distance implies, immediately.
        assert_eq!(
            policy.tier_for(500.0 * 500.0, talking, false),
            CognitionTier::Reflex
        );
    }

    #[test]
    fn promotion_is_immediate_and_demotion_is_not() {
        let policy = TierPolicy::default();
        let deliberative = policy.tier_for(1.0, CognitionTier::Reflex, false);
        assert_eq!(deliberative, CognitionTier::Deliberative);
        // Just outside the close band: held, because the margin has not been crossed.
        let just_out = (policy.close + 0.5) * (policy.close + 0.5);
        assert_eq!(
            policy.tier_for(just_out, deliberative, false),
            CognitionTier::Deliberative
        );
        // Well outside: demoted to the band the distance implies.
        let clear = (policy.close * 1.8) * (policy.close * 1.8);
        assert_eq!(
            policy.tier_for(clear, deliberative, false),
            CognitionTier::Reactive
        );
    }

    #[test]
    fn shares_sum_to_the_budget_exactly() {
        for total in [0_u64, 1, 3, 4, 999, 16_600_000, u64::MAX / 2] {
            let split = BudgetShares::default().split(VirtualCost::new(total));
            let sum: u64 = split.iter().map(|slot| slot.get()).sum();
            if total >= 4 {
                assert_eq!(sum, total, "budget {total} split into {split:?}");
                assert!(split.iter().all(|slot| slot.get() >= 1));
            }
        }
    }

    #[test]
    fn the_social_reserve_survives_a_flood() {
        // The property a player notices: the conversation keeps its 5% however many
        // reflex agents are queued. Arithmetic, not policy: the shares do not know how
        // many participants exist.
        let split = BudgetShares::default().split(VirtualCost::from_micros(10_000));
        assert_eq!(split[3].get(), 500_000, "5% of 10 ms");
        assert_eq!(split[0].get(), 5_500_000, "55% of 10 ms");
    }

    #[test]
    fn a_broken_share_layout_falls_back_to_the_default() {
        let silly = BudgetShares {
            reflex: 0,
            reactive: 0,
            deliberative: 0,
            social: 0,
        };
        assert_eq!(silly.normalised(), BudgetShares::default());
        // A layout that does not sum to 1000 is scaled rather than trusted.
        let lopsided = BudgetShares {
            reflex: 1,
            reactive: 1,
            deliberative: 1,
            social: 1,
        };
        let normalised = lopsided.normalised();
        assert_eq!(
            u32::from(normalised.reflex)
                + u32::from(normalised.reactive)
                + u32::from(normalised.deliberative)
                + u32::from(normalised.social),
            1000
        );
    }

    #[test]
    fn the_ladder_is_ordered_and_complete() {
        for (index, tier) in CognitionTier::ALL.iter().enumerate() {
            assert_eq!(tier.index(), index);
            assert_eq!(CognitionTier::from_index(index), *tier);
            assert!(!tier.name().is_empty());
        }
        assert!(!CognitionTier::Reflex.uses_tree());
        assert!(CognitionTier::Reactive.uses_tree());
        assert!(CognitionTier::Social.uses_tree());
        assert!(CognitionTier::Reflex < CognitionTier::Social);
    }

    #[test]
    fn an_absurd_policy_falls_back() {
        assert_eq!(TierPolicy::new(50.0, 10.0, 0.5), TierPolicy::default());
        assert_eq!(TierPolicy::new(1.0, 2.0, -1.0), TierPolicy::default());
    }
}
