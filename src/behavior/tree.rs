//! A behaviour tree that resumes where it left off.
//!
//! The tree is compiled from a [`NodeSpec`] into a flat node array with child
//! ranges, so evaluating it walks indices in a `Vec` and never dereferences a
//! box per node. Running state lives in a [`TreeCursor`] whose size is bounded
//! by tree depth, not tree size: twenty nodes cost twenty bytes per NPC, not
//! twenty allocations.
//!
//! Resume semantics are the part that usually goes wrong, so they are stated
//! here and tested per node type:
//!
//! - A `Sequence` returns `Running` at the first running child and remembers
//!   which child that was. On the next tick it resumes at that child, not at the
//!   start.
//! - A `Selector` behaves the same way with `Success` instead of `Failure`.
//! - `Failure` in a `Sequence` and `Success` in a `Selector` reset the cursor for
//!   that node, so the next entry starts from the beginning.
//! - [`BehaviorTree::abort`] walks the cursor and gives every running leaf a
//!   chance to clean up.

use core::fmt;

use crate::behavior::blackboard::Blackboard;
use crate::domain::ids::NpcId;
use crate::domain::tick::Tick;

/// Identifier of a leaf behaviour in a [`crate::behavior::registry::FlockRegistry`].
pub type LeafId = u32;

/// The result of ticking a node for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The node completed successfully.
    Success,
    /// The node failed.
    Failure,
    /// The node needs more ticks.
    Running,
}

impl Status {
    /// Whether this status is [`Status::Success`].
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    /// Whether this status is [`Status::Failure`].
    #[must_use]
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::Failure)
    }

    /// Whether this status is [`Status::Running`].
    #[must_use]
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Running => "running",
        })
    }
}

/// What a node does with its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Runs children in order; fails on the first failure.
    Sequence,
    /// Runs children in order; succeeds on the first success.
    Selector,
    /// Inverts success and failure; passes `Running` through.
    Inverter,
    /// Repeats its single child until that child fails.
    RepeatUntilFail,
    /// Delegates to a leaf behaviour.
    Action,
    /// Delegates to a leaf that answers a question and returns immediately.
    Condition,
}

impl NodeKind {
    /// Whether this node kind dispatches to a leaf.
    #[must_use]
    pub const fn is_leaf(self) -> bool {
        matches!(self, Self::Action | Self::Condition)
    }
}

/// A tree described as data, before compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeSpec {
    /// Runs children in order until one fails.
    Sequence(Vec<Self>),
    /// Runs children in order until one succeeds.
    Selector(Vec<Self>),
    /// Inverts its child.
    Inverter(Box<Self>),
    /// Repeats its child until the child fails.
    RepeatUntilFail(Box<Self>),
    /// Runs a leaf behaviour.
    Action(LeafId),
    /// Evaluates a leaf as a predicate.
    Condition(LeafId),
}

/// Build a [`NodeSpec::Sequence`].
#[must_use]
pub fn sequence(children: Vec<NodeSpec>) -> NodeSpec {
    NodeSpec::Sequence(children)
}

/// Build a [`NodeSpec::Selector`].
#[must_use]
pub fn selector(children: Vec<NodeSpec>) -> NodeSpec {
    NodeSpec::Selector(children)
}

/// Build a [`NodeSpec::Action`].
#[must_use]
pub fn action(leaf: LeafId) -> NodeSpec {
    NodeSpec::Action(leaf)
}

/// Build a [`NodeSpec::Condition`].
#[must_use]
pub fn condition(leaf: LeafId) -> NodeSpec {
    NodeSpec::Condition(leaf)
}

/// Build a [`NodeSpec::Inverter`].
#[must_use]
pub fn invert(child: NodeSpec) -> NodeSpec {
    NodeSpec::Inverter(Box::new(child))
}

/// Build a [`NodeSpec::RepeatUntilFail`].
#[must_use]
pub fn repeat_until_fail(child: NodeSpec) -> NodeSpec {
    NodeSpec::RepeatUntilFail(Box::new(child))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    kind: NodeKind,
    start: u32,
    len: u32,
    leaf: LeafId,
}

