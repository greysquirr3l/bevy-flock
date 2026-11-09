//! The extension surface.
//!
//! Everything a game can plug in lives behind a trait here, registered into one
//! [`FlockRegistry`]. The traits are deliberately small and object-safe: a game
//! registers `Box<dyn LeafBehavior>`, not a generic monomorphisation, so the
//! registry can hold behaviours written in another crate without the crate
//! knowing their types.
//!
//! Duplicate identifiers are rejected at registration time, which is a
//! build-time failure in practice: a plugin that registers twice fails the first
//! `app.update()`, not the ten-thousandth NPC.

use std::collections::BTreeMap;

use bevy_math::Vec3;

use crate::behavior::blackboard::Blackboard;
use crate::behavior::tree::{BehaviorTree, LeafContext, LeafDispatch, LeafId, Status};
use crate::domain::cost::VirtualCost;
use crate::domain::ids::{GroupId, NpcId};
use crate::domain::num;
use crate::domain::tick::Tick;
use crate::error::RegistrationError;

/// A game-defined leaf behaviour.
///
/// Implement this for anything an NPC can *do*: greet someone, pick up a crate,
/// play a animation and wait for it, path to a market stall. Called on the NPC's
/// think tick, which may be many frames apart at low level of detail, so
/// implementations must be able to resume from a `Running` status without
/// assuming consecutive frames.
pub trait LeafBehavior: Send + Sync {
    /// Advance one step.
    ///
    /// Takes `&self`, not `&mut self`, on purpose: one instance is registered and
    /// shared by every NPC running it, so per-NPC memory belongs on the
    /// [`Blackboard`]. A leaf that kept `remaining: u32` in a field would give the
    /// entire crowd one countdown — a bug that is invisible until two NPCs move in
    /// lockstep.
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status;

    /// Release any state the leaf is holding.
    ///
    /// Called when the NPC's behaviour is replaced, it despawns, or a `Timeout`
    /// cuts its subtree short. State here means blackboard entries: clearing them
    /// is how an NPC that was mid-conversation does not come back to a stale one.
    fn abort(&self, ctx: &mut LeafContext<'_>) {
        let _ = ctx;
    }

    /// The cost one tick of this leaf declares, in virtual nanoseconds.
    ///
    /// Charged by the think scheduler (ADR 0009) and checked against measurement
    /// by the calibration gate. The default is deliberately pessimistic so an
    /// undeclared leaf cannot pretend to be cheap.
    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::DEFAULT
    }

    /// A short label for trace output.
    fn label(&self) -> &'static str;
}

/// A group-level decision maker.
///
/// Called once per group on a fixed cadence and returns at most one directive.
/// Returning `None` means "no opinion", which is different from "hold position".
pub trait GroupDirector: Send + Sync {
    /// Decide something for the group.
    fn tick(&mut self, ctx: &DirectorContext<'_>) -> Option<Directive>;

    /// A short label for trace output.
    fn label(&self) -> &'static str;
}

/// A population-level decision maker: festivals, patrols, panics, migrations.
pub trait WorldDirector: Send + Sync {
    /// Decide something for the world. Commands are applied in the order
    /// returned, on a fixed tick, so a replay reproduces them exactly.
    fn tick(&mut self, ctx: &DirectorContext<'_>) -> Vec<Directive>;

    /// A short label for trace output.
    fn label(&self) -> &'static str;
}

/// Context handed to a director.
///
/// Read-only by construction: a director that could mutate the world directly
/// would be impossible to replay, so it decides and the scheduler applies.
#[derive(Debug, Clone, Copy)]
pub struct DirectorContext<'a> {
    /// The tick the decision is being made on.
    pub tick: Tick,
    /// The group the director is deciding for, if it is a group director.
    pub group: Option<GroupId>,
    /// Read access to the group's shared working memory.
    pub blackboard: &'a Blackboard,
    /// Live population, so a director can be density-aware.
    pub population: u32,
}

/// Something a director can order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Directive {
    /// What to do.
    pub kind: DirectiveKind,
    /// How long the order stands.
    pub ttl_ticks: u64,
}

impl Directive {
    /// A directive with a time to live.
    #[must_use]
    pub const fn new(kind: DirectiveKind, ttl_ticks: u64) -> Self {
        Self { kind, ttl_ticks }
    }

    /// Whether this directive is still in force at `issued_at + age`.
    #[must_use]
    pub const fn is_expired(&self, age_ticks: u64) -> bool {
        age_ticks >= self.ttl_ticks
    }
}

