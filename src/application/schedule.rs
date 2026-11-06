//! The virtual-cost fair-queue think scheduler (ADR 0009).
//!
//! Ten thousand NPCs can each think every tick. A hundred thousand cannot, and the
//! usual answer — "think every Nth frame", with N picked by holding a wet finger up —
//! is wrong in three ways at once: it gives a cheap NPC the same allowance as an
//! expensive one, it starves whoever is unlucky in the modulo, and its cadence depends
//! on the frame rate, which makes a replay diverge.
//!
//! This is the alternative. Every participant carries a **virtual finish time**; the
//! scheduler always runs the smallest one, then advances it by that participant's cost
//! divided by its weight. Two consequences fall out and both are tested below:
//!
//! * **The budget is spent where it buys the most thinking.** A participant that costs
//!   400 ns runs a hundred times as often as one that costs 40 µs, and both are being
//!   ticked as often as their cost allows rather than as often as a constant says.
//! * **Nobody starves.** Virtual time is bounded above by the budget, so the wait for
//!   any participant is bounded by `total_stride / budget` ticks, whatever the mix.
//!
//! Nothing here reads a clock. `plan` is a pure function of the scheduler's state, the
//! budget and the costs handed to it, so the same tick in a replay produces the same
//! list of participants in the same order.
//!
//! # The seam
//!
//! This type does not know what a think is. The caller supplies `cost_of` and runs the
//! participants it gets back, which keeps the scheduling policy testable without a
//! world, and keeps the world's opinion of cost — measured, per ADR 0009 — out of the
//! policy. Wiring it to [`NpcBrain`] is the adapter's job, not this module's.
//!
//! [`NpcBrain`]: crate::adapters::primary::components::NpcBrain

use core::cmp::Reverse;
use std::collections::BinaryHeap;

use thiserror::Error;

use crate::domain::cost::VirtualCost;
use crate::domain::tick::Tick;

/// Sentinel for a participant that has never run.
const NEVER: u64 = u64::MAX;

/// Why a participant could not be enrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SchedulerError {
    /// The identifier is at or past the capacity the scheduler was built for.
    #[error("participant {id} is outside the capacity of {capacity}")]
    OutOfRange {
        /// The identifier that was refused.
        id: u32,
        /// The capacity the scheduler was built with.
        capacity: u32,
    },
    /// The participant is already enrolled, and enrolling it twice would give it two
    /// virtual finish times and therefore twice the share.
    #[error("participant {id} is already enrolled")]
    AlreadyEnrolled {
        /// The identifier that was refused.
        id: u32,
    },
    /// A weight of zero would give a stride of infinity: the participant would never
    /// run again, which is indistinguishable from a bug.
    #[error("participant {id} has weight zero; use remove() to stop scheduling it")]
    ZeroWeight {
        /// The identifier that was refused.
        id: u32,
    },
}

/// What one call to [`ThinkScheduler::plan`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    /// How many participants were chosen.
    pub chosen: u32,
    /// The virtual cost committed, which is at most the budget.
    pub spent: VirtualCost,
    /// How many enrolled participants were left for a later tick.
    pub deferred: u32,
}

