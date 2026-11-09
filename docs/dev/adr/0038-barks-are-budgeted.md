# ADR 0038 — Barks are a budgeted, cooldown-gated channel

**Status:** Accepted
**Date:** 2026-11-05

## Context

Barks — "nice weather", "you again", "thief!" — are the cheapest way to make a crowd
feel inhabited and the fastest way to make it unbearable. The failure mode is not a
bug, it is arithmetic: two hundred NPCs noticing the player in the same second produce
two hundred lines, the audio mixer saturates, and the player turns the voices off
forever.

The second failure is repetition. An NPC with five greetings who says the same one
every time is worse than an NPC who says nothing.

## Decision

Barks go through a single choke point, in three stages:

1. **Request.** Game code asks; it does not speak. Requests queue in a bounded deque
   (1024) and a full queue drops with a counter rather than growing.
2. **Gate.** Per NPC, per category cooldowns (`BarkState`) with per-category windows,
   so an alarm can fire while a greeting is still on cooldown. Per frame, a cap
   (`Barks::max_per_tick`, default 8) bounds how much of the crowd can speak at once.
   Requests over the cap stay queued for the next tick rather than being dropped: a
   town's background chatter should not depend on how many NPCs happened to notice at
   the same instant.
3. **Selection.** A line is picked from the category's table, avoiding the NPC's own
   last line. The picker takes `&mut RngStream`, so selection is deterministic per
   NPC and reproducible in a replay.

The state lives in a component (`NpcBarks`), not in the resource, because it is per
person: a shared `BarkState` would give the whole crowd one cooldown, the same failure
the leaf contract in ADR 0035 exists to prevent.

## Consequences

* A crowd of ten thousand produces at most eight lines a tick, by construction.
* `Barks::suppressed()` and the queue depth are observable, so a game can tune the cap
  against its mixer rather than guessing.
* Determinism holds because selection is stream-based and the request order is the
  order game code submitted them.

## Alternatives considered

* **Speak on the spot.** Simplest, and it puts an unbounded amount of audio in one
  frame the first time a hundred NPCs see the player.
* **Random suppression instead of a cap.** Randomness makes the frame cost vary and
  makes a replay disagree with the original run.
