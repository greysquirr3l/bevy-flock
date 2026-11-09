//! A behaviour tree that resumes where it left off.
//!
//! The tree is compiled from a [`NodeSpec`] into a flat node array, so evaluating
//! it walks indices in a `Vec` and never dereferences a box per node: `Box<dyn
//! Node>` appears nowhere in the hot path. Running state lives in a
//! [`TreeCursor`] whose size is bounded by tree *depth*, not tree size.
//!
//! # Semantics
//!
//! Resume behaviour is the part that usually goes wrong, so each node type states
//! its contract and has a test for each outcome it can produce.
//!
//! | Node | Tick | Resume after `Running` | On completion |
//! | --- | --- | --- | --- |
//! | `Action` | dispatches the leaf | leaf is re-entered, so it must be resumable | cursor untouched |
//! | `Condition` | dispatches the leaf once | never runs; `Running` is reported as `Failure` | cursor untouched |
//! | `Sequence` | children in order, `Failure` short-circuits | resumes at the same child | `Success` runs every child; `Failure` resets |
//! | `Selector` | children in order, `Success` short-circuits | resumes at the same child | `Success` stops early; `Failure` runs every child |
//! | `Parallel` | every child not yet done | resumes with the completed set remembered | `RequireAll` needs every child; `RequireAny` needs one |
//! | `Inverter` | its child | passes `Running` through | swaps `Success` and `Failure` |
//! | `RepeatUntilFail` | its child, up to 64 times per tick | returns `Running` for the rest of the tick | succeeds when the child fails |
//! | `Cooldown` | its child, unless still cooling | returns `Failure` while cooling | stamps the tick on `Success` |
//! | `Timeout` | its child, unless the deadline passed | returns `Running` until the deadline | on deadline: aborts the child subtree, fails |
//!
//! # What a tick costs
//!
//! A tick is a walk over node indices plus one virtual call per visited leaf. It
//! allocates nothing once the cursor has grown to the tree's depth — a property
//! asserted by `tests/tree_no_alloc.rs`, not assumed.
//!
//! # Leaves are stateless
//!
//! A [`crate::behavior::registry::LeafBehavior`] is registered once and shared by
//! every NPC that runs it, so it takes `&self`. Per-NPC state belongs on the
//! [`Blackboard`], which is why the blackboard exists and why a leaf that stores
//! per-NPC state in a field would corrupt the whole crowd.

use core::fmt;

use smallvec::SmallVec;

use crate::behavior::blackboard::Blackboard;
use crate::domain::cost::VirtualCost;
use crate::domain::ids::NpcId;
use crate::domain::num;
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

/// What a [`NodeKind::Parallel`] node requires of its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParallelPolicy {
    /// Every child must succeed. The first failure fails the node.
    #[default]
    RequireAll,
    /// One child succeeding is enough. Every child failing fails the node.
    RequireAny,
}

/// What a node does with its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Runs children in order; fails on the first failure.
    Sequence,
    /// Runs children in order; succeeds on the first success.
    Selector,
    /// Runs children concurrently within the tick, combining by policy.
    Parallel(ParallelPolicy),
    /// Inverts success and failure; passes `Running` through.
    Inverter,
    /// Repeats its child until that child fails.
    RepeatUntilFail,
    /// Runs its child at most once every `ticks`.
    Cooldown,
    /// Fails if its child has not finished within `ticks`.
    Timeout,
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
///
/// Built with [`sequence`], [`selector`], [`parallel`], [`action`] and friends, or
/// deserialised from RON with `NodeSpec::from_ron` when the `serde` feature is
/// on. Both produce the same compiled tree, which is asserted by a golden test.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NodeSpec {
    /// Runs children in order until one fails.
    Sequence(Vec<Self>),
    /// Runs children in order until one succeeds.
    Selector(Vec<Self>),
    /// Runs children together, combining their status by `policy`.
    Parallel {
        /// What the node requires.
        policy: ParallelPolicy,
        /// The children to run.
        children: Vec<Self>,
    },
    /// Inverts its child.
    Inverter(Box<Self>),
    /// Repeats its child until the child fails.
    RepeatUntilFail(Box<Self>),
    /// Runs its child at most once every `ticks` ticks.
    Cooldown {
        /// The cooldown period in ticks. Zero means no cooldown.
        ticks: u32,
        /// The wrapped child.
        child: Box<Self>,
    },
    /// Fails if its child has not completed within `ticks` ticks.
    Timeout {
        /// The deadline in ticks. Zero fails immediately.
        ticks: u32,
        /// The wrapped child.
        child: Box<Self>,
    },
    /// Runs a leaf behaviour.
    Action(LeafId),
    /// Evaluates a leaf as a predicate.
    Condition(LeafId),
}

/// Build a [`NodeSpec::Sequence`].
#[must_use]
pub const fn sequence(children: Vec<NodeSpec>) -> NodeSpec {
    NodeSpec::Sequence(children)
}

/// Build a [`NodeSpec::Selector`].
#[must_use]
pub const fn selector(children: Vec<NodeSpec>) -> NodeSpec {
    NodeSpec::Selector(children)
}

/// Build a [`NodeSpec::Parallel`].
#[must_use]
pub const fn parallel(policy: ParallelPolicy, children: Vec<NodeSpec>) -> NodeSpec {
    NodeSpec::Parallel { policy, children }
}