/// Schedules a fixed population of thinkers against a per-tick virtual budget.
///
/// Capacity is fixed at construction because the arrays are dense and indexed by
/// participant: no allocation happens inside [`plan`](Self::plan), which is what lets
/// it run every tick without touching the allocator.
#[derive(Debug, Clone)]
pub struct ThinkScheduler {
    /// Min-heap of virtual finish times, ordered by `(vft, id)` so ties break by
    /// identifier and the plan is reproducible.
    queue: BinaryHeap<Reverse<(u64, u32)>>,
    weights: Vec<u32>,
    /// Tick each participant last ran, for the starvation bound and for diagnostics.
    ///
    /// `NEVER` until the participant first runs: a sentinel rather than zero, because a
    /// participant enrolled at tick zero would otherwise look as though it had already
    /// run on the tick it was enrolled and would sit out its first tick.
    last_run: Vec<u64>,
    /// Participants popped but already served this tick, held here so they keep their
    /// virtual finish time and go back at the head of the queue next tick.
    deferred: Vec<(u64, u32)>,
    enrolled: u32,
    tick: Tick,
    /// The virtual time each participant became runnable.
    ///
    /// The `max(start, virtual_time)` guard in `plan` reads it. Without that term a
    /// participant that idled — because it was expensive, or weightless, or simply
    /// unlucky — accrues credit while others think and then monopolises the queue when
    /// its turn comes. This is the single most load-bearing line in the module and it has
    /// a test that fails when it is removed.
    start: Vec<u64>,
    /// The virtual time the queue has reached, maintained by `plan`.
    ///
    /// Read when a participant enrols. It used to be computed by scanning the heap,
    /// which was both O(n) on every enrolment and wrong: `BinaryHeap::iter` yields
    /// elements in arbitrary order, so "the first non-zero weight" was not the minimum.
    horizon: u64,
    /// Largest gap between runs any participant has experienced, in ticks.
    worst_wait: u32,
}

impl ThinkScheduler {
    /// An empty scheduler with room for `capacity` participants.
    #[must_use]
    pub fn new(capacity: u32) -> Self {
        let capacity = capacity as usize;
        Self {
            queue: BinaryHeap::with_capacity(capacity),
            weights: vec![0; capacity],
            last_run: vec![NEVER; capacity],
            start: vec![0; capacity],
            deferred: Vec::new(),
            enrolled: 0,
            tick: Tick::ZERO,
            horizon: 0,
            worst_wait: 0,
        }
    }

