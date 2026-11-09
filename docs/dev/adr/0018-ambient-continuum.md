# ADR 0018: The ambient continuum, for populations past the reflex ceiling

- Status: Proposed
- Date: 2026-11-04
- Deciders: Nick
- Supersedes: none
- Relates to: ADR 0007 (no renderer), ADR 0009 (virtual cost), ADR 0010 (social memory),
  ADR 0016 (subject ceiling), ADR 0017 (tiered cognition)

## Context

ADR 0017 measured the reflex tier and corrected its own arithmetic: a reflex think costs
**~217 ns** end to end in the ECS, not the 150 ns its declared cost implied, so 64,000
reflex thinkers per tick saturate a 14 ms budget rather than the 93,000 the declared costs
suggested. The 64,000/100,000 run in this repository's benchmark suite is the verification
of that number.

That number is not a property of the rules. A reflex rule set is one blackboard read and
one write; it costs tens of nanoseconds. The 217 ns is the cost of *reaching* the NPC:
query traversal, archetype access, change detection, and a mutable blackboard borrow per
entity. Every one of those is per-entity overhead on a per-tick schedule, and it does not
shrink when the logic does. Ten thousand agents all thinking every tick cost 2.17 ms; a
hundred thousand cost 21 ms; a million is out of the question, at any budget.

The library's stated ambition is a hundred thousand inhabitants, and cities larger than
that. The current ceiling is a hard wall at 64,000 reflex agents per tick, and the wall is
made of ECS access patterns rather than of thinking. Three ways past it exist and two of
them are wrong:

- **Raise the budget.** The budget is not the constraint; the frame is. At 64,000 the
  planner is already spending 14 ms of a 16.6 ms frame and leaving 4.2 ms for the crowd
  that does not think at all. Raising it buys a slower frame, not more inhabitants.
- **Make reflex cheaper per entity.** Necessary, not sufficient: a packed loop can reach
  ~20–40 ns per agent, which raises the ceiling to ~350,000 and still leaves it O(n) per
  tick, per entity, forever.
- **Stop making every inhabitant an entity.** This is the only one that changes the
  asymptote, because it removes n from the frame instead of shrinking its coefficient.

## Decision

Population gets two representations, and the boundary between them is a **manifest band**
around the player.

1. **Inside the band: entities, as today.** Tiers, weights, budgets, blackboards, trees,
   memory. Nothing changes; a player cannot tell that anything was optimised.
2. **Outside the band: a continuum.** Population is not entities. It is a per-cell count
   and a four-float state vector, advected by the flow field. No identifiers, no
   blackboards, no brains, no per-agent work. The cost is O(cells), and cells are sized so
   that a million inhabitants fit in a few tens of thousands of cells.
3. **Crossing the boundary is a deterministic transformation, not a spawn.** Walking in
   *manifests* concrete NPCs reconstructed from the cell's state; walking out *folds* them
   back into the cell, conserving population exactly.
4. **Folding removes an agent from every per-entity system, not just the scheduler.**
   Steering, spatial partitioning, perception and cognition all iterate entities; folding
   an NPC takes it out of all of them at once. This is the part the verification below
   forced, and it is the part that matters: at 100,000 the crowd systems cost 198 ms and
   the scheduler costs 14 ms, so a continuum that only shrank `ThinkStats` would be
   theatre.

Plus one orthogonal change, because the manifest band still needs cheap per-entity work:
**the packed reflex batch**. Reflex-tier agents inside the band stop using the ECS query
path and are processed from a contiguous structure-of-arrays buffer, 64 agents at a time,
with no change detection and no blackboard borrow. This is what makes a 5,000-agent band
cost 0.2 ms rather than 1.1 ms.

### The continuation is an identity LOD, not a rendering LOD

This is the decision that carries the risk, so it is stated before the mechanics.

Conventional crowd LODs keep every agent simulated and render distant ones as impostors.
That is a rendering answer to a simulation bill, and it fails the moment anyone asks a
distant agent a question: who lives there, who saw the murder, which faction holds the
square. The continuum answers those from the cell, at a coarser grain, and refuses the
individual question by design, because there is no individual.

That trade is only acceptable with two guarantees, and both are load-bearing:

- **Anything the player can form a relationship with is never folded.** Subjects of social
  memory (ADR 0010) are *anchored*: pinned to entity form regardless of distance. An NPC
  you have wronged cannot evaporate because you walked away.
- **Anything the player can observe individually is inside the band.** The manifest radius
  is at least the deliberative band radius multiplied by 1.5, and it is a designer-visible
  constant, not an implementation detail.

### The cell

```rust
/// One patch of ambient population. Deliberately small: twelve bytes as built, so a
/// million inhabitants over four square kilometres is 480 KB.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CellState {
    pub population: u32,      // the conserved quantity
    pub velocity: [i16; 2],   // mm/s, so a cell compares and copies bitwise
    pub alarm: u16,           // permille
    pub occupation: Occupation,
}

pub struct AmbientField {
    cells: Vec<CellState>,
    /// Metres per cell edge. Constant; the grid does not resample, it clips.
    spacing: f32,
    origin: [f32; 2],
    /// The stream every manifest decision draws from, so a replay agrees with the run.
    seed: u64,
}
```

