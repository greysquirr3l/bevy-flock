# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A complete behaviour-tree node set: `Sequence`, `Selector`, `Parallel` with
  `RequireAll`/`RequireAny`, `Inverter`, `RepeatUntilFail`, `Cooldown`, `Timeout`,
  `Condition` and `Action`. Compiled to a flat node array; a tick allocates
  nothing, which `tests/tree_no_alloc.rs` measures rather than assumes.
- Abort semantics that reach running leaves deepest-first, with subtree ranges in
  the compiled layout so a timeout can cancel the work underneath it.
- `NodeSpec::from_ron` behind the `serde` feature, so a designer can author a tree
  without recompiling; a golden test asserts it evaluates identically to the
  builder equivalent.
- `VirtualCost`, the declared-cost newtype the think scheduler will spend
  (ADR 0009), plus `LeafBehavior::cost_hint` and `FlockRegistry::tree_cost`.
- `StandardLeaves`: sixteen built-in leaves (actions and predicates) that are
  stateless by construction, so one registered instance serves the whole crowd,
  each with a cost calibrated against `benches/brain.rs`.
- `Archetype::logic`: a behaviour tree per archetype with a pinned declared cost —
  guard 27 vns, merchant 46, urchin 36, noble 35, wanderer 35.
- `NpcBrain`: a component holding a shared tree, a per-NPC cursor and the cached
  cost, ready for the scheduler task to drive.

- Social memory behind the `memory` feature (ADR 0010): `MemoryStore`, a flat
  array of 16-byte slots indexed by eligibility and subject, with lazy integer
  decay, a LIFO free list, and exact `implant`/`release` round-tripping.
- `MemoryArchive`, a bounded cold tier that evicts the faintest record by lowest
  total familiarity then lowest `NpcId`, so eviction is a pure function of what is
  stored.
- `DecayPolicy` with half-life in ticks and a `NONE` variant for grudges that never
  fade; memory decays to exactly zero rather than to a permanent sliver.
- `benches/memory.rs`: remember 7.7 ns, recall 5.5 ns, decayed recall 8.5 ns, sweep
  6.9 ns per NPC over 16,384 eligible NPCs.

### Changed

- `LeafBehavior::tick` and `abort` take `&self`: a leaf is a shared function over
  the blackboard, not per-NPC state. This makes the "one instance, whole crowd"
  model explicit instead of a footgun.
- `VirtualCost::DEFAULT` is 40 vns — several times the priciest built-in leaf —
  rather than an arbitrary 1,000.

- `MemorySnapshot` (ADR 0012): the save format. Little-endian, magic `BFLK`, version
  1, records sorted by identity, times stored as ages so a save/load round trip
  does not age a grudge, CRC32 on header and body, and typed errors naming the
  reason — bad magic, unknown version, truncation, checksum, duplicate identity,
  implausible count, capacity, subject range.
- `SavedSlot`, `SaveRecord`, `Origin` and `LoadReport`; `rebase_offline` for a
  game that wants the world to have moved on while it was closed.
- A format test pins the size claim: 10,000 NPCs with two relationships each is
  430,032 bytes, not "roughly 430 KB".

- Format version 2 (ADR 0013): a sectioned container with a subject registry, so a
  content patch that adds, removes or reorders subjects translates memories instead
  of silently re-pointing them at the wrong person.
- `SubjectRegistry`, `SubjectEntry`, `SubjectKey`, `SubjectKind`: dense identifiers
  at runtime, stable keys in the file, labels for humans only.
- `LoadReport` replaces `RestoreReport`, and reports `dropped_slots`, `translated`
  and `unverified_mapping` so a lossy load says what it lost.
- `LegacySubjectPolicy::{Refuse, AssumeDense}`: a version 1 file has no keys, so it
  is refused by default and loaded only when the game says so out loud.
- Unknown sections are skipped, so additive format changes no longer need a version.

- `benches/saves.rs`: version 1 against version 2 at 50,000 NPCs. The container costs
  0.4 ms of encode and 0.4 ms of decode over version 1 for 140 extra bytes; the
  original whole-file trailer checksum cost 6.7 ms of a 14.9 ms encode and now covers
  the header and table only.
- `registry_telemetry` example and `domain::telemetry`: ghost rows, orphan subjects
  and orphan slots as permille integers, plus a dependency-free SVG of accumulation
  over twelve sessions and references per subject.
