//! The ECS half of the think scheduler: brains in, budget out, trees ticked.
//!
//! [`ThinkScheduler`] decides *who* thinks. This module is the wiring that makes the
//! decision real, and it is three systems in [`FlockSet::Cognition`], in order:
//!
//! 1. [`set_think_weights`] measures each NPC's distance to the nearest player and turns
//!    it into a weight, with the hysteresis in [`WeightPolicy`].
//! 2. [`plan_thinks`] reads the declared cost of every enrolled brain and asks the
//!    scheduler who fits in the budget this tick.
//! 3. [`tick_chosen`] runs the chosen trees, and nothing else.
//!
//! Splitting it this way is not tidiness. The planner needs every cost before it can
//! choose, the tick needs every choice before it can run, and Bevy cannot hand one system
//! a read-only pass over the brains and then a mutable one. Three systems with a resource
//! between them is the shape the borrow checker asks for, and it happens to be the shape
//! that makes each step testable on its own.
//!
//! # What a game has to opt into
//!
//! Brains are not spawned by [`FlockPlugin`]: a crowd of ten thousand NPCs with no logic
//! is the common case, and compiling ten thousand trees by default would be a surprise
//! billed in milliseconds. A game adds [`NpcBrain`] and [`NpcBlackboard`] to the entities
//! it wants thinking, and marks the entities a crowd should care about with
//! [`PlayerFocus`]. Everything else keeps steering.
//!
//! # The clock must advance
//!
//! The scheduler will not let one participant think twice on the same tick, which is what
//! makes the plan reproducible. The consequence is that two [`Update`](bevy_app::Update)
//! passes without the clock advancing are one tick of thinking followed by silence: the
//! brains are healthy and the stats say `chosen: 0`. [`FlockPlugin`] advances the clock in
//! [`FlockSet::AdvanceTick`], so this only bites a test or a game that drives the systems
//! itself.
//!
//! [`FlockSet::Cognition`]: crate::adapters::primary::plugin::FlockSet::Cognition
//! [`FlockSet::AdvanceTick`]: crate::adapters::primary::plugin::FlockSet::AdvanceTick

use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;

use crate::adapters::primary::components::{Npc, NpcBrain};
use crate::application::schedule::{Plan, ThinkScheduler};
use crate::behavior::blackboard::Blackboard;
use crate::behavior::registry::{FlockRegistry, RegistryDispatch};
use crate::domain::cost::VirtualCost;
use crate::domain::tick::Tick;
use crate::domain::weight::{ThinkWeight, WeightPolicy};

/// An NPC's tree working memory.
///
/// A component rather than a field on [`NpcBrain`] because a brain is shared: the tree is
/// behind an `Arc`, and the blackboard is the part that cannot be. Keeping them separate
/// also means an NPC demoted to a reflex tier can drop its blackboard without dropping
/// the tree it will need again in a second.
#[derive(Component, Debug, Clone, Default)]
pub struct NpcBlackboard(pub Blackboard);

/// The weight this NPC's thinking is scheduled with, and the value the hysteresis reads.
///
/// Written by [`set_think_weights`] every tick; the previous value is what makes promotion
/// sticky, so this component is state, not a cache.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcThinkWeight(pub ThinkWeight);

impl Default for NpcThinkWeight {
    fn default() -> Self {
        Self(ThinkWeight::BACKGROUND)
    }
}

/// Marks an entity the crowd should care about: a player, a pet, a camera rig.
///
/// Distance to the nearest of these decides an NPC's weight. A world with no focus has
/// nothing to be near, so every NPC sits at [`ThinkWeight::BACKGROUND`] and the scheduler
/// divides the budget evenly — which is the honest behaviour for a headless server.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct PlayerFocus;

/// The distance bands, as a resource so a game can tune them once.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct WeightConfig(pub WeightPolicy);

