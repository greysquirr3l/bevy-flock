# 0010 — Social memory as a bounded, integer, two-tier store

Status: Accepted (amended by ADR 0016: the subject ceiling is sixty-four, not sixteen)

## Context

An NPC that remembers the player is the difference between a crowd and a world.
It is also the fastest way to destroy every number this crate has published: a
`Vec` of grudges per NPC turns ten thousand agents into ten thousand allocations,
and a million agents into a million pointers to nothing.

The request was specific about the shape of the risk: this should not be something
"every random hundred million NPCs" does, it should be something that gets
elevated to the NPCs near enough to care, and it should be feature-gated.

## Decision

Memory is an opt-in, dependency-free `memory` feature holding two tiers and no
simulation loop of its own.

**Hot tier: [`MemoryStore`].** One flat array of 16-byte slots, indexed
`memory_index × subjects + subject`. One allocation for the whole crowd, iteration
in address order, and a `fill` to wipe it. There is no map from `NpcId` to a row:
the row handle (`MemoryIndex`) lives on the entity, the way `NpcId` does, because a
map lookup per remembered NPC per frame is exactly the cost this crate exists to
avoid.

**Cold tier: [`MemoryArchive`].** A bounded `BTreeMap<NpcId, NpcMemories>` for
NPCs that left the hot tier. Bounded because an archive that grows with the
population is the same bug wearing a different hat; `BTreeMap` because eviction
has to be deterministic and eviction order is decided by iteration order. It
evicts the lowest total familiarity, breaking ties by lowest `NpcId`.

**Subjects are dense identifiers, not players.** `SubjectId` covers a player, a
pet, or another NPC. Guards remembering each other is the same mechanism, and it is
the more interesting feature: a crowd that holds grudges against itself does more
for a world than one that only notices the protagonist.

**Decay is lazy and integer.** Intensity is stored at the moment of the encounter
and halved when read, from `now - last_seen`. Nothing walks ten thousand records
per frame, and no float enters a decision a replay has to reproduce. `DecayPolicy`
carries a half-life in ticks; `DecayPolicy::NONE` makes a grudge permanent.

**Retention is the caller's policy.** `MemoryStore::release` returns a
`NpcMemories`; the game drops it (memory dies with eligibility), parks it in the
archive (memory survives), or writes it to a save file (memory is world state).
Three strategies, two primitives, no policy in the library.

**Promotion is where the value is.** The best-known implementation of this idea is
not a database, it is a promotion: a few relationships get names, scars, and
something to say.
Storage is the cheap part. The plan's `memory-in-cognition` task wires the scalar
tier into trees and utility scoring; the authored tier belongs with dialogue,
because an antagonist with nothing to say is a stat.

## Consequences

Measured on 16,384 eligible NPCs with four subjects (`benches/memory.rs`):

| Operation | Cost | Per NPC |
| --- | --- | --- |
| `remember` (one encounter each) | 125.6 µs | 7.7 ns |
| `recall` | 90.1 µs | 5.5 ns |
| `recall_decayed` | 139.9 µs | 8.5 ns |
| `sweep`, nothing faded | 113.7 µs | 6.9 ns |
| `sweep`, everything reclaimed | 288.4 µs | 17.6 ns |

16,384 NPCs × 8 subjects costs 2 MB. Recalling a memory in a think costs 8.5 ns,
against a guard think measured at 45 ns, so memory roughly a fifth of a think —
affordable at ten thousand NPCs, which is the only place it is asked for.

- **Sweeping is not a per-frame operation.** 288 µs is 1.7% of a frame, which is
  too much to pay every frame for housekeeping. It belongs on a slow cadence, and
  the plugin contract has to say so rather than leaving it to be discovered.
- **A tick stamp is 32 bits.** At 60 Hz a `u32` wraps after 828 days of simulated
  time. A wrapped age reads as very old, which decays memory to nothing — the safe
  direction, but it means a world that runs for years needs the stamp widened.
- **Recycled identity and memory are in tension.** A row keyed by a recycled slot
  would hand one NPC's grudge to whoever inherits the slot. Memory therefore
  requires unique identities, or a generation counter in the key. This is a
  documented constraint on the plugin, not a footnote.
- **The ambient tier is absent here on purpose.** A million inhabitants cannot have
  individual memories; a district that remembers the player is a per-cell scalar
  and belongs to the continuum tier. Storing per-individual memory at that scale
  would be the exact failure this ADR is written to prevent.

## Alternatives rejected

- **`Vec<Memory>` per NPC.** One allocation per NPC, a pointer chase per lookup, a
  cache miss per stranger. At 10k NPCs it is 10k allocations; the flat array is
  one. Rejected on measurement grounds before it was written.
- **`HashMap<(NpcId, SubjectId), Memory>`.** Hashing in the read path, iteration
  order that decides eviction, and no way to iterate a crowd's memory in a stable
  order.
- **Per-frame decay loop.** Turns an `O(1)` read into an `O(N)` write, and makes
  the frame cost scale with the number of memories rather than the number of
  thinkers.
- **Floats for valence and decay.** Replay breaks on the last bit. Integers with a
  shift-based curve are deterministic by construction, and the staircase is not
  visible in a value that a behaviour tree threshold compares against.
- **Embeddings / a vector index.** Buys "this one reminds me of that one" at the
  cost of a dependency, a budget of floats, and inference that must be bit-identical
  across platforms to stay replayable. Social memory is a small relationship table
  plus promotion; it does not need semantic similarity, and pretending it does
  would put a model in the simulation loop.
- **Memory in components.** A component per memory is a component explosion at
  100k NPCs, cannot be compacted or snapshotted cheaply, and makes the archetype
  layout depend on how many people an NPC has met.

## Integration

How memory's cost and weight reach the scheduler is fixed by ADR 0011: leaves
declare what a store read costs, relationships raise weight within a clamp, and
eligibility follows the scheduler rather than the store.

Persistence is fixed by ADR 0012: ages rather than stamps, identities rather
than indices, a versioned little-endian record with per-section CRC32.

## Open questions

- Whether the authored (promoted) tier belongs in this crate or in the dialogue
  layer. Current intent: this crate stores the scalar, dialogue owns the words.
- Whether a generation counter should be part of `MemoryIndex` now, before the
  recycled-population path exists and someone builds on the weaker key.
