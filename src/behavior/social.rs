//! Leaves that turn "there is a person over there" into behaviour.
//!
//! Three leaves, all reading the keys the perception system writes. They are the
//! smallest possible bridge between a crowd and a player character, and they are
//! what makes a town feel like it has seen you: a greeting, a refusal to come
//! closer, and a warning passed on.
//!
//! Costs are declared and calibrated against measurement like every other built-in
//! leaf (ADR 0009, ADR 0017). All three read at most two blackboard slots and write
//! at most one, which puts them at the cheap end of the scale — the number matters
//! because a greeting that fires for ten thousand NPCs a second is not free just
//! because it is small.

use crate::behavior::blackboard::BlackboardKey;
use crate::behavior::registry::LeafBehavior;
use crate::behavior::tree::{LeafContext, Status};
use crate::domain::cost::VirtualCost;

/// Set when the NPC has decided to speak to the person it noticed.
pub const KEY_TALK_TO: BlackboardKey = BlackboardKey::new(30);

/// Set when a player character is within notice range.
///
/// The perception adapter writes this; these leaves read it. The constant lives here rather than beside the
/// system that writes it because a behaviour must not depend on an adapter — the arrow points from `adapters` to
/// `behavior`, and a leaf that imported `crate::adapters::primary::perception` to learn its own inputs inverted it.
/// A self-contained consumer of this crate can therefore implement its own perception and keep the same contract,
/// because the contract is a number in the core, not a function of the engine.
pub const KEY_PLAYER_SEEN: BlackboardKey = BlackboardKey::new(20);

/// The distance to the nearest noticed player, in metres. Absent when nobody is in
/// range, which is different from zero.
pub const KEY_PLAYER_DISTANCE: BlackboardKey = BlackboardKey::new(21);

/// Set when the NPC has decided the noticed person is a threat.
pub const KEY_WARY_OF: BlackboardKey = BlackboardKey::new(31);

/// The distance, in metres, inside which a wary NPC backs away.
pub const PERSONAL_SPACE: f32 = 3.0;

/// Greet a player character that has been noticed.
///
/// Succeeds once and sets [`KEY_TALK_TO`], which a bark emitter can watch. Fails
/// immediately when nobody has been noticed, so it costs a single slot read in the
/// common case — which is the case for everyone in the crowd who is not looking at
/// the player.
#[derive(Debug, Clone, Copy, Default)]
pub struct GreetKnown;

impl LeafBehavior for GreetKnown {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        if !ctx.flag(KEY_PLAYER_SEEN) {
            return Status::Failure;
        }
        let _ = ctx
            .blackboard
            .set(KEY_TALK_TO, crate::behavior::blackboard::Value::Bool(true));
        Status::Success
    }

    fn abort(&self, ctx: &mut LeafContext<'_>) {
        let _ = ctx.blackboard.remove(KEY_TALK_TO);
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(3)
    }

    fn label(&self) -> &'static str {
        "greet_known"
    }
}

/// Back away from a noticed person who is too close.
///
/// Succeeds when the NPC is inside [`PERSONAL_SPACE`], which the caller turns into a
/// retreat target; fails otherwise. Deliberately not a flee: backing off is what a
/// shopkeeper does, and a crowd where everyone flees a customer reads as broken.
#[derive(Debug, Clone, Copy, Default)]
pub struct AvoidKnown;

impl LeafBehavior for AvoidKnown {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        if !ctx.flag(KEY_PLAYER_SEEN) {
            return Status::Failure;
        }
        let Some(distance) = ctx.number(KEY_PLAYER_DISTANCE) else {
            return Status::Failure;
        };
        if distance >= PERSONAL_SPACE {
            return Status::Failure;
        }
        let _ = ctx
            .blackboard
            .set(KEY_WARY_OF, crate::behavior::blackboard::Value::Bool(true));
        Status::Success
    }

    fn abort(&self, ctx: &mut LeafContext<'_>) {
        let _ = ctx.blackboard.remove(KEY_WARY_OF);
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(4)
    }

    fn label(&self) -> &'static str {
        "avoid_known"
    }
}

