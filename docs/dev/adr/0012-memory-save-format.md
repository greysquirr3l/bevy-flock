# 0012, Relationships persist as ages in a versioned binary record

- Status: Accepted
- Date: 2026-11-04
- Relates to: ADR 0010 (social memory), ADR 0011 (memory in the scheduler)
- Superseded in part by: ADR 0013, which replaces the version 1 layout with a
  sectioned container and adds the subject registry this record left as an open
  question. Version 1 files remain readable.

## Context

A grudge that does not survive a save is a mood. ADR 0010 built a store that is
cheap enough to keep and explicitly left retention to the caller: drop it, park it
in the archive, or serialise it. This record fixes what "serialise it" means, and
the four questions that a naive answer gets wrong.

1. **Ticks are session-local.** `MemorySlot` stores `first_seen` and `last_seen` as
   `u32` stamps of the simulation clock. A save is loaded into a session whose
   clock starts at zero, or continues from somewhere else, or is a replay at a
   different speed. Storing absolute stamps means every load silently re-dates
   every relationship.
2. **Identities outlive indices.** `MemoryIndex` is a row handle, valid only for
   the store that issued it. A save must carry `NpcId` and `SubjectId` and let the
   loader re-enrol, or a reloaded world remembers the wrong people.
3. **Derived state must not be trusted.** Free lists, eligibility, tiers, tags and
   weights are all recomputed. A format that stores them invites a loader that
   believes a hostile file.
4. **Density is not a property of the store.** Most NPCs have met nobody, and most
   who have met somebody have met one. A format that writes every slot for every
   row spends 8× the bytes on zeros.

## Decision

**A little-endian binary format, one version, records sorted by identity, times
stored as ages.**

Format version 1, magic `BFLK`. All integers little-endian, fixed width, no
padding, no alignment requirements on the reader.

### Header, 24 bytes

| Offset | Width | Field |
| --- | --- | --- |
| 0 | 4 | magic `BFLK` |
| 4 | 2 | format version, `1` |
| 6 | 2 | flags: bit 0 = times are ages (always set in v1), rest must be zero |
| 8 | 4 | decay half-life in ticks, `0` meaning memory never fades |
| 12 | 2 | subjects the writer knew about (the reader's store must support at least this many) |
| 14 | 4 | record count |
| 18 | 2 | reserved, zero on write, ignored on read |
| 20 | 4 | CRC32 of bytes 0..20 |

### Records, `7 + 18 × slot_count` bytes each, ascending `npc`

| Width | Field |
| --- | --- |
| 4 | `npc` identity |
| 1 | origin: `0` hot store, `1` archive |
| 2 | `slot_count` |
| 2 × n | subject identities, ascending |
| 16 × n | slot: familiarity `u16`, valence `i16`, encounters `u16`, flags `u16`, first age `u32`, last age `u32` |

Only non-empty slots are written. A record with one slot costs 25 bytes; the same
record in a dense format costs 8 subjects × 16 bytes plus a fixed header.

### Trailer, 8 bytes

| Width | Field |
| --- | --- |
| 4 | CRC32 of every byte from offset 0 through the last record byte |
| 4 | reserved, zero |

### Times are ages

`age = saved_at_tick − stamp`, wrapped as `u32`. Restoring at `now` writes
`stamp = now − age`. So a relationship saved at 100 ticks old reloads at 100 ticks
old, whatever the new clock says, and a save/load round trip is lossless by
construction, the ages are the same integers before and after.

**Decay is frozen across sessions by default.** A grudge does not fade because the
player went to bed; the world is paused. A game that wants the world to have moved
on calls `rebase_offline(elapsed_ticks)` on load, which is one addition and a
deliberate act rather than an emergent consequence of how the file was written.

### What is not in the file

- Memory indices, free lists, enrolment, tiers, weights, tags: derived, recomputed
  on load, never trusted.
- Names, archetypes, dialogue, promoted relationship prose: content, owned by the
  game, and not the store's business (ADR 0010's promotion split).
- Ambient per-cell aggregates: a different tier with a different shape, and a
  future section rather than a field smuggled into v1.

### Loader obligations

1. Verify the header CRC before parsing anything else.
2. Refuse an unknown version with a typed error naming it. Never guess.
3. Refuse a record count or slot count that cannot fit in the remaining bytes,
   before allocating.
4. Refuse duplicate identities, out-of-range subjects, and a store too small to
   hold the records, all as typed errors, all without partial mutation.
5. Verify the trailer CRC after parsing, so a truncated file cannot be mistaken for
   a short one.

The crate produces and consumes bytes. It does not open files: durability, write
to a temporary path, fsync, rename, is the game's job, and a simulation crate that
does file I/O has stopped being a simulation crate.

## Why not the obvious formats

- **`bincode` / `postcard` over the structs.** The wire format becomes the Rust
  field order, so a field reorder or a `#[non_exhaustive]` addition is a silent
  breaking change, and both have a history of breaking-version churn. A save file
  outlives a refactor; the format must be independent of the layout.
- **RON or JSON as the shipping format.** A 10,000-record save is roughly 900 KB of
  text against 430 KB of binary, parsed at save/load time on the main thread, and
  it invites floats into a format whose whole point is integers. A text form is
  still useful for inspection and mods, so one is planned behind `serde` with a
  golden test that both decode to the same store, but it is not the shipping path.
- **SQLite or an embedded database.** A dependency, a file format we do not
  control, and a query engine for a table with one index.
- **One file per NPC.** Ten thousand files, ten thousand open/close pairs, and a
  directory that cannot be atomically replaced.

## Consequences

- **430,032 bytes for 10,000 NPCs with two relationships each; 790 KB at four.**
  ADR 0013's version 2 is 430,094 bytes for the same world, plus a registry. A game
  with a tighter budget gzips the bytes, which compresses well because slot fields
  are mostly zero and repeat.
- **Save time is a function of records, not population.** NPCs who have met nobody
  cost nothing to save; a crowd of hermits saves to 32 bytes.
- **Version 1 must be loadable forever.** That is a promise with a cost: additive
  changes go in new sections with the reserved bits, and anything that reinterprets
  an existing field is version 2 with a documented migration rather than a new
  meaning for an old byte.
- **`u32` ages wrap after 828 days of simulated time**, the same ceiling ADR 0010
  already carries for stamps. A save written by a very old world has already lost
  that precision.
- **Frozen decay is a design position, not a default nobody noticed.** A game that
  wants "the world moved on" opts in, and the ADR says so where the option is
  invented rather than in a changelog.
- **No integrity against tampering.** CRC32 catches corruption, not a player who
  wants a friendlier world. Saves are moddable by design; a game that cares signs
  the bytes itself.

## Open questions

- Whether promoted relationships need a reference in the file (a stable id pointing
  at authored content) or whether the game correlates them by `(npc, subject)` on
  its own side. Current intent: correlate, because the store does not know what a
  name is.
- Whether `SubjectId` should carry a registry table in v1 so a save is
  self-describing when the game's subject list changes. Current intent: no, the
  loader validates against the store's subject count and the game owns the mapping.