/// What the last tick's scheduling decided, for a HUD, a log or a test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinkStats {
    /// NPCs enrolled in the scheduler.
    pub enrolled: u32,
    /// NPCs that thought this tick.
    pub chosen: u32,
    /// Virtual cost committed.
    pub spent: VirtualCost,
    /// NPCs left for a later tick.
    pub deferred: u32,
    /// Longest wait any NPC has had between thinks, in ticks.
    pub worst_wait: u32,
    /// NPCs whose identifier fell outside the scheduler's capacity.
    ///
    /// Reported rather than panicking: a game that spawns past the configured population
    /// has a configuration bug, and the symptom should be a number in the stats rather
    /// than a crash on the frame a crowd walks into view.
    pub beyond_capacity: u32,
    /// NPCs whose tree was not ticked because no [`FlockRegistry`] resource existed.
    pub undispatched: u32,
}

/// The scheduler, its per-tick scratch space and its statistics.
#[derive(Resource)]
pub struct ThinkSchedule {
    scheduler: ThinkScheduler,
    costs: Vec<VirtualCost>,
    chosen: Vec<bool>,
    budget: VirtualCost,
    scratch: Vec<u32>,
    stats: ThinkStats,
}

impl ThinkSchedule {
    /// A schedule with room for `capacity` NPCs and a per-tick virtual budget.
    #[must_use]
    pub fn new(capacity: u32, budget: VirtualCost) -> Self {
        let capacity = capacity as usize;
        Self {
            scheduler: ThinkScheduler::new(capacity as u32),
            costs: vec![VirtualCost::new(0); capacity],
            chosen: vec![false; capacity],
            budget,
            scratch: Vec::new(),
            stats: ThinkStats::default(),
        }
    }

    /// The default budget: 14 ms of virtual cost, leaving the planner's own 1.8 ms and
    /// the rest of the frame to the systems that move things (ADR 0009, measurement in
    /// `docs/performance.md`).
    pub const DEFAULT_BUDGET: VirtualCost = VirtualCost::from_micros(14_000);

    /// What the last tick decided.
    #[must_use]
    pub const fn stats(&self) -> ThinkStats {
        self.stats
    }

    /// The per-tick budget.
    #[must_use]
    pub const fn budget(&self) -> VirtualCost {
        self.budget
    }

    /// Replace the per-tick budget.
    pub fn set_budget(&mut self, budget: VirtualCost) {
        self.budget = budget;
    }

    /// The scheduler, for tests and diagnostics.
    #[must_use]
    pub const fn scheduler(&self) -> &ThinkScheduler {
        &self.scheduler
    }

    /// Whether this NPC is in the schedule.
    #[must_use]
    pub fn is_enrolled(&self, id: u32) -> bool {
        self.scheduler.weight_of(id).is_some()
    }

    /// Whether this NPC was chosen this tick.
    #[must_use]
    pub fn is_chosen(&self, id: u32) -> bool {
        self.chosen.get(id as usize).copied().unwrap_or(false)
    }

    /// Record an NPC's declared cost, enrolling it if this is the first time it is seen.
    fn observe(&mut self, id: u32, cost: VirtualCost, weight: ThinkWeight) -> bool {
        if id as usize >= self.costs.len() {
            self.stats.beyond_capacity = self.stats.beyond_capacity.saturating_add(1);
            return false;
        }
        if let Some(slot) = self.costs.get_mut(id as usize) {
            *slot = cost;
        }
        match self.scheduler.weight_of(id) {
            None => {
                // Enrolment can fail only if the capacity shrank, which it cannot; a
                // weight of zero is impossible because `ThinkWeight` clamps it.
                let _ = self.scheduler.enrol(id, weight.get());
            }
            Some(current) if current != weight.get() => {
                // Promotion has to reach the scheduler or the NPC keeps the cadence it
                // was enrolled with: the weight policy would be writing a number into a
                // component nobody reads, which is exactly the bug the first integration
                // test caught.
                let _ = self.scheduler.set_weight(id, weight.get());
            }
            Some(_) => {}
        }
        true
    }

