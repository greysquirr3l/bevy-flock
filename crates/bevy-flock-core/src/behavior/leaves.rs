//! The built-in leaf catalogue, and the cost each leaf declares.
//!
//! Two rules shape every leaf here, and they are the rules a game's own leaves
//! should follow:
//!
//! 1. **A leaf holds no per-NPC state.** One instance is registered and shared by
//!    every NPC running it, so `tick` takes `&self` and all working memory lives on
//!    the [`Blackboard`]. A leaf with a `remaining: u32` field would have every NPC
//!    in the crowd sharing one countdown.
//! 2. **A leaf declares its cost.** [`crate::domain::cost::VirtualCost`] is what the
//!    think scheduler spends (ADR 0009). The numbers below are measured on the
//!    reference machine and pinned by `benches/brain.rs`; the calibration gate
//!    fails the build when a leaf drifts more than 20% from its declaration.
//!
//! The declared numbers are small integers because a leaf really is that cheap:
//! a predicate is about two nanoseconds and a countdown about seven, measured
//! with `cargo bench --bench brain`. A declaration that said "100" for every leaf
//! would be a guess dressed as data, and the scheduler's fairness arithmetic would
//! be built on it.
//!
//! A leaf that needs to know about the world — is it hungry, is the alarm up —
//! reads it from the blackboard, where a perception system wrote it this tick. That
//! keeps leaves pure and makes their cost measurable.

use crate::behavior::blackboard::{Blackboard, BlackboardKey, Value};
use crate::behavior::registry::{FlockRegistry, LeafBehavior};
use crate::behavior::tree::{LeafContext, LeafId, Status};
use crate::domain::cost::VirtualCost;
use crate::domain::temperament::Needs;
use crate::error::RegistrationError;

/// Energy below this fraction of maximum counts as tired.
pub const TIRED_THRESHOLD: f32 = 0.3;
/// Satiety below this fraction counts as hungry.
pub const HUNGRY_THRESHOLD: f32 = 0.3;
/// Company below this fraction counts as lonely.
pub const LONELY_THRESHOLD: f32 = 0.3;

/// Perception: the NPC's energy, 0.0 to 1.0.
pub const KEY_ENERGY: BlackboardKey = BlackboardKey::new(1);
/// Perception: the NPC's satiety, 0.0 to 1.0.
pub const KEY_SATIETY: BlackboardKey = BlackboardKey::new(2);
/// Perception: the NPC's company, 0.0 to 1.0.
pub const KEY_COMPANY: BlackboardKey = BlackboardKey::new(3);
/// Perception: the NPC's safety, 0.0 to 1.0.
pub const KEY_SAFETY: BlackboardKey = BlackboardKey::new(4);
/// Perception: whether something has alarmed this NPC.
pub const KEY_ALARM: BlackboardKey = BlackboardKey::new(5);
/// Working state: ticks left in the current piece of work.
pub const KEY_WORK_REMAINING: BlackboardKey = BlackboardKey::new(6);
/// Working state: ticks left travelling.
pub const KEY_TRAVEL_REMAINING: BlackboardKey = BlackboardKey::new(7);
/// Working state: ticks left in the current conversation.
pub const KEY_TALK_REMAINING: BlackboardKey = BlackboardKey::new(8);
/// Working state: ticks left running away.
///
/// Separate from [`KEY_TRAVEL_REMAINING`] on purpose: "is this NPC fleeing or
/// patrolling" is the first question anyone asks of a crowd bug, and a shared
/// counter cannot answer it.
pub const KEY_FLEE_REMAINING: BlackboardKey = BlackboardKey::new(9);

/// Write the current need levels into the blackboard.
///
/// This is the perception step: leaves never read components, so whatever a game
/// wants them to react to is copied here first. Deterministic by construction —
/// four `f32`s in, four `f32`s out.
pub fn write_perception(board: &mut Blackboard, needs: &Needs) {
    board.set(KEY_ENERGY, Value::Float(needs.energy));
    board.set(KEY_SATIETY, Value::Float(needs.satiety));
    board.set(KEY_COMPANY, Value::Float(needs.company));
    board.set(KEY_SAFETY, Value::Float(needs.safety));
}

/// Set or clear the alarm flag.
pub fn write_alarm(board: &mut Blackboard, alarmed: bool) {
    board.set(KEY_ALARM, Value::Bool(alarmed));
}