/// What a director asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DirectiveKind {
    /// Stand still.
    Hold,
    /// Move to a world position.
    MoveTo(Vec3),
    /// Break line of sight and get behind cover.
    TakeCover,
    /// Run away from a point.
    Flee(Vec3),
    /// Resume whatever the NPC would do without an order.
    Release,
}

/// A registry of every extension a game has installed.
///
/// Ordered storage rather than a hash map: iteration order feeds simulation, and
/// a hash map's order is exactly the thing that breaks a replay.
#[derive(Default)]
#[cfg_attr(feature = "ecs", derive(bevy_ecs::prelude::Resource))]
pub struct FlockRegistry {
    leaves: Vec<LeafEntry>,
    names: BTreeMap<String, u32>,
    group_directors: BTreeMap<String, Box<dyn GroupDirector>>,
    world_directors: BTreeMap<String, Box<dyn WorldDirector>>,
}

struct LeafEntry {
    name: String,
    behavior: Box<dyn LeafBehavior>,
}

impl std::fmt::Debug for FlockRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlockRegistry")
            .field("leaves", &self.leaves.len())
            .field("names", &self.names)
            .field("group_directors", &self.group_directors.len())
            .field("world_directors", &self.world_directors.len())
            .finish()
    }
}

impl FlockRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a leaf behaviour under a stable identifier.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError::Duplicate`] if `name` is already registered.
    pub fn register_leaf(
        &mut self,
        name: impl Into<String>,
        behavior: impl LeafBehavior + 'static,
    ) -> Result<LeafId, RegistrationError> {
        let name = name.into();
        if self.names.contains_key(&name) {
            return Err(RegistrationError::Duplicate {
                id: name,
                kind: "leaf behaviour",
            });
        }
        let id = num::usize_to_u32(self.leaves.len());
        self.names.insert(name.clone(), id);
        self.leaves.push(LeafEntry {
            name,
            behavior: Box::new(behavior),
        });
        Ok(id)
    }

    /// The identifier assigned to a leaf name.
    #[must_use]
    pub fn leaf_id(&self, name: &str) -> Option<LeafId> {
        self.names.get(name).copied()
    }

    /// The label a leaf reports.
    #[must_use]
    pub fn leaf_label(&self, id: LeafId) -> Option<&'static str> {
        self.leaves
            .get(id as usize)
            .map(|entry| entry.behavior.label())
    }

    /// The identifier a leaf was registered under.
    #[must_use]
    pub fn leaf_name(&self, id: LeafId) -> Option<&str> {
        self.leaves
            .get(id as usize)
            .map(|entry| entry.name.as_str())
    }

    /// How many leaves are registered.
    #[must_use]
    pub const fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    /// Register a group director.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError::Duplicate`] if the name is taken.
    pub fn register_group_director(
        &mut self,
        name: impl Into<String>,
        director: impl GroupDirector + 'static,
    ) -> Result<(), RegistrationError> {
        let name = name.into();
        if self.group_directors.contains_key(&name) {
            return Err(RegistrationError::Duplicate {
                id: name,
                kind: "group director",
            });
        }
        self.group_directors.insert(name, Box::new(director));
        Ok(())
    }

    /// Register a world director.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError::Duplicate`] if the name is taken.
    pub fn register_world_director(
        &mut self,
        name: impl Into<String>,
        director: impl WorldDirector + 'static,
    ) -> Result<(), RegistrationError> {
        let name = name.into();
        if self.world_directors.contains_key(&name) {
            return Err(RegistrationError::Duplicate {
                id: name,
                kind: "world director",
            });
        }
        self.world_directors.insert(name, Box::new(director));
        Ok(())
    }

    /// Run every group director once, in name order.
    pub fn tick_group_directors(&mut self, ctx: &DirectorContext<'_>) -> Vec<Directive> {
        self.group_directors
            .values_mut()
            .filter_map(|director| director.tick(ctx))
            .collect()
    }

    /// Run every world director once, in name order.
    pub fn tick_world_directors(&mut self, ctx: &DirectorContext<'_>) -> Vec<Directive> {
        self.world_directors
            .values_mut()
            .flat_map(|director| director.tick(ctx))
            .collect()
    }

    /// The cost one leaf declares.
    ///
    /// An unknown identifier returns [`VirtualCost::DEFAULT`]: an unregistered
    /// leaf is a wiring bug, and charging the pessimistic default keeps it from
    /// quietly getting a free share of the budget.
    #[must_use]
    pub fn leaf_cost(&self, id: LeafId) -> VirtualCost {
        self.leaves
            .get(id as usize)
            .map_or(VirtualCost::DEFAULT, |entry| entry.behavior.cost_hint())
    }

    /// Total declared cost of every registered leaf.
    ///
    /// This is the catalogue's cost, not a tree's: registering a leaf does not
    /// mean any NPC runs it. Use [`FlockRegistry::tree_cost`] for the number the
    /// scheduler actually charges.
    #[must_use]
    pub fn declared_cost(&self) -> VirtualCost {
        self.leaves
            .iter()
            .map(|entry| entry.behavior.cost_hint())
            .sum()
    }

    /// The declared cost of one think of a tree, given the leaves it runs.
    #[must_use]
    pub fn tree_cost(&self, tree: &BehaviorTree) -> VirtualCost {
        tree.declared_cost(|id| self.leaf_cost(id))
    }

    /// A dispatcher that borrows this registry for one tree evaluation.
    pub const fn dispatcher(&mut self) -> RegistryDispatch<'_> {
        RegistryDispatch { registry: self }
    }
}