- `tests/save_compat.rs` with a committed fixture corpus and `BLESS_SAVES=1` blessing
  (ADR 0014): the frozen writer, a version 1 file that still loads, a future version
  refused by number, an unknown section skipped, every bit flipped and every prefix
  tried without a panic.
- ADR 0014 (compatibility testing) and ADR 0015 (memory diagnostics).

- `MAX_SUBJECTS` raised from 16 to 64 (ADR 0016): a player, a party, a faction and its
  officers fit in one store. Sizing stays per request, so a game asking for four pays
  for four.
- `MemoryStore::footprint()`: `rows × (subjects × 16 + 8)` bytes, reported rather than
  estimated, with tests pinning the model and the 4× cost of sixty-four subjects.
- `benches/memory.rs` `memory_stride`: the sweep costs the same on ghost rows at 16 and
  64 subjects, and 3.3× on populated rows (71 µs → 232 µs at 4,096 rows).
- The telemetry example's layout is computed from the subject count rather than
  hard-coded: sixty-four columns used to put the panel 600 pixels off the canvas.

- `compat/`: the forward direction of save compatibility (ADR 0014, layer 4). Its own
  workspace, pinned to the `v0.1.0-rc.1` tag rather than a path, run in CI over the
  committed corpus: loads what it understands, refuses what it does not by name, never
  panics. First release candidate tagged.
- The telemetry example's chart is derived from the subject count, and its render is
  committed at `docs/images/registry-telemetry.png` so a layout regression is visible.

- `application::schedule::ThinkScheduler`: the virtual-cost fair-queue think scheduler
  (ADR 0009). A min-heap of `(virtual finish time, id)`, one think per participant per
  tick, weights that buy proportional share, and a starvation bound measured rather
  than claimed. 11 tests; `benches/think.rs` covers 1,000 to 100,000 participants.
- Measured: a 16.6 ms virtual budget serves 88.7% of 10,000 NPCs per tick (worst wait
  3 ticks) and 12.1% of 100,000 (worst wait 45 ticks), with the planner itself costing
  1.34 ms and 1.83 ms respectively — the number that makes this a budget rather than a
  trick.

- `domain::weight`: the proximity weight policy the scheduler's numbers were always
  missing. Distance bands (close / nearby / distant / background) with hysteresis, so an
  NPC on a boundary keeps the weight it earned instead of flickering between 8 and 1
  forty times a second. Six tests.
- ADR 0017 proposes tiered cognition — reflex, reactive, deliberative, social — where the
  tier selects the *logic* and the weight selects the *cadence*, with a reserved budget
  share per tier. Measured: ~7,400 full deliberation thinks fit in a 16.6 ms frame;
  a 95/4/1 tier mix fits ~110,000 because the average think drops from 2.25 µs to 150 ns.

- The ECS half of the scheduler: `NpcBlackboard`, `NpcThinkWeight`, `PlayerFocus`,
  `ThinkSchedule` and three ordered systems. Weights are measured from distance to the
  nearest player with hysteresis; costs come from each brain; only the chosen trees tick.
  A game opts in by adding a brain, which keeps ten thousand brainless crowd agents the
  cheap default.
- Measured end to end in a real `App`: 10,000 brains add 0.73 ms to a 4.28 ms frame.
  Comfortable maximum is **7,000 deliberative thinkers per tick** at a 14 ms budget and a
  2 µs deliberation; 6,000 with headroom.

- Tiered cognition (ADR 0017): `CognitionTier`, `TierPolicy` with hysteresis and
  engagement, `BudgetShares` with reserved per-tier budgets, `TieredSchedule` with one
  queue per tier, and the ECS systems that assign tiers under a promotion ration.
- Measured: at 10,000 NPCs a 14 ms budget spends 2.17 ms of frame on reflex (every NPC
  thinks every tick), 0.62 ms on reactive, 0.54 ms on deliberative and 0.45 ms on social.
  The reflex ceiling is ~64,000 per tick, not the ~93,000 arithmetic suggested — the
  difference is the cost of reaching the components.

- ADR 0018 proposes the ambient continuum: population outside a manifest band becomes
  per-cell density advected by the flow field, manifested and folded deterministically,
  with a packed reflex batch for the band itself.
