# 0013 — Subjects are matched by stable key, and the file says which keys it used

- Status: Accepted
- Date: 2026-11-04
- Supersedes in part: ADR 0012 (the version 1 layout and its size figure)
- Relates to: ADR 0010 (social memory), ADR 0011 (memory in the scheduler)

## Context

ADR 0012 wrote a version 1 file that stored `SubjectId` — a dense `u16` that means
"position in the game's subject list". The record ends with an open question that
should have been a blocker:

> A save validates against the store's subject count, so if a game reorders its
> subject list between versions, old saves silently attribute memories to the wrong
> subjects.

That is the worst failure mode a save format can have. It is silent, it is
plausible (every NPC still remembers *someone*), and it corrupts the one thing
this feature exists to produce. A guard who hated the captain now hates the
player's new companion, at full strength, with a five-hundred-tick history
attached, and nothing in the logs says a word. The player experiences it as the
game being broken in a way they cannot describe.

Content changes are normal: a companion is added, a rival is cut in a patch, a
cast list is reordered to match a script, a mod adds three characters. Every one
of those shifts positions. A format that identifies subjects by position cannot
survive the first content update, and "do not reorder your cast" is not a
constraint a library can impose on a game.

Version 1 also had a structural problem. Its layout is positional — header, then
records, then trailer — so there is nowhere to put a table without reinterpreting
existing bytes. Adding the registry is not an additive change; it is a new
container.

## Decision

**Version 2: a sectioned container, and a registry section that carries a stable
key for every subject.**

Two changes, in order of importance: what identifies a subject, and where the
answer lives in the file.

### Subjects are identified by key, never by position

```rust
SubjectKey(u64)      // stable, chosen by the game, never reused
SubjectKind          // Player, Npc, Other
SubjectEntry { key, kind, label }
SubjectRegistry      // dense: position *is* the SubjectId
```

The game assigns keys from something that already outlives a save — an authored
character id, a row id in a content table, a persistent entity id. The registry
is dense because runtime lookups need it to be, and the file carries the keys
because density is exactly what does not survive an edit.

`label` is for humans and mods. It is never used to match, because names are the
least stable thing in a game: they change with localisation, with a rewrite, with
a typo fix. A format that matched on labels would break on a translation patch.

### The file is a section table, so it can grow without another version

```text
header (24)     magic BFLK, version 2, flags, half-life, subjects, records,
                section count, header CRC32
table           count × 16 bytes: id u16, reserved u16, offset u32, len u32, CRC32
section 1       subject registry
section 2       relationship records (unchanged encoding from v1)
trailer (8)     CRC32 over the header and table, reserved
```

Rules that make it a container rather than a bag of bytes:

- Section identifiers are ascending and unique. Anything else is `NotCanonical`
  or `DuplicateSection`, not something to interpret generously.
- Every payload must lie between the end of the table and the start of the
  trailer; a table entry that points elsewhere is `SectionOutOfBounds`.
- Each section carries its own CRC32, so a corrupt file can be reported by *which*
  part is corrupt rather than "this save is bad".
- The trailer checksums the header and the table, not the payloads. Amended after
  measurement (see below): hashing two megabytes twice cost 6.7 ms of a 14.9 ms
  encode at 50,000 NPCs, and the payloads were already covered.
- **Unknown sections are skipped.** A newer writer may add one; refusing to read a
  save because it contains something this build does not use would make the format
  impossible to grow, which is the mistake version 1 made in the first place.

### The fingerprint is a fast path, not the identity check

The registry section header carries a 32-bit FNV-1a fingerprint over every
`(key, kind)` pair in identifier order. If the current registry's fingerprint
matches the file's, positions were identical when the file was written and the
loader skips translation entirely — no per-subject work on the common path.

The fingerprint covers keys and kinds, deliberately not labels: relabelling a
subject is free, re-keying one is a mapping change. It is a checksum of intent,
not of content, and the decoder recomputes it to catch a hand-edited key.

### Translation is by key, and drops are counted

```text
stored subject --key--> current registry --found?--> current SubjectId
                                        `--missing--> slot dropped, counted