/// Bridges a [`FlockRegistry`] to a compiled behaviour tree.
pub struct RegistryDispatch<'a> {
    registry: &'a mut FlockRegistry,
}

impl<'a> RegistryDispatch<'a> {
    /// A dispatcher over a game's registered leaves.
    ///
    /// Public because the ECS layer owns the registry as a resource and has to build one
    /// per tick; before the cognition systems existed nothing outside this module needed
    /// to construct one.
    #[must_use]
    pub const fn new(registry: &'a mut FlockRegistry) -> Self {
        Self { registry }
    }
}

impl LeafDispatch for RegistryDispatch<'_> {
    fn tick(&mut self, leaf: LeafId, ctx: &mut LeafContext<'_>) -> Status {
        // An unregistered leaf is a wiring bug; failing the node is the only safe
        // answer, and the label lookup will surface which one.
        self.registry
            .leaves
            .get(leaf as usize)
            .map_or(Status::Failure, |entry| entry.behavior.tick(ctx))
    }

    fn abort(&mut self, leaf: LeafId, ctx: &mut LeafContext<'_>) {
        if let Some(entry) = self.registry.leaves.get(leaf as usize) {
            entry.behavior.abort(ctx);
        }
    }
}

/// An NPC whose cognition is entirely the game's business.
///
/// Attaching this to an entity makes the built-in scheduler skip it. The context
/// is read-only plus a command buffer, so external logic cannot corrupt
/// simulation state by writing a component mid-iteration.
pub trait NpcLogic: Send + Sync {
    /// Decide what this NPC does next.
    fn tick(&mut self, ctx: &NpcLogicContext<'_>);

    /// A short label for trace output.
    fn label(&self) -> &'static str;
}

/// What external logic is allowed to see.
#[derive(Debug, Clone, Copy)]
pub struct NpcLogicContext<'a> {
    /// The NPC.
    pub npc: NpcId,
    /// The tick.
    pub tick: Tick,
    /// Read-only working memory.
    pub blackboard: &'a Blackboard,
    /// Where the NPC is.
    pub position: Vec3,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::blackboard::Blackboard;
    use crate::testkit::{OrDie, OrDieErr};

    struct Greet;

    impl LeafBehavior for Greet {
        fn tick(&self, _ctx: &mut LeafContext<'_>) -> Status {
            Status::Success
        }

