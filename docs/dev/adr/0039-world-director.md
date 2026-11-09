# ADR 0039, The world director decides, and something else applies

**Status:** Accepted
**Date:** 2026-11-05

## Context

Populations need a hand: a festival every few hours, a patrol when tension rises, a
panic when an alarm spreads, a quiet hour after midnight. Done ad hoc, each of those is
a system that mutates the world at an unpredictable point in the frame, and the
simulation stops being reproducible.

The other requirement is legibility. "Why is everyone suddenly fleeing?" should have an
answer that is not a debugger session.

## Decision

The director is a policy behind a trait:

```rust
fn decide(&mut self, inputs: &DirectorInputs, out: &mut DirectorCommands);
```

It returns commands; it does not touch the world. `DirectorBoard` owns the state
(clock, alarm field, trace), applies the commands, and keeps a bounded trace of the
last 256 with the tick they were issued on.

Inputs are a small, cheap snapshot: tick, population, tier counts, mean mood, tension,
alarm count and ticks since the player was last seen. The crowd is sampled once a
second (`CrowdPulse`), not every frame, so a million-NPC town costs the same to direct
as a village.

Alarms live in a bounded `AlarmField` and decay with time. `fear_at(position, tick)` is
available to game code and to utility considerations, which is what makes a fire spread
through a crowd as behaviour rather than as a scripted event.

The default policy is `AmbientDirector`: a working day, with the population busier at
midday and quiet after dark. A game replaces it with `set_policy` and keeps the board,
the trace and the alarm field.

## Consequences

* Deterministic replay: given the same inputs, a policy produces the same commands in
  the same order.
* Because commands are values, a director can be tested by asserting on them, and a
  trace can be printed in a bug report.
* The ECS adapter (`world_director`) is thin: it builds inputs, runs the policy and
  publishes the mood and alarms as resources. Nothing iterates the crowd.

## Alternatives considered

* **Directors as Bevy systems.** Easier to write, and impossible to replay or test
  without a world.
* **Directors that mutate entities.** Rejected for the same reason: a decision you
  cannot record is a decision you cannot debug.
