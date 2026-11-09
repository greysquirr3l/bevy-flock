# 0042, Explain on demand, draw nothing, fuzz the decoders

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** ADR 0009 (scheduler), ADR 0012–0014 (save format and compatibility),
  ADR 0022 (boundary harness), ADR 0040 (ports, not dependencies), ADR 0041 (memory)

## Three problems, one shape

A crowd engine has three debugging problems that look unrelated:

1. **"Why did that NPC do that?"** The tree path says which branch ran; it does not say
   that the branch ran because the NPC was hungry, in a bad mood, standing next to a
   remembered friend, in a group whose leader had died.
2. **"What is the crowd doing right now?"** Counters exist (ADR 0033's pulse), but a
   number in a log is not an overlay on the screen where the bug is.
3. **"What happens if the save file is garbage?"** A save arrives from disk, a cloud sync
   or a friend's machine. It is untrusted input, but it is not a network packet, so it is
   usually parsed with the assumption that it is fine.

The shared shape: each answer is expensive, rare, and must not be on the hot path.

## Decisions

### 1. Explain is a function, not a component

`explain_npc(&mut World, Entity) -> Result<String, ExplainError>` walks the entity's
components, identity, tier, think weight, tree size and cursor depth, mood, needs, group,
every blackboard slot sorted by key, memory row, and formats one human-readable block.

It takes `&mut World` because the answer spans components no single system owns, and it
allocates a `String` because the consumer is a log line, a test assertion or a debug
overlay. That is affordable precisely because nothing calls it per frame: the budget is
"when a human asks".

The alternative, a `WhyDidThat` component kept up to date every tick, would cost every
NPC every frame to answer a question about one NPC per playtest.

### 2. The overlay is data; the crate draws nothing

`collect_debug_overlay` fills `DebugOverlay { labels: Vec<DebugLabel> }` with the nearest
`max_labels` NPCs within `radius`, sorted by distance so the labels that matter win the
budget. The game draws them with whatever it already has. `DebugOverlayConfig::enabled`
defaults to **false**: the system runs, clears the list and returns, which is one branch
per frame.

This is ADR 0040 applied to debugging. Shipping a gizmo dependency to draw npc 7's tier
would put a renderer in a crowd crate that deliberately has none.

### 3. Fuzz the decoders, and keep a stable suite that runs in CI

Two layers, because one of them is not enough:

- **`fuzz/`**, three libFuzzer targets (`save_decode`, `tree_ron`, `dialog_ron`) with the
  contract "error, never panic, never allocate wildly". Coverage-guided, nightly, run by
  hand or on a schedule.
- **`tests/fuzz_decoders.rs`**, the same contract on stable, with deterministic
  generators: every truncation of a valid file, one flipped bit per sampled byte, a lying
  header and version, 512 pseudo-random byte strings. These run in CI on every push.

The stable suite earns its place by being the layer that cannot be forgotten. It also
catches the specific failure that matters most for saves, a decoder that accepts a
*truncated* file, which is what a half-flushed write or a killed process produces. The
test asserts that **zero** truncations decode, and that every successful decode can be
re-encoded and decoded again, which is the save/load/save cycle a player actually performs.

## What was rejected

- **A tracing span per tick.** Useful, and cheap only until it is not: spans allocate, and
  ten thousand of them per frame is a different program. Games that want tracing wrap the
  systems they care about; the crate exposes the counters that make that decision easy.
- **Logging inside leaves.** A leaf that prints is a leaf that allocates. Explanation is a
  query over the state a leaf read, not an event it emitted.
- **Fuzzing only in nightly CI.** It would have been green for months and never run.
- **Asserting *which* error** a corrupt file produces. A decoder is allowed to be
  conservative; the contract is about not crashing, not about diagnosis.

## Consequences

- A bug report can carry `explain_npc` output, which turns "the guard ignored me" into a
  state dump.
- The overlay costs nothing when disabled, and its absence is a missing label rather than a
  missing frame.
- Corrupt saves fail closed. The one asymmetry worth noting: the stable suite cannot prove
  the absence of a panic in the same way the fuzzer can, so both layers stay.
