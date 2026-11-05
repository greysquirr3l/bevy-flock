# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
- **Reciprocal local avoidance (ADR 0045).** A second pass over the neighbour set the kernel
  already gathers removes the excess closing speed between each pair, split by priority, so two
  agents walking at each other stop at the separation floor instead of sharing space.
  `NpcAvoidancePriority` (default 1.0) is how a game says who yields; `BoidsConfig::with_avoidance`
  turns it off. On by default. The pass runs on the same `SteeringRoad` as steering, is held to bit
  equality between the scalar and threaded roads, and allocates nothing in a steady frame. Measured
  at 10,000 agents: 2.25 ms steering, 3.27 ms with avoidance; at 100,000: 25.74 ms and 38.08 ms.
- **Obstacle-driven navigation (ADR 0046).** `ObstacleNavPlugin` plus a `NavObstacle` component
  derive a `GridNavSource` from the world's blockers, rebuilding at most `cells_per_tick` cells a
  frame, a whole row at a time, and publishing atomically: the version increments once, on the step
  that finishes the rebuild, so no NPC ever paths against a half-built wall.
- **Stimulus perception (ADR 0047).** `StimulusBuffer` is a bounded, decaying ring of sights,
  sounds and smells; `perceive` applies range, cone, age and an optional `Occluder`. `StimulusPlugin`
  publishes from `StimulusSource` entities and writes `PerceivedStimuli` per observer on a tick
  interval, so a guard can lose sight of a target and search without a special case in its tree.
- **Navigation links and an optional navmesh (ADR 0048).** `NavLink` with a cost and a `LinkTag`
  participates in flow-field relaxation rather than being post-processed, so a ladder can be the
  only route to a floor, and `FlowField::link_tag_at` tells a leaf what it is about to walk onto.
  The `navmesh` feature adds a polygon source with area costs and a funnel, with the funnel's
  pinch-restart branches fixed to advance the apex to the opposite corner.
- **Perception keeps six slots, not thousands.** `perceive` collected every hit and sorted them
  before truncating to `max_results`; it now inserts in order into a bounded result set, which
  costs one comparison per slot and no allocation. 75.7 ms → 35.8 ms on the dense benchmark, and
  `the_bounded_insert_keeps_exactly_the_best_slots` is the parity test against the old behaviour.
- **Three fixes found by the above.** `steer_flock` wrote the kernel's desired velocity as
  `(x, 0, y)`, dropping vertical and lateral motion; it now writes all three axes. Sorting a
  `LinkTable` placed same-key edges after every equal key, leaving insertion order to decide ties.
  `Crowd::begin` and `Crowd::order_by_cell` now share a count reset, so the order is idempotent and
  the avoidance pass can reuse it.
- **Steering uses every core (ADR 0044).** `Crowd::steer` cuts a frame into contiguous slices of
  agents and hands them to the `ComputeTaskPool`, with `SteeringRoad { Auto, Wide, Threads, Scalar }`
  to pin the choice and `BoidsConfig::with_road` to set it. All four roads assert bit equality with
  the scalar reference, so determinism and the whole-world digest are untouched. Measured on six
  vCPUs: 8.48 ms → 4.23 ms at 10,000 agents and 84.0 ms → 29.6 ms at 100,000, against the wide road
  this supersedes. New `threads` feature (on by default) enables `bevy_tasks/multi_threaded`; without
  it the pool has one thread and `Auto` stays on the wide road. `flock_frame_road` joins the
  benchmark suite so the four roads are always measured on the same host in one run.
- **The book, and a README you can check.** `docs/book/src` is an eight-chapter guide
  (getting started, the model, crowds at scale, behaviour, memory, extending, debugging,
  costs) that reads as Markdown on a forge and builds with `mdbook`. The README gains a
  ten-minute verification section: every claim it makes is mapped to the test, benchmark or
  script that pins it.