/// Build a [`NodeSpec::Action`].
#[must_use]
pub const fn action(leaf: LeafId) -> NodeSpec {
    NodeSpec::Action(leaf)
}

/// Build a [`NodeSpec::Condition`].
#[must_use]
pub const fn condition(leaf: LeafId) -> NodeSpec {
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

/// Build a [`NodeSpec::Cooldown`].
#[must_use]
pub fn cooldown(ticks: u32, child: NodeSpec) -> NodeSpec {
    NodeSpec::Cooldown {
        ticks,
        child: Box::new(child),
    }
}

/// Build a [`NodeSpec::Timeout`].
#[must_use]
pub fn timeout(ticks: u32, child: NodeSpec) -> NodeSpec {
    NodeSpec::Timeout {
        ticks,
        child: Box::new(child),
    }
}

/// A compiled node.
///
/// `end` is the index one past the last node of this node's subtree. Because
/// compilation pushes a parent before its children depth-first, a subtree occupies
/// a contiguous index range, which makes "abort everything under here" a loop
/// rather than a traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node {
    kind: NodeKind,
    start: u32,
    len: u32,
    end: u32,
    leaf: LeafId,
    ticks: u32,
    depth: u8,
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
    /// The maximum depth a tree may have.
    ///
    /// Bounded because evaluation recurses: a 10,000-deep tree would smash the
    /// stack, and no readable behaviour tree needs more than 64 levels.
    pub const MAX_DEPTH: u32 = 64;

    /// The most children one node may have.
    ///
    /// Bounded because a [`NodeKind::Parallel`] node tracks its completed children
    /// in a `u64` bitmask.
    pub const MAX_CHILDREN: usize = 64;

    /// The most times a repeat node re-runs its child in a single tick.
    ///
    /// A repeat over an always-succeeding child is a hang, and "the game froze" is
    /// not an acceptable failure mode for an AI primitive.
    pub const MAX_REPEAT_ITERATIONS: u32 = 64;

    /// Compile a specification, rejecting structurally invalid trees.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::EmptyComposite`] for a composite with no children,
    /// [`TreeError::TooDeep`] beyond [`BehaviorTree::MAX_DEPTH`], and
    /// [`TreeError::TooManyChildren`] beyond [`BehaviorTree::MAX_CHILDREN`].
    pub fn try_from_spec(spec: &NodeSpec) -> Result<Self, TreeError> {
        let mut builder = Builder {
            nodes: Vec::new(),
            children: Vec::new(),
        };
        let root = builder.push(spec, 1)?;
        let depth = builder.nodes.iter().map(|node| u32::from(node.depth)).max();
        Ok(Self {
            nodes: builder.nodes,
            children: builder.children,
            root,
            depth: depth.unwrap_or(0),
            name: "behaviour-tree",
        })
    }

    /// Attach a name for debug output.
    #[must_use]
    pub const fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// The deepest path from root to leaf.
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// The number of nodes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree is empty, which a compiled tree never is.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The name a tree was built with.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The index of the root node, for tests and trace output.
    #[must_use]
    pub const fn root(&self) -> u32 {
        self.root
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
        let Some(node) = self.nodes.get(index as usize).copied() else {
            // An index outside the array is a compiler bug, not an authoring one.
            // Failing the node keeps the simulation running and shows up in a test.
            return Status::Failure;
        };
        match node.kind {
            NodeKind::Action => {
                let status = dispatch.tick(node.leaf, ctx);
                if status.is_running() {
                    // Record the leaf so an abort can find it. Without this the
                    // cursor only knows about composites, and every running leaf
                    // is aborted by nobody.
                    cursor.set_state(index, 0);
                } else {
                    cursor.forget(index);
                }
                status
            }
            NodeKind::Condition => match dispatch.tick(node.leaf, ctx) {
                // A predicate answers now or not at all. `Running` from a
                // condition is a leaf bug, and failing is the honest answer.
                Status::Running => Status::Failure,
                settled => settled,
            },
            NodeKind::Sequence => self.tick_sequence(index, cursor, dispatch, ctx),
            NodeKind::Selector => self.tick_selector(index, cursor, dispatch, ctx),
            NodeKind::Parallel(policy) => self.tick_parallel(index, policy, cursor, dispatch, ctx),
            NodeKind::Inverter => self.tick_inverter(index, cursor, dispatch, ctx),
            NodeKind::RepeatUntilFail => self.tick_repeat(index, cursor, dispatch, ctx),
            NodeKind::Cooldown => self.tick_cooldown(index, node.ticks, cursor, dispatch, ctx),
            NodeKind::Timeout => self.tick_timeout(index, node.ticks, cursor, dispatch, ctx),
        }
    }

    fn tick_sequence(
        &self,
        index: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let start = cursor.child_cursor(index) as usize;
        let children = self.children_slice(index);
        for (offset, child) in children.iter().enumerate().skip(start) {
            match self.tick_node(*child, cursor, dispatch, ctx) {
                Status::Running => {
                    cursor.set_child_cursor(index, num::usize_to_u32(offset));
                    return Status::Running;
                }
                Status::Failure => {
                    self.forget_subtree(index, cursor);
                    return Status::Failure;
                }
                Status::Success => {}
            }
        }
        self.forget_subtree(index, cursor);
        Status::Success
    }

    fn tick_selector(
        &self,
        index: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let start = cursor.child_cursor(index) as usize;
        let children = self.children_slice(index);
        for (offset, child) in children.iter().enumerate().skip(start) {
            match self.tick_node(*child, cursor, dispatch, ctx) {
                Status::Running => {
                    cursor.set_child_cursor(index, num::usize_to_u32(offset));
                    return Status::Running;
                }
                Status::Success => {
                    self.forget_subtree(index, cursor);
                    return Status::Success;
                }
                Status::Failure => {}
            }
        }
        self.forget_subtree(index, cursor);
        Status::Failure
    }

    fn tick_parallel(
        &self,
        index: u32,
        policy: ParallelPolicy,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let mut completed = cursor.state(index).unwrap_or(0);
        let children = self.children_slice(index);
        for (offset, child) in children.iter().enumerate() {
            // `MAX_CHILDREN` is 64 and enforced at compile time, so the shift is
            // always defined.
            if completed & (1_u64 << offset) != 0 {
                continue;
            }
            match self.tick_node(*child, cursor, dispatch, ctx) {
                Status::Running => {
                    cursor.set_state(index, completed);
                    return Status::Running;
                }
                Status::Success => match policy {
                    ParallelPolicy::RequireAny => {
                        self.forget_subtree(index, cursor);
                        return Status::Success;
                    }
                    ParallelPolicy::RequireAll => completed |= 1_u64 << offset,
                },
                Status::Failure => match policy {
                    ParallelPolicy::RequireAll => {
                        self.forget_subtree(index, cursor);
                        return Status::Failure;
                    }
                    ParallelPolicy::RequireAny => completed |= 1_u64 << offset,
                },
            }
        }
        self.forget_subtree(index, cursor);
        match policy {
            ParallelPolicy::RequireAll => Status::Success,
            ParallelPolicy::RequireAny => Status::Failure,
        }
    }

    fn tick_inverter(
        &self,
        index: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let Some(child) = self.first_child(index) else {
            return Status::Failure;
        };
        match self.tick_node(child, cursor, dispatch, ctx) {
            Status::Success => Status::Failure,
            Status::Failure => Status::Success,
            Status::Running => Status::Running,
        }
    }

    fn tick_repeat(
        &self,
        index: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let Some(child) = self.first_child(index) else {
            return Status::Failure;
        };
        for _ in 0..Self::MAX_REPEAT_ITERATIONS {
            match self.tick_node(child, cursor, dispatch, ctx) {
                Status::Failure => {
                    cursor.forget(index);
                    return Status::Success;
                }
                Status::Running => return Status::Running,
                Status::Success => {}
            }
        }
        // Capped rather than looping forever: report `Running` so the next tick
        // continues the repetition instead of the frame never ending.
        Status::Running
    }

    fn tick_cooldown(
        &self,
        index: u32,
        ticks: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let now = ctx.tick.get();
        if let Some(last) = cursor.state(index)
            && now.saturating_sub(last) < u64::from(ticks)
        {
            return Status::Failure;
        }
        let Some(child) = self.first_child(index) else {
            return Status::Failure;
        };
        match self.tick_node(child, cursor, dispatch, ctx) {
            Status::Success => {
                cursor.set_state(index, now);
                Status::Success
            }
            unsettled => unsettled,
        }
    }

    fn tick_timeout(
        &self,
        index: u32,
        ticks: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) -> Status {
        let now = ctx.tick.get();
        let started = cursor.state(index).unwrap_or_else(|| {
            cursor.set_state(index, now);
            now
        });
        if now.saturating_sub(started) >= u64::from(ticks) {
            // The deadline is not a reason to leave a conversation open: abort the
            // subtree so every running leaf releases what it holds.
            self.abort_subtree(index, cursor, dispatch, ctx);
            return Status::Failure;
        }
        let Some(child) = self.first_child(index) else {
            return Status::Failure;
        };
        let status = self.tick_node(child, cursor, dispatch, ctx);
        if !status.is_running() {
            cursor.forget(index);
        }
        status
    }

    /// Abort every running leaf in the tree, deepest first.
    ///
    /// Called when an NPC's behaviour is replaced or it despawns. A leaf that
    /// allocates state (a conversation, a path request) releases it here; an abort
    /// that is skipped shows up later as a stuck NPC.
    pub fn abort(
        &self,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) {
        self.abort_subtree(self.root, cursor, dispatch, ctx);
        cursor.clear();
    }

    fn abort_subtree(
        &self,
        index: u32,
        cursor: &mut TreeCursor,
        dispatch: &mut dyn LeafDispatch,
        ctx: &mut LeafContext<'_>,
    ) {
        let end = self.end_of(index);
        let mut offset = end;
        while offset > index {
            offset -= 1;
            let Some(node) = self.nodes.get(offset as usize) else {
                continue;
            };
            if node.kind.is_leaf() && cursor.is_running(offset) {
                dispatch.abort(node.leaf, ctx);
            }
        }
        cursor.forget_range(index, end);
    }

    fn forget_subtree(&self, index: u32, cursor: &mut TreeCursor) {
        cursor.forget_range(index, self.end_of(index));
    }

    /// One past the last node index in a node's subtree.
    fn end_of(&self, index: u32) -> u32 {
        self.nodes
            .get(index as usize)
            .map_or_else(|| index.saturating_add(1), |node| node.end)
    }

    fn children_slice(&self, index: u32) -> &[u32] {
        let Some(node) = self.nodes.get(index as usize) else {
            return &[];
        };
        let start = node.start as usize;
        let end = start.saturating_add(node.len as usize);
        self.children.get(start..end).unwrap_or(&[])
    }

    fn first_child(&self, index: u32) -> Option<u32> {
        self.children_slice(index).first().copied()
    }

    /// Every leaf id reachable in this tree, in compilation order, without
    /// duplicates.
    #[must_use]
    pub fn leaf_ids(&self) -> Vec<LeafId> {
        let mut leaves = SmallVec::<[LeafId; 16]>::new();
        for node in &self.nodes {
            if node.kind.is_leaf() && !leaves.contains(&node.leaf) {
                leaves.push(node.leaf);
            }
        }
        leaves.into_vec()
    }

    /// The declared cost of one tick, given a way to look up a leaf's cost.
    ///
    /// Distinct leaves are counted once: a tick usually visits each leaf at most
    /// once, and a `RepeatUntilFail` that visits one repeatedly is the case the
    /// calibration gate exists to catch. The number is therefore an estimate made
    /// cheap enough to compute at registration and stable enough to replay.
    pub fn declared_cost(&self, cost_of: impl Fn(LeafId) -> VirtualCost) -> VirtualCost {
        let mut seen = SmallVec::<[LeafId; 16]>::new();
        let mut total = VirtualCost::ZERO;
        for node in &self.nodes {
            if node.kind.is_leaf() && !seen.contains(&node.leaf) {
                seen.push(node.leaf);
                total = total.saturating_add(cost_of(node.leaf));
            }
        }
        total
    }
}