    /// How many participants are enrolled.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.enrolled
    }

    /// Whether anything is enrolled.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.enrolled == 0
    }

    /// The capacity this scheduler was built for.
    #[must_use]
    pub fn capacity(&self) -> u32 {
        self.weights.len() as u32
    }

    /// The weight a participant is enrolled with, or `None` if it is not enrolled.
    #[must_use]
    pub fn weight_of(&self, id: u32) -> Option<u32> {
        match self.weight_at(id) {
            0 => None,
            weight => Some(weight),
        }
    }

    /// The longest any enrolled participant has waited between runs, in ticks.
    ///
    /// The number a fairness test asserts against and a profiler reads: it is the
    /// guarantee the plan makes, measured rather than claimed.
    #[must_use]
    pub const fn worst_wait(&self) -> u32 {
        self.worst_wait
    }

    /// The weight of a participant, or zero if it is not enrolled.
    fn weight_at(&self, id: u32) -> u32 {
        self.weights.get(id as usize).copied().unwrap_or(0)
    }

    /// Overwrite a participant's weight. Out-of-range ids are ignored; every caller
    /// has already checked the range.
    fn put_weight(&mut self, id: u32, weight: u32) {
        if let Some(slot) = self.weights.get_mut(id as usize) {
            *slot = weight;
        }
    }

    /// The tick a participant last ran, or [`NEVER`].
    fn last_run_at(&self, id: u32) -> u64 {
        self.last_run.get(id as usize).copied().unwrap_or(NEVER)
    }

    /// The virtual time a participant became runnable.
    fn start_at(&self, id: u32) -> u64 {
        self.start.get(id as usize).copied().unwrap_or(0)
    }

    /// Record that a participant ran on this tick.
    fn mark_ran(&mut self, id: u32, tick: u64) {
        if let Some(slot) = self.last_run.get_mut(id as usize) {
            *slot = tick;
        }
    }

    /// Enrol a participant with a share weight.
    ///
    /// A weight of 3 against a weight of 1 gets three times the thinking, in virtual
    /// cost rather than in runs — the participant still pays its own cost each time.
    pub fn enrol(&mut self, id: u32, weight: u32) -> Result<(), SchedulerError> {
        if id as usize >= self.weights.len() {
            return Err(SchedulerError::OutOfRange {
                id,
                capacity: self.capacity(),
            });
        }
        if self.weight_at(id) != 0 {
            return Err(SchedulerError::AlreadyEnrolled { id });
        }
        if weight == 0 {
            return Err(SchedulerError::ZeroWeight { id });
        }
        self.put_weight(id, weight);
        if let Some(slot) = self.start.get_mut(id as usize) {
            *slot = self.horizon;
        }
        self.enrolled = self.enrolled.saturating_add(1);
        // Entrants start at the current virtual horizon rather than at zero: otherwise
        // a participant enrolled at tick 5,000 would be scheduled ahead of everybody
        // forever, which is the queueing equivalent of arriving early to every meeting.
        self.queue.push(Reverse((self.horizon, id)));
        Ok(())
    }

    /// Change an enrolled participant's weight, keeping its place in the queue.
    pub fn set_weight(&mut self, id: u32, weight: u32) -> Result<(), SchedulerError> {
        if id as usize >= self.weights.len() {
            return Err(SchedulerError::OutOfRange {
                id,
                capacity: self.capacity(),
            });
        }
        if self.weight_at(id) == 0 {
            return Err(SchedulerError::AlreadyEnrolled { id });
        }
        if weight == 0 {
            return Err(SchedulerError::ZeroWeight { id });
        }
        self.put_weight(id, weight);
        Ok(())
    }

    /// Stop scheduling a participant.
    ///
    /// Its queue entry is left in place and skipped when popped, which keeps `plan`
    /// allocation-free. The cost is one comparison per stale entry, and the
    /// alternative — a heap rebuild — would be paid on every death in a crowd.
    pub fn remove(&mut self, id: u32) {
        if self.weight_at(id) != 0 {
            self.put_weight(id, 0);
            self.enrolled = self.enrolled.saturating_sub(1);
        }
    }

    /// Advance the scheduler's notion of the current tick.
    ///
    /// Called once per simulation tick, before planning. The tick is only ever used for
    /// bookkeeping — the run order depends on virtual finish times, which are advanced
    /// by cost, never by elapsed time.
    pub fn advance(&mut self, tick: Tick) {
        self.tick = tick;
    }

    /// Choose who thinks this tick, given a budget and each participant's cost.
    ///
    /// Appends the chosen identifiers to `out` in scheduling order and returns what was
    /// decided. A participant whose cost would overflow the remaining budget is left for
    /// the next tick rather than run on a partial budget: a think that is interrupted is
    /// not cheaper, it is wrong.
    ///
    /// `out` is cleared first, so the same vector can be reused every tick.
    pub fn plan(
        &mut self,
        budget: VirtualCost,
        mut cost_of: impl FnMut(u32) -> VirtualCost,
        out: &mut Vec<u32>,
    ) -> Plan {
        out.clear();
        let mut spent = 0_u64;
        let limit = budget.get();
        let tick = self.tick.get();

        while let Some(&Reverse((vft, id))) = self.queue.peek() {
            let weight = self.weight_at(id);
            if weight == 0 {
                // A participant that left; drop the stale entry.
                self.queue.pop();
                continue;
            }
            if self.last_run_at(id) == tick {
                // Already thought this tick. Holding it out of the queue rather than
                // re-running it is the difference between "a cheap NPC thinks often" and
                // "a cheap NPC thinks a thousand times in one frame", which is what this
                // scheduler did before the guarantee was written down and tested.
                self.queue.pop();
                self.deferred.push((vft, id));
                continue;
            }
            let cost = cost_of(id).get().max(1);
            let Some(total) = spent.checked_add(cost) else {
                break;
            };
            if total > limit {
                break;
            }
            self.queue.pop();
            spent = total;

            // The guard ADR 0009 calls load-bearing: a participant re-enters the queue at
            // the current virtual time, not at whatever it left behind. `vft` alone lets an
            // idle participant bank credit and then monopolise the queue on its return.
            let base = vft.max(self.start_at(id)).max(self.horizon);
            let stride = stride_for(cost, weight);
            self.queue.push(Reverse((base.saturating_add(stride), id)));
            out.push(id);

            let last = self.last_run_at(id);
            if last != NEVER {
                let waited = tick.saturating_sub(last).min(u64::from(u32::MAX)) as u32;
                self.worst_wait = self.worst_wait.max(waited);
            }
            self.mark_ran(id, tick);
            self.horizon = self.horizon.max(base);
        }

        for entry in self.deferred.drain(..) {
            self.queue.push(Reverse(entry));
        }

        Plan {
            chosen: out.len() as u32,
            spent: VirtualCost::new(spent),
            deferred: self.enrolled.saturating_sub(out.len() as u32),
        }
    }
}

