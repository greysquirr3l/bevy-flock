# ADR 0027, Spawn placement

**Status:** Accepted (2026-11-04)
**Related:** ADR 0002 (deterministic RNG), ADR 0003 (spatial grid)

## The problem

Placement started life as a free function, `spawn_position(index)`, returning the golden-angle
spiral. That is a good default and a poor only option. Three different customers want three
different things on the first frame:

- a **village**: people at the doorways, the well, the market stalls, positions a designer
  authored, not a spiral;
- an **ambient city**: a crowd scattered over a district, which reads as random even though it
  must replay identically;
- a **demo or benchmark**: a compact disc at a known density, which is what the spiral gives.

The spiral also has a failure mode that is invisible until it bites: its radius grows with
`sqrt(n)`, so at some population the crowd is wider than the simulated volume. The grid clamps
stray positions into its border cells, so the symptom is not "NPCs are outside the world", it is
"the frame got slower", and the cause is a configuration line several hundred metres away from
the effect.

## The decision

`Placement`, a domain type in `src/domain/spawn.rs`, four variants:

| Variant | What it does | Draws from the stream |
| --- | --- | --- |
| `Spiral { centre, spacing }` | Golden-angle sunflower, area-uniform, deterministic by index | none |
| `UniformRect { min, max }` | Uniform random inside a rectangle on the plane | 2 |
| `UniformDisc { centre, radius }` | Uniform random inside a disc, area-uniform (`r = R·sqrt(u)`) | 2 |
| `Fixed { positions }` | Authored points, `positions[i % len]` | none |

`FlockConfig::with_placement` validates the choice against `BoidsConfig::grid()` before it can
reach the spawner:

- a spacing or radius that is not finite and positive → `ConfigError::InvalidDistance`;
- a rectangle with its corners swapped, or an empty position list → `ConfigError::Empty`;
- a centre, corner or disc edge outside the simulated volume → `ConfigError::PlacementOutsideGrid`,
  naming the field, the axis and the range.

That last one is the whole reason the type exists. A stranger's first bug report should not be
"why is my 60 fps crowd 12 fps", answered by "because your spawn disc is wider than your grid".

### The default stays the spiral

`Placement::default()` is `Spiral` at 1.6 m. Two reasons: it is compact at any population, so the
spawner cannot overflow the volume on its own; and it draws nothing from the RNG, so it changes
nothing about the people a seed produces. Every temperament and archetype in a seeded world is
identical whether the placement is a spiral or an authored list.

Random placements draw **after** temperament and archetype, and 2 draws each: a game that switches
to randomness gets a scattered crowd of exactly the same *people*. `a_random_placement_still_
produces_the_same_people` holds that.

### Determinism

Each NPC draws from its own stream (`RngStream::for_npc(seed, id)`), never a shared one, so a
placement is reproducible without reference to spawn order, the property ADR 0002 depends on. A
counted sort, an authored list and a disc all replay identically, and
`the_same_seed_places_the_same_crowd` asserts it through the plugin rather than the domain type.

### Fewer places than people

`Fixed` cycles. Seven NPCs into one authored point puts all seven there, and separation pulls them
apart on the opening frames. This is documented rather than refused: a level can legitimately
author three doorways and expect the queue to sort itself out, and refusing would be a library
deciding a game design question. The alternative, silently jittering authored points, would break
"these positions are the level".

## Consequences

- `FlockConfig` is no longer `Copy` (a placement may own a `Vec<Vec3>`), so `FlockPlugin` is
  `Clone` and the plugin clones the configuration into its resource. Callers who passed a config by
  value are unaffected; callers who relied on `Copy` need a `.clone()`.
- `spawn_position(index)` remains, documented as the default placement's answer, because the
  common case should not need a type.
- Placement changes density and therefore frame cost. The published numbers
  (`docs/dev/tuning/performance.md`) use the spiral at 1.6 m, which is ~0.39 agents/m². A uniform disc of the
  same area and population matches it; a crowd spread over a whole 1 km district is far cheaper per
  agent and far less of a crowd.

## What was not done

Placement is a two-dimensional decision. A game with a multi-storey building needs `y`, which means
per-floor volumes and a grid that is not planar, the same assumption ADR 0026 records for the
steering kernel. Doing one without the other would produce crowds that stack vertically and steer
as one, so both wait for the same piece of work.