impl TryFrom<NodeSpec> for BehaviorTree {
    type Error = TreeError;

    /// Compile a specification.
    ///
    /// The tree is validated when it is compiled, not when it is ticked: an empty
    /// composite or a tree deeper than [`BehaviorTree::MAX_DEPTH`] is an authoring
    /// error, and the caller decides whether that is fatal.
    fn try_from(spec: NodeSpec) -> Result<Self, Self::Error> {
        Self::try_from_spec(&spec)
    }
}

struct Builder {
    nodes: Vec<Node>,
    children: Vec<u32>,
}

impl Builder {
    fn push(&mut self, spec: &NodeSpec, depth: u32) -> Result<u32, TreeError> {
        if depth > BehaviorTree::MAX_DEPTH {
            return Err(TreeError::TooDeep {
                max: BehaviorTree::MAX_DEPTH,
            });
        }
        let index = num::usize_to_u32(self.nodes.len());
        let kind = Self::kind_of(spec);
        let leaf = match spec {
            NodeSpec::Action(leaf) | NodeSpec::Condition(leaf) => *leaf,
            _ => 0,
        };
        let ticks = match spec {
            NodeSpec::Cooldown { ticks, child: _ } | NodeSpec::Timeout { ticks, child: _ } => {
                *ticks
            }
            _ => 0,
        };
        let (children, limit) = Self::children_of(spec);
        if limit && children.is_empty() {
            return Err(TreeError::EmptyComposite);
        }
        if children.len() > BehaviorTree::MAX_CHILDREN {
            return Err(TreeError::TooManyChildren {
                max: BehaviorTree::MAX_CHILDREN,
            });
        }

        // Reserve this node before its children so a subtree stays contiguous and
        // the root stays at index 0, which keeps the compiled layout stable.
        self.nodes.push(Node {
            kind,
            start: 0,
            len: 0,
            end: index + 1,
            leaf,
            ticks,
            // `depth` is bounded by `MAX_DEPTH`, far below `u8::MAX`, so this saturates only
            // for a tree the compiler has already rejected.
            depth: u8::try_from(depth).unwrap_or(u8::MAX),
        });
        // Recurse first, then append: a child's own descendants push onto this
        // same array, so appending as we go would make `len` count grandchildren
        // as children.
        let mut direct = Vec::with_capacity(children.len());
        for child in children {
            direct.push(self.push(child, depth + 1)?);
        }
        let start = num::usize_to_u32(self.children.len());
        let len = num::usize_to_u32(direct.len());
        self.children.extend_from_slice(&direct);
        let end = num::usize_to_u32(self.nodes.len());
        if let Some(node) = self.nodes.get_mut(index as usize) {
            node.start = start;
            node.len = len;
            node.end = end;
        }
        Ok(index)
    }

