//! Four queues instead of one, with the budget split before anything is planned (ADR 0017).
//!
//! [`ThinkScheduler`] is fair to participants. It cannot be fair to *tiers*, because it
//! does not know what one is: give it ten thousand reflex agents and one conversation and
//! it will spend the budget in cost order, which means the conversation waits behind
//! whoever is cheapest. That is correct queueing, and it is the wrong game.
//!
//! This type keeps one scheduler per tier and gives each a reserved share of the budget.
//! The split happens first, so the conversation's 5% exists whether or not a hundred
//! thousand agents are queued, and the crowd's 55% cannot be eaten by a long dialogue.
//!
//! Nothing here reads a clock, and nothing depends on insertion order: the plan is a pure
//! function of who is enrolled, in which tier, at what cost, against which budgets.

use crate::domain::cognition::{BudgetShares, CognitionTier};
use crate::domain::cost::VirtualCost;
use crate::domain::tick::Tick;

use super::schedule::{Plan, ThinkScheduler};

/// What one tier did this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TierStat {
    /// Participants enrolled in this tier.
    pub enrolled: u32,
    /// Participants that thought.
    pub chosen: u32,
    /// Virtual cost committed.
    pub spent: VirtualCost,
    /// Participants left for a later tick.
    pub deferred: u32,
    /// The tier's budget for this tick.
    pub budget: VirtualCost,
    /// Longest wait any participant in this tier has had, in ticks.
    pub worst_wait: u32,
}

/// What the whole plan decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TieredStats {
    /// Per-tier detail, indexed by [`CognitionTier::index`].
    pub per_tier: [TierStat; 4],
    /// Cost committed across every tier.
    pub spent: VirtualCost,
    /// Cost available across every tier.
    pub budget: VirtualCost,
}

impl TieredStats {
    /// The statistics for one tier.
    #[must_use]
    pub fn tier(&self, tier: CognitionTier) -> TierStat {
        self.per_tier.get(tier.index()).copied().unwrap_or_default()
    }
}

/// Participants chosen this tick, grouped by tier.
///
/// Reused between ticks, so a running game does not allocate a vector per tier per frame.
#[derive(Debug, Clone, Default)]
pub struct Chosen {
    /// The identifiers chosen in each tier, in scheduling order.
    pub per_tier: [Vec<u32>; 4],
}

impl Chosen {
    /// An empty set of lists, ready to be filled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The identifiers chosen in one tier.
    #[must_use]
    pub fn tier(&self, tier: CognitionTier) -> &[u32] {
        self.per_tier.get(tier.index()).map_or(&[], Vec::as_slice)
    }

    /// Whether a participant was chosen, in any tier.
    #[must_use]
    pub fn contains(&self, id: u32) -> bool {
        self.per_tier.iter().any(|list| list.contains(&id))
    }
}

/// One scheduler per tier, each with a reserved share of the frame's virtual budget.
#[derive(Debug, Clone)]
pub struct TieredSchedule {
    tiers: [ThinkScheduler; 4],
    budgets: [VirtualCost; 4],
    shares: BudgetShares,
    total: VirtualCost,
    /// Which tier a participant is enrolled in, so a promotion can move it rather than
    /// enrol it twice.
    tier_of: Vec<Option<CognitionTier>>,
    scratch: Vec<u32>,
    stats: TieredStats,
}

impl TieredSchedule {
    /// A schedule with room for `capacity` participants and a total per-tick budget.
    #[must_use]
    pub fn new(capacity: u32, total: VirtualCost) -> Self {
        let shares = BudgetShares::default();
        let budgets = shares.split(total);
        Self {
            tiers: core::array::from_fn(|_| ThinkScheduler::new(capacity)),
            budgets,
            shares,
            total,
            tier_of: vec![None; capacity as usize],
            scratch: Vec::new(),
            stats: TieredStats::default(),
        }
    }

    /// The total per-tick budget.
    #[must_use]
    pub const fn budget(&self) -> VirtualCost {
        self.total
    }

    /// Replace the total budget, re-splitting the reservations.
    pub fn set_budget(&mut self, total: VirtualCost) {
        self.total = total;
        self.budgets = self.shares.split(total);
    }

    /// Replace the reserved shares, re-splitting the budget.
    pub fn set_shares(&mut self, shares: BudgetShares) {
        self.shares = shares.normalised();
        self.budgets = self.shares.split(self.total);
    }

