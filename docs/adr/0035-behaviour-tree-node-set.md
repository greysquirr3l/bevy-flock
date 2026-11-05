# ADR 0035 — The behaviour tree node set, and why leaves are shared

**Status:** Accepted
**Date:** 2026-11-05
**Supersedes:** nothing. **Amends:** ADR 0006.

## Context

ADR 0006 chose trees plus state machines and left the node set open. Building the
engine made three questions concrete:

1. Which composites and decorators ship?
2. Where does per-NPC state live?
3. What stops a tree from being an allocation site on every tick?

The third is the one that decides the other two. A tree that allocates a vector of
children per evaluation cannot run ten thousand times a frame, and a tree whose nodes
are boxed per NPC cannot be shared across a crowd that all run the same brain.

## Decision

Ship six composites and four decorators — `Sequence`, `Selector`, `Parallel`,
`SequenceWithMemory`, `SelectorWithMemory`, `RandomSelector`; `Inverter`,
`Succeeder`, `Repeat`, `Timeout` — and no more. A node that only one game needs is a
node that game should write, which is why `LeafBehavior` is public and the node set is
not extensible by design.

Leaves are **stateless and shared**. `LeafBehavior::tick` takes `&self`: one instance
serves every NPC running it. All per-NPC memory lives in the `Blackboard`, keyed by
`BlackboardKey`, bounded at sixteen slots. A leaf that kept a countdown in a field
would give the whole crowd one countdown — a bug that is invisible until two NPCs move
in lockstep, which is why the contract is stated in the trait's documentation rather
than left as folklore.

Evaluation is **allocation-free**. A compiled tree is a flat `Vec<NodeRecord>` with
child ranges, walked by a `TreeCursor` that holds a fixed-size stack. No recursion, no
boxing, no per-tick allocation; `tests/tree_no_alloc.rs` installs a counting allocator
and fails the build's test run if a tick allocates.

Every leaf declares a cost in virtual nanoseconds. The default is deliberately
pessimistic (40 ns against a measured 3–9 ns for the built-ins) so an undeclared leaf
cannot hide as cheap and quietly take more than its share of the think budget.

## Consequences

* A tree can be shared by the entire crowd and evaluated inside a budgeted slice.
* Leaves cannot hold state, so anything time-based must be blackboard-backed. This is
  the most common mistake when writing a new leaf, and it is documented on the trait.
* Trees are authored as `NodeSpec` and compiled once; RON authoring is available under
  the `serde` feature and validated at load, not at first tick.

## Alternatives considered

* **Boxed, per-NPC trees.** Flexible, and the obvious first implementation. Rejected on
  measurement: the first crowd benchmark spent more time in the allocator than in the
  leaves.
* **Behaviour trees with per-node state.** Convenient for timeouts and repeats, and
  rejected for the shared-countdown failure above. The shipped `Timeout` and `Repeat`
  decorators keep their timers on the cursor's frame stack instead, which is per-NPC
  by construction and costs nothing to allocate.