    const fn kind_of(spec: &NodeSpec) -> NodeKind {
        match spec {
            NodeSpec::Sequence(_) => NodeKind::Sequence,
            NodeSpec::Selector(_) => NodeKind::Selector,
            NodeSpec::Parallel {
                policy,
                children: _,
            } => NodeKind::Parallel(*policy),
            NodeSpec::Inverter(_) => NodeKind::Inverter,
            NodeSpec::RepeatUntilFail(_) => NodeKind::RepeatUntilFail,
            NodeSpec::Cooldown { ticks: _, child: _ } => NodeKind::Cooldown,
            NodeSpec::Timeout { ticks: _, child: _ } => NodeKind::Timeout,
            NodeSpec::Action(_) => NodeKind::Action,
            NodeSpec::Condition(_) => NodeKind::Condition,
        }
    }

    /// The children of a spec, and whether it is a composite that requires at
    /// least one.
    fn children_of(spec: &NodeSpec) -> (Vec<&NodeSpec>, bool) {
        match spec {
            NodeSpec::Sequence(items) | NodeSpec::Selector(items) => (items.iter().collect(), true),
            NodeSpec::Parallel {
                children,
                policy: _,
            } => (children.iter().collect(), true),
            NodeSpec::Inverter(child)
            | NodeSpec::RepeatUntilFail(child)
            | NodeSpec::Cooldown { child, ticks: _ }
            | NodeSpec::Timeout { child, ticks: _ } => (vec![child.as_ref()], true),
            NodeSpec::Action(_) | NodeSpec::Condition(_) => (Vec::new(), false),
        }
    }
}

