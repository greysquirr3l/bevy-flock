//! The ECS half of the think scheduler: tiers, weights, budgets, trees.
//!
//! [`ThinkScheduler`] decides who thinks; [`TieredSchedule`] decides who thinks *with
//! what*, by giving each tier of cognition its own queue and its own reserved share of the
//! frame (ADR 0017). This module is the wiring that makes both real, as four ordered
//! systems in [`FlockSet::Perception`] and [`FlockSet::Cognition`]:
//!
//! 1. [`assign_tiers`] turns distance and engagement into a tier, with hysteresis and a
//!    promotion ration, and writes the weight that goes with it.
//! 2. [`plan_thinks`] reads every brain's declared cost and asks each tier who fits in its
//!    reserved budget.
//! 3. [`reflex_think`] runs the fixed rules for tier zero, which has no tree.
//! 4. [`tick_chosen`] ticks the trees of the chosen NPCs in the tiers that have one.
//!
//! Splitting it this way is not tidiness: the planner needs every cost before it can
//! choose, the tick needs every choice before it can run, and Bevy cannot hand one system a
//! read-only pass over the brains and then a mutable one.
//!
//! # Why a ration
//!
//! Promotion is a cost spike. A reflex agent that becomes deliberative needs a blackboard,
//! a perception snapshot and a compiled tree, and if a hundred of them are promoted on the
//! frame the player rounds a corner, the budget is gone before anything thinks. Promotions
//! are therefore rationed per tick and applied in identifier order, which keeps the choice
//! deterministic; demotions are free and are applied immediately.
//!
//! # The clock must advance
//!
//! The scheduler will not let one participant think twice on the same tick. Two
//! [`Update`](bevy_app::Update) passes without the clock advancing are one tick of thinking
//! followed by silence: healthy brains, `chosen: 0`. [`FlockPlugin`] advances the clock in
//! [`FlockSet::AdvanceTick`], so this only bites a test or a game that drives the systems
//! itself.
//!
//! [`FlockSet::Perception`]: crate::adapters::primary::plugin::FlockSet::Perception
//! [`FlockSet::Cognition`]: crate::adapters::primary::plugin::FlockSet::Cognition
//! [`FlockSet::AdvanceTick`]: crate::adapters::primary::plugin::FlockSet::AdvanceTick

use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;

use crate::adapters::primary::components::{Npc, NpcBrain};
use crate::application::tiered::{Chosen, TierStat, TieredSchedule, TieredStats};
use crate::behavior::blackboard::{Blackboard, Value};
use crate::behavior::registry::{FlockRegistry, RegistryDispatch};
use crate::domain::cognition::{BudgetShares, CognitionTier, TierPolicy};
use crate::domain::cost::VirtualCost;
use crate::domain::tick::Tick;
use crate::domain::weight::{ThinkWeight, WeightPolicy};

/// An NPC's tree working memory.
///
/// A component rather than a field on [`NpcBrain`] because a brain is shared: the tree is
/// behind an `Arc`, and the blackboard is the part that cannot be. Keeping them separate
/// also lets a demoted NPC keep its blackboard, so walking across a boundary does not reset
/// a conversation.
#[derive(Component, Debug, Clone, Default)]
pub struct NpcBlackboard(pub Blackboard);

/// The weight this NPC's thinking is scheduled with, and the value hysteresis reads.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcThinkWeight(pub ThinkWeight);

impl Default for NpcThinkWeight {
    fn default() -> Self {
        Self(ThinkWeight::BACKGROUND)
    }
}

/// The tier this NPC is thinking at.
///
/// Written by [`assign_tiers`] every tick. A game may read it to swap logic — the tier is a
/// statement about how much thinking is warranted, and the cheapest way to honour a
/// demotion is to give the NPC a smaller tree.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NpcTier(pub CognitionTier);

/// Marks an NPC the player is currently talking to.
///
/// Engagement outranks distance: a conversation promotes to [`CognitionTier::Social`]
/// whatever the range, and holds it until the game removes this component. Set it when
/// dialogue starts, remove it when dialogue ends.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NpcEngaged;

/// Marks an entity the crowd should care about: a player, a pet, a camera rig.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct PlayerFocus;

/// The distance bands and budget shares, as resources so a game tunes them once.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct WeightConfig(pub WeightPolicy);

