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
pub mod blackboard;
pub mod dialog;
pub mod leaves;
pub mod memory_leaves;
pub mod provider;
pub mod registry;
pub mod social;
pub mod state_machine;
pub mod tree;
pub mod utility;

pub use archetypes::ArchetypeLogic;
pub use bark::{
    BarkCategory, BarkError, BarkEvent, BarkLine, BarkQueue, BarkRequest, BarkState, BarkTable,
};
pub use blackboard::{BLACKBOARD_CAPACITY, Blackboard, BlackboardKey, Value};
pub use dialog::{
    Condition, DialogChoice, DialogError, DialogGraph, DialogNode, DialogSession, Speaker,
};
pub use leaves::StandardLeaves;
pub use memory_leaves::{
    GRUDGE_VALENCE, GreetFriend, KEY_MEMORY_FAMILIARITY, KEY_MEMORY_FLAGS, KEY_MEMORY_VALENCE,
    MemoryLeaves, RECOGNITION_THRESHOLD, RecogniseKnown, Remembered, ShunGrudge, WARM_VALENCE,
};
pub use provider::{
    DialogContext, DialogPrompt, DialogProvider, DialogReply, ProviderQueue, ReplySource,
    TemplateProvider,
};
pub use registry::{
    Directive, DirectiveKind, DirectorContext, FlockRegistry, GroupDirector, LeafBehavior,
    NpcLogic, NpcLogicContext, RegistryDispatch, WorldDirector,
};
pub use social::{AvoidKnown, GreetKnown, GreetingGuard, KEY_TALK_TO, KEY_WARY_OF};
pub use state_machine::{Guard, StateMachine, Transition, always, never};
pub use tree::{
    BehaviorTree, LeafContext, LeafDispatch, LeafId, NodeKind, NodeSpec, ParallelPolicy, Status,
    TreeCursor, TreeError, action, condition, cooldown, invert, parallel, repeat_until_fail,
    selector, sequence, timeout,
};
pub use utility::{
    BehaviorId, CandidateScore, Consideration, Curve, Explanation, Inhibition, Input,
    MAX_CONSIDERATIONS, Selection, UtilityEntry, UtilityError, UtilitySet, UtilityTable,
};