/// Count down a work counter, succeeding when it reaches zero.
///
/// The counter lives on the blackboard precisely so that the leaf, which is shared
/// by the whole crowd, stays stateless.
fn countdown(ctx: &mut LeafContext<'_>, key: BlackboardKey, ticks: u32) -> Status {
    let remaining = ctx
        .blackboard
        .get(key)
        .and_then(Value::as_int)
        .unwrap_or_else(|| i64::from(ticks));
    if remaining <= 0 {
        ctx.blackboard.remove(key);
        return Status::Success;
    }
    ctx.blackboard.set(key, Value::Int(remaining - 1));
    Status::Running
}

/// Below a threshold written by perception.
fn below(ctx: &LeafContext<'_>, key: BlackboardKey, threshold: f32) -> Status {
    match ctx.number(key) {
        // Unknown is not the same as satisfied: a perception step that has not run
        // yet must not make every NPC act hungry.
        Some(value) if value < threshold => Status::Success,
        _ => Status::Failure,
    }
}

/// Stand still and do nothing, successfully.
#[derive(Debug, Clone, Copy, Default)]
pub struct Idle;

impl LeafBehavior for Idle {
    fn tick(&self, _ctx: &mut LeafContext<'_>) -> Status {
        Status::Success
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(2)
    }

    fn label(&self) -> &'static str {
        "idle"
    }
}

/// Walk aimlessly for a while.
#[derive(Debug, Clone, Copy, Default)]
pub struct Wander;

impl LeafBehavior for Wander {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_WORK_REMAINING, 48)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(7)
    }

    fn label(&self) -> &'static str {
        "wander"
    }
}

/// Walk toward the current target.
#[derive(Debug, Clone, Copy, Default)]
pub struct Travel;

impl LeafBehavior for Travel {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_TRAVEL_REMAINING, 32)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(8)
    }

    fn label(&self) -> &'static str {
        "travel"
    }
}

/// Walk a patrol route.
#[derive(Debug, Clone, Copy, Default)]
pub struct Patrol;

impl LeafBehavior for Patrol {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_TRAVEL_REMAINING, 64)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(7)
    }

    fn label(&self) -> &'static str {
        "patrol"
    }
}

/// Scan for trouble. Cheap, and always finishes in one tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct Watch;

impl LeafBehavior for Watch {
    fn tick(&self, _ctx: &mut LeafContext<'_>) -> Status {
        Status::Success
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(2)
    }

    fn label(&self) -> &'static str {
        "watch"
    }
}

/// Serve a customer at a stall.
#[derive(Debug, Clone, Copy, Default)]
pub struct Serve;

impl LeafBehavior for Serve {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_WORK_REMAINING, 24)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(8)
    }

    fn label(&self) -> &'static str {
        "serve"
    }
}

/// Haggle over a price.
#[derive(Debug, Clone, Copy, Default)]
pub struct Trade;

impl LeafBehavior for Trade {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_TALK_REMAINING, 12)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(9)
    }

    fn label(&self) -> &'static str {
        "trade"
    }
}

/// Look over goods without buying anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct Browse;

impl LeafBehavior for Browse {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_WORK_REMAINING, 20)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(7)
    }

    fn label(&self) -> &'static str {
        "browse"
    }
}

/// Talk to whoever is nearby.
#[derive(Debug, Clone, Copy, Default)]
pub struct Chatter;

impl LeafBehavior for Chatter {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_TALK_REMAINING, 16)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(8)
    }

    fn label(&self) -> &'static str {
        "chatter"
    }
}

/// Run away from whatever alarmed the NPC.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flee;

impl LeafBehavior for Flee {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_FLEE_REMAINING, 40)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(7)
    }

    fn label(&self) -> &'static str {
        "flee"
    }
}

/// Recover energy.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rest;

impl LeafBehavior for Rest {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_WORK_REMAINING, 96)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(7)
    }

    fn label(&self) -> &'static str {
        "rest"
    }
}

/// Eat something.
#[derive(Debug, Clone, Copy, Default)]
pub struct Eat;

impl LeafBehavior for Eat {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        countdown(ctx, KEY_WORK_REMAINING, 30)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(7)
    }

    fn label(&self) -> &'static str {
        "eat"
    }
}

/// Whether the alarm is up.
#[derive(Debug, Clone, Copy, Default)]
pub struct IsAlarmed;