/// The tier ladder's bands and reserved shares.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct TierConfig {
    /// How distance and engagement pick a tier.
    pub policy: TierPolicy,
    /// The budget each tier is guaranteed.
    pub shares: BudgetShares,
    /// The most promotions allowed on one tick.
    pub promotions_per_tick: u32,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            policy: TierPolicy::default(),
            shares: BudgetShares::default(),
            // Sixteen: enough that a scene reads as responsive, few enough that the spike
            // is a rounding error against a 14 ms budget.
            promotions_per_tick: 16,
        }
    }
}

/// What the last tick's scheduling decided, per tier and in total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinkStats {
    /// Per-tier detail, indexed by [`CognitionTier::index`].
    pub per_tier: [TierStat; 4],
    /// NPCs enrolled across every tier.
    pub enrolled: u32,
    /// NPCs that thought this tick.
    pub chosen: u32,
    /// Virtual cost committed.
    pub spent: VirtualCost,
    /// NPCs whose identifier fell outside the scheduler's capacity.
    pub beyond_capacity: u32,
    /// Promotions applied this tick.
    pub promoted: u32,
}

impl ThinkStats {
    /// The statistics for one tier.
    #[must_use]
    pub fn tier(&self, tier: CognitionTier) -> TierStat {
        self.per_tier.get(tier.index()).copied().unwrap_or_default()
    }
}

/// The scheduler, its per-tier queues and its statistics.
#[derive(Resource)]
pub struct ThinkSchedule {
    tiers: TieredSchedule,
    costs: Vec<VirtualCost>,
    /// Which tier chose each NPC this tick; `None` when it did not think.
    chosen: Vec<Option<CognitionTier>>,
    chosen_set: Chosen,
    stats: ThinkStats,
}

impl ThinkSchedule {
    /// The default total budget: 14 ms of virtual cost, leaving the planner's own overhead
    /// and the rest of the frame to the systems that move things.
    pub const DEFAULT_BUDGET: VirtualCost = VirtualCost::from_micros(14_000);

    /// A schedule with room for `capacity` NPCs and a total per-tick budget.
    #[must_use]
    pub fn new(capacity: u32, budget: VirtualCost) -> Self {
        Self {
            tiers: TieredSchedule::new(capacity, budget),
            costs: vec![VirtualCost::new(0); capacity as usize],
            chosen: vec![None; capacity as usize],
            chosen_set: Chosen::new(),
            stats: ThinkStats::default(),
        }
    }

    /// What the last tick decided.
    #[must_use]
    pub const fn stats(&self) -> ThinkStats {
        self.stats
    }

    /// The total per-tick budget.
    #[must_use]
    pub const fn budget(&self) -> VirtualCost {
        self.tiers.budget()
    }

    /// Replace the per-tick budget, re-splitting the reservations.
    pub fn set_budget(&mut self, budget: VirtualCost) {
        self.tiers.set_budget(budget);
    }

    /// Replace the reserved shares, re-splitting the budget.
    pub fn set_shares(&mut self, shares: BudgetShares) {
        self.tiers.set_shares(shares);
    }

    /// The reserved shares in force.
    #[must_use]
    pub const fn shares(&self) -> BudgetShares {
        self.tiers.shares()
    }

    /// Whether this NPC was chosen this tick.
    #[must_use]
    pub fn is_chosen(&self, id: u32) -> bool {
        self.chosen.get(id as usize).copied().flatten().is_some()
    }

    /// The tier that chose this NPC this tick, if any.
    #[must_use]
    pub fn chosen_tier(&self, id: u32) -> Option<CognitionTier> {
        self.chosen.get(id as usize).copied().flatten()
    }

    /// The tier an NPC is enrolled in.
    #[must_use]
    pub fn tier_of(&self, id: u32) -> Option<CognitionTier> {
        self.tiers.tier_of(id)
    }

    /// Whether an identifier is inside the schedule's capacity.
    #[must_use]
    pub const fn in_range(&self, id: u32) -> bool {
        (id as usize) < self.costs.len()
    }

    /// Note an identifier the schedule cannot serve.
    pub const fn note_beyond_capacity(&mut self) {
        self.stats.beyond_capacity = self.stats.beyond_capacity.saturating_add(1);
    }

    /// Reset the per-tick counters.
    ///
    /// Called once at the top of the tick, before any system observes a tier, so the
    /// promotion ration counts what this tick promoted rather than what the last one did.
    pub const fn begin_tick(&mut self) {
        self.stats.promoted = 0;
        self.stats.beyond_capacity = 0;
    }