`population == 0` is the common case and must be cheap: cells are dense in the array and
sparse in population, so advection skips zeros with a branch that predicts perfectly in a
city and perfectly in a wilderness for opposite reasons.

### Advection

The continuum ticks on a stride, not every frame. Default: every 8 ticks, which at 60 Hz
is 7.5 Hz, faster than a crowd visibly changes and 8× cheaper than per-frame.

```text
for each cell with population > 0:
    flux = flow.sample(cell centre) * population * stride * dt
    distribute flux to the four neighbouring cells, bilinearly
    decay alarm by the stride's worth
    advance occupation from the cell's schedule (market opens, shift ends)
```

Conservation is not a by-product of the distribution; it is the test. The four-way split is
computed in integers in units of "persons moved", with the remainder assigned to the
largest-share neighbour so the sum is exact. A million people advected for a day of game
time must still be a million people, to the person.

### Manifestation

```rust
/// The identities a cell yields when it manifests, as a pure function of its state, the
/// tick and the ordinal. Two runs with the same seed yield the same people.
pub fn manifest(field: &AmbientField, cell: CellId, count: u32, tick: Tick) -> ManifestIter;
```

Three properties make this defensible:

1. **Determinism without storage.** The identity of the *k*-th person to manifest from cell
   *c* at tick *t* is `NpcId(hash(seed, c, t, k))`. Nothing is stored per NPC, so
   manifesting ten thousand people costs ten thousand hashes, not a table.
2. **Continuity of state.** A manifested NPC inherits the cell's mean velocity, alarm and
   occupation as its initial blackboard, so it arrives doing what its neighbours are doing.
   Without this, the band's edge is visible as a ring of people who suddenly start walking.
3. **Bounded churn.** Manifestation is rationed like promotion (ADR 0017) and per-frame
   count deltas are asserted in tests: a camera crossing the boundary at 30 m/s must not
   manifest more than `k` NPCs on one frame, or the frame it crosses is the frame that
   stutters.

Folding is the inverse: the NPC's cell is found from its position, its population is
incremented, and its contribution to the cell's velocity and alarm is averaged in. A folded
NPC that was mid-conversation is a bug the caller must prevent, which is what anchoring is
for.

### Anchoring

```rust
/// Never folded, whatever the distance. Subjects with memory, quest actors, anything a
/// save file names.
#[derive(Component)]
pub struct Anchored;
```

Anchored entities are exempt from folding and are counted against the manifest band's
budget rather than the continuum's population. A game that anchors ten thousand NPCs has
built a different game and should expect the 64,000 ceiling; the continuum is for the
people the player has not met.

## What this costs, honestly

| Population | Today (entities) | With the packed batch | With the continuum |
| --- | --- | --- | --- |
| 10,000 | 2.17 ms | ~0.45 ms | ~0.45 ms (all in band) |
| 64,000 | 14 ms | ~2.6 ms | ~2.6 ms |
| 100,000 | 21 ms (over budget, served at 64%) | ~4 ms | ~1.2 ms: 5,000 in band, 95,000 in cells |
| 1,000,000 | not reachable | not reachable | ~1.4 ms: 5,000 in band, 40,000 cells |

The last column is the argument. It is also an estimate, and this record should be judged
by the gate below rather than by the table.

## The gate

The task is not complete when the code compiles. It is complete when these hold, measured:

1. **1,000,000 ambient agents, 2,000 manifested:** frame under 6 ms, continuum tick under
   0.5 ms, measured by `benches/continuum.rs`.
2. **Conservation:** 10,000 ticks of manifest/fold churn with wandering population; the sum
   of cell populations plus live entities equals the initial total, to the unit, every tick.
3. **Determinism:** replay of 1,000 ticks reproduces the manifest identifier sequence and
   the fold counts exactly.
4. **Parity:** a 5-minute scenario run twice, once with the continuum, once with the same
   population as entities, agrees on per-cell population within 1% and mean velocity
   within 5%. Without this test the continuum is a rumour with a data structure.
5. **No popping:** a camera crossing the manifest boundary at 30 m/s never manifests more
   than the ration on one frame, and the count of visible agents never jumps by more than
   2% between frames.
6. **Anchoring:** an anchored NPC at 10 km is still an entity after 10,000 ticks, and its
   memory is intact.
7. **No allocation:** the continuum tick and the packed batch allocate nothing after
   construction, asserted by the counting allocator already used in `tests/tree_no_alloc.rs`.

## Rejected alternatives

- **Raise the budget.** Confuses the budget with the frame; 64,000 already leaves 4.2 ms
  for the crowd that does not think.
- **Per-entity micro-optimisation only.** The packed batch is adopted here, but on its own
  it moves the wall from 64,000 to ~350,000 and leaves the asymptotic shape unchanged.
- **Sleep distant NPCs.** They stop being population. A market that empties when the camera
  turns is worse than a market that never existed.