        fn label(&self) -> &'static str {
            "greet"
        }
    }

    struct Shouty;

    impl LeafBehavior for Shouty {
        fn tick(&self, _ctx: &mut LeafContext<'_>) -> Status {
            Status::Success
        }

        fn cost_hint(&self) -> VirtualCost {
            VirtualCost::new(42)
        }

        fn label(&self) -> &'static str {
            "shout"
        }
    }

    struct AlwaysHold;

    impl GroupDirector for AlwaysHold {
        fn tick(&mut self, _ctx: &DirectorContext<'_>) -> Option<Directive> {
            Some(Directive::new(DirectiveKind::Hold, 10))
        }

        fn label(&self) -> &'static str {
            "always-hold"
        }
    }

    struct Festival;

    impl WorldDirector for Festival {
        fn tick(&mut self, _ctx: &DirectorContext<'_>) -> Vec<Directive> {
            vec![
                Directive::new(DirectiveKind::Hold, 5),
                Directive::new(DirectiveKind::Release, 1),
            ]
        }

        fn label(&self) -> &'static str {
            "festival"
        }
    }

    #[test]
    fn registers_leaves_and_assigns_stable_ids() {
        let mut registry = FlockRegistry::new();
        let greet = registry.register_leaf("greet", Greet).or_die();
        let shout = registry.register_leaf("shout", Shouty).or_die();
        assert_eq!(greet, 0);
        assert_eq!(shout, 1);
        assert_eq!(registry.leaf_id("greet"), Some(greet));
        assert_eq!(registry.leaf_count(), 2);
    }

    #[test]
    fn duplicate_leaf_names_are_rejected_with_the_name_in_the_error() {
        let mut registry = FlockRegistry::new();
        registry.register_leaf("greet", Greet).or_die();
        let error = registry.register_leaf("greet", Greet).or_die_err();
        assert!(error.to_string().contains("greet"), "{error}");
        assert_eq!(
            error,
            RegistrationError::Duplicate {
                id: "greet".to_owned(),
                kind: "leaf behaviour"
            }
        );
    }

    #[test]
    fn directors_are_rejected_on_duplicate_names_too() {
        let mut registry = FlockRegistry::new();
        registry
            .register_group_director("hold", AlwaysHold)
            .or_die();
        assert!(
            registry
                .register_group_director("hold", AlwaysHold)
                .is_err()
        );
        registry
            .register_world_director("festival", Festival)
            .or_die();
        assert!(
            registry
                .register_world_director("festival", Festival)
                .is_err()
        );
    }

    #[test]
    fn director_output_is_ordered_and_deterministic() {
        let mut registry = FlockRegistry::new();
        registry
            .register_world_director("festival", Festival)
            .or_die();
        let board = Blackboard::new();
        let ctx = DirectorContext {
            tick: Tick::ZERO,
            group: None,
            blackboard: &board,
            population: 10,
        };
        let first = registry.tick_world_directors(&ctx);
        let second = registry.tick_world_directors(&ctx);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
    }

    #[test]
    fn declared_cost_sums_the_hints() {
        let mut registry = FlockRegistry::new();
        let shout = registry.register_leaf("shout", Shouty).or_die();
        assert_eq!(registry.declared_cost(), VirtualCost::new(42));
        assert_eq!(registry.leaf_cost(shout), VirtualCost::new(42));
    }

    #[test]
    fn an_unknown_leaf_is_charged_the_pessimistic_default() {
        let registry = FlockRegistry::new();
        assert_eq!(registry.leaf_cost(7), VirtualCost::DEFAULT);
    }

    #[test]
    fn a_leaf_without_a_declaration_gets_the_default_not_free_ride() {
        let mut registry = FlockRegistry::new();
        let greet = registry.register_leaf("greet", Greet).or_die();
        assert_eq!(registry.leaf_cost(greet), VirtualCost::DEFAULT);
        assert!(VirtualCost::DEFAULT > VirtualCost::ZERO);
    }

    #[test]
    fn tree_cost_counts_only_the_leaves_the_tree_uses() {
        use crate::behavior::tree::{action, sequence};
        let mut registry = FlockRegistry::new();
        registry.register_leaf("greet", Greet).or_die();
        let shout = registry.register_leaf("shout", Shouty).or_die();
        let tree = BehaviorTree::try_from_spec(&sequence(vec![action(shout)])).or_die();
        assert_eq!(registry.tree_cost(&tree), VirtualCost::new(42));
        assert!(registry.tree_cost(&tree) < registry.declared_cost());
    }

    #[test]
    fn dispatch_reaches_the_registered_leaf() {
        let mut registry = FlockRegistry::new();
        let leaf = registry.register_leaf("greet", Greet).or_die();
        let mut board = Blackboard::new();
        let mut ctx = LeafContext {
            blackboard: &mut board,
            tick: Tick::ZERO,
            npc: NpcId::unique(0),
        };
        let mut dispatch = registry.dispatcher();
        assert_eq!(dispatch.tick(leaf, &mut ctx), Status::Success);
        assert_eq!(dispatch.tick(999, &mut ctx), Status::Failure);
    }

    #[test]
    fn directive_expiry_is_inclusive() {
        let directive = Directive::new(DirectiveKind::Hold, 10);
        assert!(!directive.is_expired(9));
        assert!(directive.is_expired(10));
    }
}