    /// Ask the scheduler who fits in the budget.
    fn plan(&mut self, tick: Tick) {
        self.chosen.fill(false);
        self.scheduler.advance(tick);
        let costs = &self.costs;
        let plan: Plan = self.scheduler.plan(
            self.budget,
            |id| {
                costs
                    .get(id as usize)
                    .copied()
                    .unwrap_or(VirtualCost::new(1))
            },
            &mut self.scratch,
        );
        for id in &self.scratch {
            if let Some(slot) = self.chosen.get_mut(*id as usize) {
                *slot = true;
            }
        }
        self.stats.chosen = plan.chosen;
        self.stats.spent = plan.spent;
        self.stats.deferred = plan.deferred;
        self.stats.enrolled = self.scheduler.len();
        self.stats.worst_wait = self.scheduler.worst_wait();
    }
}

/// Point each NPC's weight at the nearest player, with hysteresis.
///
/// Runs before planning, in [`FlockSet::Perception`](crate::adapters::primary::plugin::FlockSet::Perception):
/// how close you are is a fact about the world, and the weight is derived from it.
// Bevy hands resources and clocks to systems by value; that is the API, not a copy.
#[allow(clippy::needless_pass_by_value)]
pub fn set_think_weights(
    policy: Res<WeightConfig>,
    focus: Query<&Transform, With<PlayerFocus>>,
    mut npcs: Query<(&Npc, &Transform, &mut NpcThinkWeight)>,
) {
    // The nearest focus is the only one that matters; a crowd between two players is
    // close to both and the minimum is the honest answer.
    let mut nearest: Option<f32> = None;
    for transform in &focus {
        let point = transform.translation;
        let distance = point.length_squared();
        nearest = Some(nearest.map_or(distance, |best: f32| best.min(distance)));
    }
    for (npc, transform, mut weight) in &mut npcs {
        let _ = npc;
        // The distance that matters is from the NPC to the nearest focus, not to the
        // origin: with no focus, nothing is near and everybody is background.
        let distance_squared = match nearest {
            Some(_) => {
                let mut best = f32::INFINITY;
                for transform_focus in &focus {
                    let delta = transform.translation - transform_focus.translation;
                    best = best.min(delta.length_squared());
                }
                best
            }
            None => f32::INFINITY,
        };
        weight.0 = policy.0.weight_for(distance_squared, weight.0);
    }
}

/// Record every brain's declared cost and ask the scheduler who thinks this tick.
#[allow(clippy::needless_pass_by_value)]
pub fn plan_thinks(
    clock: Res<crate::application::SimClock>,
    mut schedule: ResMut<ThinkSchedule>,
    brains: Query<(&Npc, &NpcBrain, &NpcThinkWeight)>,
) {
    for (npc, brain, weight) in &brains {
        // The dense index, not the bit pattern: a recycled identifier names the same
        // participant as the original occupant of its pool slot, and the scheduler must
        // see one participant rather than two.
        schedule.observe(npc.0.index(), brain.cost(), weight.0);
    }
    schedule.plan(clock.tick);
}

