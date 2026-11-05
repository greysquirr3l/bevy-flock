# Crowds at scale

Ten thousand entities is the point where a `Query` stops being free. Past it, the useful
question is not "how many entities can I afford" but "which entities deserve to be
entities". `bevy-flock` answers with three layers.

## The three layers

| Layer | Population | What exists |
| --- | --- | --- |
| Simulated crowd | 1 – 10,000 | real entities, real steering, real brains |
| Band | 10,000 – 1,000,000 | the near ring is entities; the middle ring is entities at a distance; the far ring is a density field |
| Continuum | 1,000,000+ | no entities at all: population *is* a scalar per cell, and the manifest list is the handful that materialise |

The transition between layers is where the interesting bugs live, which is why ADR 0022's
boundary harness drives a camera through every boundary for 10,000 ticks and asserts no
allocation, no pop-in and no duplicated resident.

## Bands: entities where you are looking

```rust
use bevy_flock::prelude::*;

let mut app = App::new();
app.add_plugins(FlockPlugin::new(FlockConfig::preset(CrowdPreset::CityAmbient)))
    .add_plugins(BandPlugin::default());
```

A `Band` owns:

- **An anchor** — usually the player. The anchor's presence keeps a manifested NPC alive even
  when it drifts outside the ring, because an NPC you can see must not vanish (ADR 0019).
- **A near ring** — real entities with brains and steering, sized by `BandConfig`.
- **A middle ring** — entities with movement but a cheaper path, kept wide so the transition
  is a change of cost rather than a change of appearance.
- **A far field** — `AmbientField`, a grid of `CellState`s holding `Occupation` (how many, how
  busy) rather than anyone in particular.

The fold is the operation that matters: when a cell's population changes, the field is
updated and the manifest list is reconciled — residents that should exist are spawned,
residents that should not are absorbed into the field. That reconciliation used to churn
(a resident spawned, then folded, then respawned), and ADR 0025 fixed it with hysteresis:
a fold only happens when the cell is over its threshold by a margin, and a manifest entry is
only dropped when it is under by a margin. The churn test (`tests/band_churn.rs`) asserts
steady-state churn is zero once the crowd has settled.

Measured, on the reference host:

| Configuration | Frame |
| --- | --- |
| 100,000 total, 5,000 manifested | 3.72 ms |
| 250,000 total, 5,000 manifested | 3.75 ms |
| 1,000,000 total, 5,000 manifested | **3.78 ms** |

The number that matters is the second column: **the cost tracks what is manifested, not what
exists.** A million inhabitants cost what five thousand cost — 3.72 ms at a hundred thousand
in total and 3.78 ms at a million is a 1.6% difference for ten times the population — because
only five thousand of them are entities. The 1,000,000 row re-measured at 3.80 ms during the
reproducibility pass, which is the intended relationship between a table and a re-run.

## Traversal: a ladder is an edge, not a special case

A grid flow field answers "walk there". It cannot answer "walk there, up the ladder", so the
usual workaround is a behaviour tree that teleports the NPC when it reaches a trigger. That works
until two NPCs need to path to the same upstairs room and the tree has to reimplement A*.

bevy-flock treats traversal as navigation data (ADR 0048):

```rust,ignore
use bevy_flock::domain::links::{LinkTag, NavLink};
use bevy_flock::domain::nav::{GridNavSource, NavCell};

let mut source = GridNavSource::open(Vec3::ZERO, 64, 64, 1.0)?;
// A ladder from the ground floor to the mezzanine, four seconds of climbing.
source.add_link(NavLink::new(
    NavCell::new(12, 40),
    NavCell::new(12, 8),
    240,
    LinkTag::new(1),
))?; // cost in the solver's units: 240 is four seconds at 60 ticks

// The field now routes through the ladder when it is the cheaper way,
// and around when it is not.
let field = solve(&source, FlowTarget::at_cell(&source, NavCell::new(50, 8)));
assert_eq!(field.link_tag_at(NavCell::new(12, 40)), Some(LinkTag::new(1)));
```

The link is an edge in the same relaxation that builds the field, not a post-processed shortcut,
so it can be the *only* route to a cell. The tag rides along in the field, which is what a leaf
reads when it wants to know whether the route it is following goes up a ladder:

```rust,ignore
// In a tree: "if the way ahead is a ladder, slow down and play the climb".
condition(|ctx| ctx.field_link_tag() == Some(LinkTag::new(1)))
```

## The continuum: population as density

`AmbientField` is the far layer, and it is deliberately boring: a sparse grid of cells, each
holding an occupation count, a mood, an alarm level and a couple of stamps. Nine gates keep
it honest (recorded in `docs/gates/continuum.md`), and the five that matter to a game are:

1. **No allocation per tick** — the field is sized once; folding and advection reuse buffers.
2. **Conservation** — people are moved between cells, never created or destroyed.
3. **Continuity** — a cell crossing a density threshold does not teleport its occupants.
4. **Anchor exemption** — a manifested NPC near the player is never folded away.
5. **Bounded growth** — a crowd left running for hours does not creep upward in population.

Advection is the tick's only expensive step and it is `O(cells)`, with cells bounded by the
`AmbientField`'s configured extent rather than by the population.

## When to use which

- **Under 10,000, always the plain crowd.** Bands add a layer of indirection and a boundary
  to test; a village does not need them.
- **10,000–100,000, one band.** The near ring is your playable area, the far field is the
  rest of the city.
- **Above 100,000, multiple bands** (ADR 0021) or the continuum alone, if the distant
  population is decoration. A million people who never appear on screen do not need
  identities; they need a density and a mood.

## The number to remember

Per-agent cost is flat and slightly *falls* with population: 0.92 µs at ten thousand, 0.90 µs
at a hundred thousand. The reason is locality — every agent's work is its own cell and its
neighbours, so a bigger crowd does not make any one agent more expensive. That is the property
that makes bands possible at all: if per-agent cost grew with population, adding a ring would
never help.
