# ADR 0037, Dialog graphs are validated before they are played

**Status:** Accepted
**Date:** 2026-11-05

## Context

Authored conversation is content, and content has bugs: a choice pointing at a node
that does not exist, a node no path reaches, a cycle with no exit. In a shipped game
those become a soft-lock in a quest the tester did not walk, discovered by a player.

The alternative, checking at runtime, when a choice is taken, moves the failure to
the worst possible moment: mid-conversation, in front of the player.

## Decision

`DialogGraph::new` (and `from_ron`) validate the whole graph up front and refuse the
bad ones with a typed error carrying the offending node or choice:

* every choice's `next` resolves;
* the root exists;
* every node is reachable from the root, because an unreachable node is authored
  content that will never be seen, a silent content bug;
* choice conditions reference keys the session can evaluate, and are total: a missing
  blackboard slot means "not met", never a panic.

Playback is a `DialogSession` holding a cursor and the facts gathered so far. The
session never mutates the graph, so one graph is shared by every NPC running it, and a
session is `Clone` so a save can capture a conversation in progress.

Conditions read the NPC's `Blackboard`, which is the same memory the trees read. A
conversation can therefore change behaviour without a special channel: the shopkeeper
who learns your name sets a flag, and the tree that greets you reads it.

## Consequences

* Authoring errors are load errors, which is where a content pipeline can catch them.
* A graph is immutable after construction; hot-reload means building a new graph and
  swapping the resource, which is a deliberate cost.
* Reachability checking makes "draft" nodes impossible without marking them. That is
  the intended pressure: a node nobody can reach is either a bug or not yet wired, and
  both deserve attention before shipping.

## Alternatives considered

* **Validate on first play.** Cheap at load, and it moves the failure to the player.
* **No validation.** Rejected: the errors above are exactly the ones that survive
  playtests, because they hide in branches nobody took.
