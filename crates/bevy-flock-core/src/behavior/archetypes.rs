//! What each archetype does all day, and what one think costs.
//!
//! An archetype is a personality preset; this module gives it a *life*. Each of
//! the five built-in archetypes composes the standard leaves into a small tree,
//! and the tree's declared cost is what the think scheduler charges when that NPC
//! thinks (ADR 0009).
//!
//! | Archetype | Tree shape | Declared cost |
//! | --- | --- | --- |
//! | Guard | alarm → flee, tired → rest, else patrol and watch | measured |
//! | Merchant | alarm → flee, hungry → eat, tired → rest, else serve and haggle | measured |
//! | Urchin | alarm → flee, hungry → browse, lonely → chatter, else travel | measured |
//! | Noble | alarm → flee, lonely → chatter, tired → rest, else browse | measured |
//! | Wanderer | tired → rest, hungry → eat, else wander and travel | measured |
//!
//! The point of the table is not that these are the best possible trees. It is
//! that a crowd of ten thousand has a *known* per-think cost before it runs, so
//! the budget can be sized to the crowd instead of the crowd to the frame.
//!
//! Costs are looked up through a closure rather than baked in, so a game that
//! replaces a leaf with a more expensive one gets an honest number:
//!
//! ```
//! # use bevy_flock_core::behavior::FlockRegistry;
//! # use bevy_flock_core::domain::cost::VirtualCost;
//! # use bevy_flock_core::domain::temperament::Archetype;
//! let mut registry = FlockRegistry::new();
//! let leaves = registry.register_standard_leaves().expect("fresh registry");
//! let guard = Archetype::Guard
//!     .logic(&leaves, |id| registry.leaf_cost(id))
//!     .expect("built-in trees are valid");
//! assert!(guard.cost() > VirtualCost::ZERO);
//! ```

use crate::behavior::leaves::StandardLeaves;
use crate::behavior::tree::{
    BehaviorTree, LeafId, NodeSpec, TreeError, action, condition, selector, sequence,
};
use crate::domain::cost::VirtualCost;
use crate::domain::temperament::Archetype;

/// An archetype's behaviour tree and the cost one think of it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchetypeLogic {
    archetype: Archetype,
    tree: BehaviorTree,
    cost: VirtualCost,
}

impl ArchetypeLogic {
    /// Which archetype this describes.
    #[must_use]
    pub const fn archetype(&self) -> Archetype {
        self.archetype
    }

    /// The compiled tree.
    #[must_use]
    pub const fn tree(&self) -> &BehaviorTree {
        &self.tree
    }

    /// The declared cost of one think, in virtual nanoseconds.
    #[must_use]
    pub const fn cost(&self) -> VirtualCost {
        self.cost
    }

    /// Take the tree, for handing to a brain.
    #[must_use]
    pub fn into_tree(self) -> BehaviorTree {
        self.tree
    }
}

impl Archetype {
    /// Compose this archetype's behaviour tree from the standard leaves.
    ///
    /// The cost closure is how a leaf's declared cost reaches the tree; pass
    /// `|id| registry.leaf_cost(id)` for a registry holding the standard
    /// catalogue.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] only if a built-in tree is malformed, which is a bug
    /// in this module rather than in the caller's configuration. It is a `Result`
    /// anyway: the crate does not panic, and a caller deserves the chance to fall
    /// back instead of crashing a game.
    pub fn logic(
        self,
        leaves: &StandardLeaves,
        cost_of: impl Fn(LeafId) -> VirtualCost,
    ) -> Result<ArchetypeLogic, TreeError> {
        let tree = BehaviorTree::try_from_spec(&self.spec(leaves))?.named(self.as_str());
        let cost = tree.declared_cost(&cost_of);
        Ok(ArchetypeLogic {
            archetype: self,
            tree,
            cost,
        })
    }