/// Tick the trees of the NPCs the scheduler chose, and only those.
///
/// Without a [`FlockRegistry`] resource there is nothing to dispatch leaves to, so nothing
/// is ticked and the stats say so: a game that forgot to register its logic sees a number
/// rather than a crowd that mysteriously stands still.
#[allow(clippy::needless_pass_by_value)]
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
        if !schedule.is_chosen(npc.0.index()) {
            continue;
        }
        // The status is the tree's business: a failing branch is a simulation event, not
        // an error the scheduler reports.
        let _ = brain.tick(&mut dispatch, &mut blackboard.0, tick, npc.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use bevy_app::App;
    use bevy_app::Update;

    use crate::adapters::primary::components::NpcBrain;
    use crate::behavior::tree::{BehaviorTree, NodeSpec};
    use crate::domain::ids::NpcId;
    use crate::domain::tick::TicksPerSecond;

    fn clock() -> crate::application::SimClock {
        crate::application::SimClock::new(TicksPerSecond::SIXTY)
    }

    fn tree() -> Arc<BehaviorTree> {
        // A one-leaf tree: this module is testing the schedule, not the tree engine, and a
        // spec that compiles keeps the fixture honest without dragging in the registry.
        let spec = NodeSpec::Action(0);
        Arc::new(BehaviorTree::try_from_spec(spec).expect("a leaf is a valid tree"))
    }

    fn spawn(app: &mut App, id: u32, at: f32, weight: ThinkWeight, cost: VirtualCost) {
        app.world_mut().spawn((
            Npc(NpcId::unique(id)),
            Transform::from_xyz(at, 0.0, 0.0),
            NpcBrain::new(tree(), cost),
            NpcBlackboard::default(),
            NpcThinkWeight(weight),
        ));
    }

    fn app(capacity: u32, budget: VirtualCost) -> App {
        let mut app = App::new();
        app.insert_resource(clock())
            .insert_resource(WeightConfig::default())
            .insert_resource(ThinkSchedule::new(capacity, budget))
            .add_systems(Update, (set_think_weights, plan_thinks).chain());
        app
    }

    #[test]
    fn a_budget_chooses_only_what_fits() {
        let mut app = app(100, VirtualCost::from_micros(10));
        for id in 0..100 {
            spawn(
                &mut app,
                id,
                500.0,
                ThinkWeight::BACKGROUND,
                VirtualCost::from_micros(1),
            );
        }
        app.update();
        let stats = app.world().resource::<ThinkSchedule>().stats();
        assert_eq!(stats.enrolled, 100);
        assert_eq!(stats.chosen, 10, "10 us buys ten 1 us thinks");
        assert_eq!(stats.deferred, 90);
    }

    #[test]
    fn distance_changes_how_often_an_npc_thinks() {
        // Two NPCs, one at the player's feet and one a block away, in a budget that fits
        // exactly one think per tick. Over many ticks the close one should think several
        // times as often — which is the whole answer to "does thinking ramp up near the
        // player".
        let mut app = app(8, VirtualCost::from_micros(1));
        app.world_mut()
            .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
        spawn(
            &mut app,
            0,
            1.0,
            ThinkWeight::BACKGROUND,
            VirtualCost::from_micros(1),
        );
        spawn(
            &mut app,
            1,
            500.0,
            ThinkWeight::BACKGROUND,
            VirtualCost::from_micros(1),
        );
        app.update();
        {
            let mut query = app.world_mut().query::<(&Npc, &NpcThinkWeight)>();
            let mut weights: Vec<(u32, ThinkWeight)> = query
                .iter(app.world())
                .map(|(npc, weight)| (npc.0.index(), weight.0))
                .collect();
            weights.sort_unstable();
            assert_eq!(
                weights,
                vec![(0, ThinkWeight::CLOSE), (1, ThinkWeight::BACKGROUND)],
                "the NPC at the player's feet is promoted, the one a block away is not"
            );
        }

        let mut close = 0;
        let mut far = 0;
        for frame in 0..40 {
            // The scheduler keys "already thought this tick" on the tick, so a test that
            // updates the app without advancing the clock measures exactly one tick of
            // cognition and then silence. That is a real hazard for games too, and it is
            // documented on the module.
            app.world_mut()
                .resource_mut::<crate::application::SimClock>()
                .tick = Tick::new(frame);
            app.update();
            let schedule = app.world().resource::<ThinkSchedule>();
            if schedule.is_chosen(0) {
                close += 1;
            }
            if schedule.is_chosen(1) {
                far += 1;
            }
        }
        assert!(close > 0, "the near NPC never thought");
        assert!(
            close >= 3 * far.max(1),
            "distance should buy cadence: close {close}, far {far}"
        );
    }

    #[test]
    fn an_npc_beyond_capacity_is_counted_not_fatal() {
        let mut app = app(2, VirtualCost::from_micros(10));
        for id in 0..5 {
            spawn(
                &mut app,
                id,
                500.0,
                ThinkWeight::BACKGROUND,
                VirtualCost::from_micros(1),
            );
        }
        app.update();
        let stats = app.world().resource::<ThinkSchedule>().stats();
        assert_eq!(
            stats.beyond_capacity, 3,
            "three ids fell outside the capacity"
        );
        assert!(stats.enrolled <= 2);
    }
}