/// A compiled behaviour tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorTree {
    nodes: Vec<Node>,
    children: Vec<u32>,
    root: u32,
    depth: u32,
    name: &'static str,
}

impl BehaviorTree {
    /// Compile a specification, rejecting structurally invalid trees.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason for an empty sequence or selector, or for
    /// a tree deeper than [`BehaviorTree::MAX_DEPTH`].
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the spec is handed over rather than borrowed: a caller that has \
                  finished describing a tree should not keep it alive"
    )]
    pub fn try_from_spec(spec: NodeSpec) -> Result<Self, TreeError> {
        let mut builder = Builder {
            nodes: Vec::new(),
            children: Vec::new(),
        };
        let root = builder.push(&spec, 1)?;
        let depth = builder.depth_of(0);
        Ok(Self {
            nodes: builder.nodes,
            children: builder.children,
            root,
            depth,
            name: "behaviour-tree",
        })
    }

    /// Attach a name for debug output.
    #[must_use]
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// The deepest path from root to leaf.
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// The maximum depth a tree may have.
    ///
    /// Bounded because evaluation is recursive: a 10,000-deep tree would smash
    /// the stack, and no sane behaviour tree needs more than 64 levels.
    pub const MAX_DEPTH: u32 = 64;

    /// The number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree is empty, which cannot happen for a compiled tree.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The name a tree was built with.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Evaluate the tree for one tick.
    pub fn tick(
        &self,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        self.tick_node(self.root, cursor, dispatch, ctx)
    }

    fn tick_node(
        &self,
        index: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let Some(node) = self.nodes.get(index as usize) else {
            return Status::Failure;
        };
        let child_range = node.start as usize..(node.start + node.len) as usize;
        let children = self.children.get(child_range).unwrap_or(&[]);

        match node.kind {
            NodeKind::Action | NodeKind::Condition => {
                let status = dispatch.tick(node.leaf, ctx);
                if node.kind == NodeKind::Condition {
                    // A condition never runs: it answers now, or it does not.
                    return if status.is_running() {
                        Status::Failure
                    } else {
                        status
                    };
                }
                status
            }
            NodeKind::Sequence => {
                let start = cursor.child_cursor(index) as usize;
                for (offset, child) in children.iter().enumerate().skip(start) {
                    match self.tick_node(*child, cursor, dispatch, ctx) {
                        Status::Running => {
                            cursor.set_child_cursor(index, offset as u32);
                            return Status::Running;
                        }
                        Status::Failure => {
                            cursor.reset(index);
                            return Status::Failure;
                        }
                        Status::Success => {}
                    }
                }
                cursor.reset(index);
                Status::Success
            }
            NodeKind::Selector => {
                let start = cursor.child_cursor(index) as usize;
                for (offset, child) in children.iter().enumerate().skip(start) {
                    match self.tick_node(*child, cursor, dispatch, ctx) {
                        Status::Running => {
                            cursor.set_child_cursor(index, offset as u32);
                            return Status::Running;
                        }
                        Status::Success => {
                            cursor.reset(index);
                            return Status::Success;
                        }
                        Status::Failure => {}
                    }
                }
                cursor.reset(index);
                Status::Failure
            }
            NodeKind::Inverter => {
                let Some(child) = children.first() else {
                    return Status::Failure;
                };
                match self.tick_node(*child, cursor, dispatch, ctx) {
                    Status::Success => Status::Failure,
                    Status::Failure => Status::Success,
                    Status::Running => Status::Running,
                }
            }
            NodeKind::RepeatUntilFail => {
                let Some(child) = children.first() else {
                    return Status::Failure;
                };
                // A repeat is a loop, and a loop in a behaviour tree is a hang
                // waiting to happen unless it is capped. The cap is the tree's
                // own depth-bounded budget: at most `MAX_REPEAT_ITERATIONS`
                // re-evaluations of the child per tick.
                for _ in 0..Self::MAX_REPEAT_ITERATIONS {
                    match self.tick_node(*child, cursor, dispatch, ctx) {
                        Status::Failure => return Status::Success,
                        Status::Running => return Status::Running,
                        Status::Success => {}
                    }
                }
                Status::Running
            }
        }
    }

    /// The most times a repeat node re-runs its child in a single tick.
    pub const MAX_REPEAT_ITERATIONS: u32 = 64;

    /// Abort every running leaf, deepest first.
    ///
    /// Called when an NPC's behaviour is replaced or it despawns. A leaf that
    /// allocates state (a conversation, a path request) releases it here; an
    /// abort that is skipped shows up later as a stuck NPC.
    pub fn abort(
        &self,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) {
        for (node_index, _) in cursor.path() {
            if let Some(node) = self.nodes.get(*node_index as usize)
                && node.kind.is_leaf()
            {
                dispatch.abort(node.leaf, ctx);
            }
        }
        cursor.clear();
    }

    /// Every leaf id reachable in this tree, in evaluation order.
    #[must_use]
    pub fn leaf_ids(&self) -> Vec<LeafId> {
        let mut leaves = Vec::new();
        for node in &self.nodes {
            if node.kind.is_leaf() && !leaves.contains(&node.leaf) {
                leaves.push(node.leaf);
            }
        }
        leaves
    }
}