- **Platform and feature audit (`docs/platforms.md`).** Nine host configurations and four
  `wasm32-unknown-unknown` configurations are now compiled and recorded, and the audit found
  a real bug: with `--no-default-features`, the memory leaves and the diagnostics reached
  into `memory`-gated modules, so the server build did not compile. The leaves now restate
  the encounter flag values with a test that pins them to `EncounterKind::flag`. New CI jobs:
  `wasm` (four configurations) and `portability` (Windows and macOS `--all-features`).
- **A release process (`RELEASING.md`)** with a tag-triggered `release` CI job that packages
  the crate, checks the API against the last published release and re-runs the decision
  scripts. `cargo package --locked` is warning-free; benches and examples now ship with the
  package so every `[[bench]]` target stops warning on publish.

- **The engine, not just the crowd.** The remaining open plan tasks land together
  (ADRs 0035-0040). Behaviour trees gain the full node set — six composites, four
  decorators, flat compiled nodes, a fixed-size cursor stack, stateless shared leaves —
  and `tests/tree_no_alloc.rs` keeps a tick allocation-free. Beside them,
  `behavior::utility` picks goals by weighted considerations with inhibition, count
  compensation and an `explain` that prints the arithmetic. `domain::generator`
  produces reproducible NPCs (names, temperament, needs, background) from a seed and an
  index. `domain::groups` adds formations with stable slots and faction relations;
  `application::director` decides by returning commands and keeps a bounded trace, which
  is what makes a crowd's mood replayable. `behavior::dialog` validates graphs at load
  (dangling choice, unreachable node, loop without exit), `behavior::bark` funnels every
  line through one bounded, cooldown-gated channel with a per-frame cap, and
  `behavior::provider` keeps a slow line source off the tick. `domain::nav`/`domain::flow`
  give destination-driven movement a port, a grid and a hand-authored source, with a
  version-keyed field cache.
- **Integrations are ports (ADR 0040).** `PhysicsPort`, the `NpcNetState` wire format with
  a bounded `InterestSet`, and a sorted `RenderPlan` are engine-free: a game writes a
  dozen lines for its own Avian, replicon or renderer instead of inheriting a version
  conflict. `NpcNetState::BYTES` is pinned by a test, and `benches/replication.rs`
  measures the filter at 1k/10k/100k plus encode and decode.
- **Proof for the new surface.** Six property tests over arbitrary inputs (blackboard
  capacity and write survival, RNG determinism, grid/brute-force parity, wire round trip,
  weight clamping, tick monotonicity), four runnable examples (`hello_flock`,
  `custom_behavior`, `tavern_dialog`, `ambient_metropolis`) and benches for the scheduler
  (`cognition`), groups, flow fields and replication. CI compiles every bench and runs the
  examples. 443 library tests, the whole integration suite and clippy at `-D warnings`
  across all targets and features.
- **Scaling regression suite and a generated report (ADR 0034).** `scripts/scaling_regression.py` gates
  CI on speedup ratios rather than wall clock, so a four-core runner is not a regression: thresholds
  widen with the measured spread (`fail = max(25%, 2.0 x widest)`, `warn = max(12%, 1.5 x widest)`), a
  different fixture is refused instead of compared, and no shared core count is a failure rather than a
  pass. The tolerances are exported as constants, printed by the report and stated in ADR 0034 with what
  they mean in seconds; the job carries a 15-minute timeout so the CI budget is enforced too.
  `scripts/scaling_report.py --check` fails when the committed report or ADR 0034 stops matching the
  code and the baseline, which is how the "differences below 18%" contradiction got caught.
