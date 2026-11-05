# Memory

Memory is opt-in (`features = ["memory"]`) because it is the one part of the crate that is
a *resource* rather than arithmetic: rows, slots and a subject ceiling. A crowd of ten
thousand can carry it; a million should not.

## The shape of the thing

```text
MemoryStore ──┬── row per NPC (fixed capacity)  ──▶ slot per remembered subject
              └── MemoryArchive (cold, unbounded, sparse)
                        │
                        ▼
            MemorySnapshot::capture  ──▶  bytes  ──▶  from_bytes ──▶  restore
```

- **`MemoryStore`** is a flat slot array: `capacity` rows × `MAX_SUBJECTS` (64) slots, with a
  free list for rows. Sweeping it is a linear scan of a contiguous array, which is the only
  reason a million-tick simulation stays affordable (ADR 0016).
- **`MemoryArchive`** is the cold tier: rows for NPCs nobody is near, sparse and cheap, so a
  city can remember a player it will not see for an hour.
- **Decay is lazy.** A slot stores a stamp; the value you read is decayed to *now* on the way
  out. Nothing walks the store to age it, and no tick pays for a crowd's memory.

## What gets remembered

`MemoryStore::remember(row, subject, encounter)` folds an encounter into a slot:

```rust
use bevy_flock::prelude::*;

let mut store = MemoryStore::new(1_000, 64);
let row = store.enrol(NpcId::unique(7)).expect("a row");
// Encounter kind and tick; the valence is what the encounter did to the relationship.
let encounter = Encounter::new(EncounterKind::Spoke, tick).with_valence(180);
store.remember(row, SubjectId::new(3), &encounter);
// Recall is raw; `strongest` is the decayed view a tree reads.
let raw = store.recall(row, SubjectId::new(3));
```

`EncounterKind` covers the events a crowd actually has: `Saw`, `Spoke`, `Traded`, `Helped`,
`Insulted`, `Attacked`, `Robbed` and `Witnessed`. Each carries a flag bit, and the flags survive
decay longer than the valence: a grudge is an event, not a feeling.

## Rows are a resource, spent nearest-first

Nothing enrols a thousand NPCs because the player walked into a square. Enrolment is a
budget:

```rust
let config = MemoryConfig::default()
    .with_publish_radius(30.0)  // who is close enough to remember you
    .with_enrol_budget(64);     // how many new rows per tick
```

`domain::memory::select_enrolment` implements the rule as a pure function — nearest first,
ties broken by identity — and the ECS bridge uses the same comparator, so a replay of the
same approach grants the same rows.

## Reaching a behaviour tree

A tree asks "do I know you?" several times a tick, from arbitrary branches. Handing it a
store handle would make ticks `O(subjects)` and would teach every leaf about decay. Instead
the bridge publishes the strongest memory as three integers, and the leaves read them:

| Slot | Meaning |
| --- | --- |
| `KEY_MEMORY_VALENCE` | how the subject feels, signed |
| `KEY_MEMORY_FAMILIARITY` | how well they are known, decayed |
| `KEY_MEMORY_FLAGS` | encounter-kind bits |

```rust
use bevy_flock::prelude::*;

// Registered once; the three leaves then behave like any other.
let memory = MemoryLeaves::register(&mut registry)?;
let tree = selector(vec![
    action(memory.greet_friend),   // succeeds for a welcome face, opens a conversation
    action(memory.shun_grudge),    // succeeds for someone who hurt them
    action(memory.recognise),      // succeeds for anyone familiar
]);
```

Three rules make the view safe to read blindly:

1. **Familiarity gates it.** A valence without familiarity is not a memory, so a decayed
   grudge cannot make an NPC hostile to strangers.
2. **Flags outrank intensity.** `Attacked` and `Robbed` survive a decay that softened the
   number.
3. **Absence means stranger.** No slots is `None`; "never met" and "indifferent" stay
   different answers.

The leaves are **not** feature-gated. With `memory` off they always answer `Failure`, so a
game authors one set of trees and turns memory on underneath them.

## When the player leaves

The bridge clears the three slots and leaves the row alone. A view is a claim about the
present; a row is a claim about the past. Conflating them produces a shopkeeper who greets a
customer who left ten minutes ago.

`MemoryBridgeStats` reports what happened:

| Field | Meaning |
| --- | --- |
| `enrolled` | rows created this tick |
| `published` | views written this tick |
| `cleared` | views removed because the player left |
| `budget_bound` | the store was full; the crowd outgrew its memory |

`budget_bound` is the number to watch: it means the crowd is bigger than the memory budget,
and the fix is a bigger store or a closer publish radius, not an eviction policy that
deletes the relationship the player spent longest building.

## Saving it

`MemorySnapshot::capture(store, archive, now, half_life, registry)` produces bytes; the
format is versioned, carries a subject registry (ADR 0013) so attributes cannot silently
shift meaning, and ends with a checksum over the section table. `restore` reports what it
could not do rather than failing silently:

- `store_records`, `archive_records`, `slots` — what came back;
- `dropped_slots` — slots whose subject is gone from the current registry;
- `translated` — the registry moved, so identifiers were remapped;
- `unverified_mapping` — a version 1 file, whose subjects were taken on trust.

`docs/adr/0012` to `0014` and `docs/memory.md` carry the details, including the measured
encoding and decoding costs at 50,000 rows.

## The honest limits

- **64 subjects per NPC.** A crowd member remembers a player, a few companions, a rival and
  the shopkeeper. Not a thousand players. `MAX_SUBJECTS` is a compile-time constant because
  the slot array is flat (ADR 0016 shows the footprint at 16 and 64).
- **One strongest subject at the tree boundary.** "Which of these three players robbed me"
  needs the store directly, and the `O(subjects)` cost that comes with it.
- **No transcript.** The view is a summary. Promises, quests and exact dialogue belong in the
  game's own state, read alongside the summary for tone.