    /// Record an NPC's tier and cost, enrolling or moving it as needed.
    ///
    /// A promotion that the ration refuses is not queued: the NPC keeps the tier it holds
    /// and the caller's next observation retries. That is simpler than a backlog and has
    /// the property that matters — a missed promotion costs a tick, never a cost spike and
    /// never a leak.
    pub fn observe(
        &mut self,
        id: u32,
        tier: CognitionTier,
        cost: VirtualCost,
        weight: ThinkWeight,
        ration: u32,
    ) {
        if !self.in_range(id) {
            self.note_beyond_capacity();
            return;
        }
        if let Some(slot) = self.costs.get_mut(id as usize) {
            *slot = cost;
        }
        // New participants enrol at the cheapest tier and climb under the same ration as
        // everybody else. Enrolling them straight into the tier the distance implies would
        // let a crowd spawned around the player spend the entire budget on the frame it
        // appears — the exact spike the ration exists to prevent.
        let current = if let Some(tier) = self.tiers.tier_of(id) {
            tier
        } else {
            self.tiers.place(id, CognitionTier::Reflex, weight.get());
            CognitionTier::Reflex
        };
        match tier.cmp(&current) {
            core::cmp::Ordering::Equal => {
                self.tiers.place(id, tier, weight.get());
            }
            core::cmp::Ordering::Greater => {
                if self.stats.promoted < ration {
                    self.tiers.place(id, tier, weight.get());
                    self.stats.promoted = self.stats.promoted.saturating_add(1);
                }
            }
            core::cmp::Ordering::Less => {
                // Demotion is free and immediate.
                self.tiers.place(id, tier, weight.get());
            }
        }
    }

    /// Plan every tier and record which NPCs were chosen.
    fn plan(&mut self, tick: Tick) {
        let costs = &self.costs;
        self.tiers.plan(
            tick,
            |id| {
                costs
                    .get(id as usize)
                    .copied()
                    .unwrap_or(VirtualCost::new(1))
            },
            &mut self.chosen_set,
        );

        self.chosen.fill(None);
        for tier in CognitionTier::ALL {
            for id in self.chosen_set.tier(tier) {
                if let Some(slot) = self.chosen.get_mut(*id as usize) {
                    *slot = Some(tier);
                }
            }
        }

        let tiered: TieredStats = self.tiers.stats();
        self.stats.per_tier = tiered.per_tier;
        self.stats.spent = tiered.spent;
        self.stats.enrolled = tiered.per_tier.iter().map(|tier| tier.enrolled).sum();
        self.stats.chosen = tiered.per_tier.iter().map(|tier| tier.chosen).sum();
    }
}

/// Turn distance and engagement into a tier and a weight, with hysteresis and a ration.
///
/// Runs in [`FlockSet::Perception`](crate::adapters::primary::plugin::FlockSet::Perception):
/// how close you are is a fact about the world, and the tier is derived from it.
/// What `assign_tiers` reads and writes per NPC, lifted out of the signature so the system reads
/// as a system rather than as a nested tuple.
type TierTargets<'a> = (
    &'a Npc,
    &'a Transform,
    &'a mut NpcTier,
    &'a mut NpcThinkWeight,
    Option<&'a NpcEngaged>,
);

/// Turn distance and engagement into a tier and a weight, with hysteresis and a ration.
///
/// Runs in [`FlockSet::Perception`](crate::adapters::primary::plugin::FlockSet::Perception):
/// how close you are is a fact about the world, and the tier is derived from it.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn assign_tiers(
    weight_config: Res<WeightConfig>,
    tier_config: Res<TierConfig>,
    mut schedule: ResMut<ThinkSchedule>,
    focus: Query<&Transform, With<PlayerFocus>>,
    mut npcs: Query<TierTargets<'_>>,
) {
    let ration = tier_config.promotions_per_tick;
    schedule.begin_tick();
    for (npc, transform, mut tier, mut weight, engaged) in &mut npcs {
        let distance_squared = nearest_focus_squared(transform, &focus);
        let next_tier = tier_config
            .policy
            .tier_for(distance_squared, tier.0, engaged.is_some());
        tier.0 = next_tier;
        // The tier sets a floor on the weight so a promoted NPC does not think once a
        // second; the weight policy can still raise it further.
        let by_distance = weight_config.0.weight_for(distance_squared, weight.0);
        weight.0 = by_distance.max(next_tier.weight_floor());
        // Recording the tier here means the plan sees promotions in the same tick they are
        // decided, and the ration is applied where the cost is known.
        schedule.observe(
            npc.0.index(),
            next_tier,
            VirtualCost::new(0),
            weight.0,
            ration,
        );
    }
}