- The gate refuses to be green over an empty test: a cell whose spread pushes the failure line past 25%
  is `unverified`, the sequential cell's spread propagates to every row that divides by it, and a run
  where fewer than half the cells could be verified exits 2. Found by the first full verification, where
  one 17.7 s outlier on the one-core denominator turned five rows into fake 9-12% gains.
  `scripts/scaling_report.py` writes `docs/reports/scaling-report.md` — wall clock, speedup and
  efficiency matrices, the measured optimum against the cap formula, an ASCII chart and the verdict —
  from the same baseline the gate uses. The sweep moves to millisecond timing, two repeats per cell and
  a `meta` record, and corrects an earlier defect: shape B built six-crate bundles from an eight-crate
  fixture, so four of its eighteen builds never happened. The CI job runs a reduced matrix (shape A,
  one core and the runner's own, concurrency 1/2/4) and uploads the raw sweep and verdict as artifacts.


- **Group scaling measured at 1, 2, 3 and 6 cores, both task shapes.** `scripts/scaling_sweep.sh`
  pins every run with `taskset` and gives each task a cold target directory; `scripts/scaling_curves.py`
  plots wall clock and speedup with the raw rows checked in at `docs/data/scaling-sweep.json`. Findings:
  the cap formula is a floor with headroom (six one-thread tasks take 8 s, all eight take 5 s, because
  cargo's I/O start overlaps), threads per task dominates the curve, and a six-thread task is already
  queue-limited on three cores. ADR 0033 records all three, including the case where the formula
  under-admits.


- **ADR 0033 states the group capping policy once.** `cap = nproc / threads_per_task`, measured per
  task rather than assumed, with group sizes taken from `wiggum validate` because wiggum injects audit
  tasks the phases do not declare (23 authored, 24 scheduled, measured). `scripts/group_cap.py` gains
  `--groups-from` for the exact answer, `--check-plan` for the authored one, and a self-test that CI
  runs on every push: five recommendation cases, eleven evidence rows, wiggum's wave rule, cycle
  detection, and a plan that still parses. Capacity stays out of CI on purpose — runners have two to
  four cores, so a strict gate would be switched off within a week.


- **ADR 0032, amendment 2: the group cap is a thread budget.** Eight one-core codegen tasks on six
  cores: sequential 24 s, capped at half the cores 15 s, capped at six 10 s, uncapped 8 s. The
  earlier "cap at half the cores" advice was measured on workspace builds that each spawn `cargo -j6`
  and is wrong for any other shape. `cap = nproc / threads_per_task`, measured with
  `scripts/group_cap.py --measure "<command>"`; the old rule is withdrawn rather than quietly dropped.


- **ADR 0032 amended: the build lock is nearly free and `CARGO_TARGET_DIR` isolation does not pay.**
  Three cold compile tasks on six cores: one target sequential 45 s, one target concurrent 43 s
  (+4%), three targets concurrent 46 s, three targets sequential 82 s. The original advice to isolate
  a cold-building group was wrong — three cold dependency trees oversubscribe six cores and give back
  what the overlap won. The shared target directory stays the default; isolation is now documented as
  a debugging tool, and compile-heavy groups are capped at about half the cores.


- **ADR 0031's re-open trigger is now a CI gate.** `scripts/cell_size_gate.py` reads the
  `flock_frame_cell` estimates and compares 9 m against 13.5 m: below a 3% saving the decision
  stands, 3-10% annotates the run, above 10% the build fails and the ADR is wrong. The thresholds
  are asymmetric because a runner cannot resolve 3% and a flapping gate gets disabled. The
  `cell-size-gate` CI job runs the benchmark and the script on every push, and the plan carries the
  dependency as `cell-size-reopen-gate`, which watches the `benchmark-suite` regression-gate work.


- **The cell budget is configurable, and the ECS sweep says leave the default alone (ADR 0031).**
  `SpatialGridConfig::max_cells` and `with_cell_budget` let a game raise the grid past 2^20 cells;
  `BoidsConfig::with_grid` exposes it through the plugin. Measured whole-frame time at 10,000 and
  100,000 agents: 1.4M cells (9 m) is free, 2.9M (7 m) costs ~1% at 100,000, and neither moves the
  frame the way it moves the kernel — tiering caps the steered population at a few thousand, so the
  8-20% kernel preference for 9 m never reaches the frame. `MAX_CELLS` stays at 2^20; the knob is
  there for games that steer more of their crowd.


- **The transposed crowd kernel is on by default, and cell sizing is documented (ADR 0031).** A
  sweep of five cell sizes at 10,000 and 100,000 agents found the road winning at every density —
  including the 3-agents-per-bucket case ADR 0030 was cautious about — by 27-37% at 10k and 19-31%
  at 100k, so `simd` joined the default features (`default-features = false` restores a
  dependency-free build). The kernel's preferred density is 6-10 agents per occupied cell: four per
  bucket is the minimum for a block, and 48 per bucket is the slowest configuration on both roads.
  The default grid lands on 13.5 m, which is 19% better than scalar and 8-20% short of the 9 m
  optimum; the recommendation is documented rather than enforced, because raising the cell budget
  would also raise the cost of the ECS query path that ADR 0003 sized it for. `examples/grid_sweep.rs`
  re-runs the measurement; end to end at 100,000 agents the frame is 79.07 ms, down from 90.0 ms.


- **The crowd kernel can now use SIMD — the transposed shape (ADR 0030).** ADR 0029 rejected
  lane-per-neighbour vectorisation for good reasons: the lanes shared nothing and the masked
  arithmetic threw away every rejected neighbour. The shape that works is the band kernel's
  transpose — block four agents that share a cell and broadcast each neighbour into all four lanes.
  Behind `--features simd`, with a scalar road for buckets that cannot fill a block and for
  ambient-tier agents, it is 22% faster planar, 21% volumetric and 29% faster on a dense crowd
  (10,000 agents, kernel only), and `the_transposed_road_agrees_with_the_scalar_road` holds it to
  identical counters and desired velocities against `Crowd::steer_scalar`. Off by default: it costs
  a per-frame structure-of-arrays copy that a sparse crowd does not earn back.


- **SIMD was measured against the 3D crowd kernel and rejected (ADR 0029).** A structure-of-arrays,
  four-neighbours-at-a-time scan with a scalar tail was built behind the `simd` feature, held to
  parity with the scalar road in the same process, and benchmarked on planar, volumetric and dense
  crowds: it lost by 16%, 26% and 8%, and AVX2 did not change the verdict. The scalar scan rejects
  out-of-range neighbours before doing any work, and that is precisely what masked SIMD throws away.
  The code is removed rather than flagged off; the `crowd_kernel` benchmark group and ADR 0029 keep
  the measurement reproducible. The `simd` feature still vectorises the band kernel (ADR 0023),
  where it wins.


- **Crowds are three-dimensional (ADR 0028).** The steering kernel and the spawn placement no
  longer assume the plane `y = 0`: positions, velocities and desired velocities are `[f32; 3]`,
  cells come from all three axes with the grid's own arithmetic, and the scan walks a box of cells
  and tests true 3D distance. New placements `uniform_volume` (a box) and `uniform_sphere`
  (volume-uniform, `r = R·∛u`) join `fixed`, which now honours the height it was given. Nothing else
  had to change — the grid and the integration system were already volumetric. Cost: 19% on a
  planar crowd (`flock_frame/10000` 7.71 ms → 9.18 ms, still 1.8× inside the frame budget), because
  the price is per-neighbour arithmetic rather than empty-cell bookkeeping. Breaking for direct
  `Crowd` users only: `push` and `desired` use `[f32; 3]`, and `configure` lost its `plane`
  argument.


### Added

- **Spawn placement is a validated type (ADR 0027).** `Placement` offers four answers —
  `spiral` (the default, 1.6 m, draw-free and compact at any population), `uniform_disc`,
  `uniform_rect` and `fixed` (authored positions, cycled when the population is larger than the
  list) — and `FlockConfig::with_placement` validates the choice against the simulated volume
  before it reaches the spawner. That check is the point: a crowd placed outside the grid does not
  fail loudly, it piles into the border cells and the frame gets slower. Random placements draw
  from each NPC's own stream, after temperament and archetype, so a replay scatters the same people
  into the same spots and switching to an authored list changes nobody's identity.
- `FlockConfig` is no longer `Copy` (a placement may own a list of points) and `FlockPlugin` is
  `Clone` instead; `spawn_position(index)` remains as the default placement's answer.



### Changed

- **Crowd steering is cell-major (ADR 0026).** `steer_flock` now gathers the crowd into arrays in
  spatial-grid order, steers it with one pass per agent over the cells in range, and scatters the
  results. The model is unchanged — same weights, same priority truncation, same arrival curve —
  and a parity test holds it within 1e-3 of `desired_velocity`. What went away is the per-agent
  `Vec<Neighbour>`, the distance sort inside `SpatialGrid::radius`, and the second and third
  passes over the neighbour buffer: `flock_frame/10000` fell from **18.2 ms to 7.71 ms** (130 fps,
  2.2× inside the 16.6 ms budget), 100,000 agents from 192 ms to 73.5 ms, and a steady frame
  allocates nothing (`tests/crowd_no_alloc.rs`).
- **The crowd no longer averages an agent into its own cohesion.** The grid returns an agent as
  its own nearest neighbour, and the old loop divided the alignment and cohesion sums by a count
  that included it, diluting both by `1/n`. The kernel excludes the agent, which is what the
  equations meant; `the_kernel_does_not_average_an_agent_into_its_own_cohesion` pins the old bias
  so the difference is documented rather than discovered in a changelog.
- New: `FlockSteerReport` publishes what the last steering pass cost (`steered`, `isolated`,
  `pairs`, `cells`), and `steer_flock_reference` keeps the old per-agent path available for games
  that want to inspect neighbour lists.



### Reproducibility

- `Cargo.lock` and `compat/Cargo.lock` are committed, and every CI job runs `--locked`, with a
  `lockfile` job that fails if either is stale. A dependency bump is now a commit rather than a
  silent resolution, which matters because `docs/performance.md` publishes timings that only
  mean something against an exact dependency graph.
- `docs/performance.md` grew a reproduction section: the toolchain and profile that are pinned,
  the lockfiles, the exact `cargo bench` command for each row, the host the numbers were taken
  on (6 vCPU Neoverse-N1, single-threaded paths), and what still varies between runs (1-4% run
  to run; a different machine is a different measurement, which is why the document argues in
  ratios). It also records a cross-check: `flock_frame/10000` re-measured at 18.75 ms against
  the committed 18.2 ms.
- Stated plainly rather than left to be inferred: the 10,000-agent crowd frame is 18.2 ms, i.e.
  55 fps against a 16.6 ms budget. The 60 Hz route at that crowd size is the band path.


### Fixed

- **The band no longer manifolds what it folds.** The manifest window was square, so its
  corners reached 1.41 radius while folding began at 1.15 radius: every agent manifested from
  there was born outside the band and folded on the next tick, and the band held one ration of
  agents (256) forever instead of its 5,000 target. The window is now a disc bounded by the
  manifest *and* fold radii, and the spawn point is the one that was vetted. ADR 0025.
- `seed_field` now distributes the population uniformly over the field instead of filling it
  from the origin and stopping when the count ran out — which had left the map's centre empty
  and made the manifest behave like a vacuum cleaner.


### Documentation

- `docs/memory.md`: heap and cache profile of the packed kernel, scalar and wide.
- `docs/adr/0024`: the layout as a cost contract, the ordering constraints, and the parity rule.
- `docs/release-checklist.md`: gate ledger, performance and memory sign-off for the
  1,000,000-inhabitant milestone, with the open manifest/fold churn listed as a 1.0 blocker.
- `examples/memory_probe.rs`: allocation and footprint probe, cachegrind-friendly.


### Added

- `simd` feature: the steering kernel processes four agents at a time with `wide::f32x4`,
  behind a cell-major block order. 1,000,000-inhabitant frame 4.75 -> 3.78 ms; packed steer
  at 50,000 agents 138 -> 56.5 ms. Bit-identical to the scalar path, which is kept as the
  reference and tested against it. See ADR 0023.


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

- ADR 0021 implemented and measured: near band (5,000 @ 60 Hz) plus mid band (15,000 @
  20 Hz) on the packed kernel. **1,000,000 total inhabitants at 5.02 ms**, under the 6 ms
  gate, with the near band's count unchanged. `BandConfig::mid_extent` exists because the
  mid pack inherited the near band's 512 m grid and clamped every agent into its edges.

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
