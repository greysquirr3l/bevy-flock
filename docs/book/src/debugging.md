# When it goes wrong

A crowd bug has four usual shapes: somebody did something inexplicable, everybody did the
same thing at once, the frame spiked, or the save came back wrong. Each has a tool.

## "Why did that NPC do that?"

One call, one block of text:

```rust
use bevy_flock::prelude::*;

let why = explain_npc(app.world_mut(), entity)?;
println!("{why}");
# Ok::<(), Box<dyn std::error::Error>>(())
```

It prints the entity's identity, tier and think weight, its tree's shape and where the
cursor is, mood, needs, group, every blackboard slot sorted by key, and the memory row if
there is one. That is usually enough to see the whole causal chain: the branch that ran, the
slot that made it run, and the mood that made the slot what it was.

It allocates and takes `&mut World`, which is why nothing calls it per frame. Bind it to a key
and call it for the NPC under the cursor.

## "Why is the crowd doing that?"

`ThinkStats` is reported every tick:

```rust
let stats = world.resource::<ThinkSchedule>().stats();
println!(
    "enrolled {} chosen {} spent {:?} promoted {} beyond capacity {}",
    stats.enrolled, stats.chosen, stats.spent, stats.promoted, stats.beyond_capacity
);
for tier in [CognitionTier::Reflex, CognitionTier::Reactive, CognitionTier::Deliberative, CognitionTier::Social] {
    let stat = stats.tier(tier);
    println!("{tier:?}: {} chosen / {} enrolled", stat.chosen, stat.enrolled);
}
```

Read them in this order:

| Symptom | Field | Usual cause |
| --- | --- | --- |
| Everyone reacts at once | `promoted` pinned at the limit | promotion rate limiting is doing its job; the scene promoted too many at once |
| Nobody near the player thinks | `chosen` much lower than `enrolled` | budget too small, or the tier's share is being spent on cheap NPCs |
| Some NPCs never think | `beyond_capacity` above zero | population exceeds the schedule's capacity; rebuild it with the real size |
| A frame spikes when a crowd appears | `spent` near the budget | expected: the spike is bounded by design, not eliminated |

The overlay is the spatial view of the same data:

```rust
use bevy_flock::prelude::*;

app.insert_resource(DebugOverlayConfig { enabled: true, max_labels: 32, radius: 40.0 });
// `collect_debug_overlay` fills `DebugOverlay`; your renderer draws the labels.
```

Disabled is the default, and disabled costs one branch. The crate draws nothing: a crowd
library with a gizmo dependency is a crowd library that cannot be used on a server.

## "The frame spiked"

1. **Check `spent` first.** If the scheduler is inside its budget, the spike is not cognition.
2. **Check the spawn budget.** A `with_spawn_budget` that is too large makes frame one pay for
   the whole crowd; the default spreads it.
3. **Check the grid volume.** The rebuild is a fixed cost sized from the configured volume:
   a 1 km box is ~1.0 ms per frame regardless of population, a 200 m box ~20 µs.
4. **Profile.** `docs/dev/tuning/performance.md` records the flamegraph method and what it found: the
   steering kernel, then the neighbour walk, then the write pattern.

## "It is not deterministic"

The digest test (`tests/determinism.rs`) hashes the whole world after N ticks; if it fails,
the cause is one of:

- **Something read the clock.** `Instant::now` in a leaf, a system or a provider.
- **Something iterated a `HashMap`.** Iteration order of a hash map is not a contract.
- **Something used a global RNG.** `rand::thread_rng` is not seeded by the world.
- **Something depended on spawn order** without using the seeded stream, an `Entity` id is
  not stable across runs, an `NpcId` is.
- **Something used `f32` transcendentals** compiled differently; the crate uses `libm` for
  exactly this reason.

## "The save is wrong"

Two layers answer this:

- **`MemorySnapshot::from_bytes` fails closed.** A truncation, a bad checksum, a lying
  version: all are errors, and `tests/fuzz_decoders.rs` asserts that no truncation of a valid
  file is ever accepted. If a save loads and is wrong, the bytes are wrong, not the parser.
- **`LoadReport` says what happened.** `dropped_slots` means subjects disappeared from the
  registry; `translated` means the registry moved and identifiers were remapped;
  `unverified_mapping` means a version 1 file's subjects were taken on trust.

`examples/registry_telemetry.rs` renders the registry's fragmentation and orphan counts as an
SVG, which is how you see a save format drifting over a long play session.

## "The gates"

The crate's own correctness claims are machine-checked, and the same harnesses are usable as
a template:

| Gate | Where |
| --- | --- |
| Boundary continuity, pop-in, anchor exemption | `tests/band_continuity.rs` (10,000 ticks, no allocation) |
| Manifest/fold churn | `tests/band_churn.rs` |
| No allocation in a tree tick | `tests/tree_no_alloc.rs` |
| No allocation in the crowd kernel | `tests/crowd_no_alloc.rs` |
| Determinism digest | `tests/determinism.rs` |
| Decoder robustness | `tests/fuzz_decoders.rs`, `fuzz/` |
| Performance regression | `scripts/scaling_regression.py` (CI job) |
| Cell-size decision still valid | `scripts/cell_size_gate.py` (CI job) |

When you change something and a gate fails, the gate is usually right. The ADRs record why
each one exists, which is the difference between a failing test and a mystery.

## A last resort

`Diagnostics` runs invariants over the world and returns violations rather than panicking:
duplicate identities, an NPC without a tier, a mood outside its range. It is the cheapest way
to answer "is the world sane before I blame the AI", and it is what `explain_npc` is built on.