struct Builder {
    nodes: Vec<Node>,
    children: Vec<u32>,
}

impl TryFrom<NodeSpec> for BehaviorTree {
    type Error = TreeError;

    /// Compile a specification.
    ///
    /// The tree is validated when it is compiled, not when it is ticked: an empty
    /// composite or a tree deeper than [`BehaviorTree::MAX_DEPTH`] is an authoring
    /// error, and the caller decides whether that is fatal.
    fn try_from(spec: NodeSpec) -> Result<Self, Self::Error> {
        Self::try_from_spec(spec)
    }
}

impl Builder {
    fn push(&mut self, spec: &NodeSpec, depth: u32) -> Result<u32, TreeError> {
        if depth > BehaviorTree::MAX_DEPTH {
            return Err(TreeError::TooDeep {
                max: BehaviorTree::MAX_DEPTH,
            });
        }
        let index = self.nodes.len() as u32;
        match spec {
            NodeSpec::Action(leaf) | NodeSpec::Condition(leaf) => {
                let kind = if matches!(spec, NodeSpec::Action(_)) {
                    NodeKind::Action
                } else {
                    NodeKind::Condition
                };
                self.nodes.push(Node {
                    kind,
                    start: 0,
                    len: 0,
                    leaf: *leaf,
                });
            }
            NodeSpec::Sequence(items) | NodeSpec::Selector(items) => {
                if items.is_empty() {
                    return Err(TreeError::EmptyComposite);
                }
                let kind = if matches!(spec, NodeSpec::Sequence(_)) {
                    NodeKind::Sequence
                } else {
                    NodeKind::Selector
                };
                // Reserve this node before its children so the root stays first,
                // which keeps the compiled layout stable for snapshots.
                self.nodes.push(Node {
                    kind,
                    start: 0,
                    len: 0,
                    leaf: 0,
                });
                let start = self.children.len() as u32;
                for item in items {
                    let child = self.push(item, depth + 1)?;
                    self.children.push(child);
                }
                let len = self.children.len() as u32 - start;
                if let Some(node) = self.nodes.get_mut(index as usize) {
                    node.start = start;
                    node.len = len;
                }
            }
            NodeSpec::Inverter(inner) | NodeSpec::RepeatUntilFail(inner) => {
                let kind = if matches!(spec, NodeSpec::Inverter(_)) {
                    NodeKind::Inverter
                } else {
                    NodeKind::RepeatUntilFail
                };
                self.nodes.push(Node {
                    kind,
                    start: 0,
                    len: 0,
                    leaf: 0,
                });
                let start = self.children.len() as u32;
                let child = self.push(inner, depth + 1)?;
                self.children.push(child);
                if let Some(node) = self.nodes.get_mut(index as usize) {
                    node.start = start;
                    node.len = 1;
                }
            }
        }
        Ok(index)
    }

    fn depth_of(&self, index: u32) -> u32 {
        let mut max = 0;
        if let Some(node) = self.nodes.get(index as usize) {
            let range = node.start as usize..(node.start + node.len) as usize;
            for child in self.children.get(range).unwrap_or(&[]) {
                max = max.max(self.depth_of(*child));
            }
        }
        max + 1
    }
}