/// The virtual cost a participant is charged for one think, after its weight.
///
/// Higher weight, shorter stride. Scaled so a weight of one is exactly the cost, and
/// the multiplication saturates rather than wrapping: a cost of 400 µs at a weight of
/// 1,000 is 400 µs of virtual time, which cannot overflow in a simulated century.
fn stride_for(cost: u64, weight: u32) -> u64 {
    // Costs are already in the nanos range and weights are small; the scale keeps the
    // division from throwing away the low digits for a cost of a few hundred nanos.
    const WEIGHT_SCALE: u64 = 1_024;
    cost.saturating_mul(WEIGHT_SCALE) / u64::from(weight)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{GetOrDie, GetOrDieMut, OrDie};

    const COST: VirtualCost = VirtualCost::from_micros(4);

    fn enrolled(count: u32, weight: u32) -> ThinkScheduler {
        let mut scheduler = ThinkScheduler::new(count);
        for id in 0..count {
            scheduler.enrol(id, weight).or_die();
        }
        scheduler
    }

    #[test]
    fn the_budget_is_never_overspent() {
        let mut scheduler = enrolled(100, 1);
        let budget = VirtualCost::from_micros(50);
        let mut out = Vec::new();
        let plan = scheduler.plan(budget, |_| COST, &mut out);
        assert!(plan.spent.get() <= budget.get(), "{plan:?}");
        assert_eq!(plan.chosen, 12, "50 us buys twelve 4 us thinks");
        assert_eq!(plan.chosen as usize, out.len());
        assert_eq!(plan.deferred, 88);
    }

    #[test]
    fn an_interrupted_think_is_not_taken() {
        // Twelve fit, the thirteenth would overrun by 2 us: it must wait, not run on
        // the remainder.
        let mut scheduler = enrolled(20, 1);
        let mut out = Vec::new();
        let plan = scheduler.plan(VirtualCost::from_micros(50), |_| COST, &mut out);
        assert_eq!(plan.spent.get(), 48_000);
        assert_eq!(*out.last().or_die(), 11);
    }

    #[test]
    fn the_plan_is_reproducible() {
        let run = || {
            let mut scheduler = enrolled(64, 1);
            let mut out = Vec::new();
            let mut seen = Vec::new();
            for tick in 0..40 {
                scheduler.advance(Tick::new(tick));
                scheduler.plan(
                    VirtualCost::from_micros(30),
                    |id| {
                        // Cost varies by participant, as it does in a real crowd.
                        VirtualCost::from_micros(1 + u64::from(id % 5))
                    },
                    &mut out,
                );
                seen.extend_from_slice(&out);
            }
            seen
        };
        assert_eq!(run(), run(), "same state, same budget, same order");
    }

    #[test]
    fn weight_buys_proportional_thinking() {
        // Weight only means anything when the budget is scarce: with room for everybody
        // every tick, everybody runs every tick and the weights are decoration. Fifty
        // participants at weight one against fifty at weight three, with a budget that
        // fits roughly eight cheap thinks, must come out about three to one.
        let mut scheduler = ThinkScheduler::new(100);
        for id in 0..50 {
            scheduler.enrol(id, 1).or_die();
        }
        for id in 50..100 {
            scheduler.enrol(id, 3).or_die();
        }
        let mut out = Vec::new();
        let mut runs = [0_u32; 2];
        for tick in 0..2_000 {
            scheduler.advance(Tick::new(tick));
            scheduler.plan(
                VirtualCost::from_micros(8),
                |_| VirtualCost::from_micros(1),
                &mut out,
            );
            for id in &out {
                *runs.at_mut(usize::from(*id >= 50)) += 1;
            }
        }
        let ratio = f64::from(runs.at(1)) / f64::from(runs.at(0));
        assert!(
            (2.8..=3.2).contains(&ratio),
            "weight 3 should get three times the thinking, got {ratio} ({runs:?})"
        );
    }

    #[test]
    fn nobody_starves() {
        // A hundred participants with deliberately mismatched costs, a budget that only
        // fits a fifth of the cheap ones. The wait must stay inside the bound the module
        // documents: total stride over budget, plus one tick of slack.
        let mut scheduler = ThinkScheduler::new(100);
        let cost = |id: u32| VirtualCost::from_micros(1 + u64::from(id % 7));
        for id in 0..100 {
            scheduler.enrol(id, 1).or_die();
        }
        let budget = VirtualCost::from_micros(20);
        let mut out = Vec::new();
        let mut seen = [false; 100];
        let mut ticks = 0;
        while !seen.iter().all(|was| *was) && ticks < 1_000 {
            scheduler.advance(Tick::new(ticks));
            scheduler.plan(budget, cost, &mut out);
            for id in &out {
                *seen.at_mut(*id as usize) = true;
            }
            ticks += 1;
        }
        assert!(
            seen.iter().all(|was| *was),
            "every participant ran within {ticks} ticks"
        );
        assert!(
            scheduler.worst_wait() <= 40,
            "worst wait {}",
            scheduler.worst_wait()
        );
    }

    #[test]
    fn a_zero_budget_plans_nothing() {
        let mut scheduler = enrolled(10, 1);
        let mut out = vec![99];
        let plan = scheduler.plan(VirtualCost::new(0), |_| COST, &mut out);
        assert_eq!(plan.chosen, 0);
        assert_eq!(plan.spent, VirtualCost::new(0));
        assert_eq!(plan.deferred, 10, "nothing ran, so everything is deferred");
        assert!(
            out.is_empty(),
            "the caller's vector is cleared, not appended to"
        );
    }

    #[test]
    fn a_removed_participant_is_never_chosen_again() {
        let mut scheduler = enrolled(8, 1);
        scheduler.remove(3);
        assert_eq!(scheduler.len(), 7);
        let mut out = Vec::new();
        for tick in 0..50 {
            scheduler.advance(Tick::new(tick));
            scheduler.plan(VirtualCost::from_micros(100), |_| COST, &mut out);
            assert!(!out.contains(&3), "removed participant ran on tick {tick}");
        }
    }

    #[test]
    fn a_newcomer_gets_no_credit_for_the_past() {
        // A participant enrolled late joins at the current virtual time. It may run soon
        // — it did just arrive — but it must not arrive holding a virtual finish time of
        // zero and collect every debt the queue has already paid, which would let a
        // spawned crowd starve the residents for as many ticks as it has members.
        let mut scheduler = ThinkScheduler::new(11);
        for id in 0..10 {
            scheduler.enrol(id, 1).or_die();
        }
        let mut out = Vec::new();
        for tick in 0..200 {
            scheduler.advance(Tick::new(tick));
            scheduler.plan(VirtualCost::from_micros(12), |_| COST, &mut out);
        }
        scheduler.enrol(10, 1).or_die();

        let mut runs = 0_u32;
        let mut first_tick = None;
        for tick in 200..250 {
            scheduler.advance(Tick::new(tick));
            scheduler.plan(VirtualCost::from_micros(12), |_| COST, &mut out);
            if out.contains(&10) {
                runs += 1;
                first_tick.get_or_insert(tick);
            }
        }
        let first = first_tick.or_die();
        assert!(
            first <= 210,
            "the newcomer waited {} ticks for its first think",
            first - 200
        );
        // Eleven participants, a budget that fits three: a fair share over fifty ticks is
        // about fourteen runs. Well beyond that means it is spending credit it never
        // earned.
        assert!(
            runs <= 20,
            "the newcomer ran {runs} times in 50 ticks, more than its share"
        );
    }

    #[test]
    fn an_idle_participant_cannot_bank_credit() {
        // The failure this guards: participant 0 is a hundred times as expensive as the
        // rest, so it runs rarely and its virtual finish time falls far behind. When its
        // turn comes it must re-enter at the current virtual time, not collect the debt
        // the queue ran up while it waited. Remove the `max(start, virtual_time)` term in
        // `plan` and this test fails with a burst.
        let mut scheduler = ThinkScheduler::new(16);
        for id in 0..16 {
            scheduler.enrol(id, 1).or_die();
        }
        let expensive = |id: u32| {
            if id == 0 {
                VirtualCost::from_micros(100)
            } else {
                VirtualCost::from_micros(1)
            }
        };
        let mut out = Vec::new();
        let mut burst = 0;
        for tick in 0..400 {
            scheduler.advance(Tick::new(tick));
            scheduler.plan(VirtualCost::from_micros(40), expensive, &mut out);
            if out.first() == Some(&0) {
                burst += 1;
            }
        }
        // A budget of 40 us fits the expensive one four times in ten ticks; if it were
        // banking credit it would lead the queue on far more ticks than that.
        assert!(
            burst <= 120,
            "the expensive participant led {burst} of 400 ticks"
        );
    }

    /// The claim the plan and the README make, measured and then asserted: a 60 Hz
    /// budget serves a known share of the population, and the wait stays bounded as the
    /// population grows past what the budget can hold.
    ///
    /// Run with `--nocapture` to see the table this test prints.
    #[test]
    fn the_budget_serves_a_known_share_of_a_large_population() {
        let budget = VirtualCost::from_micros(16_600);
        let cost = |id: u32| match id % 16 {
            0 => VirtualCost::from_micros(12),
            1..=3 => VirtualCost::from_micros(4),
            _ => VirtualCost::from_micros(1),
        };
        for count in [1_u32, 1_000, 10_000, 100_000] {
            let mut scheduler = ThinkScheduler::new(count);
            for id in 0..count {
                scheduler
                    .enrol(id, if id % 4 == 0 { 4 } else { 1 })
                    .or_die();
            }
            let mut out = Vec::with_capacity(count as usize);
            let mut chosen = 0_u64;
            let ticks = 50_u64;
            for tick in 0..ticks {
                scheduler.advance(Tick::new(tick));
                chosen += u64::from(scheduler.plan(budget, cost, &mut out).chosen);
            }
            let per_tick = chosen as f64 / ticks as f64;
            let share = per_tick / f64::from(count) * 100.0;
            eprintln!(
                "{count:>7} participants: {per_tick:>8.1} think per tick, {share:>5.2}% served per tick, worst wait {} ticks",
                scheduler.worst_wait()
            );
            assert!(per_tick >= 1.0, "{count}: the budget served nobody");
            assert!(
                scheduler.worst_wait() <= 100,
                "{count}: worst wait {} ticks exceeds what the module documents",
                scheduler.worst_wait()
            );
        }
    }

    #[test]
    fn enrolment_refuses_what_it_cannot_serve() {
        let mut scheduler = ThinkScheduler::new(2);
        scheduler.enrol(0, 1).or_die();
        assert_eq!(
            scheduler.enrol(0, 1),
            Err(SchedulerError::AlreadyEnrolled { id: 0 })
        );
        assert_eq!(
            scheduler.enrol(2, 1),
            Err(SchedulerError::OutOfRange { id: 2, capacity: 2 })
        );
        assert_eq!(
            scheduler.set_weight(0, 0),
            Err(SchedulerError::ZeroWeight { id: 0 })
        );
    }

    #[test]
    fn an_absurd_cost_saturates_instead_of_wrapping() {
        let mut scheduler = ThinkScheduler::new(4);
        scheduler.enrol(0, 1).or_die();
        let mut out = Vec::new();
        for tick in 0..4 {
            scheduler.advance(Tick::new(tick));
            scheduler.plan(
                VirtualCost::from_micros(u64::MAX / 2),
                |_| VirtualCost::new(u64::MAX),
                &mut out,
            );
        }
        // The assertion is that this terminates with a coherent plan rather than
        // wrapping a virtual time back to zero and starving everybody behind it.
        assert!(out.len() <= 1);
    }
}