impl LeafBehavior for IsAlarmed {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        if ctx.flag(KEY_ALARM) {
            Status::Success
        } else {
            Status::Failure
        }
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(2)
    }

    fn label(&self) -> &'static str {
        "is-alarmed"
    }
}

/// Whether the NPC needs rest.
#[derive(Debug, Clone, Copy, Default)]
pub struct IsTired;

impl LeafBehavior for IsTired {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        below(ctx, KEY_ENERGY, TIRED_THRESHOLD)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(2)
    }

    fn label(&self) -> &'static str {
        "is-tired"
    }
}

/// Whether the NPC needs food.
#[derive(Debug, Clone, Copy, Default)]
pub struct IsHungry;

impl LeafBehavior for IsHungry {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        below(ctx, KEY_SATIETY, HUNGRY_THRESHOLD)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(2)
    }

    fn label(&self) -> &'static str {
        "is-hungry"
    }
}

/// Whether the NPC wants company.
#[derive(Debug, Clone, Copy, Default)]
pub struct IsLonely;

impl LeafBehavior for IsLonely {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        below(ctx, KEY_COMPANY, LONELY_THRESHOLD)
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(2)
    }

    fn label(&self) -> &'static str {
        "is-lonely"
    }
}

/// The identifiers of every built-in leaf, once registered.
///
/// A struct of named fields rather than a map: the archetype trees are written
/// against these names, and a missing field should be a compile error rather than
/// a runtime lookup that returns `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandardLeaves {
    /// Do nothing.
    pub idle: LeafId,
    /// Walk aimlessly.
    pub wander: LeafId,
    /// Walk to the current target.
    pub travel: LeafId,
    /// Walk a patrol route.
    pub patrol: LeafId,
    /// Scan for trouble.
    pub watch: LeafId,
    /// Serve a customer.
    pub serve: LeafId,
    /// Haggle.
    pub trade: LeafId,
    /// Browse goods.
    pub browse: LeafId,
    /// Talk.
    pub chatter: LeafId,
    /// Run away.
    pub flee: LeafId,
    /// Recover energy.
    pub rest: LeafId,
    /// Eat.
    pub eat: LeafId,
    /// Is the alarm up.
    pub is_alarmed: LeafId,
    /// Needs rest.
    pub is_tired: LeafId,
    /// Needs food.
    pub is_hungry: LeafId,
    /// Wants company.
    pub is_lonely: LeafId,
}

