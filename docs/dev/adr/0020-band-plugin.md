# ADR 0020: The manifest band plugin

- Status: Proposed
- Date: 2026-11-04
- Relates to: ADR 0017 (tiered cognition), ADR 0018 (continuum), ADR 0019 (memory aging)

## Context

ADR 0018 proposed the continuum and ADR 0019 made memory safe for NPCs nobody simulates.
Neither is wired into the ECS. This record specifies that wiring precisely enough to build,
and it exists because a measurement changed the shape of the problem twice.

**First**: gate 8 failed at 9.47 ms for a 5,000-entity band, and advection was 0.04 ms of
it. The crowd path costs ~1.9 ms per 1,000 steered NPCs, so folding was necessary and not
sufficient.

**Second**: the packed steering kernel (`application::packed`) measures **1.50 ms** to
steer, integrate and advance 5,000 agents in a 512 m square, against 9.47 ms through the
ECS. 249 µs at 1,000. The arithmetic was never the problem.

But the same bench shows the limit honestly: 5,000 agents in a 128 m square cost 10.9 ms,
and 50,000 in 512 m cost 137 ms. The packed path is bound by **neighbour count**, not by
agent count. So the band plugin's job is not only to fold; it is to keep the band's density
constant as the population grows, by scaling the manifest radius with the manifest count.

## Decision

One plugin, `FlockBandPlugin`, extends `FlockPlugin` with three responsibilities and a
strict contract about who owns which data.

### 1. The band is a ring, not a sphere

```
manifest_radius = sqrt(manifest_target / (density_target * PI))
```

with `density_target` defaulting to 1 agent per 40 m² (5,000 agents ≈ 252 m radius). A game
that raises `manifest_target` must raise the radius, or it buys neighbours instead of
people. The plugin computes the radius from the target and the density, and exposes both so
a designer can see the trade rather than discover it in a profile.

### 2. Ownership

| Data | Owner | Sync |
| --- | --- | --- |
| `AmbientField` | the plugin, as a resource | advected on stride; never per frame |
| Band agents inside the radius | ECS entities, steered by the packed kernel | copied into `Pack` at the start of the steering system, out at the end |
| Band agents outside the radius | the field's cells | folded once, at the boundary, with hysteresis |
| Anchored entities | ECS, always | never folded, never counted against the band |

The `Pack` is a resource with capacity for `manifest_target`. Copy-in and copy-out are
`O(n)` slice writes into a pre-sized buffer: no per-frame allocation, and no per-agent
component fetch during steering.

### 3. Ordering

```
advect (stride)  →  fold (hysteresis)  →  manifest (ration)  →  pack_in  →  steer
                 →  pack_out  →  assign_tiers  →  plan_thinks  →  tick_chosen
```

Folding precedes manifestation so a person leaving and arriving in the same frame cannot be
counted twice. `pack_in` follows manifestation so a newly manifested agent is steered on the
frame it appears rather than standing still for one.

### Requirements

1. **One fold per entity, ever.** Folding despawns the entity and increments the cell. An
   entity that is despawned twice, or folded and left in the world, is a population error
   that the conservation test must catch.
2. **Manifestation takes before it spawns.** `AmbientField::take` gates every spawn; a
   failed take means no entity. This is the only defence against a crowd that doubles.
3. **Hysteresis on both edges.** Fold radius = manifest radius × 1.15. Without it, an agent
   on the boundary oscillates between entity and density every frame.
4. **Ration both directions.** Manifest and fold counts per frame are bounded by a policy
   value, defaulting to 256. A camera crossing the band at speed must not spend a frame
   spawning.
5. **Anchored entities are invisible to the band.** They are never folded, and their count
   is reported separately so a game can see when its anchors have outgrown the budget.
6. **The field is authoritative for absent population.** No system may spawn an NPC outside
   the band radius except through manifestation.
7. **Allocation-free steady state.** `Pack`, the field and the advection scratch are sized
   at plugin build. The no-allocation test extends to the band.

### Closing the open gates

| Gate | How it closes | Test |
| --- | --- | --- |
| 4 parity | continuum vs. full simulation, 5 min | `tests/band_parity.rs`, per-cell population within 1% |
| 5 no popping | camera crossing at 30 m/s, rationed | `tests/band_popping.rs`, count delta < 2% per frame |
| 6 anchoring | anchored NPC at 10 km for 10k ticks | `tests/band_anchoring.rs`, still an entity, memory intact |
| 7 no allocation | counting allocator across a band frame | extend `tests/tree_no_alloc.rs` |
| 8 100k / 5,000 | 1.50 ms packed steer + band overhead < 8 ms | `benches/band.rs` |
| 9 fold completeness | a folded NPC matches no query | `tests/band_fold.rs`, query counts before/after |

Gate 8's arithmetic is now in hand: 5,000 agents steer in 1.50 ms, and the non-steering
band overhead is the part this plugin adds. The gate stays open until `benches/band.rs`
measures a frame, not until the kernel is fast.

## Rejected alternatives

- **Steer entities with the packed kernel in place.** Requires the same copy-in/copy-out
  work while still paying query traversal; the pack would be a faster inner loop around an
  unchanged outer cost.
- **Fold by despawning without a field.** Population becomes unaccountable: nobody can say
  how many people the world has, which the conservation gate exists to prevent.
- **Manifest everyone at load, fold on distance.** The spawn cost moves to load time and
  the memory cost to the whole session; the band would exist only to save frame time.
- **A fixed manifest count.** Density then grows with population, and the packed path
  degrades exactly as the bench shows (10.9 ms at 128 m).

## Consequences

The band radius becomes a design parameter with a visible formula, and the frame cost
becomes roughly independent of total population: 5,000 agents is 1.5 ms whether the world
holds five thousand or five million. The cost of that is a hard ceiling on how many people
can be *individually* present at once, which is the honest form of the trade ADR 0018 made
when it gave up identity outside the band.

Two things must not be forgotten when this is built: anchors are the exception to
everything, and conservation is a test rather than a comment.
