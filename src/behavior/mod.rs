//! Pluggable NPC logic: behaviour trees, state machines and the registry.
//!
//! Four layers, each answering a different question:
//!
//! - [`tree`] answers *what do I do next*, with resume semantics and a flat
//!   compiled layout that costs no allocations per evaluation.
//! - [`state_machine`] answers *what mode am I in*, with dwell times and ordered
//!   guards so a patrol cannot be abandoned every frame.
//! - [`registry`] answers *what can this game plug in*, with object-safe traits
//!   and build-time rejection of duplicate identifiers.
//! - [`leaves`] and [`archetypes`] answer *what does the built-in crowd actually
//!   do, and what does one think cost*.
//!
//! [`blackboard`] is the small typed store they all share.

pub mod archetypes;
pub mod bark;
pub mod dialog;
pub mod provider;
pub mod blackboard;
pub mod leaves;
pub mod registry;
pub mod state_machine;
pub mod tree;
pub mod utility;

pub use archetypes::ArchetypeLogic;
pub use blackboard::{BLACKBOARD_CAPACITY, Blackboard, BlackboardKey, Value};
pub use leaves::StandardLeaves;
pub use registry::{
    Directive, DirectiveKind, DirectorContext, FlockRegistry, GroupDirector, LeafBehavior,
    NpcLogic, NpcLogicContext, RegistryDispatch, WorldDirector,
};
pub use state_machine::{Guard, StateMachine, Transition, always, never};
pub use bark::{
    BarkCategory, BarkError, BarkEvent, BarkLine, BarkQueue, BarkRequest, BarkState, BarkTable,
};
pub use dialog::{
    Condition, DialogChoice, DialogError, DialogGraph, DialogNode, DialogSession, Speaker,
};
pub use provider::{
    DialogContext, DialogPrompt, DialogProvider, DialogReply, ProviderQueue, ReplySource,
    TemplateProvider,
};
pub use utility::{
    BehaviorId, CandidateScore, Consideration, Curve, Explanation, Inhibition, Input, Selection,
    UtilityEntry, UtilityError, UtilitySet, UtilityTable, MAX_CONSIDERATIONS,
};
pub use tree::{
    BehaviorTree, LeafContext, LeafDispatch, LeafId, NodeKind, NodeSpec, ParallelPolicy, Status,
    TreeCursor, TreeError, action, condition, cooldown, invert, parallel, repeat_until_fail,
    selector, sequence, timeout,
};