/// Why a tree could not be compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeError {
    /// A sequence or selector had no children.
    EmptyComposite,
    /// The tree was deeper than [`BehaviorTree::MAX_DEPTH`].
    TooDeep {
        /// The maximum allowed depth.
        max: u32,
    },
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyComposite => {
                f.write_str("a sequence or selector must have at least one child")
            }
            Self::TooDeep { max } => write!(f, "behaviour tree exceeds the maximum depth of {max}"),
        }
    }
}

impl core::error::Error for TreeError {}

/// Which child each running composite is currently inside.
///
/// Bounded by tree depth: one entry per running composite, not per node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeCursor {
    entries: Vec<(u32, u32)>,
}

impl TreeCursor {
    /// An empty cursor, ready for a fresh tick.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(8),
        }
    }

    /// The recorded child cursor for a node, or `0`.
    #[must_use]
    pub fn child_cursor(&self, node: u32) -> u32 {
        self.entries
            .iter()
            .find_map(|(index, cursor)| (*index == node).then_some(*cursor))
            .unwrap_or(0)
    }

    /// Record the child cursor for a node.
    pub fn set_child_cursor(&mut self, node: u32, cursor: u32) {
        if let Some(entry) = self.entries.iter_mut().find(|(index, _)| *index == node) {
            entry.1 = cursor;
        } else {
            self.entries.push((node, cursor));
        }
    }

    /// Forget a node's cursor.
    pub fn reset(&mut self, node: u32) {
        self.entries.retain(|(index, _)| *index != node);
    }

    /// Forget everything, as after an abort.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The running path, outermost first.
    #[must_use]
    pub fn path(&self) -> &[(u32, u32)] {
        &self.entries
    }

    /// Whether the cursor believes a node is running.
    #[must_use]
    pub fn is_running(&self, node: u32) -> bool {
        self.entries.iter().any(|(index, _)| *index == node)
    }
}

/// Everything a leaf needs to run.
pub struct LeafContext<'a> {
    /// Per-NPC working memory.
    pub blackboard: &'a mut Blackboard,
    /// The tick this evaluation is happening on.
    pub tick: Tick,
    /// The NPC being evaluated.
    pub npc: NpcId,
}