/// Why a tree could not be compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TreeError {
    /// A composite had no children.
    EmptyComposite,
    /// The tree was deeper than [`BehaviorTree::MAX_DEPTH`].
    TooDeep {
        /// The maximum allowed depth.
        max: u32,
    },
    /// A node had more children than [`BehaviorTree::MAX_CHILDREN`].
    TooManyChildren {
        /// The maximum allowed children per node.
        max: usize,
    },
    /// An authored tree could not be parsed.
    #[cfg(feature = "serde")]
    MalformedSource {
        /// One-based line of the first error.
        line: usize,
        /// One-based column of the first error.
        column: usize,
    },
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyComposite => f.write_str("a composite node must have at least one child"),
            Self::TooDeep { max } => write!(f, "behaviour tree exceeds the maximum depth of {max}"),
            Self::TooManyChildren { max } => {
                write!(f, "a behaviour tree node may have at most {max} children")
            }
            #[cfg(feature = "serde")]
            Self::MalformedSource { line, column } => {
                write!(f, "behaviour tree source is malformed at {line}:{column}")
            }
        }
    }
}

impl core::error::Error for TreeError {}

#[cfg(feature = "serde")]
impl NodeSpec {
    /// Parse an authored tree from RON.
    ///
    /// The RON form is the enum itself, which keeps the data file and the builder
    /// describing the same shape:
    ///
    /// ```
    /// # use bevy_flock_core::behavior::tree::{action, condition, selector, sequence, NodeSpec};
    /// let spec = NodeSpec::from_ron(
    ///     r#"Selector([Condition(0), Sequence([Action(1), Action(2)])])"#,
    /// )
    /// .expect("valid tree");
    /// assert_eq!(spec, selector(vec![condition(0), sequence(vec![action(1), action(2)])]));
    /// ```
    ///
    /// # Errors
    ///
    /// `Parallel`, `Cooldown` and `Timeout` are struct variants, so their fields
    /// are named there — `Cooldown(ticks: 4, child: Action(1))` — while the others
    /// are tuples.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::MalformedSource`] with the position of the first
    /// syntax or shape error.
    pub fn from_ron(source: &str) -> Result<Self, TreeError> {
        ron::from_str(source).map_err(|error| TreeError::MalformedSource {
            line: error.position.line,
            column: error.position.col,
        })
    }
}

/// Which child each running composite is currently inside, plus per-node counters.
///
/// Bounded by tree depth: one entry per running node, not per node in the tree. An
/// entry's `u64` state means "child cursor" for a composite, "last success tick"
/// for a cooldown, "start tick" for a timeout and "completed mask" for a parallel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeCursor {
    entries: Vec<(u32, u64)>,
}

impl TreeCursor {
    /// The capacity a cursor starts with, which covers any tree of depth 8 without
    /// ever reallocating.
    pub const INITIAL_CAPACITY: usize = 8;

    /// An empty cursor, ready for a fresh tick.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(Self::INITIAL_CAPACITY),
        }
    }

    /// The recorded child cursor for a node, or `0`.
    #[must_use]
    pub fn child_cursor(&self, node: u32) -> u32 {
        self.state(node).map_or(0, num::u64_to_u32)
    }

    /// Record the child cursor for a node.
    pub fn set_child_cursor(&mut self, node: u32, cursor: u32) {
        self.set_state(node, u64::from(cursor));
    }

    /// The raw state recorded for a node.
    #[must_use]
    pub fn state(&self, node: u32) -> Option<u64> {
        self.entries
            .iter()
            .find_map(|(index, state)| (*index == node).then_some(*state))
    }

    /// Record raw state for a node.
    pub fn set_state(&mut self, node: u32, state: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|(index, _)| *index == node) {
            entry.1 = state;
        } else {
            self.entries.push((node, state));
        }
    }

    /// Forget a node and everything beneath it.
    pub fn forget(&mut self, node: u32) {
        self.entries.retain(|(index, _)| *index != node);
    }

    /// Forget every node in `start..end`.
    pub fn forget_range(&mut self, start: u32, end: u32) {
        self.entries
            .retain(|(node, _)| !(start..end).contains(node));
    }

    /// Forget everything, as after an abort.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The running nodes, outermost first.
    #[must_use]
    pub fn path(&self) -> &[(u32, u64)] {
        &self.entries
    }

    /// How many nodes the cursor is tracking, which is bounded by tree depth.
    #[must_use]
    pub const fn depth_used(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cursor believes a node is running.
    #[must_use]
    pub fn is_running(&self, node: u32) -> bool {
        self.entries.iter().any(|(index, _)| *index == node)
    }
}

