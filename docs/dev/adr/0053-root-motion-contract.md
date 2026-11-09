# ADR 0053: root motion is a decision the profile records

- Status: Accepted
- Date: 2026-11-09
- Supersedes: none
- Related: ADR 0052 (animation binding contract), ADR 0001 (deterministic identifiers)

## Context

Some walk cycles translate their own root: the animator moves the hips and the engine is expected to
take the hint and drop the translation, because the movement is already in the simulation. Others —
mocap, mostly — carry the movement in the clip and expect the engine to apply it and then place the
body's collision and AI where the clip put it.

Both are reasonable, and they are mutually exclusive. Whichever one the code does by accident, the
symptom is the same in kind and opposite in direction: either the body travels twice as far as its legs
(a clip and steering both moving it), or its legs cycle in place while the steering slides it (a clip's
translation being dropped when the game expected it applied).

The binding layer cannot decide this for a game. It also cannot leave it undecided, which is what the
"apply what you find" default would be.

## Decision

Root motion is a per-profile flag, `RootMotion`, with two values and a documented default.

- `RootMotion::Simulation` (default) — the simulation owns movement. A reported clip translation is
  **consumed and dropped** on the tick it arrives. A game with root-motion clips that never opts in
  gets a body that walks where the steering put it, not twice as far.
- `RootMotion::Clip` — the clip owns movement. The translation is quantised, added to the body's
  transform, and added to `Locomotion`'s distance walked, so the next tick's blend does not drag the
  legs back to where the steering thought they were.

Three supporting decisions make the flag usable rather than merely present.

**Extraction lives in the game, not here.** Bevy owns the skeleton and its joint hierarchy; reading
the root joint of an arbitrary rig is the game's animation bridge's job. The bridge calls
`RootOffset::report(world_translation)` and the crate decides what happens to it. This keeps the crate
free of an assumption about which joint is "the root".

**Application is idempotent by construction.** `RootOffset` is consumed, not read: `take()` clears the
report, so a bridge that runs twice in a frame cannot move the body twice, and a reconciler that runs
after a skipped tick cannot apply a stale offset twice over.

**Each tick's offset is quantised to a micrometre (`QUANTUM_METRES = 1e-6 m`).** Determinism is the
reason, not tidiness: a clip's root channel is `f32`, and the accumulated sum of a thousand
non-representable values is a rounding question that can answer differently on two machines or two
runs. Snapping per tick bounds the drift and makes the sequence a function of the clip data alone.
`two_runs_of_the_same_clips_produce_the_same_transform_and_digest` runs 240 deliberately non-round
offsets and compares transform bits and the `RootMotionTrace` digest.

## Consequences

- A game that ships root-motion clips without reading this ADR gets the safe behaviour and a
  `RootMotionTrace` with a non-zero `discarded` count and a zero-effect digest, which is the shape of
  the bug rather than a body moving twice.
- Vertical translation is ignored in both modes: the simulation has no opinion about floors, and a
  clip that lifts its root (a jump) must not move the body through the crowd's collision plane.
- The trace is a `u64` FNV-1a digest over applied micrometre pairs plus two counters. It is cheap
  enough to leave on in a shipping build and is what a replay comparison asserts on.
- Switching a profile from `Simulation` to `Clip` changes the meaning of a body's position, so
  existing saves and replays are not compatible across the flag. That is stated here rather than
  discovered: the flag belongs in the versioned snapshot alongside the profile's other numbers, and a
  save written under one value is not loadable under the other.