```

[`LoadReport`] carries `dropped_slots`, `translated` and `unverified_mapping`, so
a load that lost a relationship says so. A record whose every slot is dropped is
not written at all, because allocating a row to remember nobody is how a bounded
store fills up with ghosts.

The lookup is a linear scan per subject. Subject counts are tens, not thousands;
a game with thousands builds a map once and passes a registry it already indexed.

### Version 1 files are refused unless the caller says otherwise

`LegacySubjectPolicy` has two variants and the default is the safe one:

| Policy | Behaviour |
| --- | --- |
| `Refuse` (default) | `SaveError::MissingSubjectRegistry` |
| `AssumeDense` | Loads, trusting positions, and reports `unverified_mapping: true` |

A version 1 file cannot be verified — it has no keys — so the decision belongs to
the game, is explicit at the call site, and is visible in the report. Silently
assuming the old behaviour would preserve exactly the bug this record exists to
fix.

## What it costs

`benches/saves.rs`, 50,000 NPCs with two relationships each:

| | version 1 | version 2, whole-file trailer | version 2, table trailer |
| --- | --- | --- | --- |
| bytes | 2,150,032 | 2,150,172 | 2,150,172 |
| encode | 8.27 ms | 14.87 ms | 8.67 ms |
| decode | 11.50 ms | 18.84 ms | 11.93 ms |

The container is effectively free: 0.4 ms of encode and 0.4 ms of decode over
version 1, against 140 extra bytes. The checksum choice was not free, which is why
it changed.

| | Bytes |
| --- | --- |
| Version 1, 10,000 NPCs × 2 relationships | 430,032 |
| Version 2, same, plus a two-subject registry | 430,094 |
| Version 2, same, plus a twenty-subject registry with 16-byte labels | 430,540 |

The registry is a rounding error: 30 bytes for two subjects, roughly 500 for
twenty named ones, on a file whose bulk is relationship data. A test pins the
430,094 figure so the claim cannot rot.

The section table costs 32 bytes for two sections and 16 per section thereafter,
which is the price of never needing version 3 for an additive change.

## Consequences

- **A content patch no longer corrupts saves.** Adding, removing or reordering
  subjects produces a translation instead of a mis-attribution, and the report says
  how many relationships were lost with the subjects that went away.
- **Games must choose stable keys and keep them.** This is the one new obligation,
  and it cannot be checked by the library: reuse a key for a different character and
  the save will faithfully hand one character's history to another. The doc comment
  on `SubjectKey` says so where the game will read it.
- **A dropped subject is a dropped relationship.** There is no repair: a memory of
  someone who no longer exists has nowhere to go. The alternative — keeping it under
  a tombstone id — preserves data for content the game cannot display.
- **Version 1 files still load, under a named policy.** The promise in ADR 0012
  survives: the old reader stays, and the new writer never emits it again.
- **The format now has a growth story.** Promoted relationships, ambient cell
  aggregates and any future section can be added without a version bump, and an
  older build will skip them rather than refuse the file.
- **Saves are larger by an amount nobody will notice, and more self-describing by
  an amount a developer will.** A save opened in a hex editor now says "the player",
  "the captain" next to the keys.

## Alternatives rejected

- **Match on labels.** Breaks on localisation, on a rename, on a typo fix. Labels
  are for eyes.
- **Store only a hash of the subject list.** Detects that the list changed but
  cannot repair anything; the load either fails or guesses, which is where we
  started.
- **Keep positions and add a "content version" the game must not change.** Moves
  the invariant from the format to a discipline, and disciplines fail silently.
- **Refuse any registry mismatch.** Correct but hostile: every content patch would
  invalidate saves, so games would ship `AssumeDense` and we would be back to silent
  mis-attribution with extra steps.
- **A per-game migration table in the save.** Puts format knowledge in every game
  and makes a save file an executable translation program. The registry is the same
  information expressed as data.
- **Keep the positional layout and append the registry after the records.** The
  reader cannot find it without a table, and "the registry is always the last thing
  in the file" is an invariant that breaks the first time two sections are added.
- **`bincode`/`serde` for the container.** Still ties the wire format to struct
  layout, which is what ADR 0012 rejected and this record does not revisit.

## Open questions

- Whether promoted relationships store a `SubjectKey` reference of their own or
  rely on the game correlating `(npc, subject)` as ADR 0012 proposed. Current
  intent: the game correlates, and the registry gives it the key to correlate *with*.
- Whether the registry should carry a game-supplied content version so a save can
  name the patch it was written under, for support tickets. Cheap to add as a new
  section; not in v2, because a version number that nothing validates is decoration.
