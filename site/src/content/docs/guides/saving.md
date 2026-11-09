---
title: Saving a crowd
description: The format, the four rules that keep it loadable, and what a save does not contain.
---

A crowd is state: positions, velocities, needs, temperaments, memory rows, the clock. The `serde` feature serialises it,
and the format carries a version so an old save is either loaded correctly or rejected loudly.

```toml
[dependencies]
bevy-flock = { version = "0.1", features = ["serde"] }
```

## What is in a save

```text
version         format version (2)
tick            the tick the save was taken at
seed            material the run started from
npcs[]          id, transform, velocity, needs, temperament
memory[]        subject rows, only with the memory feature
registry        subject id to row mapping
```

The registry is why v2 exists. v1 stored attributes positionally, so adding one shifted the meaning of every row that
followed it, silently. v2 keys by subject, and an unknown subject is an error rather than a misread value.

## Four rules

1. **Treat the format as an API.** Adding a field is compatible. Reordering or repurposing one is a version bump.
2. **Keep the seed.** A save plus its seed can reproduce a run. A save without it can only resume one.
3. **Never save handles.** Entity ids are not stable across sessions. The subject registry is what lets memory survive a
   reload.
4. **Load an old fixture in CI.** The compatibility suite loads a file written by the previous release; that test is the
   only thing standing between you and a silent migration bug.

## When a load is refused

A refused load is the feature working. The crate returns an error naming the section and the version rather than
partially applying a save, because a half-loaded crowd is worse than a failed load: it looks like a simulation bug for
the next three hours.

Design references: `docs/dev/adr/0012-*` (format), `0013-*` (registry), `0014-*` (compatibility framework).