impl FlockRegistry {
    /// Register every built-in leaf and return their identifiers.
    ///
    /// Registration order is fixed, so the identifiers are stable across runs,
    /// platforms and rebuilds. A game that wants to replace one of these can
    /// register its own under a different name and never call this.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError::Duplicate`] if any standard name is already
    /// taken, which means the caller registered the catalogue twice.
    pub fn register_standard_leaves(&mut self) -> Result<StandardLeaves, RegistrationError> {
        Ok(StandardLeaves {
            idle: self.register_leaf("idle", Idle)?,
            wander: self.register_leaf("wander", Wander)?,
            travel: self.register_leaf("travel", Travel)?,
            patrol: self.register_leaf("patrol", Patrol)?,
            watch: self.register_leaf("watch", Watch)?,
            serve: self.register_leaf("serve", Serve)?,
            trade: self.register_leaf("trade", Trade)?,
            browse: self.register_leaf("browse", Browse)?,
            chatter: self.register_leaf("chatter", Chatter)?,
            flee: self.register_leaf("flee", Flee)?,
            rest: self.register_leaf("rest", Rest)?,
            eat: self.register_leaf("eat", Eat)?,
            is_alarmed: self.register_leaf("is-alarmed", IsAlarmed)?,
            is_tired: self.register_leaf("is-tired", IsTired)?,
            is_hungry: self.register_leaf("is-hungry", IsHungry)?,
            is_lonely: self.register_leaf("is-lonely", IsLonely)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::NpcId;
    use crate::domain::tick::Tick;
    use crate::testkit::OrDie;

    fn with_board() -> Blackboard {
        Blackboard::new()
    }

    fn ctx(board: &mut Blackboard) -> LeafContext<'_> {
        LeafContext {
            blackboard: board,
            tick: Tick::ZERO,
            npc: NpcId::unique(0),
        }
    }

    fn needs(energy: f32, satiety: f32, company: f32) -> Needs {
        Needs {
            energy,
            satiety,
            company,
            safety: 1.0,
        }
    }

    #[test]
    fn work_leaves_run_for_their_declared_duration_then_succeed() {
        let mut board = with_board();
        let mut ctx = ctx(&mut board);
        let mut ticks = 0;
        while Wander.tick(&mut ctx).is_running() {
            ticks += 1;
            assert!(ticks < 1_000, "a countdown must terminate");
        }
        assert_eq!(ticks, 48);
        assert!(
            board.get(KEY_WORK_REMAINING).is_none(),
            "the counter is cleaned up on completion"
        );
    }

    #[test]
    fn a_second_run_starts_from_a_fresh_counter() {
        let mut board = with_board();
        let mut ctx = ctx(&mut board);
        for _ in 0..48 {
            let _ = Wander.tick(&mut ctx);
        }
        assert_eq!(Wander.tick(&mut ctx), Status::Success);
        assert_eq!(
            Wander.tick(&mut ctx),
            Status::Running,
            "the leaf restarts, because the shared leaf holds no state"
        );
    }

    #[test]
    fn every_work_leaf_terminates_and_declares_a_cost() {
        let leaves: [&dyn LeafBehavior; 10] = [
            &Wander, &Travel, &Patrol, &Serve, &Trade, &Browse, &Chatter, &Flee, &Rest, &Eat,
        ];
        for leaf in leaves {
            let mut board = with_board();
            let mut ctx = ctx(&mut board);
            let mut ticks = 0;
            while leaf.tick(&mut ctx).is_running() {
                ticks += 1;
                assert!(ticks < 1_000, "{} did not terminate", leaf.label());
            }
            assert!(
                leaf.cost_hint().get() > 0,
                "{} declares no cost",
                leaf.label()
            );
        }
    }

    #[test]
    fn conditions_gate_on_perception() {
        let mut board = with_board();
        write_perception(&mut board, &needs(0.1, 0.9, 0.9));
        let mut ctx = ctx(&mut board);
        assert_eq!(IsTired.tick(&mut ctx), Status::Success);
        assert_eq!(IsHungry.tick(&mut ctx), Status::Failure);
        assert_eq!(IsLonely.tick(&mut ctx), Status::Failure);
    }

    #[test]
    fn unknown_perception_does_not_read_as_hungry() {
        let mut board = with_board();
        let mut ctx = ctx(&mut board);
        assert_eq!(IsHungry.tick(&mut ctx), Status::Failure);
    }

    #[test]
    fn alarm_is_a_flag_not_a_number() {
        let mut board = with_board();
        write_alarm(&mut board, true);
        let mut armed = ctx(&mut board);
        assert_eq!(IsAlarmed.tick(&mut armed), Status::Success);

        let mut fresh = with_board();
        write_perception(&mut fresh, &needs(1.0, 1.0, 1.0));
        let mut after = ctx(&mut fresh);
        assert_eq!(
            IsAlarmed.tick(&mut after),
            Status::Failure,
            "perception did not touch the alarm"
        );
    }

    #[test]
    fn the_catalogue_registers_once_and_is_stable() {
        let mut registry = FlockRegistry::new();
        let leaves = registry.register_standard_leaves().or_die();
        assert_eq!(leaves.idle, 0);
        assert_eq!(leaves.wander, 1);
        assert_eq!(leaves.is_lonely, 15);
        assert_eq!(registry.leaf_count(), 16);
        assert!(
            registry.register_standard_leaves().is_err(),
            "a second registration is a caller bug, not a silent duplicate"
        );
    }

    #[test]
    fn declared_costs_rank_the_cheap_and_expensive_leaves_sensibly() {
        // A sanity guard on the table rather than on exact numbers: predicates
        // must be cheapest, social and commercial leaves the most expensive.
        let mut registry = FlockRegistry::new();
        let leaves = registry.register_standard_leaves().or_die();
        let cost = |id| registry.leaf_cost(id);
        assert!(
            cost(leaves.is_tired) <= cost(leaves.idle),
            "a predicate is cheapest"
        );
        assert!(
            cost(leaves.trade) > cost(leaves.travel),
            "haggling costs more than walking"
        );
        assert!(
            cost(leaves.chatter) > cost(leaves.watch),
            "talking costs more than looking"
        );
        assert_eq!(
            cost(leaves.watch),
            cost(leaves.idle),
            "looking costs what idling costs"
        );
    }
}