/// The squared distance to the nearest focus, or infinity when there is none.
fn nearest_focus_squared(
    transform: &Transform,
    focus: &Query<&Transform, With<PlayerFocus>>,
) -> f32 {
    let mut best = f32::INFINITY;
    for focal in focus {
        let delta = transform.translation - focal.translation;
        best = best.min(delta.length_squared());
    }
    best
}

/// Record every brain's declared cost and ask each tier who thinks this tick.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn plan_thinks(
    clock: Res<crate::application::SimClock>,
    config: Res<TierConfig>,
    mut schedule: ResMut<ThinkSchedule>,
    mut brains: Query<(&Npc, &mut NpcBrain, &NpcTier, &NpcThinkWeight)>,
) {
    // The declared cost lives on the brain and is refreshed here rather than in
    // `assign_tiers`, because `assign_tiers` cannot borrow the brains mutably in the same
    // pass as the transforms.
    let mut costs: Vec<(u32, CognitionTier, VirtualCost, ThinkWeight)> = Vec::new();
    for (npc, brain, tier, weight) in &mut brains {
        costs.push((npc.0.index(), tier.0, brain.cost(), weight.0));
    }
    let ration = config.promotions_per_tick;
    for (id, tier, cost, weight) in costs {
        // Capacity was already reported by `assign_tiers`; counting it twice would make
        // the stat depend on how many systems happen to look at the NPC.
        if !schedule.in_range(id) {
            continue;
        }
        schedule.observe(id, tier, cost, weight, ration);
    }
    schedule.plan(clock.tick);
}

