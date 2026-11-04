//! Pluggable NPC logic: behaviour trees, state machines and the registry.
//!
//! Three layers, each answering a different question:
//!
//! - [`tree`] answers *what do I do next*, with resume semantics and a flat
//!   compiled layout that costs no allocations per evaluation.
//! - [`state_machine`] answers *what mode am I in*, with dwell times and ordered
//!   guards so a patrol cannot be abandoned every frame.
//! - [`registry`] answers *what can this game plug in*, with object-safe traits
//!   and build-time rejection of duplicate identifiers.
//!
//! [`blackboard`] is the small typed store they all share.

pub mod blackboard;
pub mod registry;
pub mod state_machine;
pub mod tree;

pub use blackboard::{BLACKBOARD_CAPACITY, Blackboard, BlackboardKey, Value};
pub use registry::{
    Directive, DirectiveKind, DirectorContext, FlockRegistry, GroupDirector, LeafBehavior,
    NpcLogic, NpcLogicContext, RegistryDispatch, WorldDirector,
};
pub use state_machine::{Guard, StateMachine, Transition, always, never};
pub use tree::{
    BehaviorTree, LeafContext, LeafDispatch, LeafId, NodeKind, NodeSpec, Status, TreeCursor,
    TreeError, action, condition, invert, repeat_until_fail, selector, sequence,
};