- Full-suite measurement at 64,000 and 100,000 (see `docs/performance.md`): 64,000 reflex
  agents thinking every tick cost 11.44 ms of frame; 100,000 cost 14.01 ms. The crowd path
  with no brains at all costs **198 ms** at 100,000 — the real ceiling, and the one the
  continuum is aimed at.

- `domain::continuum`: ambient population as twelve-byte cells with exact integer
  conservation, deterministic manifestation and O(1) folding. Advection of a million
  inhabitants over 40,000 cells measures 0.315 ms per stride; manifesting 2,000 identities
  is 10.5 µs.
- `MemoryStore::sweep_keeping` / `age` (ADR 0019): reclamation is a store-level cadence
  with an anchor exemption, so NPCs nobody simulates cannot leak rows, and anchored
  memories fade in warmth without losing their record.
- Gate 8 of ADR 0018 **fails**: a 5,000-entity manifest band costs 9.47 ms because the
  crowd path costs ~1.9 ms per 1,000 steered NPCs. See `docs/gates/continuum.md`.

- `application::packed`: steering over contiguous arrays with a bucket grid. 5,000 agents
  steer, integrate and advance in **1.50 ms**, against 9.47 ms through the ECS crowd path.
  Density matters more than count: the same 5,000 in a 128 m square costs 10.92 ms.
- ADR 0020 specifies the manifest band plugin: radius derived from a target density,
  ownership table, system ordering, seven requirements, and how each open gate closes.
- `tests/memory_aging.rs`: four integration tests over long-duration simulation, covering
  the absent-NPC leak, the anchored grudge that outlives an hour of silence, per-owner
  exemption, and bounded reclamation work.

- `BandPlugin` (ADR 0020) wires the packed kernel into the ECS: fold, manifest and steer in
  one ordered set. Gate 8 **passes**: 100,000 inhabitants with 5,000 manifested measure
  **3.37 ms**, and 250,000 measure the same — flat in total population.
- ADR 0021 proposes concentric bands (near 60 Hz, mid 20 Hz, far as density) to take the
  same flatness past 250,000.

### Documentation

- ADR 0011: how social memory integrates with the virtual-cost stride scheduler —
  cost as declared leaves, weight as a clamped boost, eligibility following the
  scheduler, and the feedback loop those rules close.

### Planned

The budgeted think scheduler, the utility scorer, the continuum ambient tier,
dialogue graphs and barks, snapshots and replay, rollback, and the render, `avian`
and `replicon` adapters are specified in `plan.toml` and not implemented yet.

## [0.1.0] — 2026-11-04

The simulation foundation.

### Added

- `FlockPlugin` with `FlockConfig`, four `CrowdPreset`s, and budgeted spawning
  driven by `SpawnQueue` (unique and recycled identity modes, 1 to 10,000+ NPCs,
  spawn budget per frame).
- `FlockSet` scheduling stages (`Spawn`, `Perception`, `Cognition`, `Steering`,
  `Integration`, `AdvanceTick`), chained so a game can join any of them.
- Boids steering with separation, alignment, cohesion, seek and arrive combined
  by priority-weighted truncation rather than a weighted sum.
- `SpatialGrid`: a uniform grid over a fixed volume with radius and k-nearest
  queries, deterministic result ordering, and no steady-state allocation.
- `RngStream`: PCG32 with per-NPC stream derivation from `(seed, NpcId)`,
  verified against the reference implementation and pinned by a golden vector.
- `Temperament` (Big Five plus bravery, sociability, industriousness),
  `Archetype` presets, `Needs` with decay and satisfaction, and a two-axis
  `Mood`.
- `BehaviorTree` with resume semantics, abort, a depth cap, and a flat compiled
  layout; `StateMachine` with dwell times and ordered guards; `Blackboard` with
  fixed capacity and compile-checked keys.
- `FlockRegistry` with the `LeafBehavior`, `GroupDirector`, `WorldDirector` and
  `NpcLogic` extension traits, and duplicate-identifier rejection at
  registration.
- Criterion benchmarks for whole frames at 1, 1,000, 10,000 and 100,000 NPCs,
  plus the spatial query and steering inner loops.
- Determinism tests that hash full NPC state in `NpcId` order and assert that
  identical seeds produce identical worlds, and that a larger population keeps
  the shared prefix of individuals.

### Notes

- MSRV is 1.97.1, matching Bevy 0.20.
- `glam` is pinned to the 0.33 line; `cargo tree -d -i glam` should never show
  two versions.