/// Tick the trees of the chosen NPCs in the tiers that have one.
///
/// Without a [`FlockRegistry`] resource there is nothing to dispatch leaves to, so nothing
/// is ticked: a game that forgot to register its logic sees `chosen` counts with no effect
/// rather than a crowd that mysteriously stands still.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn tick_chosen(
    clock: Res<crate::application::SimClock>,
    schedule: Res<ThinkSchedule>,
    registry: Option<ResMut<FlockRegistry>>,
    mut brains: Query<(&Npc, &mut NpcBrain, &mut NpcBlackboard)>,
) {
    let Some(mut registry) = registry else {
        return;
    };
    let mut dispatch = RegistryDispatch::new(&mut registry);
    let tick = clock.tick;
    for (npc, mut brain, mut blackboard) in &mut brains {
        let id = npc.0.index();
        let Some(tier) = schedule.chosen_tier(id) else {
            continue;
        };
        if tier.uses_tree() {
            let _ = brain.tick(&mut dispatch, &mut blackboard.0, tick, npc.0);
            continue;
        }
        // Tier zero has no tree. Its rule set is deliberately one read and one write: a
        // reflex agent that costs more than the tree it replaced is a demotion nobody
        // asked for. The flee countdown is the archetypal case — it must keep ticking
        // between tree evaluations, or an NPC mid-flight forgets it was fleeing.
        let Some(Value::Int(remaining)) = blackboard
            .0
            .get(crate::behavior::leaves::KEY_FLEE_REMAINING)
        else {
            continue;
        };
        if remaining > 0 {
            blackboard.0.set(
                crate::behavior::leaves::KEY_FLEE_REMAINING,
                Value::Int(remaining - 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testkit::OrDie;
    use std::sync::Arc;

    use bevy_app::{App, Update};

    use super::*;
    use crate::behavior::tree::{BehaviorTree, NodeSpec};
    use crate::domain::ids::NpcId;
    use crate::domain::tick::TicksPerSecond;

    fn clock() -> crate::application::SimClock {
        crate::application::SimClock::new(TicksPerSecond::SIXTY)
    }

    fn tree() -> Arc<BehaviorTree> {
        Arc::new(BehaviorTree::try_from_spec(&NodeSpec::Action(0)).or_die())
    }

    fn spawn(app: &mut App, id: u32, x: f32, cost: VirtualCost) {
        app.world_mut().spawn((
            Npc(NpcId::unique(id)),
            Transform::from_xyz(x, 0.0, 0.0),
            NpcBrain::new(tree(), cost),
            NpcBlackboard::default(),
            NpcThinkWeight::default(),
            NpcTier::default(),
        ));
    }

    fn reflex_only() -> TierConfig {
        TierConfig {
            shares: BudgetShares {
                reflex: 1000,
                reactive: 0,
                deliberative: 0,
                social: 0,
            },
            ..TierConfig::default()
        }
    }

    fn app(capacity: u32, budget: VirtualCost, tiers: TierConfig) -> App {
        let mut app = App::new();
        app.insert_resource(clock())
            .insert_resource(WeightConfig::default())
            .insert_resource(tiers)
            .insert_resource(ThinkSchedule::new(capacity, budget))
            .add_systems(Update, (assign_tiers, plan_thinks).chain());
        app
    }

    #[test]
    fn a_tier_budget_chooses_only_what_fits() {
        let mut app = app(100, VirtualCost::from_micros(10), reflex_only());
        for id in 0..100 {
            spawn(&mut app, id, 500.0, VirtualCost::from_micros(1));
        }
        app.update();
        let stats = app.world().resource::<ThinkSchedule>().stats();
        assert_eq!(stats.enrolled, 100);
        // Reflex holds the whole budget here (shares 100/0/0/0, normalised), 10 us buys ten.
        assert!(
            stats.chosen <= 10,
            "chosen {} exceeded the budget",
            stats.chosen
        );
        assert!(stats.spent.get() <= VirtualCost::from_micros(10).get());
    }

    #[test]
    fn distance_promotes_and_engagement_promotes_further() {
        let mut app = app(8, ThinkSchedule::DEFAULT_BUDGET, TierConfig::default());
        app.world_mut()
            .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
        spawn(&mut app, 0, 1.0, VirtualCost::from_micros(4));
        spawn(&mut app, 1, 500.0, VirtualCost::from_micros(1));
        app.update();

        let mut query = app.world_mut().query::<(&Npc, &NpcTier)>();
        let mut tiers: Vec<(u32, CognitionTier)> = query
            .iter(app.world())
            .map(|(npc, tier)| (npc.0.index(), tier.0))
            .collect();
        tiers.sort_unstable();
        assert_eq!(
            tiers,
            vec![(0, CognitionTier::Deliberative), (1, CognitionTier::Reflex)],
            "distance walks the ladder"
        );

        // Engagement outranks distance: the far NPC becomes social while it is spoken to.
        let entity = {
            let mut query = app.world_mut().query::<(&Npc, Entity)>();
            query
                .iter(app.world())
                .find(|(npc, _)| npc.0.index() == 1)
                .map(|(_, entity)| entity)
                .or_die()
        };
        app.world_mut().entity_mut(entity).insert(NpcEngaged);
        app.update();
        let tier = app
            .world()
            .get::<NpcTier>(entity)
            .map(|tier| tier.0)
            .or_die();
        assert_eq!(tier, CognitionTier::Social);
    }

    #[test]
    fn promotions_are_rationed_and_queued() {
        // Twenty NPCs walk into range on the same tick with a ration of four.
        let config = TierConfig {
            promotions_per_tick: 4,
            ..TierConfig::default()
        };
        let mut app = app(32, ThinkSchedule::DEFAULT_BUDGET, config);
        app.world_mut()
            .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
        for id in 0..20 {
            spawn(&mut app, id, 1.0, VirtualCost::from_micros(2));
        }
        app.update();
        let stats = app.world().resource::<ThinkSchedule>().stats();
        assert_eq!(stats.promoted, 4, "the ration is a hard ceiling per tick");

        // The sixteen that missed out keep the tier they hold, so nothing is lost: they
        // simply retry on later ticks, at the ration's pace.
        let schedule = app.world().resource::<ThinkSchedule>();
        let promoted = (0..20)
            .filter(|id| schedule.tier_of(*id) == Some(CognitionTier::Deliberative))
            .count();
        assert_eq!(promoted, 4, "only the rationed promotions landed");

        // Four more ticks of ration: at most sixteen more promotions, so all twenty fit.
        for _ in 0..4 {
            let tick = app
                .world()
                .resource::<crate::application::SimClock>()
                .tick
                .get()
                + 1;
            app.world_mut()
                .resource_mut::<crate::application::SimClock>()
                .tick = Tick::new(tick);
            app.update();
        }
        let schedule = app.world().resource::<ThinkSchedule>();
        let promoted = (0..20)
            .filter(|id| schedule.tier_of(*id) == Some(CognitionTier::Deliberative))
            .count();
        assert_eq!(promoted, 20, "the backlog drains at the ration's pace");
    }

    #[test]
    fn an_npc_beyond_capacity_is_counted_not_fatal() {
        let mut app = app(2, VirtualCost::from_micros(10), reflex_only());
        for id in 0..5 {
            spawn(&mut app, id, 500.0, VirtualCost::from_micros(1));
        }
        app.update();
        let stats = app.world().resource::<ThinkSchedule>().stats();
        assert_eq!(stats.beyond_capacity, 3);
        assert!(stats.enrolled <= 2);
    }
}