- **Impostors only.** A rendering answer to a simulation bill: the individual question , 
  who saw what, who lives here, remains unanswerable, and the count is wrong.
- **Hierarchical ECS islands.** Still entities, still O(n) per tick, and the transition
  between islands has the same determinism problem with none of the compression.
- **Statistical population with no manifestation.** Cheapest of all, and it makes the
  library a city generator rather than a crowd engine: nothing can ever be promoted to a
  person. Manifestation is what keeps the continuum honest.

## Consequences

**Gained.** The ceiling stops being a property of the ECS and becomes a property of the
manifest band, which is a design decision a game can tune. A million inhabitants at 60 Hz
becomes arithmetic instead of aspiration. Saves shrink, because a cell is 12 bytes and a
thousand folded NPCs are one cell.

**Lost.** Identity outside the band. The library can no longer answer "where is villager
#4,382" for anyone the player has not met, and no future feature can promise it. That is
the trade, stated once, in the record.

**New failure mode.** Continuum drift: aggregate statistics diverge from what full
simulation would have produced, and the divergence is invisible until a player walks into
the band and finds the market empty at noon. Gate 4 (parity) is the only thing standing
between the continuum and that failure, which is why it is a test and not an aspiration.

**Migration.** `NpcTier::Reflex` becomes "reflex, inside the band". The continuum is not a
fifth tier: it is the absence of an entity, and giving it a tier number would put it in the
scheduler's arrays and defeat the point. `ThinkStats` grows a `folded` count so a game can
see the split it is actually running.

**Follow-on work.** The flow field this advection samples is the navigation task's output,
so the continuum lands after it or ships with a trivial uniform-flow default. Saves need a
cell section (ADR 0012's format has room: a new section id, a version bump, and a
compatibility fixture).

## Verification at 64,000, and what it changed

Measured 2026-11-04, the full suite (`crowd`, `brain`, `tiers`, `think`, `memory`,
`saves`), 1 s measurement per case, on the reference sandbox. Frame times in milliseconds;
"baseline" is the same world with the same plugin and no brains at all.

| Population | Baseline | Reflex | Reactive | Deliberative | Social | 95/4/1 mix |
| --- | --- | --- | --- | --- | --- | --- |
| 1,000 | 4.26 | 4.38 | 4.38 | 4.27 | 4.24 | 4.42 |
| 10,000 | 4.18 | 6.37 | 4.87 | 4.75 |, | 6.42 |
| 64,000 | 4.19 | **11.44** | 7.93 | 7.68 | 7.55 | 11.99 |
| 100,000 | 4.22 | 14.01 | 9.74 | 9.52 | 9.03 | 14.67 |

Three things follow, and the first two are corrections to ADR 0017.

**1. The reflex ceiling is higher than 64,000, and the published per-think cost was not a
constant.** 64,000 reflex agents, all thinking every tick, cost 11.44 ms, 7.25 ms over
baseline, inside a 16.6 ms frame with room to spare. 100,000 wants 14.01 ms. But the
per-think figure implied by each row is 219 ns at 10,000 and 113 ns at 64,000: a 2× spread
that no single number explains, and that probably reflects cache residency rather than a
property of the rules. The honest conclusion is that **frame time is the measurement and
per-think cost is not**, and that ADR 0017's 217 ns should be read as "roughly a hundred
nanoseconds, sometimes two" rather than as a constant. The 64,000 figure was therefore
conservative, not wrong in kind.

**2. The declared reflex cost is too high by about 2×, and the budget believes it.** At
100,000 with a 200 ns declaration, the scheduler serves ~70,000 per tick and defers 30,000
it could afford. A declared cost is a contract (ADR 0009), and a 2× overestimate wastes a
third of the frame's cognition capacity. Recalibration belongs after the packed batch
exists, because the batch will change the measurement again.

**3. The binding constraint at 100,000 is not cognition at all.** The crowd benchmark , 
100,000 NPCs, no brains, steering and spatial partitioning only, measures **198 ms per
frame**. Fourteen times the cognition cost for the same population, and thirty times the
no-brain baseline of the tier benchmarks.

That single number reorders this decision. The ceiling ADR 0017 published was a ceiling on
*thinking*, and thinking is not what stops a hundred thousand inhabitants from running.
Steering is. A continuum that folds agents out of the scheduler and leaves them in the
spatial grid would take a 100,000-agent frame from 198 ms to perhaps 195 ms and claim
victory in the wrong currency.

**Amended gate.** In addition to gates 1–7 above:

8. **The crowd path, not the scheduler, is the acceptance test.** 100,000 inhabitants with
   5,000 manifested must sustain a frame under 8 ms, a 25× improvement on the measured
   198 ms. If the continuum ships and this gate fails, it has optimised the wrong system
   and the task is not done.
9. **Folding is complete.** A folded NPC appears in no ECS query: not the steering query,
   not the spatial index, not the scheduler. Asserted by counting query matches before and
   after a fold.

The 198 ms figure also explains why this optimisation cannot be deferred into polish: at
the population the library claims, the crowd systems are the product.