    /// The authored shape, before compilation.
    ///
    /// Ordered by priority, because a selector takes the first success: being
    /// alarmed outranks being tired, which outranks being hungry, which outranks
    /// the day job. That ordering is the whole design of an archetype's behaviour.
    #[must_use]
    pub fn spec(self, leaves: &StandardLeaves) -> NodeSpec {
        let flee = sequence(vec![condition(leaves.is_alarmed), action(leaves.flee)]);
        match self {
            Self::Guard => selector(vec![
                flee,
                sequence(vec![condition(leaves.is_tired), action(leaves.rest)]),
                sequence(vec![action(leaves.patrol), action(leaves.watch)]),
            ]),
            Self::Merchant => selector(vec![
                flee,
                sequence(vec![condition(leaves.is_hungry), action(leaves.eat)]),
                sequence(vec![condition(leaves.is_tired), action(leaves.rest)]),
                sequence(vec![action(leaves.serve), action(leaves.trade)]),
                action(leaves.idle),
            ]),
            Self::Urchin => selector(vec![
                flee,
                sequence(vec![condition(leaves.is_hungry), action(leaves.browse)]),
                sequence(vec![condition(leaves.is_lonely), action(leaves.chatter)]),
                action(leaves.travel),
            ]),
            Self::Noble => selector(vec![
                flee,
                sequence(vec![condition(leaves.is_lonely), action(leaves.chatter)]),
                sequence(vec![condition(leaves.is_tired), action(leaves.rest)]),
                action(leaves.browse),
            ]),
            Self::Wanderer => selector(vec![
                sequence(vec![condition(leaves.is_tired), action(leaves.rest)]),
                sequence(vec![condition(leaves.is_hungry), action(leaves.eat)]),
                sequence(vec![action(leaves.wander), action(leaves.travel)]),
                action(leaves.idle),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::blackboard::Blackboard;
    use crate::behavior::leaves::{
        KEY_FLEE_REMAINING, KEY_TRAVEL_REMAINING, KEY_WORK_REMAINING, write_alarm, write_perception,
    };
    use crate::behavior::registry::FlockRegistry;
    use crate::behavior::tree::{LeafContext, Status, TreeCursor};
    use crate::domain::ids::NpcId;
    use crate::domain::temperament::Needs;
    use crate::domain::tick::Tick;
    use crate::testkit::OrDie;

    fn setup() -> (FlockRegistry, StandardLeaves) {
        let mut registry = FlockRegistry::new();
        let leaves = registry.register_standard_leaves().or_die();
        (registry, leaves)
    }

    fn logic(archetype: Archetype) -> ArchetypeLogic {
        let (registry, leaves) = setup();
        archetype
            .logic(&leaves, |id| registry.leaf_cost(id))
            .or_die()
    }

    const CONTENT: Needs = Needs {
        energy: 1.0,
        satiety: 1.0,
        company: 1.0,
        safety: 1.0,
    };

    #[test]
    fn every_archetype_compiles_with_a_positive_cost() {
        for archetype in Archetype::ALL {
            let logic = logic(archetype);
            assert!(
                logic.cost() > VirtualCost::ZERO,
                "{archetype} declares no cost"
            );
            assert!(
                logic.tree().depth() <= 6,
                "{archetype} is deeper than its design"
            );
            assert_eq!(logic.tree().name(), archetype.as_str());
        }
    }

    #[test]
    fn declared_cost_is_the_sum_of_distinct_leaves() {
        let (registry, leaves) = setup();
        let guard = Archetype::Guard
            .logic(&leaves, |id| registry.leaf_cost(id))
            .or_die();
        let expected = [
            leaves.is_alarmed,
            leaves.flee,
            leaves.is_tired,
            leaves.rest,
            leaves.patrol,
            leaves.watch,
        ]
        .into_iter()
        .map(|id| registry.leaf_cost(id))
        .fold(VirtualCost::ZERO, VirtualCost::saturating_add);
        assert_eq!(guard.cost(), expected);
    }

    #[test]
    fn a_guard_patrols_when_nothing_is_wrong() {
        let (mut registry, leaves) = setup();
        let guard = Archetype::Guard
            .logic(&leaves, |id| registry.leaf_cost(id))
            .or_die();
        let mut dispatch = registry.dispatcher();
        let mut board = Blackboard::new();
        write_perception(&mut board, &CONTENT);
        let mut cursor = TreeCursor::new();
        let mut ctx = LeafContext {
            blackboard: &mut board,
            tick: Tick::ZERO,
            npc: NpcId::unique(1),
        };
        assert_eq!(
            guard.tree().tick(&mut cursor, &mut dispatch, &mut ctx),
            Status::Running,
            "patrolling takes ticks"
        );
        assert!(
            board.get(KEY_TRAVEL_REMAINING).is_some(),
            "the patrol counter started"
        );
    }

    #[test]
    fn an_alarmed_npc_flees_instead_of_working_its_post() {
        let (mut registry, leaves) = setup();
        let guard = Archetype::Guard
            .logic(&leaves, |id| registry.leaf_cost(id))
            .or_die();
        let mut dispatch = registry.dispatcher();
        let mut board = Blackboard::new();
        write_perception(&mut board, &CONTENT);
        write_alarm(&mut board, true);
        let mut cursor = TreeCursor::new();
        let mut ctx = LeafContext {
            blackboard: &mut board,
            tick: Tick::ZERO,
            npc: NpcId::unique(1),
        };
        assert_eq!(
            guard.tree().tick(&mut cursor, &mut dispatch, &mut ctx),
            Status::Running
        );
        assert!(
            board.get(KEY_FLEE_REMAINING).is_some(),
            "the flee counter is what a frightened NPC writes"
        );
        assert!(
            board.get(KEY_TRAVEL_REMAINING).is_none(),
            "and its patrol never started"
        );
    }

    #[test]
    fn a_tired_guard_rests_before_patrolling() {
        let (mut registry, leaves) = setup();
        let guard = Archetype::Guard
            .logic(&leaves, |id| registry.leaf_cost(id))
            .or_die();
        let mut dispatch = registry.dispatcher();
        let mut board = Blackboard::new();
        write_perception(
            &mut board,
            &Needs {
                energy: 0.05,
                ..CONTENT
            },
        );
        let mut cursor = TreeCursor::new();
        let mut ctx = LeafContext {
            blackboard: &mut board,
            tick: Tick::ZERO,
            npc: NpcId::unique(1),
        };
        let _ = guard.tree().tick(&mut cursor, &mut dispatch, &mut ctx);
        assert!(
            board.get(KEY_WORK_REMAINING).is_some(),
            "resting is what a tired guard starts"
        );
        assert!(board.get(KEY_TRAVEL_REMAINING).is_none());
    }

    #[test]
    fn every_tree_gates_on_a_predicate() {
        let (_, leaves) = setup();
        let predicates = [
            leaves.is_alarmed,
            leaves.is_tired,
            leaves.is_hungry,
            leaves.is_lonely,
        ];
        for archetype in Archetype::ALL {
            let spec = archetype.spec(&leaves);
            let text = format!("{spec:?}");
            assert!(
                predicates
                    .iter()
                    .any(|id| text.contains(&format!("Condition({id})"))),
                "{archetype} never checks a condition"
            );
        }
    }

    #[test]
    fn the_declared_cost_table_is_pinned() {
        // Changing a leaf's cost changes these numbers, and a test that has to be
        // updated deliberately is the point: the scheduler's fairness weights are
        // these integers, so they should never move by accident.
        let expected = [
            (Archetype::Guard, 27),
            (Archetype::Merchant, 46),
            (Archetype::Urchin, 36),
            (Archetype::Noble, 35),
            (Archetype::Wanderer, 35),
        ];
        for (archetype, cost) in expected {
            assert_eq!(
                logic(archetype).cost(),
                VirtualCost::new(cost),
                "{archetype} declared cost changed"
            );
        }
    }

    #[test]
    fn the_cheapest_archetype_is_the_one_that_does_least() {
        // A design guard, not a performance claim: the urchin's day is short, the
        // merchant's involves two multi-tick leaves and must cost more.
        let urchin = logic(Archetype::Urchin).cost();
        let merchant = logic(Archetype::Merchant).cost();
        assert!(merchant > urchin, "{merchant} should exceed {urchin}");
    }

    #[test]
    fn the_whole_crowd_has_a_known_cost() {
        // What a game multiplies by its population to size the think budget before
        // the first frame runs. Printed so a cost change is visible in CI output.
        let mut total = VirtualCost::ZERO;
        for archetype in Archetype::ALL {
            let cost = logic(archetype).cost();
            println!("{archetype}: {cost}");
            total = total.saturating_add(cost);
        }
        println!("one of each: {total}");
        assert!(total > VirtualCost::new(100), "{total}");
    }
}