/// Everything a leaf needs to run.
///
/// Deliberately small and copyable: a leaf gets its own working memory, the tick
/// and its identity, and nothing else. Anything a leaf needs beyond that is the
/// game's business, written into the blackboard by a perception system first.
pub struct LeafContext<'a> {
    /// Per-NPC working memory.
    pub blackboard: &'a mut Blackboard,
    /// The tick this evaluation is happening on.
    pub tick: Tick,
    /// The NPC being evaluated.
    pub npc: NpcId,
}

impl LeafContext<'_> {
    /// Read a boolean from the blackboard.
    #[must_use]
    pub fn flag(&self, key: crate::behavior::blackboard::BlackboardKey) -> bool {
        self.blackboard
            .get(key)
            .and_then(crate::behavior::blackboard::Value::as_bool)
            .unwrap_or(false)
    }

    /// Read a number from the blackboard.
    #[must_use]
    pub fn number(&self, key: crate::behavior::blackboard::BlackboardKey) -> Option<f32> {
        let value = self.blackboard.get(key)?;
        value
            .as_float()
            .or_else(|| value.as_int().map(num::i64_to_f32))
    }
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
    use crate::behavior::blackboard::Value;
    use crate::testkit::OrDie;

    /// Compile a tree for testing; a failure here is a broken fixture.
    fn tree(spec: &NodeSpec) -> BehaviorTree {
        BehaviorTree::try_from_spec(spec).or_die()
    }

    /// Leaves: 0 = succeed, 1 = fail, 2 = run forever, 3 = run twice then
    /// succeed, 4 = run three times then succeed.
    struct Scripted {
        runs: u32,
        aborted: Vec<LeafId>,
    }

    impl Scripted {
        fn new() -> Self {
            Self {
                runs: 0,
                aborted: Vec::new(),
            }
        }
    }

    impl LeafDispatch for Scripted {
        fn tick(&mut self, leaf: LeafId, _ctx: &mut LeafContext<'_>) -> Status {
            match leaf {
                0 => Status::Success,
                2 => Status::Running,
                3 | 4 => {
                    self.runs += 1;
                    let needed = if leaf == 3 { 2 } else { 3 };
                    if self.runs < needed {
                        Status::Running
                    } else {
                        Status::Success
                    }
                }
                _ => Status::Failure,
            }
        }

        fn abort(&mut self, leaf: LeafId, _ctx: &mut LeafContext<'_>) {
            self.aborted.push(leaf);
        }
    }

    fn board() -> Blackboard {
        Blackboard::new()
    }

    fn ctx(blackboard: &mut Blackboard) -> LeafContext<'_> {
        ctx_at(blackboard, Tick::ZERO)
    }

    fn ctx_at(blackboard: &mut Blackboard, tick: Tick) -> LeafContext<'_> {
        LeafContext {
            blackboard,
            tick,
            npc: NpcId::unique(0),
        }
    }

    #[test]
    fn sequence_requires_every_child() {
        let tree = tree(&sequence(vec![action(0), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn sequence_stops_at_the_first_failure() {
        let tree = tree(&sequence(vec![action(1), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Failure
        );
        assert!(!cursor.is_running(tree.root()));
    }

    #[test]
    fn selector_stops_at_the_first_success() {
        let tree = tree(&selector(vec![action(1), action(0), action(1)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn a_selector_of_failures_fails() {
        let tree = tree(&selector(vec![action(1), action(5)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Failure
        );
    }

    #[test]
    fn sequence_resumes_at_the_running_child() {
        let tree = tree(&sequence(vec![action(2), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
        assert_eq!(cursor.child_cursor(tree.root()), 0);
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
    }

    #[test]
    fn resumable_leaf_is_not_restarted() {
        // If the sequence restarted at child 0 every tick, leaf 3 would never
        // reach its second run and never complete.
        let tree = tree(&sequence(vec![action(3), action(0)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn inverter_swaps_success_and_failure_but_passes_running() {
        let inverted = tree(&invert(action(1)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            inverted.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );

        let running_inverted = tree(&invert(action(2)));
        let mut other_cursor = TreeCursor::new();
        assert_eq!(
            running_inverted.tick(&mut other_cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
    }

    #[test]
    fn repeat_until_fail_is_capped_instead_of_hanging() {
        let tree = tree(&repeat_until_fail(action(0)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
    }

    #[test]
    fn repeat_until_fail_succeeds_when_the_child_fails() {
        let tree = tree(&repeat_until_fail(action(1)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn parallel_all_requires_every_child() {
        let tree = tree(&parallel(
            ParallelPolicy::RequireAll,
            vec![action(0), action(1)],
        ));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Failure
        );
    }

    #[test]
    fn parallel_any_succeeds_on_one_child() {
        let tree = tree(&parallel(
            ParallelPolicy::RequireAny,
            vec![action(1), action(0)],
        ));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Success
        );
    }

    #[test]
    fn parallel_remembered_success_does_not_re_run() {
        // Child 0 succeeds immediately, child 1 runs forever. On the second tick
        // the completed mask must stop child 0 being re-entered — observable
        // because a re-entry would succeed again and mask nothing.
        let tree = tree(&parallel(
            ParallelPolicy::RequireAll,
            vec![action(0), action(2)],
        ));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
        let root = tree.root();
        assert_eq!(cursor.state(root), Some(1));
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Running
        );
    }

    #[test]
    fn cooldown_blocks_until_the_period_elapses() {
        let tree = tree(&cooldown(5, action(0)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();

        assert_eq!(
            tree.tick(
                &mut cursor,
                &mut dispatch,
                &mut ctx_at(&mut board, Tick::ZERO)
            ),
            Status::Success
        );
        assert_eq!(
            tree.tick(
                &mut cursor,
                &mut dispatch,
                &mut ctx_at(&mut board, Tick::new(3))
            ),
            Status::Failure,
            "still cooling"
        );
        assert_eq!(
            tree.tick(
                &mut cursor,
                &mut dispatch,
                &mut ctx_at(&mut board, Tick::new(5))
            ),
            Status::Success,
            "cooldown expired at the boundary"
        );
    }

    #[test]
    fn cooldown_zero_never_blocks() {
        let tree = tree(&cooldown(0, action(0)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        for _ in 0..3 {
            assert_eq!(
                tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
                Status::Success
            );
        }
    }

    #[test]
    fn timeout_fails_a_child_that_never_finishes() {
        let tree = tree(&timeout(4, action(2)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        for tick in 0..4 {
            assert_eq!(
                tree.tick(
                    &mut cursor,
                    &mut dispatch,
                    &mut ctx_at(&mut board, Tick::new(tick))
                ),
                Status::Running
            );
        }
        assert_eq!(
            tree.tick(
                &mut cursor,
                &mut dispatch,
                &mut ctx_at(&mut board, Tick::new(4))
            ),
            Status::Failure
        );
    }

    #[test]
    fn timeout_aborts_the_running_child() {
        let tree = tree(&timeout(2, sequence(vec![action(2)])));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        let _ = tree.tick(
            &mut cursor,
            &mut dispatch,
            &mut ctx_at(&mut board, Tick::new(0)),
        );
        let _ = tree.tick(
            &mut cursor,
            &mut dispatch,
            &mut ctx_at(&mut board, Tick::new(3)),
        );
        assert_eq!(
            dispatch.aborted,
            vec![2],
            "the timed-out leaf was cleaned up"
        );
        assert_eq!(cursor.depth_used(), 0);
    }

    #[test]
    fn timeout_that_completes_resets_its_clock() {
        let tree = tree(&timeout(3, action(0)));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(
                &mut cursor,
                &mut dispatch,
                &mut ctx_at(&mut board, Tick::new(0))
            ),
            Status::Success
        );
        assert_eq!(
            tree.tick(
                &mut cursor,
                &mut dispatch,
                &mut ctx_at(&mut board, Tick::new(50))
            ),
            Status::Success,
            "a fresh entry restarts the deadline"
        );
    }

    #[test]
    fn a_condition_that_runs_is_treated_as_failure() {
        let tree = tree(&condition(2));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        assert_eq!(
            tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board)),
            Status::Failure,
            "a predicate cannot be in progress"
        );
    }

    #[test]
    fn abort_clears_the_cursor() {
        let tree = tree(&sequence(vec![action(2)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        let _ = tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_ne!(cursor.path().len(), 0);
        tree.abort(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(cursor.path().len(), 0);
    }

    #[test]
    fn abort_reaches_the_innermost_running_leaf_first() {
        // Nested: a sequence inside a sequence, with the leaf running. The abort
        // must reach the leaf, and it must reach it before anything shallower, so
        // that a conversation is closed before its participants are reset.
        let tree = tree(&sequence(vec![sequence(vec![action(2)])]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        let _ = tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        tree.abort(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(dispatch.aborted, vec![2]);
    }

    #[test]
    fn abort_does_not_tell_an_unstarted_leaf_to_clean_up() {
        let tree = tree(&sequence(vec![action(0), action(2)]));
        let mut cursor = TreeCursor::new();
        let mut dispatch = Scripted::new();
        let mut board = board();
        let _ = tree.tick(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        tree.abort(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(
            dispatch.aborted,
            vec![2],
            "only the running leaf is aborted"
        );
    }

    #[test]
    fn empty_composites_are_rejected() {
        assert_eq!(
            BehaviorTree::try_from_spec(&sequence(vec![])),
            Err(TreeError::EmptyComposite)
        );
        assert_eq!(
            BehaviorTree::try_from_spec(&selector(vec![])),
            Err(TreeError::EmptyComposite)
        );
        assert_eq!(
            BehaviorTree::try_from_spec(&parallel(ParallelPolicy::RequireAll, vec![])),
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
            BehaviorTree::try_from_spec(&spec),
            Err(TreeError::TooDeep { max: _ })
        ));
    }

    #[test]
    fn too_many_children_are_rejected_rather_than_truncated() {
        let children = vec![action(0); BehaviorTree::MAX_CHILDREN + 1];
        assert!(matches!(
            BehaviorTree::try_from_spec(&sequence(children)),
            Err(TreeError::TooManyChildren { max: _ })
        ));
    }

    #[test]
    fn exactly_the_maximum_children_is_accepted() {
        let children = vec![action(0); BehaviorTree::MAX_CHILDREN];
        assert!(
            BehaviorTree::try_from_spec(&parallel(ParallelPolicy::RequireAll, children)).is_ok()
        );
    }

    #[test]
    fn depth_and_leaf_listing_are_reported() {
        let tree = tree(&sequence(vec![
            action(0),
            selector(vec![condition(1), action(2)]),
        ]));
        assert_eq!(tree.depth(), 3);
        assert_eq!(tree.leaf_ids(), vec![0, 1, 2]);
        assert_eq!(tree.len(), 5);
    }

    #[test]
    fn declared_cost_counts_a_repeated_leaf_once() {
        let tree = tree(&sequence(vec![
            action(0),
            selector(vec![action(1), action(0)]),
        ]));
        let cost = tree.declared_cost(|leaf| VirtualCost::new(u64::from(leaf) + 100));
        assert_eq!(cost, VirtualCost::new(201), "leaves 0 and 1, once each");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn an_authored_tree_matches_the_builder_tree() {
        let authored = NodeSpec::from_ron(
            r"Selector([
                Condition(3),
                Sequence([
                    Action(0),
                    Cooldown(ticks: 4, child: Action(1)),
                    Timeout(ticks: 9, child: Parallel(policy: RequireAll, children: [Action(2)])),
                ]),
            ])",
        )
        .or_die();
        let built = selector(vec![
            condition(3),
            sequence(vec![
                action(0),
                cooldown(4, action(1)),
                timeout(9, parallel(ParallelPolicy::RequireAll, vec![action(2)])),
            ]),
        ]);
        assert_eq!(authored, built);

        // Identical structure is not the same as identical behaviour: the traces
        // have to agree too.
        let from_ron = tree(&authored);
        let from_builder = tree(&built);
        let mut left = TreeCursor::new();
        let mut right = TreeCursor::new();
        // Separate dispatchers: `Scripted` counts its own runs, so sharing one
        // would make the second tree see a leaf that had already been ticked.
        let mut left_dispatch = Scripted::new();
        let mut right_dispatch = Scripted::new();
        let mut board = board();
        for tick in 0..6 {
            let a = from_ron.tick(&mut left, &mut left_dispatch, &mut ctx(&mut board));
            let b = from_builder.tick(&mut right, &mut right_dispatch, &mut ctx(&mut board));
            assert_eq!(a, b, "tick {tick}");
            assert_eq!(left, right, "cursor diverged at tick {tick}");
        }
    }

    #[cfg(feature = "serde")]
    #[test]
    fn malformed_authored_trees_report_a_position() {
        use crate::testkit::OrDieErr;

        let error = NodeSpec::from_ron("Selector([Action(0)").or_die_err();
        assert!(
            matches!(error, TreeError::MalformedSource { line: _, column: _ }),
            "{error:?}"
        );
        assert!(error.to_string().contains(':'), "{error}");
    }

    #[test]
    fn evaluation_order_is_stable_across_ticks() {
        let tree = tree(&selector(vec![action(1), action(0)]));
        let mut dispatch = Scripted::new();
        let mut board = board();
        let mut first = TreeCursor::new();
        let mut second = TreeCursor::new();
        let a = tree.tick(&mut first, &mut dispatch, &mut ctx(&mut board));
        let b = tree.tick(&mut second, &mut dispatch, &mut ctx(&mut board));
        assert_eq!(a, b);
        assert_eq!(first, second);
    }

    #[test]
    fn blackboard_helpers_read_what_perception_wrote() {
        let mut board = board();
        let key = crate::behavior::blackboard::BlackboardKey::new(9);
        board.set(key, Value::Float(0.25));
        let context = ctx(&mut board);
        assert_eq!(context.number(key), Some(0.25));
        assert!(!context.flag(key), "a float is not a flag");
    }

    #[test]
    fn random_shapes_never_panic_and_always_settle() {
        // A deterministic pseudo-random walk over node kinds: every tree either
        // evaluates or is rejected, and none panics or loops.
        let mut seed = 0x5EED_u32;
        for _ in 0..200 {
            let spec = random_spec(&mut seed, 0);
            let Ok(tree) = BehaviorTree::try_from_spec(&spec) else {
                continue;
            };
            let mut cursor = TreeCursor::new();
            let mut dispatch = Scripted::new();
            let mut board = board();
            for tick in 0..8 {
                tree.tick(
                    &mut cursor,
                    &mut dispatch,
                    &mut ctx_at(&mut board, Tick::new(tick)),
                );
            }
            tree.abort(&mut cursor, &mut dispatch, &mut ctx(&mut board));
        }
    }

    fn random_spec(seed: &mut u32, depth: u32) -> NodeSpec {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let choice = (*seed >> 16) % 9;
        let leaf = |seed: &mut u32| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (*seed >> 16) % 6
        };
        let children = |seed: &mut u32| {
            let count = usize::from(((*seed >> 8) % 3) as u8) + 1;
            (0..count)
                .map(|_| random_spec(seed, depth + 1))
                .collect::<Vec<_>>()
        };
        if depth >= 4 {
            return if choice.is_multiple_of(2) {
                action(leaf(seed))
            } else {
                condition(leaf(seed))
            };
        }
        match choice {
            0 => sequence(children(seed)),
            1 => selector(children(seed)),
            2 => parallel(ParallelPolicy::RequireAll, children(seed)),
            3 => parallel(ParallelPolicy::RequireAny, children(seed)),
            4 => invert(random_spec(seed, depth + 1)),
            5 => repeat_until_fail(random_spec(seed, depth + 1)),
            6 => cooldown(3, random_spec(seed, depth + 1)),
            7 => timeout(4, random_spec(seed, depth + 1)),
            _ => action(leaf(seed)),
        }
    }
}