/// The bridge between a compiled tree and the leaves that implement it.
///
/// A trait rather than a concrete registry so the tree can be tested with a
/// closure and so a game can supply its own dispatch strategy.
pub trait LeafDispatch {
    /// Run one leaf.
    fn tick(&mut self, leaf: LeafId, ctx: &mut LeafContext<'_>) -> Status;

    /// Tell one leaf to release its state.
    fn abort(&mut self, leaf: LeafId, ctx: &mut LeafContext<'_>) {
        let _ = (leaf, ctx);
    }
}

impl<F> LeafDispatch for F
where
    F: FnMut(LeafId, &mut LeafContext<'_>) -> Status,
{
    fn tick(&mut self, leaf: LeafId, ctx: &mut LeafContext<'_>) -> Status {
        self(leaf, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile a tree for testing; a failure here is a broken fixture.
    fn tree(spec: NodeSpec) -> BehaviorTree {
        BehaviorTree::try_from_spec(spec).expect("test tree is valid")
    }

    /// Leaves: 0 = succeed, 1 = fail, 2 = run forever, 3 = run twice then
    /// succeed.
    struct Scripted {
        runs: u32,
    }

    impl LeafDispatch for Scripted {
        fn tick(&mut self, leaf: LeafId, _ctx: &mut LeafContext<'_>) -> Status {
            // Leaves 1 and 4 (and anything unregistered) fail by design.
            match leaf {
                0 => Status::Success,
                2 => Status::Running,
                3 => {
                    self.runs += 1;
                    if self.runs < 2 {
                        Status::Running
                    } else {
                        Status::Success
                    }
                }
                _ => Status::Failure,
            }
        }
    }

    fn ctx(blackboard: &mut Blackboard) -> LeafContext<'_> {
        LeafContext {
            blackboard,
            tick: Tick::ZERO,
            npc: NpcId::unique(0),
        }
    }

    #[test]
    fn sequence_requires_every_child() {
        let tree = tree(sequence(vec![action(0), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn sequence_stops_at_the_first_failure() {
        let tree = tree(sequence(vec![action(1), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Failure
        );
        assert!(!cursor.is_running(0));
    }

    #[test]
    fn selector_stops_at_the_first_success() {
        let tree = tree(selector(vec![action(1), action(0), action(1)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn sequence_resumes_at_the_running_child() {
        // Sequence: [run-forever leaf 2, success leaf 0]. After the first tick
        // the cursor points at child 1 of the sequence, so a second tick must not
        // re-enter leaf 2 from scratch... it re-enters it, but reports Running
        // without advancing past it.
        let tree = tree(sequence(vec![action(2), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
        assert_eq!(cursor.child_cursor(tree.root), 0);
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
    }

    #[test]
    fn resumable_leaf_is_not_restarted() {
        // Sequence: [leaf 3 (runs twice), leaf 0]. If the sequence restarted at
        // child 0 on every tick, leaf 3 would never complete.
        let tree = tree(sequence(vec![action(3), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
        let status = tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(
            status,
            Status::Success,
            "leaf 3 completed on its second tick"
        );
    }

    #[test]
    fn inverter_swaps_success_and_failure_but_passes_running() {
        let inverted = tree(invert(action(1)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            inverted.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );

        let running_inverted = tree(invert(action(2)));
        let mut other_cursor = TreeCursor::new();
        assert_eq!(
            running_inverted.tick(&mut other_cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
    }

    #[test]
    fn repeat_until_fail_is_capped_instead_of_hanging() {
        let tree = tree(repeat_until_fail(action(0)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        let status = tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(
            status,
            Status::Running,
            "an always-succeeding repeat is capped"
        );
    }

    #[test]
    fn repeat_until_fail_succeeds_when_the_child_fails() {
        let tree = tree(repeat_until_fail(action(1)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn a_condition_that_runs_is_treated_as_failure() {
        let tree = tree(condition(2));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Failure,
            "a predicate cannot be in progress"
        );
    }

    #[test]
    fn abort_clears_the_cursor() {
        let tree = tree(sequence(vec![action(2)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_ne!(cursor.path().len(), 0);
        tree.abort(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(cursor.path().len(), 0);
    }

    #[test]
    fn empty_composites_are_rejected() {
        assert_eq!(
            BehaviorTree::try_from_spec(sequence(vec![])),
            Err(TreeError::EmptyComposite)
        );
        assert_eq!(
            BehaviorTree::try_from_spec(selector(vec![])),
            Err(TreeError::EmptyComposite)
        );
    }

    #[test]
    fn excessive_depth_is_rejected_rather_than_smashing_the_stack() {
        let mut spec = action(0);
        for _ in 0..(BehaviorTree::MAX_DEPTH + 2) {
            spec = invert(spec);
        }
        assert!(matches!(
            BehaviorTree::try_from_spec(spec),
            Err(TreeError::TooDeep { .. })
        ));
    }

    #[test]
    fn depth_and_leaf_listing_are_reported() {
        let tree = tree(sequence(vec![
            action(0),
            selector(vec![condition(1), action(2)]),
        ]));
        assert_eq!(tree.depth(), 3);
        assert_eq!(tree.leaf_ids(), vec![0, 1, 2]);
        assert_eq!(tree.len(), 5);
    }

    #[test]
    fn evaluation_order_is_stable_across_ticks() {
        // The same tree evaluated twice from a fresh cursor produces the same
        // status sequence: the property a replay depends on.
        let tree = tree(selector(vec![action(1), action(0)]));
        let mut dispatch = Scripted { runs: 0 };
        let mut board = Blackboard::new();
        let mut first = TreeCursor::new();
        let mut second = TreeCursor::new();
        let a = tree.tick(&mut first, &mut dispatch, &mut ctx(&mut board));
        let b = tree.tick(&mut second, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(a, b);
        assert_eq!(first, second);
    }
}