    /// The reserved shares in force.
    #[must_use]
    pub const fn shares(&self) -> BudgetShares {
        self.shares
    }

    /// The per-tier budgets for this tick.
    #[must_use]
    pub const fn tier_budgets(&self) -> [VirtualCost; 4] {
        self.budgets
    }

    /// What the last plan decided.
    #[must_use]
    pub const fn stats(&self) -> TieredStats {
        self.stats
    }

    /// Which tier a participant is enrolled in, if any.
    #[must_use]
    pub fn tier_of(&self, id: u32) -> Option<CognitionTier> {
        self.tier_of.get(id as usize).copied().flatten()
    }

    /// Enrol a participant in a tier, or move it if it is already enrolled elsewhere.
    ///
    /// Returns the tier it was moved out of, which the caller needs in order to know
    /// whether this was a promotion (a cost spike to budget) or a demotion.
    pub fn place(&mut self, id: u32, tier: CognitionTier, weight: u32) -> Option<CognitionTier> {
        let slot = self.tier_of.get_mut(id as usize)?;
        let previous = *slot;
        if previous == Some(tier) {
            // Same tier: only the weight can have changed.
            let _ = self
                .tiers
                .get_mut(tier.index())
                .map(|scheduler| scheduler.set_weight(id, weight));
            return previous;
        }
        if let Some(previous_tier) = previous
            && let Some(scheduler) = self.tiers.get_mut(previous_tier.index())
        {
            scheduler.remove(id);
        }
        if let Some(scheduler) = self.tiers.get_mut(tier.index()) {
            let _ = scheduler.enrol(id, weight);
        }
        *slot = Some(tier);
        previous
    }

    /// Stop scheduling a participant.
    pub fn remove(&mut self, id: u32) {
        if let Some(slot) = self.tier_of.get_mut(id as usize)
            && let Some(tier) = slot.take()
            && let Some(scheduler) = self.tiers.get_mut(tier.index())
        {
            scheduler.remove(id);
        }
    }

