# ADR 0051: Temperament drives the simulation, not just the sampler

**Status:** Accepted
**Date:** 2026-11-06
**Context:** `Temperament` and `Needs` were sampled at spawn, stored on the entity, and read by
nothing. A guard was exactly as brave as an urchin, a hermit drifted into the crowd at the same
rate as a merchant, and no NPC in a six-hundred-tick run was ever hungry, tired or lonely. The
crate advertised pluggable personalities and shipped a random number generator with extra steps.

## Decision

A personality now reaches three places, in the order an NPC feels them: the geometry of its
steering, the pace at which its needs move, and the archetype it was spawned as.

### The bias is multiplicative on the configured weights

`Temperament::steering_bias(gain)` returns a `WeightBias` — five multipliers on
`[separation, alignment, cohesion, seek, arrive]`, each centred on `1.0` and clamped to
`0.25 ..= 4.0`. The kernel scales `CrowdParams::weights` per agent:

```text
effective[i] = configured[i] * bias[i]
```

Multiplicative, not absolute, for one reason: a game tunes `SteeringParams` once and every
personality is expressed relative to that tuning. Absolute per-person weights would silently
override a game's `separation = 2.5` the moment a temperament arrived, and the number in the
documentation would stop meaning what it said.

### The buffer is short, and being short is the fast path

`Crowd` holds `bias: Vec<[f32; 5]>`, cleared in `begin()`. It grows only as far as the highest
biased agent index, so a crowd nobody gave a personality to carries an empty vector and pays one
`Option::get` per agent. The resolve happens once per agent per frame, in `resolve_weights`,
shared by all three roads — a bias applied on one road and not another would be two different
crowds depending on a hardware decision.

`set_bias` clamps NaN to neutral and everything else into the band. It is a per-frame setter on a
path a game drives from data it loaded, and a crowd that freezes on one bad float is worse than a
crowd that steers oddly for a tick.

### Needs decay in bursts, and the interval is a config field

`Needs::decayed(temperament, ticks)` is linear in ticks, so decaying eight ticks in one pass is the
same arithmetic as eight single-tick passes to within float rounding. `BoidsConfig::needs` is
`Option<NeedsDecay>`, defaulting to eight ticks.

Needs decay is **on by default**, which is a behaviour change. The alternative — off by default —
leaves the drive reading a constant, and every NPC reads as content forever, which is exactly the
state this ADR exists to end. A game with its own economy turns it off with
`BoidsConfig::with_needs_decay(None)` and writes `NpcNeeds` itself.

`Needs::pressure()` and `Needs::steering_bias(influence)` are the seam: an unmet need raises gain,
so a cold, hungry or lonely NPC jostles more and clusters harder regardless of its temperament. The
two are the same shape as the temperament bias and combine by multiplication, clamped once.

### The archetype is a preset the individual deviates from

`Temperament::blend(preset, individual, share)` is now how both spawn paths build a personality:
two thirds the archetype's preset, one third a fresh sample (`share = 0.34`), with the individual's
high-neuroticism swing spreading the result so an archetype is a tendency rather than a stamp.
`drain_spawn_queue` draws the individual first and the archetype second, in the same order as
before, so a seed that produced a given person still draws the same numbers; only the blend is new.

## Measured behaviour

Bias the drive asks for, per archetype, at the shipped `TemperamentDrive::default()`:

| Archetype | Separation | Alignment | Cohesion | Arrive | Reads as |
|---|---|---|---|---|---|
| Guard | 0.918 | **1.164** | 1.118 | 1.231 | Holds rank, matches the group's heading, arrives briskly |
| Merchant | 0.991 | 1.061 | **1.237** | 1.000 | Clusters: a stall is a place to stand |
| Urchin | **1.062** | 0.918 | 1.122 | 0.868 | Scatters, does not follow, darts |
| Noble | 0.933 | 1.089 | 1.127 | 1.198 | Unhurried, keeps to its own company |
| Wanderer | 1.056 | 1.003 | 1.051 | 1.000 | Loose, indifferent to the group |

The direction surprised the plan: a guard does not push harder, it jostles *less* and follows the
crowd's heading more. Discipline is formation-keeping, and the table is what the demo's
`ashfall.run.json` reports for 2,000 NPCs over 600 ticks — guards arrive with the highest alignment
(1.164) and the lowest separation (0.918), urchins the reverse, and mean `company` falls from 1.0 to
0.74–0.77 as needs decay.

## Cost

`crowd_temperament` in `benches/crowd.rs`, 10,000 agents, volumetric fixture, release:

| Case | Median |
|---|---|
| uniform weights | 1.8766 ms |
| every agent biased | 1.8772 ms |

Within noise (0.03%), against a 1.88 ms frame: one slice lookup and five multiplies per agent. The
zero-allocation claim survives it — `tests/crowd_no_alloc.rs` runs a frame with every agent biased
and asserts zero allocations after warm-up.

## Consequences

- A crowd of individuals costs what a crowd of clones costs, so personalising the near ring is a
  design decision rather than a budget one.
- The drive is `Option`-shaped at three levels (config, component, buffer) and each level degrades
  to the configured behaviour, so the feature can be turned off without a rebuild flag. It is
  deliberately *not* a Cargo feature: it adds no dependency, no acceleration structure and no
  allocation, which is the test this crate uses for gating.
- `steer_flock_reference` applies the same bias, so the parity tests keep comparing one simulation
  to itself.
- Spawned temperaments changed for a given seed (the blend is new). Digest-level determinism is
  unaffected; per-agent personality is not reproducible against a pre-0051 build, which is why this
  is recorded as a behaviour change in the changelog.
- The ambient packed road (`application::packed`) steers uniform weights: it exists to give an
  unbounded population a cheap heading, and per-agent personality is a manifested-tier luxury until
  something measures otherwise. Stated here so nobody assumes otherwise from the default.
