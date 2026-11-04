# ADR 0006 — Behaviour trees *and* state machines, not one of them

- Status: Accepted
- Date: 2026-11-04

## Context

Three families are in common use: finite state machines, behaviour trees, and
utility scoring (as in `big-brain`). Each is good at something the others are bad
at:

- A state machine is excellent at "what mode am I in" with dwell times and
  transitions, and terrible at expressing "try these five things in order".
- A behaviour tree is excellent at hierarchical "try this, else that", and
  terrible at "stay in this mode for at least two seconds".
- Utility scoring is excellent at picking among many options by cost/benefit, and
  terrible at sequencing.

Every shipped NPC AI ends up with at least two of them, usually with one
pretending to be the other.

## Decision

Ship trees and state machines as separate, composable primitives:

- `BehaviorTree` answers *what do I do next*, with explicit resume semantics so a
  multi-tick action is not restarted every frame.
- `StateMachine` answers *what mode am I in*, with dwell times and ordered guards.
- Both are driven by the same per-NPC tick and the same `Blackboard`.

Utility scoring is specified in `plan.toml` as a scoring module that feeds the two
rather than replacing them.

## Consequences

- Resume semantics are the subtle part of any tree implementation, so they are
  stated in the module docs and tested per node type: a sequence resumes at the
  running child, a failure resets the cursor, an abort walks the cursor.
- Trees are compiled to a flat `Vec` with child ranges, so evaluation walks
  indices and never dereferences a box per node. Cursor memory is bounded by tree
  *depth*, not tree size — twenty bytes for a twenty-node tree.
- A repeat node is capped at 64 iterations per tick. An uncapped repeat over an
  always-succeeding child is a hang, and "the game froze" is not an acceptable
  failure mode for an AI primitive.
- Two concepts to learn instead of one. The module docs explain which question
  each answers, which is cheaper than a game discovering it at 3 a.m.

## Alternatives rejected

- **Trees only** — dwell times become "wait" leaves and state becomes a hack in
  the blackboard.
- **Utility only** — sequencing an animation into a conversation into a walk-away
  is not expressible without inventing a tree.
- **Scripting language** — a whole interpreter, a sandbox, and a debugger, in a
  crate whose selling point is that the simulation is inspectable Rust.