    /// Plan every tier against its reserved budget.
    ///
    /// Each tier is planned independently, so a tier that is over-subscribed cannot affect
    /// the others: that independence is the guarantee this type exists to provide.
    pub fn plan(
        &mut self,
        tick: Tick,
        mut cost_of: impl FnMut(u32) -> VirtualCost,
        chosen: &mut Chosen,
    ) {
        let mut spent_total = 0_u64;
        for index in 0..CognitionTier::ALL.len() {
            let Some(scheduler) = self.tiers.get_mut(index) else {
                continue;
            };
            let budget = self.budgets.get(index).copied().unwrap_or_default();
            scheduler.advance(tick);
            let plan: Plan = scheduler.plan(budget, &mut cost_of, &mut self.scratch);
            if let Some(list) = chosen.per_tier.get_mut(index) {
                list.clear();
                list.extend_from_slice(&self.scratch);
            }
            let stat = TierStat {
                enrolled: scheduler.len(),
                chosen: plan.chosen,
                spent: plan.spent,
                deferred: plan.deferred,
                budget,
                worst_wait: scheduler.worst_wait(),
            };
            if let Some(slot) = self.stats.per_tier.get_mut(index) {
                *slot = stat;
            }
            spent_total = spent_total.saturating_add(plan.spent.get());
        }
        self.stats.spent = VirtualCost::new(spent_total);
        self.stats.budget = self.total;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COST: VirtualCost = VirtualCost::from_micros(2);

    fn schedule(total_us: u64) -> TieredSchedule {
        TieredSchedule::new(1_000, VirtualCost::from_micros(total_us))
    }

    #[test]
    fn a_tier_never_spends_another_tiers_budget() {
        // The failure this type exists to prevent: ten thousand reflex agents and one
        // conversation, in one queue. Here the reflex tier is given a hundred agents with
        // a budget that fits five, and the social tier still gets its reservation.
        let mut schedule = schedule(1_000);
        // Three hundred agents against a reflex budget that fits 275 of them at 2 us.
        for id in 0..300 {
            schedule.place(id, CognitionTier::Reflex, 1);
        }
        schedule.place(500, CognitionTier::Social, 8);
        let mut chosen = Chosen::new();
        schedule.plan(Tick::new(0), |_| COST, &mut chosen);

        assert_eq!(
            chosen.tier(CognitionTier::Social).len(),
            1,
            "the conversation ran"
        );
        let stats = schedule.stats();
        assert_eq!(
            stats.tier(CognitionTier::Reflex).chosen,
            275,
            "55% of 1 ms at 2 us"
        );
        assert!(stats.tier(CognitionTier::Social).spent.get() > 0);
        assert!(stats.spent.get() <= stats.budget.get());
    }

    #[test]
    fn every_tier_gets_its_reserved_share() {
        let schedule = schedule(10_000);
        let budgets = schedule.tier_budgets();
        assert_eq!(budgets[0].get(), 5_500_000);
        assert_eq!(budgets[1].get(), 2_500_000);
        assert_eq!(budgets[2].get(), 1_500_000);
        assert_eq!(budgets[3].get(), 500_000);
    }

    #[test]
    fn a_promotion_moves_a_participant_rather_than_copying_it() {
        let mut schedule = schedule(1_000);
        assert_eq!(schedule.place(7, CognitionTier::Reflex, 1), None);
        assert_eq!(schedule.tier_of(7), Some(CognitionTier::Reflex));
        assert_eq!(
            schedule.place(7, CognitionTier::Deliberative, 8),
            Some(CognitionTier::Reflex)
        );
        assert_eq!(schedule.tier_of(7), Some(CognitionTier::Deliberative));

        let mut chosen = Chosen::new();
        schedule.plan(Tick::new(0), |_| COST, &mut chosen);
        assert_eq!(
            schedule.stats().tier(CognitionTier::Reflex).enrolled,
            0,
            "the old tier must not still hold it"
        );
        assert_eq!(
            schedule.stats().tier(CognitionTier::Deliberative).enrolled,
            1
        );
    }

    #[test]
    fn a_demoted_participant_is_scheduled_by_the_cheap_tier() {
        let mut schedule = schedule(1_000);
        schedule.place(7, CognitionTier::Social, 8);
        schedule.place(7, CognitionTier::Reflex, 1);
        assert_eq!(schedule.tier_of(7), Some(CognitionTier::Reflex));
        let mut chosen = Chosen::new();
        schedule.plan(Tick::new(0), |_| COST, &mut chosen);
        assert!(chosen.contains(7));
        assert!(chosen.tier(CognitionTier::Social).is_empty());
    }

    #[test]
    fn removing_a_participant_empties_its_tier() {
        let mut schedule = schedule(1_000);
        schedule.place(3, CognitionTier::Reactive, 4);
        schedule.remove(3);
        assert_eq!(schedule.tier_of(3), None);
        let mut chosen = Chosen::new();
        schedule.plan(Tick::new(0), |_| COST, &mut chosen);
        assert!(!chosen.contains(3));
        assert_eq!(schedule.stats().tier(CognitionTier::Reactive).enrolled, 0);
    }

    #[test]
    fn the_plan_is_reproducible() {
        let run = || {
            let mut schedule = schedule(1_000);
            for id in 0..50 {
                let tier = CognitionTier::from_index((id % 3) as usize);
                schedule.place(id, tier, 1);
            }
            let mut chosen = Chosen::new();
            let mut seen = Vec::new();
            for tick in 0..30 {
                schedule.plan(
                    Tick::new(tick),
                    |id| VirtualCost::from_micros(1 + u64::from(id % 4)),
                    &mut chosen,
                );
                for tier in CognitionTier::ALL {
                    seen.extend(chosen.tier(tier).iter().copied());
                }
            }
            seen
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn a_total_budget_of_zero_plans_nothing() {
        let mut schedule = TieredSchedule::new(10, VirtualCost::new(0));
        schedule.place(1, CognitionTier::Reflex, 1);
        let mut chosen = Chosen::new();
        schedule.plan(Tick::new(0), |_| COST, &mut chosen);
        assert!(chosen.per_tier.iter().all(Vec::is_empty));
        assert_eq!(schedule.stats().spent, VirtualCost::new(0));
    }

    #[test]
    fn custom_shares_are_normalised_and_applied() {
        let mut schedule = schedule(1_000);
        schedule.set_shares(BudgetShares {
            reflex: 100,
            reactive: 100,
            deliberative: 100,
            social: 700,
        });
        let budgets = schedule.tier_budgets();
        assert_eq!(budgets[3].get(), 700_000, "the social tier keeps its 70%");
        assert_eq!(
            budgets.iter().map(|budget| budget.get()).sum::<u64>(),
            1_000_000
        );
    }
}