/// Whether the NPC is mid-greeting. A condition, not a leaf: useful as a guard on a
/// state that should not be re-entered while a line is still being said.
#[derive(Debug, Clone, Copy, Default)]
pub struct GreetingGuard;

impl GreetingGuard {
    /// Whether the blackboard says a greeting is in progress.
    #[must_use]
    pub fn holds(blackboard: &crate::behavior::blackboard::Blackboard) -> bool {
        blackboard
            .get(KEY_TALK_TO)
            .and_then(crate::behavior::blackboard::Value::as_bool)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{KEY_PLAYER_DISTANCE, KEY_PLAYER_SEEN};
    use crate::behavior::blackboard::{Blackboard, Value};
    use crate::domain::ids::NpcId;
    use crate::domain::tick::Tick;

    fn context(blackboard: &mut Blackboard) -> LeafContext<'_> {
        LeafContext {
            blackboard,
            tick: Tick::ZERO,
            npc: NpcId::unique(1),
        }
    }

    #[test]
    fn greeting_needs_someone_to_greet() {
        let mut blackboard = Blackboard::new();
        let mut ctx = context(&mut blackboard);
        assert_eq!(GreetKnown.tick(&mut ctx), Status::Failure);
        assert!(!ctx.flag(KEY_TALK_TO));
    }

    #[test]
    fn greeting_sets_the_talk_key_when_a_player_is_seen() {
        let mut blackboard = Blackboard::new();
        let _ = blackboard.set(KEY_PLAYER_SEEN, Value::Bool(true));
        let mut ctx = context(&mut blackboard);
        assert_eq!(GreetKnown.tick(&mut ctx), Status::Success);
        assert!(ctx.flag(KEY_TALK_TO));
        assert_eq!(GreetKnown.cost_hint(), VirtualCost::new(3));
        assert_eq!(GreetKnown.label(), "greet_known");
    }

    #[test]
    fn aborting_a_greeting_releases_the_key() {
        let mut blackboard = Blackboard::new();
        let _ = blackboard.set(KEY_PLAYER_SEEN, Value::Bool(true));
        {
            let mut ctx = context(&mut blackboard);
            let _ = GreetKnown.tick(&mut ctx);
        }
        {
            let mut ctx = context(&mut blackboard);
            GreetKnown.abort(&mut ctx);
            assert!(!ctx.flag(KEY_TALK_TO));
        }
    }

    #[test]
    fn avoidance_respects_personal_space() {
        let mut blackboard = Blackboard::new();
        let _ = blackboard.set(KEY_PLAYER_SEEN, Value::Bool(true));
        let _ = blackboard.set(KEY_PLAYER_DISTANCE, Value::Float(PERSONAL_SPACE + 1.0));
        {
            let mut ctx = context(&mut blackboard);
            assert_eq!(AvoidKnown.tick(&mut ctx), Status::Failure);
        }
        let _ = blackboard.set(KEY_PLAYER_DISTANCE, Value::Float(1.0));
        let mut ctx = context(&mut blackboard);
        assert_eq!(AvoidKnown.tick(&mut ctx), Status::Success);
        assert!(ctx.flag(KEY_WARY_OF));
    }

    #[test]
    fn avoidance_needs_a_distance_not_just_a_sighting() {
        let mut blackboard = Blackboard::new();
        let _ = blackboard.set(KEY_PLAYER_SEEN, Value::Bool(true));
        let mut ctx = context(&mut blackboard);
        assert_eq!(AvoidKnown.tick(&mut ctx), Status::Failure);
    }

    #[test]
    fn the_guard_reads_what_the_greeting_wrote() {
        let mut blackboard = Blackboard::new();
        assert!(!GreetingGuard::holds(&blackboard));
        let _ = blackboard.set(KEY_TALK_TO, Value::Bool(true));
        assert!(GreetingGuard::holds(&blackboard));
    }
}
